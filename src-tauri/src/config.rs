//! Persistent user settings.
//!
//! # Where the config lives
//!
//! `%APPDATA%\Snipd\config.json` — per-user, and writable by the app at runtime
//! without elevation.
//!
//! # How the installer seeds it
//!
//! The Inno Setup wizard cannot reliably write to the invoking user's `%APPDATA%`,
//! because setup may run elevated as a *different* account than the one that will
//! use the app. So the installer instead drops its answers next to the executable
//! as `first-run.json`, and the app copies that into place the first time it
//! starts (see [`Settings::load_or_seed`]). This keeps the wizard's choices
//! effective on first launch with no extra steps, without guessing at profiles.
//!
//! # Forward compatibility
//!
//! Every struct is `#[serde(default)]`, so a config written by an older build —
//! or a partial one written by the installer — still loads, with anything missing
//! filled in from defaults. A malformed config is never fatal: it is moved aside
//! and replaced, because failing to start is worse than losing preferences.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::branding;

/// Bumped only for changes that need migration code, not for added fields
/// (added fields are handled by `#[serde(default)]`).
pub const CONFIG_VERSION: u32 = 1;

/// Filename the installer writes beside the executable. Consumed once.
pub const SEED_FILENAME: &str = "first-run.json";

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// How captured files are named. Mirrors wizard Page B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NamingMode {
    /// `Screenshot_2026-09-20_14-32-05.png` — derived from the capture time.
    Datetime,
    /// `MyShot_001.png` — a user prefix plus a monotonic counter.
    Prefix,
}

/// Encoded image format. Mirrors wizard Page C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    /// File extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

/// UI theme. `System` follows the Windows light/dark setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    System,
    Light,
    Dark,
}

// ---------------------------------------------------------------------------
// Settings groups
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NamingSettings {
    pub mode: NamingMode,
    /// `chrono` strftime pattern used when `mode` is `Datetime`.
    pub datetime_pattern: String,
    /// User-supplied prefix used when `mode` is `Prefix`. Sanitised on use.
    pub prefix: String,
    /// Next number to try in `Prefix` mode. Persisted so numbering survives restarts.
    pub counter: u32,
    /// Zero-padding width for the counter. Numbers wider than this are not truncated.
    pub counter_padding: usize,
}

