//! Snipd — a screenshot tool for Windows that never loses a capture.
//!
//! This module wires the capture engine to the UI. The interesting logic lives
//! in the sibling modules; what happens here is state ownership, the Tauri
//! command surface, and the lifecycle of the region-selection overlay.

pub mod branding;
pub mod capture;
pub mod clipboard;
pub mod config;
pub mod naming;

use std::sync::Mutex;

use tauri::{
    Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl, WebviewWindowBuilder,
};

use capture::win::{self, Bounds, Frame, MonitorInfo, VirtualDesktop};
use capture::{CaptureKind, CaptureRecord, CaptureRequest};
use config::{NamingSettings, Settings};

/// Window label for the region-selection overlay.
const OVERLAY_LABEL: &str = "overlay";

/// Application state shared across commands.
pub struct AppState {
    /// The live settings. Mutable because prefix-mode naming consumes a counter.
    settings: Mutex<Settings>,
    /// Pixels frozen at the moment region selection began.
    ///
    /// Region capture reads the screen *before* putting its overlay up, then
    /// crops from this. Re-reading the screen after the overlay is visible would
    /// capture the overlay itself.
    frozen: Mutex<Option<Frame>>,
}

/// What the overlay needs in order to draw itself.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionSession {
    /// The area the overlay covers, in virtual-screen coordinates.
    desktop: VirtualDesktop,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Connected displays, primary first.
#[tauri::command]
fn list_monitors() -> Vec<MonitorInfo> {
    win::monitors()
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

/// Render a filename preview without touching the disk.
///
/// Used by the Settings screen so the user sees the effect of a naming change
/// before committing to it. Mirrors what the installer wizard shows.
#[tauri::command]
fn filename_preview(naming: NamingSettings, extension: String) -> String {
    naming::preview(&naming, &extension, chrono::Local::now())
}

/// Take a capture immediately and save it.
///
/// Covers full-screen and active-window modes. Region capture goes through the
/// overlay commands below instead.
#[tauri::command]
fn capture(request: CaptureRequest, state: State<'_, AppState>) -> Result<CaptureRecord, String> {
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_string())?;
    capture::capture_and_save(request, &mut settings)
}

/// Freeze the screen and raise the selection overlay.
///
/// The overlay is a single transparent, always-on-top window stretched across
/// the whole virtual desktop. One window rather than one per display, because a
/// selection that spans two monitors must be a single continuous drag — and
/// because the frozen frame it crops from is itself one continuous image.
#[tauri::command]
async fn begin_region_capture(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<RegionSession, String> {
    // Freeze first, so the overlay can never appear in its own capture.
    let frame = win::capture_virtual_desktop().map_err(|e| e.to_string())?;
    let desktop = win::virtual_desktop();

    {
        let mut frozen = state
            .frozen
            .lock()
            .map_err(|_| "frozen-frame lock poisoned".to_string())?;
        *frozen = Some(frame);
    }

    // Reuse the overlay window if a previous selection left it around.
    if let Some(existing) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = existing.close();
    }

    let overlay = WebviewWindowBuilder::new(
        &app,
        OVERLAY_LABEL,
        WebviewUrl::App("overlay.html".into()),
    )
    .title("Snipd selection")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    // Built hidden and shown only once it is positioned, so the user never sees
    // it flash at the wrong size on the wrong monitor.
    .visible(false)
    .build()
    .map_err(|e| format!("could not create the selection overlay: {e}"))?;

    // Position in *physical* pixels. Logical coordinates would be scaled by the
    // DPI of whichever display Windows decided the window belongs to, which is
    // wrong the moment displays have different scaling.
    overlay
        .set_position(PhysicalPosition::new(desktop.x, desktop.y))
        .map_err(|e| e.to_string())?;
    overlay
        .set_size(PhysicalSize::new(desktop.width, desktop.height))
        .map_err(|e| e.to_string())?;
    overlay.show().map_err(|e| e.to_string())?;
    overlay.set_focus().map_err(|e| e.to_string())?;

    Ok(RegionSession { desktop })
}

/// The bounding box of all displays. The overlay uses this to convert the CSS
/// pixels it draws in back into virtual-screen coordinates.
#[tauri::command]
fn get_virtual_desktop() -> VirtualDesktop {
    win::virtual_desktop()
}

/// Crop the frozen frame to the user's selection and save it.
///
/// The result is announced with the `capture-complete` (or `capture-failed`)
/// event rather than only being returned. The caller is the overlay window,
/// which this function closes — so its pending response would be dropped along
/// with its webview. Events reach the main window, which is what needs to know.
#[tauri::command]
fn finish_region_capture(
    bounds: Bounds,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<CaptureRecord, String> {
    let outcome = save_region(bounds, &state);

    match &outcome {
        Ok(record) => {
            let _ = app.emit("capture-complete", record);
        }
        Err(message) => {
            let _ = app.emit("capture-failed", message);
        }
    }

    // Closed last, so the save is already done and reported by the time the
    // screen is handed back to the user.
    close_overlay(&app);
    outcome
}

/// The save half of [`finish_region_capture`], split out so the overlay is
/// closed on both the success and failure paths.
fn save_region(bounds: Bounds, state: &State<'_, AppState>) -> Result<CaptureRecord, String> {
    let frame = {
        let mut frozen = state
            .frozen
            .lock()
            .map_err(|_| "frozen-frame lock poisoned".to_string())?;
        frozen
            .take()
            .ok_or_else(|| "no region selection is in progress".to_string())?
    };

    let cropped = frame.crop(bounds).map_err(|e| e.to_string())?;
    let source = win::monitor_at((bounds.x, bounds.y)).map(|m| m.label);

    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_string())?;

    capture::save_frame(cropped, CaptureKind::Region, source, &mut settings)
}

/// Abandon a selection, discarding the frozen frame.
#[tauri::command]
fn cancel_region_capture(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    close_overlay(&app);
    let mut frozen = state
        .frozen
        .lock()
        .map_err(|_| "frozen-frame lock poisoned".to_string())?;
    // Dropping the frame here matters: a full virtual-desktop capture is tens of
    // megabytes, and this process stays resident in the tray all day.
    *frozen = None;
    Ok(())
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

fn close_overlay(app: &tauri::AppHandle) {
    if let Some(overlay) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = overlay.close();
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Must happen before any window exists, or Windows will hand us virtualised
    // coordinates and blurry captures on scaled displays.
    win::ensure_dpi_awareness();

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    let settings = Settings::load_or_seed(exe_dir.as_deref());

    // Create the save folder up front so the very first capture is not the thing
    // that discovers the configured path is unusable.
    if let Err(err) = settings.ensure_save_directory() {
        eprintln!("[startup] save directory problem: {err}");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            settings: Mutex::new(settings),
            frozen: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            list_monitors,
            get_settings,
            filename_preview,
            capture,
            get_virtual_desktop,
            begin_region_capture,
            finish_region_capture,
            cancel_region_capture,
            reveal_in_explorer,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Snipd");
}
