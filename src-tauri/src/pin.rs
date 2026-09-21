//! Pinned captures: small always-on-top windows that float over everything.
//!
//! The point is to keep a screenshot visible while working in another app, so
//! each pin is its own independent top-level window rather than a panel inside
//! the main one. Several can be open at once, and closing one leaves the others
//! alone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

use crate::AppState;

/// Label prefix for pin windows, so they can be told apart from main/overlay/editor.
const PIN_PREFIX: &str = "pin-";

/// A pinned window never opens larger than this fraction of the screen —
/// a full-desktop capture pinned at 1:1 would cover the very thing it is
/// meant to be referenced alongside.
const MAX_SCREEN_FRACTION: f64 = 0.55;

/// Smallest useful pin, in physical pixels.
const MIN_EDGE: u32 = 120;

/// What a pin window needs to draw itself.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinState {
    pub image_url: String,
    pub file_name: String,
}

/// Tracks which capture each pin window is showing.
pub type PinRegistry = Mutex<HashMap<String, PathBuf>>;

/// Open a capture as a floating always-on-top window.
#[tauri::command]
pub fn pin_capture(app: AppHandle, path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.exists() {
        return Err(format!("{path} no longer exists"));
    }

    let (width, height) = fit_to_screen(&app, &target)?;

    // Each pin gets a unique label; reusing one would replace an existing pin
    // rather than adding to it, which defeats the purpose.
    let label = format!("{PIN_PREFIX}{}", next_pin_id(&app));

    {
        let state = app.state::<AppState>();
        let mut pins = state
            .pins
            .lock()
            .map_err(|_| "pin registry lock poisoned".to_string())?;
        pins.insert(label.clone(), target);
    }

    let window = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("pin.html".into()))
        .title("Snipd pin")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(true)
        .shadow(true)
        .visible(false)
        .build()
        .map_err(|e| format!("could not create the pin window: {e}"))?;

    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())?;

    Ok(())
}

/// Tell a pin window which capture it is showing.
#[tauri::command]
pub fn pin_state(
    window: tauri::Window,
    state: tauri::State<'_, AppState>,
) -> Result<PinState, String> {
    let label = window.label().to_string();
    let path = {
        let pins = state
            .pins
            .lock()
            .map_err(|_| "pin registry lock poisoned".to_string())?;
        pins.get(&label)
            .cloned()
            .ok_or_else(|| "this pin has no capture".to_string())?
    };

    let key = crate::history::key_for(&path).ok_or_else(|| "capture is unreadable".to_string())?;
    let directory = {
        let settings = state
            .settings
            .lock()
            .map_err(|_| "settings lock poisoned".to_string())?;
        settings.save_directory.clone()
    };
    // Warm the key cache so the image protocol can answer without a folder scan.
    let _ = crate::history::resolve_thumbnail_key(&key, &directory);

    Ok(PinState {
        image_url: format!(
            "http://{}.localhost/full?k={key}",
            crate::overlay::FRAME_SCHEME
        ),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    })
}

/// Close a pin and forget it.
#[tauri::command]
pub fn close_pin(window: tauri::Window, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let label = window.label().to_string();
    if let Ok(mut pins) = state.pins.lock() {
        pins.remove(&label);
    }
    window.close().map_err(|e| e.to_string())
}

/// Lowest unused pin number, so labels do not collide with a still-open pin.
fn next_pin_id(app: &AppHandle) -> u64 {
    let mut id = 1;
    while app
        .get_webview_window(&format!("{PIN_PREFIX}{id}"))
        .is_some()
    {
        id += 1;
    }
    id
}

/// Size a pin to the image, scaled down to stay a reasonable fraction of the screen.
fn fit_to_screen(app: &AppHandle, path: &Path) -> Result<(u32, u32), String> {
    // Reads only the header, not the whole image.
    let (image_width, image_height) = image::image_dimensions(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;

    let desktop = crate::capture::win::virtual_desktop();
    let max_width = (desktop.width as f64 * MAX_SCREEN_FRACTION) as u32;
    let max_height = (desktop.height as f64 * MAX_SCREEN_FRACTION) as u32;

    let scale = (max_width as f64 / image_width as f64)
        .min(max_height as f64 / image_height as f64)
        // Never scale *up*: a tiny capture pinned at 3x would just be blurry.
        .min(1.0);

    let _ = app;
    Ok((
        ((image_width as f64 * scale) as u32).max(MIN_EDGE),
        ((image_height as f64 * scale) as u32).max(MIN_EDGE),
    ))
}
