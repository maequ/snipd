//! Snipd — a screenshot tool for Windows that never loses a capture.
//!
//! This module owns application state and wires everything together. The
//! interesting logic lives in the sibling modules:
//!
//! * [`capture`] — reading pixels and getting them safely onto disk
//! * [`overlay`] — the capture overlay and its session lifetime
//! * [`tray`] / [`shortcuts`] — summoning the app with no window open
//! * [`config`] / [`naming`] — settings and filenames

pub mod branding;
pub mod capture;
pub mod clipboard;
pub mod config;
pub mod history;
pub mod naming;
pub mod overlay;
pub mod pin;
pub mod shortcuts;
pub mod tray;

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

use capture::win::{self, MonitorInfo, VirtualDesktop};
use capture::{CaptureRecord, CaptureRequest};
use config::{NamingSettings, Settings};
use overlay::CaptureSession;

/// Command-line flag the autostart registration passes, so "start minimised"
/// applies to logging in and not to the user double-clicking the app.
const AUTOSTART_FLAG: &str = "--autostart";

/// Application state shared across commands.
pub struct AppState {
    /// Live settings. Mutable because prefix-mode naming consumes a counter.
    pub settings: Mutex<Settings>,
    /// The in-progress capture, if the overlay is open. Holds a frozen copy of
    /// the whole virtual desktop, so it is cleared the moment it is finished
    /// with rather than lingering in a tray-resident process.
    pub session: Mutex<Option<CaptureSession>>,
    /// Shortcuts that could not be bound at startup, for Settings to surface.
    pub shortcut_warnings: Mutex<Vec<String>>,
    /// Which capture each pinned window is showing, keyed by window label.
    pub pins: pin::PinRegistry,
}

/// A capture taken without showing the overlay.
#[derive(Debug, Clone, Copy)]
pub enum ImmediateMode {
    FullScreen,
    ActiveWindow,
}

/// Capture and save straight away, reporting the result by event.
///
/// Used by the tray menu and the direct-mode global shortcuts, where there is no
/// caller waiting on a return value.
pub fn capture_immediate(app: &AppHandle, mode: ImmediateMode) {
    let app = app.clone();

    // Off the UI thread: encoding a full-resolution PNG takes long enough that
    // doing it inline would visibly stall the tray menu closing.
    tauri::async_runtime::spawn_blocking(move || {
        let request = match mode {
            ImmediateMode::FullScreen => CaptureRequest::FullScreen { monitor_id: None },
            ImmediateMode::ActiveWindow => CaptureRequest::ActiveWindow,
        };

        let state = app.state::<AppState>();
        let mut settings = match state.settings.lock() {
            Ok(guard) => guard,
            Err(_) => {
                let _ = app.emit("capture-failed", "settings lock poisoned");
                return;
            }
        };

        match capture::capture_and_save(request, &mut settings) {
            Ok(record) => {
                announce_capture(&app, &record);
            }
            Err(err) => {
                let _ = app.emit("capture-failed", err);
            }
        }
    });
}

/// Announce a finished capture: tell the UI, and toast if the user wants one.
///
/// Every successful capture goes through here so the notification setting means
/// the same thing no matter which route produced the capture — overlay, tray, or
/// a direct-mode shortcut.
pub fn announce_capture(app: &AppHandle, record: &CaptureRecord) {
    let _ = app.emit("capture-complete", record);

    let wanted = app
        .state::<AppState>()
        .settings
        .lock()
        .map(|s| s.notifications.show_saved_toast)
        .unwrap_or(false);

    if !wanted {
        return;
    }

    use tauri_plugin_notification::NotificationExt;
    // A failed toast is never worth surfacing: the capture is already saved,
    // and notifications can be disabled at the OS level entirely.
    let _ = app
        .notification()
        .builder()
        .title("Screenshot saved")
        .body(format!(
            "{}  ·  {} x {}",
            record.file_name, record.width, record.height
        ))
        .show();
}

