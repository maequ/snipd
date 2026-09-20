//! Filename generation for captures.
//!
//! # Guarantees
//!
//! [`resolve`] always returns a usable path. It never returns an error and never
//! returns a path that already exists. This matters because the whole premise of
//! the app is that a capture is on disk before the user is given any chance to
//! lose it — so naming must not be a step that can fail.
//!
//! # The rules, made explicit
//!
//! These were open questions in the spec, resolved as follows:
//!
//! * **Datetime mode** produces `Screenshot_2026-09-20_14-32-05.png`. Two captures
//!   inside the same second collide, so the second one becomes
//!   `Screenshot_2026-09-20_14-32-05_2.png`, then `_3`, and so on.
//!
//! * **Prefix mode** produces `MyShot_001.png`. The counter lives in the config
//!   and is **monotonic** — it never resets, not daily, not on restart, not when
//!   the save folder is changed. This keeps numbering stable and predictable; a
//!   user who wants to start over can reset it in Settings.
//!
//! * The counter is **not** truncated by its padding. With padding 3, capture
//!   1000 is `MyShot_1000.png`, not `MyShot_000.png`.
//!
//! * If a target name is already taken — because the user copied files in, moved
//!   the folder, or reset the counter — the counter skips forward to the first
//!   free number rather than overwriting. **Nothing is ever overwritten.**
//!
//! * Prefixes are sanitised against Windows filename rules. A prefix that
//!   sanitises to nothing falls back to `Screenshot`.

use std::path::{Path, PathBuf};

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Local};

use crate::config::{NamingMode, NamingSettings};

/// Used when the configured datetime pattern is malformed.
const DEFAULT_DATETIME_PATTERN: &str = "Screenshot_%Y-%m-%d_%H-%M-%S";

/// Characters Windows forbids anywhere in a filename.
const ILLEGAL_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Device names Windows reserves. A file cannot use these as its stem, with or
/// without an extension, in any casing.
const RESERVED_STEMS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Used when a prefix or a formatted datetime sanitises down to nothing.
const FALLBACK_STEM: &str = "Screenshot";

/// How many names to try before giving up on the tidy scheme and switching to
/// the timestamped escape hatch. Generous enough that real users never hit it.
const MAX_ATTEMPTS: u32 = 10_000;

/// Longest stem we will emit. Windows' practical path limit is 260 characters
/// without long-path support; capping the stem leaves room for the directory.
const MAX_STEM_LEN: usize = 120;

/// Everything [`resolve`] needs to pick a filename.
pub struct NameRequest<'a> {
    /// Directory the file will be written into. Must already exist.
    pub dir: &'a Path,
    pub naming: &'a NamingSettings,
    /// Extension without a dot, e.g. `"png"`.
    pub extension: &'a str,
    /// When the capture was taken, used by datetime mode.
    pub taken_at: DateTime<Local>,
}

/// The chosen filename, plus any counter state the caller must persist.
#[derive(Debug, Clone)]
pub struct ResolvedName {
    /// Full path to write to. Guaranteed not to exist at the moment of return.
    pub path: PathBuf,
    /// In prefix mode, the counter value actually consumed. The caller should
    /// store `counter_used + 1` back into the config so the next capture
    /// continues the sequence. `None` in datetime mode.
    pub counter_used: Option<u32>,
}

