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
pub mod record;
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
    /// The recording in progress, if any. Only one at a time: two recordings
    /// would compete for the same encoder and produce two half-speed videos.
    pub recording: Mutex<Option<record::ActiveRecording>>,
    /// Which capture the editor window is showing.
    ///
    /// Held here rather than passed in the editor's URL: a Windows path in a
    /// query string has to survive escaping intact, and anything able to reach
    /// that page could otherwise name a file of its own choosing.
    pub editing: Mutex<Option<EditTarget>>,
}

/// What the editor window was opened on.
#[derive(Debug, Clone)]
pub struct EditTarget {
    pub path: std::path::PathBuf,
    /// True when opened by a fresh capture, which is what decides whether
    /// saving keeps the capture or writes a copy.
    pub reviewing: bool,
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

/// Window label for the editor.
const EDITOR_LABEL: &str = "editor";

/// What the editor window needs in order to draw itself.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EditorTarget {
    path: String,
    file_name: String,
    image_url: String,
    reviewing: bool,
    theme: config::Theme,
}

/// Open a capture in the editor window, creating it if needed.
///
/// A window of its own, rather than the library turning into an editor: taking
/// a capture should put an editor in front of you the way the Snipping Tool
/// does, and leave whatever you already had open alone.
#[tauri::command]
fn open_editor(app: AppHandle, path: String, reviewing: bool) -> Result<(), String> {
    open_editor_window(&app, &path, reviewing)
}

/// Open the editor window. The command above and the capture path both use this.
///
/// Rust drives this rather than the frontend asking for it. The main window is
/// deliberately left hidden when a capture is going to open in the editor, and
/// WebView2 suspends a hidden webview — so an event handler living in that
/// window may simply never run. Anything that must happen after a capture has
/// to be driven from here, where nothing can be asleep.
pub fn open_editor_window(app: &AppHandle, path: &str, reviewing: bool) -> Result<(), String> {
    let target = std::path::PathBuf::from(path);
    if !target.exists() {
        return Err(format!("{path} no longer exists"));
    }

    {
        let state = app.state::<AppState>();
        let mut editing = state
            .editing
            .lock()
            .map_err(|_| "editor lock poisoned".to_string())?;
        *editing = Some(EditTarget {
            path: target,
            reviewing,
        });
    }

    // Reused when already open, so a second capture replaces what is on screen
    // instead of stacking editors up.
    if let Some(window) = app.get_webview_window(EDITOR_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("editor-target-changed", ());
        return Ok(());
    }

    tauri::WebviewWindowBuilder::new(
        app,
        EDITOR_LABEL,
        tauri::WebviewUrl::App("editor.html".into()),
    )
    .title("Snipd editor")
    .inner_size(1040.0, 720.0)
    .min_inner_size(620.0, 460.0)
    .center()
    .build()
    .map_err(|e| format!("could not open the editor: {e}"))?;

    Ok(())
}

/// What the editor window should show.
#[tauri::command]
fn editor_target(state: State<'_, AppState>) -> Result<EditorTarget, String> {
    let editing = state
        .editing
        .lock()
        .map_err(|_| "editor lock poisoned".to_string())?;
    let target = editing
        .as_ref()
        .ok_or_else(|| "no capture is open in the editor".to_string())?;

    let theme = state
        .settings
        .lock()
        .map(|s| s.theme)
        .unwrap_or(config::Theme::System);

    Ok(EditorTarget {
        file_name: target
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        image_url: history::media_url(&target.path),
        path: target.path.to_string_lossy().into_owned(),
        reviewing: target.reviewing,
        theme,
    })
}

/// The editor window is done; refresh the library behind it.
#[tauri::command]
fn editor_finished(app: AppHandle, saved_path: Option<String>) {
    if let Ok(mut editing) = app.state::<AppState>().editing.lock() {
        *editing = None;
    }
    let _ = app.emit("editor-finished", saved_path);
}

/// Totals for the library summary strip.
#[tauri::command]
fn library_stats(state: State<'_, AppState>) -> Result<history::LibraryStats, String> {
    Ok(history::stats(&save_directory(&state)?))
}

