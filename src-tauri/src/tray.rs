//! System tray icon and menu.
//!
//! The tray is what makes the app usable without a window: Snipd is meant to sit
//! resident and be summoned, not opened. Closing the main window hides it here
//! rather than quitting, so the global shortcut keeps working.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::overlay;

/// Build the tray icon and wire its menu.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let new_capture = MenuItem::with_id(app, "capture", "New capture", true, None::<&str>)?;
    let capture_screen = MenuItem::with_id(app, "fullscreen", "Capture full screen", true, None::<&str>)?;
    let capture_window = MenuItem::with_id(app, "window", "Capture active window", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Snipd", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Snipd", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &new_capture,
            &capture_screen,
            &capture_window,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    TrayIconBuilder::with_id("snipd-tray")
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("the bundle always provides a default window icon"),
        )
        .tooltip("Snipd")
        .menu(&menu)
        // Left click opens the window; the menu belongs on right click, which is
        // what Windows users expect from a tray icon.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "capture" => overlay::begin_capture_detached(app, "rectangle"),
            "fullscreen" => crate::capture_immediate(app, crate::ImmediateMode::FullScreen),
            "window" => crate::capture_immediate(app, crate::ImmediateMode::ActiveWindow),
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

/// Reveal and focus the main window, un-minimising it if needed.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
