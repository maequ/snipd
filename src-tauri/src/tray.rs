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
    let capture_screen =
        MenuItem::with_id(app, "fullscreen", "Capture full screen", true, None::<&str>)?;
    let capture_window =
        MenuItem::with_id(app, "window", "Capture active window", true, None::<&str>)?;
    // Always present rather than added and removed as recording starts and
    // stops. The floating bar is a separate window, and a recording that cannot
    // be stopped because its bar failed to appear would quietly fill a disk —
    // so there is a second way to stop one that depends on nothing but the tray.
    let stop_recording =
        MenuItem::with_id(app, "stop-recording", "Stop recording", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Snipd", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Snipd", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &new_capture,
            &capture_screen,
            &capture_window,
            &PredefinedMenuItem::separator(app)?,
            &stop_recording,
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
            "stop-recording" => stop_recording_from_tray(app),
            "open" => show_main_window(app),
            "quit" => quit_app(app),
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

/// Quit, and make sure it happens.
///
/// `app.exit` asks the event loop to wind down, which it cannot do if a thread
/// is wedged — and a process that lingers after Quit is worse than an abrupt
/// one, because the single-instance guard then makes the *next* launch do
/// nothing at all. That is exactly what "I quit it and now it will not open"
/// looks like. So: ask nicely, then insist.
fn quit_app(app: &AppHandle) {
    crate::log::line("[app] quitting");
    app.exit(0);

    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        crate::log::line("[app] still alive after exit(0); terminating");
        let _ = app;
        std::process::exit(0);
    });
}

/// Stop a recording from the tray.
///
/// Does nothing when nothing is recording, which is why the menu item can stay
/// permanently enabled: an item that is usually greyed out is one people stop
/// looking at, and this is the fallback that has to work when the floating bar
/// has not.
fn stop_recording_from_tray(app: &AppHandle) {
    let app = app.clone();
    // Finalising an MP4 writes its index, which is slow enough that doing it on
    // the menu thread would visibly hang the tray.
    tauri::async_runtime::spawn_blocking(move || {
        let recording = {
            let state = app.state::<crate::AppState>();
            let Ok(mut slot) = state.recording.lock() else {
                return;
            };
            slot.take()
        };

        let Some(active) = recording else {
            return;
        };

        crate::record::hide_bar(&app);
        match active.stop() {
            Ok(outcome) => crate::announce_recording(&app, &outcome),
            Err(err) => crate::log::line(format!("[record] stopping from the tray failed: {err}")),
        }
    });
}

/// Reveal and focus the main window, recreating it if it is gone.
///
/// The recreation path is not a nicety. Closing the window only hides it while
/// "keep Snipd in the tray" is on; with that setting off the window is destroyed
/// for real, and the app carries on running in the tray. Without rebuilding it
/// here, every route back — the tray icon, the tray menu, a second launch — would
/// silently do nothing, and the app would be running with no way to ever show a
/// window again. That looks exactly like "it won't open".
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Un-minimise before showing, so a window that was minimised rather
        // than hidden comes back at its real size instead of being shown still
        // iconic.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    match tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
        .title("Snipd")
        .inner_size(980.0, 760.0)
        .min_inner_size(520.0, 420.0)
        .center()
        .build()
    {
        Ok(window) => {
            let _ = window.show();
            let _ = window.set_focus();
        }
        Err(err) => crate::log::line(format!(
            "[window] could not recreate the main window: {err}"
        )),
    }
}
