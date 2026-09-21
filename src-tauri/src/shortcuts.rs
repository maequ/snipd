//! System-wide capture shortcuts.
//!
//! These have to work with no Snipd window focused — or open at all — which is
//! the whole reason the app stays resident in the tray.
//!
//! # Failure is not fatal
//!
//! Registering a global shortcut fails if another application already owns that
//! combination, and there is no way to know in advance which ones are taken on a
//! given machine. A failed registration therefore produces a warning the
//! Settings screen can surface, never a startup error: the user can still
//! capture from the tray and the window while they pick a different key.

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::config::ShortcutSettings;
use crate::overlay;

/// What a bound shortcut does when pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open the overlay on its default tool.
    OpenOverlay,
    /// Open the overlay with the rectangle tool selected.
    OpenOverlayRectangle,
    /// Capture immediately, no overlay.
    FullScreen,
    /// Capture the frontmost window immediately, no overlay.
    ActiveWindow,
}

/// Re-register every shortcut from the current settings.
///
/// Existing bindings are cleared first so this can be called again whenever the
/// user changes a shortcut, without leaving the old one live.
///
/// Returns a human-readable warning per shortcut that could not be bound.
pub fn apply(app: &AppHandle, settings: &ShortcutSettings) -> Vec<String> {
    let manager = app.global_shortcut();
    let _ = manager.unregister_all();

    let bindings = [
        (
            settings.capture.as_str(),
            Action::OpenOverlay,
            "New capture",
        ),
        (
            settings.region.as_str(),
            Action::OpenOverlayRectangle,
            "Region",
        ),
        (
            settings.full_screen.as_str(),
            Action::FullScreen,
            "Full screen",
        ),
        (
            settings.active_window.as_str(),
            Action::ActiveWindow,
            "Active window",
        ),
    ];

    let mut warnings = Vec::new();
    // Two settings pointing at the same combination would otherwise register
    // twice and fire both actions.
    let mut claimed: Vec<String> = Vec::new();

    for (accelerator, action, label) in bindings {
        let trimmed = accelerator.trim();
        if trimmed.is_empty() {
            // An empty string is how the Settings screen expresses "unbound".
            continue;
        }

        let normalised = trimmed.to_ascii_lowercase();
        if claimed.contains(&normalised) {
            warnings.push(format!(
                "{label} shares the shortcut {trimmed} with another action, so it was skipped."
            ));
            continue;
        }

        match register_one(app, trimmed, action) {
            Ok(()) => claimed.push(normalised),
            Err(err) => warnings.push(format!(
                "{label} could not use {trimmed}: {err}. Another application may already own it."
            )),
        }
    }

    warnings
}

fn register_one(app: &AppHandle, accelerator: &str, action: Action) -> Result<(), String> {
    let shortcut: Shortcut = accelerator
        .parse()
        .map_err(|_| format!("{accelerator} is not a valid shortcut"))?;

    app.global_shortcut()
        .on_shortcut(shortcut, move |app, _shortcut, event| {
            // Key-down only. Without this the handler also runs on release and
            // every capture happens twice.
            if event.state() != ShortcutState::Pressed {
                return;
            }
            dispatch(app, action);
        })
        .map_err(|e| e.to_string())
}

fn dispatch(app: &AppHandle, action: Action) {
    match action {
        Action::OpenOverlay => overlay::begin_capture_detached(app, "rectangle"),
        Action::OpenOverlayRectangle => overlay::begin_capture_detached(app, "rectangle"),
        Action::FullScreen => crate::capture_immediate(app, crate::ImmediateMode::FullScreen),
        Action::ActiveWindow => crate::capture_immediate(app, crate::ImmediateMode::ActiveWindow),
    }
}