/// Apply the retention setting, if it is switched on.
///
/// Runs at startup and after settings are saved, rather than on a timer: a tool
/// that only deletes while it happens to be running is easier to reason about
/// than one with a background scheduler, and the difference to the user is
/// nothing.
pub fn apply_retention(app: &AppHandle) {
    let (enabled, days, directory) = {
        let state = app.state::<AppState>();
        let Ok(settings) = state.settings.lock() else {
            return;
        };
        (
            settings.retention.enabled,
            settings.retention.days,
            settings.save_directory.clone(),
        )
    };

    if !enabled {
        return;
    }

    match history::prune(&directory, days) {
        Ok(0) => {}
        Ok(count) => eprintln!("[retention] removed {count} capture(s) older than {days} days"),
        Err(err) => eprintln!("[retention] {err}"),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Connected displays, primary first.
#[tauri::command]
fn list_monitors() -> Vec<MonitorInfo> {
    win::monitors()
}

/// Bounding box of all displays.
#[tauri::command]
fn get_virtual_desktop() -> VirtualDesktop {
    win::virtual_desktop()
}

/// The current settings, for the UI to render.
#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    state
        .settings
        .lock()
        .map(|s| s.clone())
        .map_err(|_| "settings lock poisoned".to_string())
}

/// Shortcuts that failed to bind, so Settings can explain why one does nothing.
#[tauri::command]
fn get_shortcut_warnings(state: State<'_, AppState>) -> Vec<String> {
    state
        .shortcut_warnings
        .lock()
        .map(|w| w.clone())
        .unwrap_or_default()
}

/// Render a filename preview without touching the disk.
#[tauri::command]
fn filename_preview(naming: NamingSettings, extension: String) -> String {
    naming::preview(&naming, &extension, chrono::Local::now())
}

/// Capture immediately from the UI, bypassing the overlay.
#[tauri::command]
fn capture(request: CaptureRequest, state: State<'_, AppState>) -> Result<CaptureRecord, String> {
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_string())?;
    capture::capture_and_save(request, &mut settings)
}

/// Open File Explorer with the given file selected.
#[tauri::command]
fn reveal_in_explorer(path: String) -> Result<(), String> {
    let target = std::path::Path::new(&path);
    if !target.exists() {
        return Err(format!("{path} no longer exists"));
    }

    std::process::Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn()
        .map_err(|e| format!("could not open Explorer: {e}"))?;
    Ok(())
}

/// Open the save folder in File Explorer.
#[tauri::command]
fn open_save_folder(state: State<'_, AppState>) -> Result<(), String> {
    let directory = save_directory(&state)?;
    // Create it first: the folder may not exist yet if nothing has been
    // captured, and opening a missing path just fails silently in Explorer.
    std::fs::create_dir_all(&directory)
        .map_err(|e| format!("could not create {}: {e}", directory.display()))?;

    std::process::Command::new("explorer")
        .arg(directory.as_os_str())
        .spawn()
        .map_err(|e| format!("could not open Explorer: {e}"))?;
    Ok(())
}

/// Hide the main window to the tray.
#[tauri::command]
fn hide_to_tray(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

/// A page of capture history, newest first.
#[tauri::command]
fn history_list(
    query: history::HistoryQuery,
    state: State<'_, AppState>,
) -> Result<history::HistoryPage, String> {
    let directory = save_directory(&state)?;
    history::list(&query, &directory)
}

/// Delete one capture, and its cached thumbnail.
#[tauri::command]
fn history_delete(path: String, state: State<'_, AppState>) -> Result<(), String> {
    let directory = save_directory(&state)?;
    history::delete(std::path::Path::new(&path), &directory)
}

/// Put an existing capture back on the clipboard.
#[tauri::command]
fn copy_capture(path: String) -> Result<(), String> {
    let image = image::open(&path)
        .map_err(|e| format!("could not read {path}: {e}"))?
        .to_rgba8();
    clipboard::copy_image(&image).map(|_| ())
}

/// Save an annotated copy of a capture.
///
/// Deliberately a *copy*. The original was auto-saved the instant it was taken
/// and is the one thing this app promises never to lose, so an edit must not be
/// able to destroy it.
#[tauri::command]
fn save_edited(png: String, state: State<'_, AppState>) -> Result<CaptureRecord, String> {
    use base64::Engine;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png.as_bytes())
        .map_err(|e| format!("the edited image was not valid base64: {e}"))?;

    let image = image::load_from_memory(&bytes)
        .map_err(|e| format!("the edited image could not be decoded: {e}"))?
        .to_rgba8();

    let frame = capture::win::Frame {
        image,
        origin: (0, 0),
    };

    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_string())?;

    capture::save_frame(
        frame,
        capture::CaptureKind::Edited,
        Some("Edited".to_string()),
        &mut settings,
    )
}

