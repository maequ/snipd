//! The capture overlay: one screen, one toolbar, every mode.
//!
//! # Why the screen is frozen rather than shown through
//!
//! An earlier version made the overlay transparent so the user dragged over the
//! live desktop. That is simpler, but it breaks the moment a delay timer is
//! involved: the whole point of a timer is to open a menu or hover something
//! first, and a transparent overlay taking focus closes exactly those things.
//!
//! So the overlay instead displays a still of the desktop, captured the instant
//! before it appeared. Menus stay open in the image, nothing shifts mid-drag,
//! and window highlighting stays stable. The backdrop the user sees is a cheap
//! JPEG; the pixels that get *saved* are cropped from the lossless frame still
//! held in memory here, so the display shortcut costs nothing in quality.
//!
//! # Session lifetime
//!
//! A frozen virtual desktop is tens of megabytes, and this process is resident
//! in the tray all day. The session is therefore dropped as soon as a capture
//! completes or is cancelled — never left lying around between uses.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

use crate::capture::mask::{self, Point};
use crate::capture::win::{self, Bounds, Frame, MonitorInfo, VirtualDesktop, WindowTarget};
use crate::capture::{self, CaptureKind, CaptureRecord};
use crate::AppState;

/// Window label for the capture overlay.
pub const OVERLAY_LABEL: &str = "overlay";

/// URI scheme the overlay's backdrop image is served from.
pub const FRAME_SCHEME: &str = "snipd";

/// Everything captured and enumerated at the moment the overlay opened.
pub struct CaptureSession {
    /// Lossless pixels. Every saved capture is cropped from this.
    pub frame: Frame,
    /// JPEG of the same pixels, for the overlay to display. Shared rather than
    /// cloned because the protocol handler serves it on a different thread.
    pub preview: Arc<Vec<u8>>,
    /// Top-level windows, topmost first, for window-mode highlighting.
    pub windows: Vec<WindowTarget>,
    pub monitors: Vec<MonitorInfo>,
    pub desktop: VirtualDesktop,
    /// Changes every session, used to bust the webview's image cache.
    pub token: u64,
    /// Tool the toolbar starts on. Lets a per-mode shortcut jump straight to
    /// the right tool instead of always opening on rectangle.
    pub initial_mode: String,
}

/// What the overlay needs to draw itself.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayState {
    pub desktop: VirtualDesktop,
    pub windows: Vec<WindowTarget>,
    pub monitors: Vec<MonitorInfo>,
    /// URL of the frozen backdrop image.
    pub backdrop_url: String,
    pub initial_mode: String,
}

/// A rectangular selection, from any of the three rectangular modes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RectSelection {
    pub bounds: Bounds,
    /// Which mode produced it, so history can say so later.
    pub kind: CaptureKind,
    #[serde(default)]
    pub source: Option<String>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Open the capture overlay, optionally after a delay.
///
/// During a delay the overlay is closed and nothing is drawn, so the user is
/// free to open a menu or produce a hover state. The screen is frozen only once
/// the delay has elapsed, which is what makes the timer useful at all.
#[tauri::command]
pub async fn begin_capture(
    app: AppHandle,
    delay_ms: Option<u64>,
    mode: Option<String>,
) -> Result<(), String> {
    close_overlay(&app);

    // Get our own window out of the shot. Without this, starting a capture from
    // the main window freezes a screen with the main window sitting in it.
    let hid_main = match app.get_webview_window("main") {
        Some(window) if window.is_visible().unwrap_or(false) => {
            let _ = window.hide();
            true
        }
        _ => false,
    };

    // Hiding a window is not synchronous with the compositor actually redrawing
    // what was behind it, so a capture taken immediately still contains a ghost
    // of it. This is the shortest wait that reliably comes out clean.
    if hid_main {
        tauri::async_runtime::spawn_blocking(|| std::thread::sleep(Duration::from_millis(140)))
            .await
            .map_err(|e| format!("compositor settle failed: {e}"))?;
    }

    let delay = delay_ms.unwrap_or(0);
    if delay > 0 {
        // Blocking sleep on the runtime's blocking pool: this must not occupy an
        // async worker, and deliberately no lock is held across the await.
        tauri::async_runtime::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(delay))
        })
        .await
        .map_err(|e| format!("capture delay failed: {e}"))?;
    }

    freeze(&app, mode.unwrap_or_else(|| "rectangle".to_string()))?;
    show_overlay(&app).await
}

