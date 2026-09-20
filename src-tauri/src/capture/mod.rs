//! Capture orchestration: grab pixels, get them on disk, then everything else.
//!
//! # The ordering guarantee
//!
//! The central promise of this app is that a capture is never lost. That is not
//! a feature so much as an ordering rule, enforced in [`capture_and_save`]:
//!
//! 1. Read the pixels.
//! 2. **Write the file.**
//! 3. Everything else — counter persistence, clipboard, history metadata,
//!    notifications, opening the editor.
//!
//! Step 2 happens before any step that can fail, and no failure after it is
//! allowed to propagate as a capture failure. If the clipboard is locked or the
//! history index cannot be written, the user still has their screenshot, and the
//! problem is reported alongside a successful result rather than instead of one.

pub mod mask;
pub mod win;

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use chrono::Local;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::config::{ImageFormat, Settings};
use crate::naming::{self, NameRequest};

pub use win::{Bounds, MonitorInfo, VirtualDesktop};

/// Which capture mode produced a record. Stored in history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureKind {
    FullScreen,
    ActiveWindow,
    Region,
    /// A hand-drawn lasso selection. Always has a transparent margin.
    Freeform,
}

/// What the caller wants captured.
///
/// `rename_all_fields` is load-bearing: `rename_all` alone renames only the
/// variant names, so `monitorId` sent from the UI would never bind to
/// `monitor_id` and would silently fall back to "all displays".
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "mode")]
pub enum CaptureRequest {
    /// A single display, or the entire virtual desktop when `monitorId` is null.
    FullScreen {
        #[serde(default)]
        monitor_id: Option<String>,
    },
    /// The frontmost window that does not belong to this app.
    ActiveWindow,
    /// An explicit rectangle in virtual-screen coordinates, as produced by the
    /// selection overlay.
    Region { bounds: Bounds },
}

/// A saved capture. This is both the value returned to the UI and the shape
/// stored in the history index.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRecord {
    /// Stable identifier, derived from the capture time plus a counter.
    pub id: String,
    /// Absolute path to the saved file.
    pub path: String,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    /// RFC 3339, local time with offset.
    pub taken_at: String,
    pub kind: CaptureKind,
    /// Window title for window captures, display label for full-screen ones.
    #[serde(default)]
    pub source: Option<String>,
    pub bytes: u64,
    /// Whether the image reached the clipboard. False when auto-copy is off, or
    /// when the clipboard could not be acquired.
    pub copied_to_clipboard: bool,
    /// Non-fatal problems that happened *after* the file was safely written.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Capture, save, and copy — in that order. See the module docs.
///
/// `settings` is taken mutably because prefix-mode naming consumes a counter
/// that must be persisted for the next capture to continue the sequence.
pub fn capture_and_save(
    request: CaptureRequest,
    settings: &mut Settings,
) -> Result<CaptureRecord, String> {
    let (frame, kind, source) = grab(&request)?;
    save_frame(frame, kind, source, settings)
}

/// Save an already-captured frame.
///
/// Region capture needs this separately from [`capture_and_save`]: its pixels
/// were frozen *before* the selection overlay appeared, so re-reading the screen
/// at save time would photograph our own overlay instead of what the user chose.
pub fn save_frame(
    frame: win::Frame,
    kind: CaptureKind,
    source: Option<String>,
    settings: &mut Settings,
) -> Result<CaptureRecord, String> {
    let taken_at = Local::now();
    let mut warnings = Vec::new();

    // --- Disk, before anything else can go wrong -------------------------
    let dir = settings.ensure_save_directory()?;
    if dir != settings.save_directory {
        warnings.push(format!(
            "Configured save folder was unavailable; saved to {} instead.",
            dir.display()
        ));
    }

    // A freeform capture carries real transparency, and JPEG cannot represent
    // it — saving one as JPEG would fill the cut-away margin with black. PNG is
    // therefore forced for those, whatever the configured default is.
    let format = if kind == CaptureKind::Freeform {
        ImageFormat::Png
    } else {
        settings.format
    };

    let resolved = naming::resolve(NameRequest {
        dir: &dir,
        naming: &settings.naming,
        extension: format.extension(),
        taken_at,
    });

    let bytes = encode_to_disk(&frame.image, &resolved.path, format, settings.jpeg_quality)?;

    // The capture is now safe. Nothing below may return Err.

    // --- Persist the counter --------------------------------------------
    if let Some(used) = resolved.counter_used {
        settings.naming.counter = used.saturating_add(1);
        if let Err(err) = settings.save() {
            // Worst case the next capture reuses this number and skips forward
            // rather than overwriting, so this is a warning, not a failure.
            warnings.push(format!("Could not persist the filename counter: {err}"));
        }
    }

    // --- Clipboard -------------------------------------------------------
    let mut copied = false;
    if settings.clipboard.auto_copy {
        match crate::clipboard::copy_image(&frame.image) {
            Ok(attempts) => {
                copied = true;
                if attempts > 1 {
                    eprintln!("[clipboard] copied on attempt {attempts}");
                }
            }
            Err(err) => warnings.push(format!("Could not copy to clipboard: {err}")),
        }
    }

    let (width, height) = frame.image.dimensions();

    let record = CaptureRecord {
        id: format!("{}-{}", taken_at.format("%Y%m%d%H%M%S%3f"), width ^ height),
        path: resolved.path.to_string_lossy().into_owned(),
        file_name: resolved
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        width,
        height,
        taken_at: taken_at.to_rfc3339(),
        kind,
        source,
        bytes,
        copied_to_clipboard: copied,
        warnings,
    };

    // --- History index ---------------------------------------------------
    // Enrichment only. History is rebuilt by scanning the save folder, so a
    // failure here costs this capture its mode label in the grid -- it does not
    // cost the capture.
    let mut record = record;
    if let Err(err) = crate::history::record(&record) {
        record
            .warnings
            .push(format!("Could not update the history index: {err}"));
    }

    Ok(record)
}

