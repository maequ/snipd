//! Single source of truth for the app's identity.
//!
//! The product name appears in the window title, tray tooltip, the `%APPDATA%`
//! folder name and the installer. Keeping it here means renaming the app is a
//! change to this file plus `tauri.conf.json` and `Cargo.toml`, rather than a
//! find-and-replace across the codebase.

/// Human-facing product name. Shown in the UI, tray and notifications.
pub const APP_NAME: &str = "Snipd";

/// Folder name used under `%APPDATA%` for config and the history index.
/// Deliberately identical to `APP_NAME`, but kept separate so the on-disk
/// layout does not have to churn if the product is ever renamed.
pub const DATA_DIR_NAME: &str = "Snipd";

/// Default sub-folder created under the user's Pictures directory when the app
/// has no configured save location (i.e. it was run without the installer).
pub const DEFAULT_SAVE_SUBDIR: &str = "Snipd";

/// Shown in the Settings > About section and the README.
pub const GITHUB_URL: &str = "https://github.com/snipd-app/snipd";
