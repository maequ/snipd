//! End-to-end check of the recording engine, without the UI.
//!
//! Records a few seconds of the primary display and reports what came out.
//! Media Foundation's encoder availability varies by machine, so this is the
//! quickest way to find out whether recording works on a given setup.
//!
//! Run it with:
//!
//! ```text
//! cd src-tauri
//! cargo run --example smoke_record
//! ```

use std::time::{Duration, Instant};

use snipd_lib::capture::win::{self, Bounds};
use snipd_lib::record::{self, RecordingRequest};

fn main() {
    win::ensure_dpi_awareness();

    let seconds = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u64>().ok())
        .unwrap_or(3);

    let output = std::env::temp_dir().join("snipd-smoke-recording.mp4");
    let _ = std::fs::remove_file(&output);

    // The whole primary display by default, because that is the case that
    // actually stresses the capture loop — a small region always kept up even
    // when a full-screen recording was dropping frames badly. Pass a width to
    // test something smaller.
    let desktop = win::virtual_desktop();
    let limit = std::env::args()
        .nth(2)
        .and_then(|a| a.parse::<u32>().ok())
        .unwrap_or(u32::MAX);
    let bounds = Bounds {
        x: desktop.x,
        y: desktop.y,
        width: desktop.width.min(limit),
        height: desktop.height.min(limit),
    };

    println!("Snipd recording smoke test");
    println!(
        "Recording {}x{} for {seconds}s to {}\n",
        bounds.width,
        bounds.height,
        output.display()
    );

    let began = Instant::now();
    let active = match record::start(
        RecordingRequest {
            bounds,
            fps: 30,
            scale_percent: 100,
            bitrate_mbps: 12,
        },
        output.clone(),
    ) {
        Ok(active) => active,
        Err(err) => {
            println!("FAIL  could not start: {err}");
            std::process::exit(1);
        }
    };

    std::thread::sleep(Duration::from_secs(seconds));

    let outcome = match active.stop() {
        Ok(outcome) => outcome,
        Err(err) => {
            println!("FAIL  could not stop: {err}");
            std::process::exit(1);
        }
    };

    let wall = began.elapsed();
    let expected = seconds * 30;

    println!("  file      {}", outcome.file_name);
    println!("  size      {} x {}", outcome.width, outcome.height);
    println!(
        "  frames    {} written, {} dropped (expected about {expected})",
        outcome.frames, outcome.dropped
    );
    println!("  bytes     {}", outcome.bytes);
    println!("  wall      {:.1}s", wall.as_secs_f64());

    // A valid MP4 always carries an 'ftyp' box at the very start. Checking for
    // it catches the failure that matters: a file that exists and has bytes in
    // it but was never finalised, which no player will open.
    let header = std::fs::read(&output).unwrap_or_default();
    let has_ftyp = header.len() > 12 && &header[4..8] == b"ftyp";

    let ok = outcome.bytes > 1024 && outcome.frames > 0 && has_ftyp;
    println!(
        "\n{}  bytes>1KB:{} frames>0:{} ftyp:{}",
        if ok { "PASS" } else { "FAIL" },
        outcome.bytes > 1024,
        outcome.frames > 0,
        has_ftyp
    );

    if !ok {
        std::process::exit(1);
    }
    println!("\nPlay it to confirm: {}", output.display());
}