/// Open the overlay from a non-UI trigger (tray or global shortcut).
///
/// Errors are reported to the app rather than returned, because there is no
/// caller to receive them: the user pressed a key with no window in focus.
pub fn begin_capture_detached(app: &AppHandle, mode: &str) {
    let app = app.clone();
    let mode = mode.to_string();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = begin_capture(app.clone(), None, Some(mode)).await {
            eprintln!("[overlay] could not start capture: {err}");
            let _ = app.emit("capture-failed", err);
        }
    });
}

/// Hand the overlay everything it needs on load.
#[tauri::command]
pub fn overlay_state(app: AppHandle) -> Result<OverlayState, String> {
    let state = app.state::<AppState>();
    let session = state
        .session
        .lock()
        .map_err(|_| "capture session lock poisoned".to_string())?;
    let session = session
        .as_ref()
        .ok_or_else(|| "no capture is in progress".to_string())?;

    Ok(OverlayState {
        desktop: session.desktop,
        windows: session.windows.clone(),
        monitors: session.monitors.clone(),
        // Custom schemes are served over http://<scheme>.localhost on Windows.
        // The token defeats the webview's image cache between sessions.
        backdrop_url: format!(
            "http://{FRAME_SCHEME}.localhost/backdrop?v={}",
            session.token
        ),
        initial_mode: session.initial_mode.clone(),
    })
}

/// Save a rectangular selection: region, window or full screen.
#[tauri::command]
pub fn capture_rect(app: AppHandle, selection: RectSelection) -> Result<CaptureRecord, String> {
    finish(&app, |session, state| {
        let cropped = session
            .frame
            .crop(selection.bounds)
            .map_err(|e| e.to_string())?;
        let source = selection
            .source
            .clone()
            .or_else(|| win::monitor_at((selection.bounds.x, selection.bounds.y)).map(|m| m.label));

        let mut settings = state
            .settings
            .lock()
            .map_err(|_| "settings lock poisoned".to_string())?;
        capture::save_frame(cropped, selection.kind, source, &mut settings)
    })
}

/// Save a freeform lasso selection.
#[tauri::command]
pub fn capture_freeform(app: AppHandle, points: Vec<Point>) -> Result<CaptureRecord, String> {
    finish(&app, |session, state| {
        let masked = mask::apply_polygon(&session.frame, &points).map_err(|e| e.to_string())?;
        let source = win::monitor_at(masked.origin).map(|m| m.label);

        let mut settings = state
            .settings
            .lock()
            .map_err(|_| "settings lock poisoned".to_string())?;
        capture::save_frame(masked, CaptureKind::Freeform, source, &mut settings)
    })
}

