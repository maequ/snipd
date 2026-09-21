//! Screen recording.
//!
//! A recording runs on its own thread: grab a frame, hand it to the encoder,
//! wait until the next frame is due, repeat. Keeping it off the UI thread means
//! the app stays responsive while recording, which it must — the stop control
//! has to work.
//!
//! # Keeping time
//!
//! The loop paces itself against a fixed schedule rather than sleeping a fixed
//! amount between frames. Sleeping `1/fps` after each frame would make every
//! recording drift slower than real time, because the grab and encode take time
//! too. Instead each frame has a deadline, and the loop waits until it.
//!
//! When a frame cannot be produced in time — a large region on a slow machine —
//! the loop *drops* it rather than letting the recording fall behind, and counts
//! what it dropped. A dropped frame is visible as a small stutter; silently
//! sliding the timeline would instead make the whole video drift out of step
//! with reality, which is worse and much harder to notice.

pub mod encoder;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::capture::win::{self, Bounds, FrameGrabber};
use encoder::Encoder;

/// Bits spent per pixel per frame when deriving a bitrate floor.
///
/// Tuned for screen content rather than camera footage. Large flat areas cost
/// almost nothing to encode, so most of the budget goes on text edges — which
/// are exactly what turns to mush when a screen recording is under-encoded.
const BITS_PER_PIXEL_PER_FRAME: f64 = 0.15;

/// How a recording was asked for.
#[derive(Debug, Clone, Copy)]
pub struct RecordingRequest {
    pub bounds: Bounds,
    pub fps: u32,
    /// Percentage of the captured size to encode at. 100 means native.
    pub scale_percent: u32,
    pub bitrate_mbps: u32,
}

/// What a finished recording produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingOutcome {
    pub path: String,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub dropped: u64,
    pub duration_ms: u64,
    pub bytes: u64,
}

/// Live state of a recording in progress, readable from the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub recording: bool,
    pub elapsed_ms: u64,
    pub frames: u64,
    pub dropped: u64,
}

/// A recording in flight.
pub struct ActiveRecording {
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    started: Instant,
    worker: Option<JoinHandle<Result<RecordingOutcome, String>>>,
}

impl ActiveRecording {
    pub fn status(&self) -> RecordingStatus {
        RecordingStatus {
            recording: true,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            frames: self.frames.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }

    /// Ask the loop to stop, then wait for the file to be finalised.
    ///
    /// Waiting matters: an MP4 is not playable until its index is written, so
    /// returning before the encoder has finished would hand back a path to a
    /// file that no player can open.
    pub fn stop(mut self) -> Result<RecordingOutcome, String> {
        self.stop.store(true, Ordering::Relaxed);
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| "the recording thread panicked".to_string())?,
            None => Err("the recording was already stopped".to_string()),
        }
    }
}