impl Default for NamingSettings {
    fn default() -> Self {
        Self {
            mode: NamingMode::Datetime,
            datetime_pattern: "Screenshot_%Y-%m-%d_%H-%M-%S".into(),
            prefix: "Screenshot".into(),
            counter: 1,
            counter_padding: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClipboardSettings {
    /// Copy every capture to the clipboard automatically (wizard Page D).
    pub auto_copy: bool,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self { auto_copy: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StartupSettings {
    /// Register the app to run at login (wizard Page E).
    pub launch_on_login: bool,
    /// Start with no visible window, tray only (wizard Page E).
    pub start_minimised: bool,
}

impl Default for StartupSettings {
    fn default() -> Self {
        Self {
            launch_on_login: true,
            start_minimised: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowSettings {
    /// Closing the main window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self { close_to_tray: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotificationSettings {
    /// Show a "Screenshot saved" toast after each capture.
    pub show_saved_toast: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            show_saved_toast: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShortcutSettings {
    /// The main one: opens the capture overlay, where the mode is chosen.
    pub capture: String,
    /// Opens the overlay with the rectangle tool already selected.
    pub region: String,
    /// Captures immediately without showing the overlay at all.
    pub full_screen: String,
    /// Captures the frontmost window immediately, no overlay.
    pub active_window: String,
}

impl Default for ShortcutSettings {
    fn default() -> Self {
        // Deliberately not Win+Shift+S: that is the built-in Snipping Tool
        // binding, and taking it would stop the user falling back to the tool
        // they already know while this one is still young.
        Self {
            capture: "CommandOrControl+Alt+S".into(),
            region: "CommandOrControl+Alt+3".into(),
            full_screen: "CommandOrControl+Alt+1".into(),
            active_window: "CommandOrControl+Alt+2".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RetentionSettings {
    /// Off by default. Nothing is ever deleted unless the user opts in.
    pub enabled: bool,
    pub days: u32,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            days: 30,
        }
    }
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub version: u32,
    /// Absolute path captures are written to. Always honoured, never guessed at.
    pub save_directory: PathBuf,
    pub format: ImageFormat,
    /// JPEG encoder quality, 1-100. Ignored for PNG.
    pub jpeg_quality: u8,
    pub theme: Theme,
    pub naming: NamingSettings,
    pub clipboard: ClipboardSettings,
    pub startup: StartupSettings,
    pub window: WindowSettings,
    pub notifications: NotificationSettings,
    pub shortcuts: ShortcutSettings,
    pub retention: RetentionSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            save_directory: default_save_directory(),
            format: ImageFormat::Png,
            jpeg_quality: 92,
            theme: Theme::System,
            naming: NamingSettings::default(),
            clipboard: ClipboardSettings::default(),
            startup: StartupSettings::default(),
            window: WindowSettings::default(),
            notifications: NotificationSettings::default(),
            shortcuts: ShortcutSettings::default(),
            retention: RetentionSettings::default(),
        }
    }
}

/// `%USERPROFILE%\Pictures\Snipd`, resolved through the Windows known-folder
/// API so a relocated Pictures folder is respected. Falls back to the profile
/// root, then the current directory, so this never panics.
pub fn default_save_directory() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(branding::DEFAULT_SAVE_SUBDIR)
}

/// `%APPDATA%\Snipd` — where `config.json` and the history index live.
pub fn data_directory() -> PathBuf {
    dirs::config_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(branding::DATA_DIR_NAME)
}

/// Full path to `config.json`.
pub fn config_path() -> PathBuf {
    data_directory().join("config.json")
}

impl Settings {
    /// Load settings, applying the installer's seed file on very first run.
    ///
    /// Resolution order:
    /// 1. An existing `%APPDATA%\Snipd\config.json` wins — it reflects any
    ///    changes the user has made in the Settings screen since install.
    /// 2. Otherwise `first-run.json` beside the executable, if the installer
    ///    left one. It is copied in, then renamed so it is applied exactly once.
    /// 3. Otherwise defaults.
    ///
    /// A config that exists but cannot be parsed is renamed to
    /// `config.corrupt-<timestamp>.json` rather than deleted, so the settings
    /// stay recoverable, and startup continues with defaults.
    pub fn load_or_seed(exe_dir: Option<&Path>) -> Self {
        let path = config_path();

        if path.exists() {
            match Self::read_from(&path) {
                Ok(settings) => return settings,
                Err(err) => {
                    eprintln!(
                        "[config] {} is unreadable ({err}); moving it aside",
                        path.display()
                    );
                    quarantine(&path);
                }
            }
        }

        // First run (or recovery): try the installer's answers.
        if let Some(dir) = exe_dir {
            let seed = dir.join(SEED_FILENAME);
            if seed.exists() {
                match Self::read_from(&seed) {
                    Ok(settings) => {
                        // Mark the seed consumed so a repair-install, or a later
                        // manual run, cannot silently revert changes the user
                        // has since made in the Settings screen.
                        let _ = fs::rename(&seed, dir.join("first-run.applied.json"));
                        let _ = settings.save();
                        return settings;
                    }
                    Err(err) => {
                        eprintln!(
                            "[config] installer seed at {} is unreadable: {err}",
                            seed.display()
                        );
                    }
                }
            }
        }

        let settings = Self::default();
        let _ = settings.save();
        settings
    }

    fn read_from(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut settings: Settings = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        settings.normalise();
        Ok(settings)
    }

    /// Clamp and repair values that are representable in JSON but not sensible,
    /// so a hand-edited or installer-written config cannot put the app into a
    /// broken state.
    pub fn normalise(&mut self) {
        self.version = CONFIG_VERSION;
        self.jpeg_quality = self.jpeg_quality.clamp(1, 100);
        self.naming.counter = self.naming.counter.max(1);
        self.naming.counter_padding = self.naming.counter_padding.clamp(1, 10);

        if self.naming.datetime_pattern.trim().is_empty() {
            self.naming.datetime_pattern = NamingSettings::default().datetime_pattern;
        }
        if self.naming.prefix.trim().is_empty() {
            self.naming.prefix = NamingSettings::default().prefix;
        }
        if self.save_directory.as_os_str().is_empty() {
            self.save_directory = default_save_directory();
        }
        if self.retention.days == 0 {
            self.retention.days = 1;
        }
    }

    /// Write settings to disk, creating `%APPDATA%\Snipd` if needed.
    ///
    /// Writes to a temporary file and renames over the target, so a crash or
    /// power loss mid-write cannot leave a truncated config behind.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }

        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| format!("replacing {}: {e}", path.display()))?;
        Ok(())
    }

    /// Ensure the configured save directory exists, returning the path actually
    /// usable for writing.
    ///
    /// If the configured directory cannot be created — a disconnected network
    /// drive, a removed USB stick, a revoked permission — this falls back to the
    /// default Pictures location rather than failing the capture. Losing a
    /// capture is the one outcome this app must never produce.
    pub fn ensure_save_directory(&self) -> Result<PathBuf, String> {
        if fs::create_dir_all(&self.save_directory).is_ok() {
            return Ok(self.save_directory.clone());
        }

        let fallback = default_save_directory();
        eprintln!(
            "[config] save directory {} is unavailable; falling back to {}",
            self.save_directory.display(),
            fallback.display()
        );
        fs::create_dir_all(&fallback).map_err(|e| {
            format!(
                "fallback save directory {} unusable: {e}",
                fallback.display()
            )
        })?;
        Ok(fallback)
    }
}

/// Rename a bad config out of the way with a timestamp, keeping it recoverable.
fn quarantine(path: &Path) {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let target = path.with_file_name(format!("config.corrupt-{stamp}.json"));
    if let Err(err) = fs::rename(path, &target) {
        eprintln!("[config] could not quarantine bad config: {err}");
    }
}