/// Read the pixels for a request, and describe where they came from.
fn grab(
    request: &CaptureRequest,
) -> Result<(win::Frame, CaptureKind, Option<String>), String> {
    match request {
        CaptureRequest::FullScreen { monitor_id } => {
            let (area, label) = match monitor_id {
                Some(id) => {
                    let monitor = win::monitors()
                        .into_iter()
                        .find(|m| &m.id == id)
                        // A display can be unplugged between the menu being
                        // built and the capture firing. Falling back to the
                        // whole desktop still gets the user an image.
                        .ok_or_else(|| format!("display {id} is no longer connected"))?;
                    (
                        Bounds {
                            x: monitor.x,
                            y: monitor.y,
                            width: monitor.width,
                            height: monitor.height,
                        },
                        Some(monitor.label.clone()),
                    )
                }
                None => (
                    win::virtual_desktop().as_rect(),
                    Some("All displays".to_string()),
                ),
            };

            let frame = win::capture_area(area).map_err(|e| e.to_string())?;
            Ok((frame, CaptureKind::FullScreen, label))
        }

        CaptureRequest::ActiveWindow => {
            let target = win::active_window().map_err(|e| e.to_string())?;
            let frame = win::capture_area(target.bounds).map_err(|e| e.to_string())?;
            let source = if target.title.trim().is_empty() {
                None
            } else {
                Some(target.title)
            };
            Ok((frame, CaptureKind::ActiveWindow, source))
        }

        CaptureRequest::Region { bounds } => {
            let frame = win::capture_area(*bounds).map_err(|e| e.to_string())?;
            // Name the display the selection started on, which is the useful
            // label even when the selection spans two screens.
            let source = win::monitor_at((bounds.x, bounds.y)).map(|m| m.label);
            Ok((frame, CaptureKind::Region, source))
        }
    }
}

/// Encode and write an image, returning the file size in bytes.
///
/// Writes to a `.part` file and renames it into place, so a crash or a full disk
/// mid-write cannot leave a truncated image that history would later show as a
/// broken thumbnail.
fn encode_to_disk(
    image: &RgbaImage,
    path: &Path,
    format: ImageFormat,
    jpeg_quality: u8,
) -> Result<u64, String> {
    let temp = path.with_extension(format!("{}.part", format.extension()));

    {
        let file = File::create(&temp).map_err(|e| format!("creating {}: {e}", temp.display()))?;
        let writer = BufWriter::new(file);

        match format {
            ImageFormat::Png => {
                use image::codecs::png::{CompressionType, FilterType, PngEncoder};

                // `Fast` rather than `Default`: saving happens synchronously on
                // every capture, and the extra second that maximum compression
                // costs on a 4K screenshot is far more noticeable to the user
                // than the resulting file-size difference.
                let encoder =
                    PngEncoder::new_with_quality(writer, CompressionType::Fast, FilterType::Adaptive);
                encoder
                    .write_image(image.as_raw(), image.width(), image.height(), ExtendedColorType::Rgba8)
                    .map_err(|e| format!("encoding PNG: {e}"))?;
            }

            ImageFormat::Jpeg => {
                use image::codecs::jpeg::JpegEncoder;

                // JPEG has no alpha channel. Build a packed RGB buffer directly
                // rather than cloning the whole RGBA image just to drop a byte.
                let mut rgb = Vec::with_capacity(image.width() as usize * image.height() as usize * 3);
                for pixel in image.pixels() {
                    rgb.extend_from_slice(&pixel.0[..3]);
                }

                let mut encoder = JpegEncoder::new_with_quality(writer, jpeg_quality);
                encoder
                    .encode(&rgb, image.width(), image.height(), ExtendedColorType::Rgb8)
                    .map_err(|e| format!("encoding JPEG: {e}"))?;
            }
        }
    } // BufWriter flushed and file closed here, before the rename.

    std::fs::rename(&temp, path)
        .map_err(|e| format!("moving {} into place: {e}", temp.display()))?;

    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(bytes)
}

/// Encode a frozen frame as JPEG, purely so the overlay has a backdrop to draw.
///
/// This is display-only and never reaches a saved file: the overlay shows this,
/// but the pixels that actually get written come from cropping the lossless
/// frame still held in memory. That separation is what lets the backdrop be
/// cheap — JPEG encodes a full desktop in tens of milliseconds where PNG takes
/// several hundred, and the overlay needs to appear instantly to feel right.
pub fn encode_preview(image: &RgbaImage) -> Result<Vec<u8>, String> {
    use image::codecs::jpeg::JpegEncoder;

    let mut rgb = Vec::with_capacity(image.width() as usize * image.height() as usize * 3);
    for pixel in image.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
    }

    let mut buffer = Vec::new();
    // 82 is comfortably past the point where compression artefacts are visible
    // at 1:1 on screen content, and keeps a 4K desktop well under a megabyte.
    let mut encoder = JpegEncoder::new_with_quality(&mut buffer, 82);
    encoder
        .encode(
            &rgb,
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("encoding overlay backdrop: {e}"))?;

    Ok(buffer)
}