/// Begin recording into `output`.
pub fn start(request: RecordingRequest, output: PathBuf) -> Result<ActiveRecording, String> {
    let source = request
        .bounds
        .intersect(win::virtual_desktop().as_rect())
        .ok_or_else(|| "the recording area is off screen".to_string())?;

    if source.width < 16 || source.height < 16 {
        return Err("the recording area is too small".into());
    }

    let scale = request.scale_percent.clamp(25, 100) as f64 / 100.0;
    let target_width = ((source.width as f64 * scale).round() as u32).max(16);
    let target_height = ((source.height as f64 * scale).round() as u32).max(16);

    let fps = request.fps.clamp(5, 60);

    // Screen content is mostly flat colour and sharp text, which H.264 handles
    // well — but starve it and text is the first thing to turn to mush. A fixed
    // megabit figure that looks fine on a small region is badly short on a
    // 1440p one, so the configured rate is treated as a floor and raised to
    // suit the number of pixels actually being encoded.
    let pixels = u64::from(target_width) * u64::from(target_height);
    let suggested = (pixels * u64::from(fps)) as f64 * BITS_PER_PIXEL_PER_FRAME;
    let bitrate = (request.bitrate_mbps.clamp(1, 60) as u64 * 1_000_000)
        .max(suggested as u64)
        .min(60_000_000) as u32;

    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let dropped = Arc::new(AtomicU64::new(0));

    // The encoder is built on the worker thread, not here. A sink writer is a
    // COM object: it is not `Send`, and COM objects are meant to be used on the
    // thread that created them. Readiness comes back over a channel so a failure
    // to create it — no H.264 encoder, an unwritable path — still surfaces to
    // the caller rather than vanishing into a thread nobody is watching.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let worker = {
        let stop = Arc::clone(&stop);
        let frames = Arc::clone(&frames);
        let dropped = Arc::clone(&dropped);
        let output = output.clone();

        std::thread::Builder::new()
            .name("snipd-recorder".into())
            .spawn(move || {
                let mut encoder = match Encoder::create(
                    &output,
                    source.width,
                    source.height,
                    target_width,
                    target_height,
                    fps,
                    bitrate,
                ) {
                    Ok(encoder) => {
                        let _ = ready_tx.send(Ok(()));
                        encoder
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err.clone()));
                        return Err(err);
                    }
                };

                // Built here, on the thread that will use it: GDI objects
                // belong to their creating thread, and building it once rather
                // than per frame is most of the reason a recording can keep up
                // at all.
                let mut grabber = match FrameGrabber::new(source) {
                    Ok(grabber) => grabber,
                    Err(err) => {
                        let message = format!("could not start capturing frames: {err}");
                        eprintln!("[record] {message}");
                        let _ = encoder.finish();
                        return Err(message);
                    }
                };

                let interval = Duration::from_nanos(1_000_000_000 / fps as u64);
                let began = Instant::now();
                let mut index: u64 = 0;

                while !stop.load(Ordering::Relaxed) {
                    // Absolute deadline for this frame, so grab and encode time
                    // does not accumulate into drift.
                    let deadline = began + interval * index as u32;
                    let now = Instant::now();

                    if now < deadline {
                        std::thread::sleep(deadline - now);
                    } else if now - deadline > interval {
                        // More than a whole frame late: skip ahead rather than
                        // letting the timeline slide.
                        let behind = ((now - deadline).as_nanos() / interval.as_nanos()) as u64;
                        dropped.fetch_add(behind, Ordering::Relaxed);
                        index += behind;
                        continue;
                    }

                    match grabber.grab() {
                        Ok(pixels) => {
                            if let Err(err) = encoder.write_frame(pixels) {
                                eprintln!("[record] {err}");
                                break;
                            }
                            frames.store(encoder.frames_written(), Ordering::Relaxed);
                        }
                        Err(err) => {
                            // A single failed grab (a display mode change, a
                            // lock screen) should not end the recording.
                            eprintln!("[record] dropped a frame: {err}");
                            dropped.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    index += 1;
                }

                let elapsed = began.elapsed();
                let written = encoder.finish()?;
                let bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);

                Ok(RecordingOutcome {
                    file_name: file_name_of(&output),
                    path: output.to_string_lossy().into_owned(),
                    width: target_width,
                    height: target_height,
                    frames: written,
                    dropped: dropped.load(Ordering::Relaxed),
                    duration_ms: elapsed.as_millis() as u64,
                    bytes,
                })
            })
            .map_err(|e| format!("could not start the recording thread: {e}"))?
    };

    // Block until the encoder exists, so `start` only ever returns success for a
    // recording that is genuinely underway.
    ready_rx
        .recv()
        .map_err(|_| "the recording thread stopped before it began".to_string())??;

    Ok(ActiveRecording {
        stop,
        frames,
        dropped,
        started: Instant::now(),
        worker: Some(worker),
    })
}

/// Window label for the floating recorder bar.
pub const BAR_LABEL: &str = "recorder";

/// Raise the small always-on-top bar that shows elapsed time and stops the
/// recording.
///
/// A visible stop control is not optional. A recording with no obvious way to
/// end it is the kind of thing that fills a disk, and hiding it in a tray menu
/// is not good enough when the whole screen is being captured.
pub fn show_bar(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::{Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

    if let Some(existing) = app.get_webview_window(BAR_LABEL) {
        let _ = existing.show();
        let _ = existing.set_focus();
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(app, BAR_LABEL, WebviewUrl::App("recorder.html".into()))
        .title("Snipd recording")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(true)
        .visible(false)
        .build()
        .map_err(|e| format!("could not create the recording bar: {e}"))?;

    // Top centre of the primary display, clear of most window chrome.
    let desktop = win::virtual_desktop();
    let width = 268_u32;
    let height = 52_u32;
    let x = desktop.x + (desktop.width as i32 - width as i32) / 2;

    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    window
        .set_position(PhysicalPosition::new(x, desktop.y + 24))
        .map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())?;

    Ok(())
}

/// Dismiss the recorder bar.
pub fn hide_bar(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window(BAR_LABEL) {
        let _ = window.close();
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