/// Abandon the capture and release the frozen frame.
#[tauri::command]
pub fn cancel_capture(app: AppHandle) -> Result<(), String> {
    close_overlay(&app);
    clear_session(&app);
    Ok(())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Capture the screen and snapshot what is on it.
fn freeze(app: &AppHandle, initial_mode: String) -> Result<(), String> {
    let frame = win::capture_virtual_desktop().map_err(|e| e.to_string())?;
    let preview = capture::encode_preview(&frame.image)?;

    let session = CaptureSession {
        initial_mode,
        preview: Arc::new(preview),
        // Enumerated now rather than on demand: once the overlay is up it is the
        // topmost window on screen, and a mid-drag re-query could shift the
        // highlight under the user.
        windows: win::capturable_windows(),
        monitors: win::monitors(),
        desktop: win::virtual_desktop(),
        token: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        frame,
    };

    let state = app.state::<AppState>();
    let mut slot = state
        .session
        .lock()
        .map_err(|_| "capture session lock poisoned".to_string())?;
    *slot = Some(session);
    Ok(())
}

/// Build and show the overlay, sized to cover every display.
async fn show_overlay(app: &AppHandle) -> Result<(), String> {
    let desktop = {
        let state = app.state::<AppState>();
        let session = state
            .session
            .lock()
            .map_err(|_| "capture session lock poisoned".to_string())?;
        session
            .as_ref()
            .ok_or_else(|| "no capture is in progress".to_string())?
            .desktop
    };

    let overlay =
        WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("overlay.html".into()))
            .title("Snipd capture")
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .shadow(false)
            // Shown only once positioned, so it never flashes at the wrong size on
            // the wrong display.
            .visible(false)
            .build()
            .map_err(|e| format!("could not create the capture overlay: {e}"))?;

    // Physical pixels throughout. Logical coordinates would be scaled by the DPI
    // of whichever display Windows decides the window belongs to, which is wrong
    // as soon as two displays scale differently.
    overlay
        .set_position(PhysicalPosition::new(desktop.x, desktop.y))
        .map_err(|e| e.to_string())?;
    overlay
        .set_size(PhysicalSize::new(desktop.width, desktop.height))
        .map_err(|e| e.to_string())?;
    overlay.show().map_err(|e| e.to_string())?;
    overlay.set_focus().map_err(|e| e.to_string())?;

    Ok(())
}

/// Shared tail for every capture mode.
///
/// Takes the session (so it cannot be used twice), runs the mode-specific save,
/// announces the outcome, and closes the overlay last so the screen is handed
/// back only once the file is safely written.
fn finish<F>(app: &AppHandle, save: F) -> Result<CaptureRecord, String>
where
    F: FnOnce(&CaptureSession, &AppState) -> Result<CaptureRecord, String>,
{
    let state = app.state::<AppState>();

    let session = {
        let mut slot = state
            .session
            .lock()
            .map_err(|_| "capture session lock poisoned".to_string())?;
        slot.take()
            .ok_or_else(|| "no capture is in progress".to_string())?
    };

    // `inner()` rather than `&state`: it yields a reference tied to the app's
    // own lifetime instead of to this local, which the borrow checker needs in
    // order to hand it to the closure.
    let outcome = save(&session, state.inner());
    // Explicit, to make it obvious that tens of megabytes are released here
    // rather than at some later scope exit.
    drop(session);

    match &outcome {
        Ok(record) => {
            crate::announce_capture(app, record);
            // The editor now lives inside the main window rather than in one of
            // its own, so bringing that window up is all this needs to do. It
            // was hidden to keep it out of the shot, so it has to be brought
            // back deliberately.
            crate::tray::show_main_window(app);
        }
        Err(message) => {
            let _ = app.emit("capture-failed", message);
            crate::tray::show_main_window(app);
        }
    }

    close_overlay(app);
    outcome
}

fn clear_session(app: &AppHandle) {
    let state = app.state::<AppState>();
    // Bound with an explicit `match` rather than `if let`: the temporary a
    // trailing `if let` produces outlives `state`, which the borrow checker
    // rejects.
    let mut slot = match state.session.lock() {
        Ok(slot) => slot,
        Err(_) => return,
    };
    *slot = None;
}

pub fn close_overlay(app: &AppHandle) {
    if let Some(overlay) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = overlay.close();
    }
}

/// Serve the frozen backdrop to the overlay webview.
///
/// Serving the bytes from memory avoids both a disk round-trip and the ~33%
/// inflation that base64-ing a multi-megabyte image over the IPC bridge would
/// cost.
pub fn serve_backdrop(app: &AppHandle) -> Option<Arc<Vec<u8>>> {
    let state = app.try_state::<AppState>()?;
    let session = state.session.lock().ok()?;
    session.as_ref().map(|s| Arc::clone(&s.preview))
}