/// Rename a capture, keeping its extension.
///
/// Used by the review step so a capture can be given a meaningful name while it
/// is still fresh, which is the only moment anyone actually remembers what it
/// was of.
#[tauri::command]
fn rename_capture(
    path: String,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let directory = save_directory(&state)?;
    let current = std::path::PathBuf::from(&path);

    let root = directory
        .canonicalize()
        .map_err(|e| format!("save folder is unavailable: {e}"))?;
    let canonical = current
        .canonicalize()
        .map_err(|_| format!("{path} no longer exists"))?;
    if !canonical.starts_with(&root) {
        return Err("refusing to rename a file outside the save folder".into());
    }

    let extension = canonical
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "png".into());

    // The same sanitising the automatic namer uses, so a typed name cannot
    // produce something Windows will not accept.
    let stem = naming::sanitise_stem(&new_name);
    let mut target = directory.join(format!("{stem}.{extension}"));

    if target == canonical {
        return Ok(canonical.to_string_lossy().into_owned());
    }

    // Never clobber: if the name is taken, add a suffix rather than destroying
    // whatever is already there.
    let mut attempt = 2;
    while target.exists() {
        target = directory.join(format!("{stem}_{attempt}.{extension}"));
        attempt += 1;
        if attempt > 1000 {
            return Err("could not find a free filename".into());
        }
    }

    std::fs::rename(&canonical, &target).map_err(|e| format!("could not rename: {e}"))?;
    Ok(target.to_string_lossy().into_owned())
}

/// Begin recording the given area.
#[tauri::command]
fn start_recording(bounds: capture::Bounds, state: State<'_, AppState>) -> Result<(), String> {
    let mut slot = state
        .recording
        .lock()
        .map_err(|_| "recording lock poisoned".to_string())?;
    if slot.is_some() {
        return Err("a recording is already in progress".into());
    }

    let (request, output) = {
        let mut settings = state
            .settings
            .lock()
            .map_err(|_| "settings lock poisoned".to_string())?;

        let directory = settings.ensure_save_directory()?;
        let resolved = naming::resolve(naming::NameRequest {
            dir: &directory,
            naming: &settings.naming,
            extension: "mp4",
            taken_at: chrono::Local::now(),
        });

        // Recordings consume the same counter as stills, so a prefix sequence
        // stays continuous across both rather than colliding.
        if let Some(used) = resolved.counter_used {
            settings.naming.counter = used.saturating_add(1);
            let _ = settings.save();
        }

        (
            record::RecordingRequest {
                bounds,
                fps: settings.recording.fps,
                scale_percent: settings.recording.scale_percent,
                bitrate_mbps: settings.recording.bitrate_mbps,
            },
            resolved.path,
        )
    };

    *slot = Some(record::start(request, output)?);
    Ok(())
}

/// Start recording the area the overlay selected.
///
/// Separate from [`start_recording`] because the overlay has to be torn down
/// first — it covers the whole screen, so recording with it still up would
/// capture the overlay rather than what is behind it.
#[tauri::command]
async fn start_recording_from_overlay(
    app: AppHandle,
    bounds: capture::Bounds,
) -> Result<(), String> {
    overlay::close_overlay(&app);
    // Release the frozen desktop. Recording does not need it and it is tens of
    // megabytes.
    release_capture_session(&app);

    // The overlay is destroyed asynchronously, so recording immediately would
    // still catch it in the first frames.
    tauri::async_runtime::spawn_blocking(|| {
        std::thread::sleep(std::time::Duration::from_millis(180))
    })
    .await
    .map_err(|e| e.to_string())?;

    let state = app.state::<AppState>();
    start_recording(bounds, state)?;
    record::show_bar(&app)
}

/// Drop the frozen desktop held for a capture session.
///
/// A free function rather than inline, because binding the guard with `let …
/// else` is what keeps it from outliving the `State` it borrows from — the
/// borrow checker rejects the equivalent `if let` inside a block.
fn release_capture_session(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(mut session) = state.session.lock() else {
        return;
    };
    *session = None;
}

/// Stop the recording and finalise the file.
#[tauri::command]
fn stop_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<record::RecordingOutcome, String> {
    let active = {
        let mut slot = state
            .recording
            .lock()
            .map_err(|_| "recording lock poisoned".to_string())?;
        slot.take()
            .ok_or_else(|| "nothing is recording".to_string())?
    };

    record::hide_bar(&app);
    let outcome = active.stop()?;
    announce_recording(&app, &outcome);
    Ok(outcome)
}

