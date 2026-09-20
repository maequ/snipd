//! Browsable history of every capture ever taken.
//!
//! # The filesystem is the source of truth
//!
//! History is built by scanning the save folder, not by reading a database. A
//! sidecar index at `%APPDATA%\Snipd\history.jsonl` adds what the filesystem
//! cannot know — which mode produced a capture, which window it was — but it is
//! strictly *enrichment*.
//!
//! That ordering is deliberate. An index that owned the truth would mean a
//! corrupted or deleted index loses captures from view even though the files are
//! sitting right there, which is precisely the failure this app exists to
//! prevent. Instead: files with no index entry still appear, with their details
//! derived from the filename and timestamp; index entries whose file has been
//! deleted quietly disappear.
//!
//! # Thumbnails
//!
//! Decoding a few thousand full-resolution PNGs to draw a grid would be
//! unusable. Thumbnails are generated once, cached under `%APPDATA%\Snipd\thumbs`
//! keyed by path and modification time, and served to the webview as ordinary
//! image URLs so the browser can lazily fetch only what scrolls into view.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::capture::{CaptureKind, CaptureRecord};
use crate::config;

/// Extensions treated as captures when scanning the save folder.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg"];

/// Longest edge of a cached thumbnail, in pixels. Comfortably sharp on a
/// high-DPI display at the grid's tile size without costing much to decode.
const THUMBNAIL_EDGE: u32 = 400;

/// One row in the history grid.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub path: String,
    pub file_name: String,
    /// Milliseconds since the Unix epoch, for sorting and date filtering.
    pub taken_at_ms: u64,
    pub bytes: u64,
    /// Present only when the index knew about this file.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub kind: Option<CaptureKind>,
    pub source: Option<String>,
    /// URL the grid points an `<img>` at.
    pub thumbnail_url: String,
    /// Full-resolution image, for the viewer. Same key, different route.
    pub full_url: String,
}

/// Filters applied by the history search bar.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryQuery {
    /// Case-insensitive substring match on the filename.
    pub search: Option<String>,
    /// Inclusive lower bound, milliseconds since the Unix epoch.
    pub from_ms: Option<u64>,
    /// Inclusive upper bound, milliseconds since the Unix epoch.
    pub to_ms: Option<u64>,
    /// How many to skip, for paging.
    pub offset: Option<usize>,
    /// How many to return. Unbounded when absent.
    pub limit: Option<usize>,
}

/// A page of history, plus the total that matched.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    /// Matches before paging, so the UI can say "showing 60 of 2,431".
    pub total: usize,
}

fn index_path() -> PathBuf {
    config::data_directory().join("history.jsonl")
}

fn thumbnail_dir() -> PathBuf {
    config::data_directory().join("thumbs")
}

/// Append a freshly saved capture to the index.
///
/// Failure is deliberately swallowed by the caller: the file is already safely
/// on disk, and a capture that is missing its mode label in history is a far
/// better outcome than a capture that reports itself as failed.
pub fn record(entry: &CaptureRecord) -> Result<(), String> {
    let path = index_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let line = serde_json::to_string(entry).map_err(|e| e.to_string())?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("opening history index: {e}"))?;

    writeln!(file, "{line}").map_err(|e| format!("writing history index: {e}"))?;
    Ok(())
}

/// Read the sidecar index into a lookup keyed by absolute path.
///
/// The file is append-only, so a path can appear more than once if a capture was
/// deleted and a later one reused the name. Later lines win.
///
/// A malformed line is skipped rather than aborting the read: a partially
/// written final line after a crash must not hide the entire history.
fn load_index() -> HashMap<String, CaptureRecord> {
    let mut index = HashMap::new();

    let file = match fs::File::open(index_path()) {
        Ok(file) => file,
        Err(_) => return index,
    };

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<CaptureRecord>(&line) {
            index.insert(normalise(&record.path), record);
        }
    }

    index
}

/// Lowercase the path so lookups are not defeated by drive-letter casing.
fn normalise(path: &str) -> String {
    path.to_lowercase()
}