/// Copy an annotated image to the clipboard without saving it.
///
/// Separate from [`save_edited`] because wanting a marked-up screenshot on the
/// clipboard is not the same as wanting another file in the library.
#[tauri::command]
fn copy_edited(png: String) -> Result<(), String> {
    use base64::Engine;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png.as_bytes())
        .map_err(|e| format!("the edited image was not valid base64: {e}"))?;

    let image = image::load_from_memory(&bytes)
        .map_err(|e| format!("the edited image could not be decoded: {e}"))?
        .to_rgba8();

    clipboard::copy_image(&image).map(|_| ())
}

/// Replace the settings and act on anything that has side effects.
///
/// Shortcuts are re-registered here rather than only at startup, so changing a
/// hotkey takes effect immediately instead of at the next launch. The returned
/// warnings describe any that could not be bound.
#[tauri::command]
fn update_settings(
    app: AppHandle,
    settings: Settings,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut incoming = settings;
    // Clamp anything nonsensical before it is persisted, so a hand-edited or
    // mistyped value cannot put the app into a broken state.
    incoming.normalise();

    let shortcuts = incoming.shortcuts.clone();
    let launch_on_login = incoming.startup.launch_on_login;

    incoming.save()?;
    // Create the new folder now rather than discovering at capture time that it
    // is unusable.
    let _ = incoming.ensure_save_directory();

    {
        let mut current = state
            .settings
            .lock()
            .map_err(|_| "settings lock poisoned".to_string())?;
        *current = incoming;
    }

    let warnings = shortcuts::apply(&app, &shortcuts);
    if let Ok(mut slot) = state.shortcut_warnings.lock() {
        *slot = warnings.clone();
    }

    if let Err(err) = set_launch_on_login(launch_on_login) {
        eprintln!("[startup] {err}");
    }

    apply_retention(&app);

    Ok(warnings)
}

/// Add or remove the Run-key entry that starts Snipd at login.
///
/// The registry Run key is used rather than a Startup-folder shortcut because it
/// takes arguments cleanly — `--autostart` is what tells the app that "start
/// minimised" applies to this launch, as opposed to the user opening it.
fn set_launch_on_login(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let command = format!("\"{}\" {}", exe.display(), AUTOSTART_FLAG);

    let action = if enabled {
        format!(
            "New-ItemProperty -Path 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' \
             -Name 'Snipd' -Value '{command}' -PropertyType String -Force | Out-Null"
        )
    } else {
        "Remove-ItemProperty -Path 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' \
         -Name 'Snipd' -ErrorAction SilentlyContinue"
            .to_string()
    };

    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &action])
        .status()
        .map_err(|e| format!("could not update the startup entry: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("the startup entry could not be updated".to_string())
    }
}

/// Read the configured save folder without holding the lock any longer than needed.
fn save_directory(state: &State<'_, AppState>) -> Result<std::path::PathBuf, String> {
    state
        .settings
        .lock()
        .map(|s| s.save_directory.clone())
        .map_err(|_| "settings lock poisoned".to_string())
}

// ---------------------------------------------------------------------------
// Image protocol
// ---------------------------------------------------------------------------