/// Index a finished recording and tell the UI about it.
///
/// Shared by the floating bar and the tray fallback, so a recording stopped
/// either way lands in the library identically. If these diverged, a recording
/// stopped from the tray would be on disk but missing from the Recordings tab.
pub fn announce_recording(app: &AppHandle, outcome: &record::RecordingOutcome) {
    // Recordings go into the same index as stills, so the library has one source
    // of truth rather than two.
    let entry = CaptureRecord {
        id: format!("rec-{}", chrono::Local::now().format("%Y%m%d%H%M%S%3f")),
        path: outcome.path.clone(),
        file_name: outcome.file_name.clone(),
        width: outcome.width,
        height: outcome.height,
        taken_at: chrono::Local::now().to_rfc3339(),
        kind: capture::CaptureKind::Recording,
        source: None,
        bytes: outcome.bytes,
        copied_to_clipboard: false,
        warnings: Vec::new(),
        full_url: history::media_url(std::path::Path::new(&outcome.path)),
        duration_ms: Some(outcome.duration_ms),
    };
    let _ = history::record(&entry);

    let _ = app.emit("recording-complete", outcome);
    tray::show_main_window(app);
}

/// Live state of the recording, for the floating bar.
#[tauri::command]
fn recording_status(state: State<'_, AppState>) -> record::RecordingStatus {
    state
        .recording
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|active| active.status()))
        .unwrap_or(record::RecordingStatus {
            recording: false,
            elapsed_ms: 0,
            frames: 0,
            dropped: 0,
        })
}

/// Apply an edit to a capture in place, optionally renaming it.
///
/// Used by the review step, where the capture was taken seconds ago and the
/// annotated version simply *is* the capture the user wanted. That is different
/// from editing something out of the library later, which writes a copy so an
/// older, already-shared file is never rewritten underneath them.
#[tauri::command]
fn apply_edit(
    path: String,
    png: String,
    new_name: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    use base64::Engine;

    let directory = save_directory(&state)?;
    let target = std::path::PathBuf::from(&path);

    let root = directory
        .canonicalize()
        .map_err(|e| format!("save folder is unavailable: {e}"))?;
    let canonical = target
        .canonicalize()
        .map_err(|_| format!("{path} no longer exists"))?;
    if !canonical.starts_with(&root) {
        return Err("refusing to write outside the save folder".into());
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png.as_bytes())
        .map_err(|e| format!("the edited image was not valid base64: {e}"))?;

    // Written beside the target and renamed over it, so an interrupted write
    // cannot leave a half-written file where a good capture used to be.
    let temporary = canonical.with_extension("editing.part");
    std::fs::write(&temporary, &bytes).map_err(|e| format!("writing the edit: {e}"))?;
    std::fs::rename(&temporary, &canonical).map_err(|e| format!("replacing the capture: {e}"))?;

    match new_name {
        Some(name) if !name.trim().is_empty() => {
            rename_capture(canonical.to_string_lossy().into_owned(), name, state)
        }
        _ => Ok(canonical.to_string_lossy().into_owned()),
    }
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
fn serve_image(app: &AppHandle, path: &str, query: Option<&str>) -> tauri::http::Response<Vec<u8>> {
    let resolved = query.and_then(|q| query_param(q, "k")).and_then(|key| {
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
            // Without this the editor cannot save.
            //
            // These images are served from `snipd.localhost`, which is a
            // different origin from the page. Drawing a cross-origin image onto
            // a canvas *taints* it, and every attempt to read the pixels back —
            // `toBlob`, `toDataURL`, `getImageData` — then throws a SecurityError.
            // The editor's whole export path is a canvas read, so annotating a
            // capture and pressing Save failed silently.
            //
            // Allowing any origin is safe here: the scheme is registered by this
            // application, only ever serves files out of the user's own save
            // folder, and is not reachable from outside the webview.
            .header("Access-Control-Allow-Origin", "*")
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
            recording: Mutex::new(None),
            editing: Mutex::new(None),
        })
        // Serves the overlay's frozen backdrop straight from memory. Going via
        // disk or base64-over-IPC would both add a visible delay before the
        // overlay can draw.
        .register_uri_scheme_protocol(overlay::FRAME_SCHEME, |ctx, request| {
            serve_image(
                ctx.app_handle(),
                request.uri().path(),
                request.uri().query(),
            )
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
            //
            // Routed through the same helper the tray uses rather than calling
            // `show` directly, so there is one way to reveal this window.
            //
            // Nothing else may touch the window for a moment after this. An
            // earlier version re-asserted `set_focus` from a background thread
            // shortly after startup, as a guard against the window coming up
            // minimised; it *caused* that symptom on nine launches in ten,
            // because forcing foreground on Windows minimises and restores the
            // window and doing it off the main thread mid-startup leaves it
            // iconic. Showing once, here, measured clean 28 times out of 28.
            if !start_hidden {
                tray::show_main_window(handle);
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
            apply_edit,
            library_stats,
            rename_capture,
            start_recording,
            stop_recording,
            recording_status,
            open_editor,
            editor_target,
            editor_finished,
            start_recording_from_overlay,
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