fn modified_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// List captures, newest first.
pub fn list(query: &HistoryQuery, save_directory: &Path) -> Result<HistoryPage, String> {
    let index = load_index();
    let mut entries = Vec::new();

    let dir = match fs::read_dir(save_directory) {
        Ok(dir) => dir,
        // A save folder that does not exist yet simply means no captures, which
        // is an empty history rather than an error.
        Err(_) => {
            return Ok(HistoryPage {
                entries: Vec::new(),
                total: 0,
            })
        }
    };

    for item in dir.flatten() {
        let path = item.path();
        if !is_image(&path) {
            continue;
        }

        let metadata = match item.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };

        let path_string = path.to_string_lossy().into_owned();
        let record = index.get(&normalise(&path_string));
        let taken_at_ms = record
            .and_then(|r| parse_rfc3339_ms(&r.taken_at))
            .unwrap_or_else(|| modified_ms(&metadata));

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let key = thumbnail_key(&path_string, modified_ms(&metadata));
        remember_key(&key, &path);

        entries.push(HistoryEntry {
            thumbnail_url: format!(
                "http://{}.localhost/thumb?k={key}",
                crate::overlay::FRAME_SCHEME
            ),
            full_url: format!(
                "http://{}.localhost/full?k={key}",
                crate::overlay::FRAME_SCHEME
            ),
            path: path_string,
            file_name,
            taken_at_ms,
            bytes: metadata.len(),
            width: record.map(|r| r.width),
            height: record.map(|r| r.height),
            kind: record.map(|r| r.kind),
            source: record.and_then(|r| r.source.clone()),
        });
    }

    // Filter before sorting: there is no point ordering rows that are about to
    // be discarded.
    let search = query
        .search
        .as_ref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());

    entries.retain(|entry| {
        if let Some(needle) = &search {
            if !entry.file_name.to_lowercase().contains(needle) {
                return false;
            }
        }
        if let Some(from) = query.from_ms {
            if entry.taken_at_ms < from {
                return false;
            }
        }
        if let Some(to) = query.to_ms {
            if entry.taken_at_ms > to {
                return false;
            }
        }
        true
    });

    entries.sort_by(|a, b| b.taken_at_ms.cmp(&a.taken_at_ms));

    let total = entries.len();
    let offset = query.offset.unwrap_or(0).min(total);
    let end = match query.limit {
        Some(limit) => (offset + limit).min(total),
        None => total,
    };

    Ok(HistoryPage {
        entries: entries[offset..end].to_vec(),
        total,
    })
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Milliseconds since the epoch from an RFC 3339 timestamp.
fn parse_rfc3339_ms(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis().max(0) as u64)
}

/// Stable cache filename for a capture's thumbnail.
///
/// Keyed by path *and* modification time, so editing a capture in place
/// produces a different key and the stale thumbnail is never shown.
fn thumbnail_key(path: &str, modified_ms: u64) -> String {
    // FNV-1a: not cryptographic, but this only needs to avoid collisions
    // between filenames on one machine, and it keeps the crate count down.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalise(path).bytes().chain(modified_ms.to_le_bytes()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Produce, or reuse, the cached thumbnail bytes for a capture.
pub fn thumbnail(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    let key = thumbnail_key(&path.to_string_lossy(), modified_ms(&metadata));

    let dir = thumbnail_dir();
    let cached = dir.join(format!("{key}.jpg"));
    if let Ok(bytes) = fs::read(&cached) {
        return Ok(bytes);
    }

    let image = image::open(path).map_err(|e| format!("decoding {}: {e}", path.display()))?;
    let thumb = image.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE);

    let mut buffer = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, 78);
    let rgb = thumb.to_rgb8();
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("encoding thumbnail: {e}"))?;

    // Cache failures are not fatal — the thumbnail was still produced, it will
    // just be regenerated next time.
    if fs::create_dir_all(&dir).is_ok() {
        let _ = fs::write(&cached, &buffer);
    }

    Ok(buffer)
}

/// Maps thumbnail keys back to the files they came from.
///
/// Populated by [`list`] as a side effect, so the protocol handler can answer a
/// thumbnail request with a hash lookup. Without it, every visible tile would
/// trigger its own full scan of the save folder — sixty directory walks to paint
/// one screen, which gets worse the more captures the user has.
static KEY_CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PathBuf>>> =
    std::sync::OnceLock::new();

fn key_cache() -> &'static std::sync::Mutex<HashMap<String, PathBuf>> {
    KEY_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Find the capture a thumbnail key belongs to.