/// Serve images to the webview straight from memory or the thumbnail cache.
///
/// Two routes:
/// * `/backdrop` — the frozen desktop the capture overlay draws on
/// * `/thumb?k=…` — a history thumbnail, generated on first request and cached
///
/// Thumbnails are addressed by an opaque key rather than a file path, so the
/// webview cannot ask this handler for an arbitrary file: a key only resolves if
/// it matches something currently sitting in the save folder.
fn serve_image(
    app: &AppHandle,
    path: &str,
    query: Option<&str>,
) -> tauri::http::Response<Vec<u8>> {
    let resolved = query
        .and_then(|q| query_param(q, "k"))
        .and_then(|key| {
            let directory = app
                .state::<AppState>()
                .settings
                .lock()
                .ok()
                .map(|s| s.save_directory.clone())?;
            history::resolve_thumbnail_key(&key, &directory)
        });

    let (bytes, mime) = match path {
        "/backdrop" => (
            overlay::serve_backdrop(app).map(|b| b.as_ref().clone()),
            "image/jpeg",
        ),
        "/thumb" => (
            resolved.and_then(|file| history::thumbnail(&file).ok()),
            "image/jpeg",
        ),
        // The editor needs the real pixels, not a thumbnail of them.
        "/full" => match resolved {
            Some(file) => {
                let mime = if file
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("png"))
                    .unwrap_or(false)
                {
                    "image/png"
                } else {
                    "image/jpeg"
                };
                (std::fs::read(&file).ok(), mime)
            }
            None => (None, "image/png"),
        },
        _ => (None, "image/jpeg"),
    };

    let builder = tauri::http::Response::builder();
    match bytes {
        Some(body) => builder
            .header("Content-Type", mime)
            // Keys already change when a file does, but this removes any chance
            // of a stale frame or thumbnail surviving in the webview cache.
            .header("Cache-Control", "no-store")
            .body(body)
            .unwrap_or_else(|_| empty(500)),
        None => empty(404),
    }
}

fn empty(status: u16) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .body(Vec::new())
        .expect("an empty response is always valid")
}

/// Pull one value out of a URL query string.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.to_string())
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Must happen before any window exists, or Windows hands back virtualised
    // coordinates and blurry captures on scaled displays.
    win::ensure_dpi_awareness();

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let settings = Settings::load_or_seed(exe_dir.as_deref());

    // Create the save folder now, so the first capture is not the thing that
    // discovers the configured path is unusable.
    if let Err(err) = settings.ensure_save_directory() {
        eprintln!("[startup] save directory problem: {err}");
    }

    let launched_at_login = std::env::args().any(|arg| arg == AUTOSTART_FLAG);
    let start_hidden = launched_at_login && settings.startup.start_minimised;

    tauri::Builder::default()
        // Must be registered first, before any window exists: a second launch
        // is intercepted here and simply focuses the copy already running,
        // rather than starting a rival that cannot claim the global shortcuts.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(AppState {
            settings: Mutex::new(settings),
            session: Mutex::new(None),
            shortcut_warnings: Mutex::new(Vec::new()),
            pins: Mutex::new(std::collections::HashMap::new()),
        })
        // Serves the overlay's frozen backdrop straight from memory. Going via
        // disk or base64-over-IPC would both add a visible delay before the
        // overlay can draw.
        .register_uri_scheme_protocol(overlay::FRAME_SCHEME, |ctx, request| {
            serve_image(ctx.app_handle(), request.uri().path(), request.uri().query())
        })
        .on_window_event(|window, event| {
            // Closing the main window hides it instead of quitting, so the
            // global shortcut keeps working. Configurable, because some people
            // genuinely want the X button to mean quit.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != "main" {
                    return;
                }

                let close_to_tray = window
                    .app_handle()
                    .state::<AppState>()
                    .settings
                    .lock()
                    .map(|s| s.window.close_to_tray)
                    .unwrap_or(true);

                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(move |app| {
            let handle = app.handle();

            tray::build(handle)?;

            let warnings = {
                let state = handle.state::<AppState>();
                let settings = state
                    .settings
                    .lock()
                    .map(|s| s.shortcuts.clone())
                    .unwrap_or_default();
                shortcuts::apply(handle, &settings)
            };

            for warning in &warnings {
                eprintln!("[shortcuts] {warning}");
            }
            if let Ok(mut slot) = handle.state::<AppState>().shortcut_warnings.lock() {
                *slot = warnings;
            }

            apply_retention(handle);

            // The window is built hidden so that starting at login never flashes
            // it on screen. A manual launch shows it immediately.
            if !start_hidden {
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_monitors,
            get_virtual_desktop,
            get_settings,
            get_shortcut_warnings,
            filename_preview,
            capture,
            reveal_in_explorer,
            hide_to_tray,
            open_save_folder,
            history_list,
            history_delete,
            copy_capture,
            save_edited,
            copy_edited,
            update_settings,
            pin::pin_capture,
            pin::pin_state,
            pin::close_pin,
            overlay::begin_capture,
            overlay::overlay_state,
            overlay::capture_rect,
            overlay::capture_freeform,
            overlay::cancel_capture,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Snipd");
}