/// Strip characters Windows will not accept, collapse whitespace, and avoid
/// reserved device names.
pub fn sanitise_stem(raw: &str) -> String {
    let mut cleaned: String = raw
        .chars()
        .map(|c| {
            // Control characters are illegal in filenames too, not just the
            // well-known punctuation set.
            if ILLEGAL_CHARS.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();

    // Windows silently strips trailing dots and spaces, which would make the
    // name we chose differ from the name on disk. Remove them ourselves so the
    // history index and the filesystem agree.
    cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();

    if cleaned.chars().count() > MAX_STEM_LEN {
        cleaned = cleaned.chars().take(MAX_STEM_LEN).collect();
        cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    }

    if cleaned.is_empty() {
        return FALLBACK_STEM.to_string();
    }

    // A reserved device name is only reserved as the whole stem, so suffixing it
    // is enough to make it legal while keeping it recognisable.
    let upper = cleaned.to_ascii_uppercase();
    if RESERVED_STEMS.contains(&upper.as_str()) {
        cleaned.push('_');
    }

    cleaned
}

/// Render what a filename would look like, without touching the disk.
///
/// Used by the Settings screen's live preview, and mirrored by the Inno Setup
/// wizard so both show the user the same thing.
pub fn preview(naming: &NamingSettings, extension: &str, at: DateTime<Local>) -> String {
    match naming.mode {
        NamingMode::Datetime => {
            let stem = sanitise_stem(&format_datetime(&naming.datetime_pattern, at));
            format!("{stem}.{extension}")
        }
        NamingMode::Prefix => {
            let stem = sanitise_stem(&naming.prefix);
            format!(
                "{stem}_{:0width$}.{extension}",
                naming.counter,
                width = naming.counter_padding
            )
        }
    }
}

/// Pick a free filename in `dir`.
///
/// Never fails, never overwrites. See the module docs for the exact rules.
pub fn resolve(req: NameRequest<'_>) -> ResolvedName {
    match req.naming.mode {
        NamingMode::Datetime => resolve_datetime(&req),
        NamingMode::Prefix => resolve_prefix(&req),
    }
}

fn resolve_datetime(req: &NameRequest<'_>) -> ResolvedName {
    let base = sanitise_stem(&format_datetime(&req.naming.datetime_pattern, req.taken_at));

    // First capture in a given second gets the clean name; subsequent ones in
    // the same second get _2, _3, ...
    let first = req.dir.join(format!("{base}.{}", req.extension));
    if !first.exists() {
        return ResolvedName {
            path: first,
            counter_used: None,
        };
    }

    for n in 2..=MAX_ATTEMPTS {
        let candidate = req.dir.join(format!("{base}_{n}.{}", req.extension));
        if !candidate.exists() {
            return ResolvedName {
                path: candidate,
                counter_used: None,
            };
        }
    }

    ResolvedName {
        path: escape_hatch(req.dir, &base, req.extension),
        counter_used: None,
    }
}

fn resolve_prefix(req: &NameRequest<'_>) -> ResolvedName {
    let stem = sanitise_stem(&req.naming.prefix);
    let pad = req.naming.counter_padding;

    // Start at the stored counter and skip forward over anything already on
    // disk, so pre-existing files are never clobbered.
    let start = req.naming.counter.max(1);
    let end = start.saturating_add(MAX_ATTEMPTS);

    for n in start..end {
        let candidate = req
            .dir
            .join(format!("{stem}_{n:0pad$}.{}", req.extension, pad = pad));
        if !candidate.exists() {
            return ResolvedName {
                path: candidate,
                counter_used: Some(n),
            };
        }
    }

    ResolvedName {
        path: escape_hatch(req.dir, &stem, req.extension),
        // The tidy sequence was exhausted, so do not advance the counter past
        // where it already is; the next capture will retry from the same point.
        counter_used: None,
    }
}

/// Last-resort name used only if the normal scheme somehow cannot find a free
/// slot. Uses millisecond precision plus a disambiguating suffix, so producing
/// a collision here is effectively impossible — and even if it did, the loop
/// keeps trying rather than returning a path that exists.
fn escape_hatch(dir: &Path, base: &str, extension: &str) -> PathBuf {
    let stamp = Local::now().format("%Y%m%d-%H%M%S%.3f");
    for salt in 0..u32::MAX {
        let candidate = if salt == 0 {
            dir.join(format!("{base}_{stamp}.{extension}"))
        } else {
            dir.join(format!("{base}_{stamp}-{salt}.{extension}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    // Unreachable in practice; satisfies the compiler with a deterministic path.
    dir.join(format!("{base}_{stamp}.{extension}"))
}

/// Apply a strftime pattern, falling back to a known-good pattern if the user's
/// pattern is invalid.
///
/// `chrono` does not reject a bad format string when it is parsed — it defers,
/// and then *panics* when the result is formatted. Catching that is not an
/// option, because the release profile builds with `panic = "abort"`. So the
/// pattern is validated up front instead: `StrftimeItems` yields `Item::Error`
/// for anything malformed, which can be checked without ever formatting.
fn format_datetime(pattern: &str, at: DateTime<Local>) -> String {
    let items: Vec<Item<'_>> = StrftimeItems::new(pattern).collect();
    let valid = !items.iter().any(|item| matches!(item, Item::Error));

    if valid {
        let formatted = at.format_with_items(items.iter()).to_string();
        if !formatted.trim().is_empty() {
            return formatted;
        }
    }

    at.format(DEFAULT_DATETIME_PATTERN).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NamingMode;
    use chrono::TimeZone;

    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 20, h, m, s).unwrap()
    }

    fn datetime_settings() -> NamingSettings {
        NamingSettings {
            mode: NamingMode::Datetime,
            ..Default::default()
        }
    }

    fn prefix_settings(prefix: &str, counter: u32) -> NamingSettings {
        NamingSettings {
            mode: NamingMode::Prefix,
            prefix: prefix.into(),
            counter,
            counter_padding: 3,
            ..Default::default()
        }
    }

    #[test]
    fn datetime_name_matches_the_documented_shape() {
        let s = datetime_settings();
        assert_eq!(
            preview(&s, "png", at(14, 32, 5)),
            "Screenshot_2026-09-20_14-32-05.png"
        );
    }

    #[test]
    fn prefix_name_is_zero_padded() {
        let s = prefix_settings("MyShot", 1);
        assert_eq!(preview(&s, "png", at(0, 0, 0)), "MyShot_001.png");
    }

    #[test]
    fn counter_is_not_truncated_by_padding() {
        let s = prefix_settings("MyShot", 1000);
        assert_eq!(preview(&s, "png", at(0, 0, 0)), "MyShot_1000.png");
    }

    #[test]
    fn illegal_characters_are_replaced() {
        assert_eq!(sanitise_stem(r#"a<b>c:d"e/f\g|h?i*j"#), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn trailing_dots_and_spaces_are_stripped() {
        // Windows would strip these itself, leaving the on-disk name different
        // from the one we recorded.
        assert_eq!(sanitise_stem("report.  "), "report");
        assert_eq!(sanitise_stem("  spaced  "), "spaced");
    }

    #[test]
    fn reserved_device_names_are_escaped() {
        assert_eq!(sanitise_stem("CON"), "CON_");
        assert_eq!(sanitise_stem("nul"), "nul_");
        // Only reserved as a whole stem, so this is left alone.
        assert_eq!(sanitise_stem("CONTROL"), "CONTROL");
    }

    #[test]
    fn empty_prefix_falls_back() {
        assert_eq!(sanitise_stem("   "), FALLBACK_STEM);
        assert_eq!(sanitise_stem("///"), "___");
    }

    #[test]
    fn overlong_stems_are_capped() {
        let long = "x".repeat(500);
        assert_eq!(sanitise_stem(&long).chars().count(), MAX_STEM_LEN);
    }

    #[test]
    fn datetime_collision_within_one_second_disambiguates() {
        let dir = tempfile::tempdir().unwrap();
        let s = datetime_settings();

        let first = resolve(NameRequest {
            dir: dir.path(),
            naming: &s,
            extension: "png",
            taken_at: at(14, 32, 5),
        });
        assert_eq!(
            first.path.file_name().unwrap(),
            "Screenshot_2026-09-20_14-32-05.png"
        );
        std::fs::write(&first.path, b"x").unwrap();

        let second = resolve(NameRequest {
            dir: dir.path(),
            naming: &s,
            extension: "png",
            taken_at: at(14, 32, 5),
        });
        assert_eq!(
            second.path.file_name().unwrap(),
            "Screenshot_2026-09-20_14-32-05_2.png"
        );
    }

    #[test]
    fn prefix_mode_skips_over_existing_files_instead_of_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate files the user copied in, or a counter that was reset.
        std::fs::write(dir.path().join("MyShot_001.png"), b"x").unwrap();
        std::fs::write(dir.path().join("MyShot_002.png"), b"x").unwrap();

        let s = prefix_settings("MyShot", 1);
        let resolved = resolve(NameRequest {
            dir: dir.path(),
            naming: &s,
            extension: "png",
            taken_at: at(0, 0, 0),
        });

        assert_eq!(resolved.path.file_name().unwrap(), "MyShot_003.png");
        assert_eq!(resolved.counter_used, Some(3));
    }

    #[test]
    fn resolve_never_returns_an_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let s = prefix_settings("Shot", 1);

        // Take 50 in a row, writing each one, and assert every name was fresh.
        let mut counter = 1;
        for _ in 0..50 {
            let mut settings = s.clone();
            settings.counter = counter;
            let r = resolve(NameRequest {
                dir: dir.path(),
                naming: &settings,
                extension: "png",
                taken_at: at(0, 0, 0),
            });
            assert!(!r.path.exists(), "resolve handed back an existing path");
            std::fs::write(&r.path, b"x").unwrap();
            counter = r.counter_used.unwrap() + 1;
        }

        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 50);
    }

    #[test]
    fn invalid_datetime_pattern_does_not_panic() {
        let s = NamingSettings {
            mode: NamingMode::Datetime,
            // `%` followed by an unknown specifier is a classic bad pattern.
            datetime_pattern: "Shot_%Q%".into(),
            ..Default::default()
        };
        let name = preview(&s, "png", at(14, 32, 5));
        assert!(name.ends_with(".png"), "got {name}");
    }
}