///
/// The URL carries an opaque key rather than a path, which keeps arbitrary
/// filesystem paths out of URLs the webview can construct: only files actually
/// present in the save folder can ever be resolved and served.
pub fn resolve_thumbnail_key(key: &str, save_directory: &Path) -> Option<PathBuf> {
    if let Ok(cache) = key_cache().lock() {
        if let Some(path) = cache.get(key) {
            // Still verify existence: the cache can outlive a deleted file.
            if path.exists() {
                return Some(path.clone());
            }
        }
    }

    // Cold path — a thumbnail requested before any listing, or after the file
    // changed. Fall back to a scan, which also refreshes the cache.
    let dir = fs::read_dir(save_directory).ok()?;
    for item in dir.flatten() {
        let path = item.path();
        if !is_image(&path) {
            continue;
        }
        let metadata = match item.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        if thumbnail_key(&path.to_string_lossy(), modified_ms(&metadata)) == key {
            remember_key(key, &path);
            return Some(path);
        }
    }
    None
}

/// The thumbnail key for a file as it exists right now.
pub fn key_for(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    Some(thumbnail_key(&path.to_string_lossy(), modified_ms(&metadata)))
}

fn remember_key(key: &str, path: &Path) {
    if let Ok(mut cache) = key_cache().lock() {
        cache.insert(key.to_string(), path.to_path_buf());
    }
}

/// Delete a capture and its cached thumbnail.
///
/// Refuses anything outside the save folder, so a bad path from the UI cannot
/// be turned into an arbitrary file deletion.
pub fn delete(path: &Path, save_directory: &Path) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|_| format!("{} no longer exists", path.display()))?;
    let root = save_directory
        .canonicalize()
        .map_err(|e| format!("save folder is unavailable: {e}"))?;

    if !canonical.starts_with(&root) {
        return Err("refusing to delete a file outside the save folder".to_string());
    }

    if let Ok(metadata) = fs::metadata(&canonical) {
        let key = thumbnail_key(&canonical.to_string_lossy(), modified_ms(&metadata));
        let _ = fs::remove_file(thumbnail_dir().join(format!("{key}.jpg")));
    }

    fs::remove_file(&canonical).map_err(|e| format!("could not delete: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_image_files_are_listed() {
        assert!(is_image(Path::new("a/b/shot.png")));
        assert!(is_image(Path::new("a/b/shot.JPG")));
        assert!(!is_image(Path::new("a/b/notes.txt")));
        assert!(!is_image(Path::new("a/b/no-extension")));
    }

    #[test]
    fn thumbnail_key_changes_when_the_file_does() {
        let a = thumbnail_key(r"C:\shots\one.png", 1000);
        let b = thumbnail_key(r"C:\shots\one.png", 2000);
        assert_ne!(a, b, "an edited capture must not reuse its old thumbnail");
    }

    #[test]
    fn thumbnail_key_ignores_path_casing() {
        assert_eq!(
            thumbnail_key(r"C:\Shots\One.png", 1),
            thumbnail_key(r"c:\shots\one.png", 1)
        );
    }

    #[test]
    fn delete_refuses_paths_outside_the_save_folder() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("important.png");
        fs::write(&victim, b"x").unwrap();

        let result = delete(&victim, root.path());
        assert!(result.is_err(), "escaped the save folder");
        assert!(victim.exists(), "the file was deleted anyway");
    }

    #[test]
    fn delete_removes_a_file_inside_the_save_folder() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("shot.png");
        fs::write(&target, b"x").unwrap();

        delete(&target, root.path()).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn listing_an_absent_folder_is_empty_not_an_error() {
        let page = list(&HistoryQuery::default(), Path::new(r"Z:\nope\missing")).unwrap();
        assert_eq!(page.total, 0);
    }

    #[test]
    fn files_with_no_index_entry_still_appear() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("stray.png"), b"x").unwrap();

        // Nothing was ever recorded for this file, yet losing it from history
        // would contradict the whole premise of the app.
        let page = list(&HistoryQuery::default(), root.path()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].file_name, "stray.png");
        assert!(page.entries[0].kind.is_none());
    }

    #[test]
    fn search_filters_by_filename() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("invoice.png"), b"x").unwrap();
        fs::write(root.path().join("holiday.png"), b"x").unwrap();

        let query = HistoryQuery {
            search: Some("INVO".into()),
            ..Default::default()
        };
        let page = list(&query, root.path()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].file_name, "invoice.png");
    }

    #[test]
    fn paging_reports_the_unpaged_total() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..7 {
            fs::write(root.path().join(format!("shot{i}.png")), b"x").unwrap();
        }

        let query = HistoryQuery {
            limit: Some(3),
            ..Default::default()
        };
        let page = list(&query, root.path()).unwrap();
        assert_eq!(page.entries.len(), 3);
        assert_eq!(page.total, 7, "total must count matches, not the page");
    }
}
