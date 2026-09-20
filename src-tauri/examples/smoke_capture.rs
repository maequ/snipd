//! End-to-end check of the capture engine, without the UI.
//!
//! Screen capture cannot be unit-tested meaningfully — it depends on a real
//! desktop with real displays attached. This example exercises the whole path
//! against the machine it runs on and reports what it found, which is the
//! quickest way to confirm the engine works on a given hardware setup
//! (particularly a multi-monitor or mixed-DPI one).
//!
//! Run it with:
//!
//! ```text
//! cd src-tauri
//! cargo run --example smoke_capture
//! ```
//!
//! Captures are written to a temporary folder, not your configured save
//! location. Note that it *does* exercise the clipboard, so your clipboard
//! contents will be replaced.

use std::path::PathBuf;

use snipd_lib::capture::{self, win, CaptureRequest};
use snipd_lib::config::{ImageFormat, NamingMode, Settings};

fn main() {
    win::ensure_dpi_awareness();

    let output: PathBuf = std::env::temp_dir().join("snipd-smoke");
    std::fs::create_dir_all(&output).expect("could not create the output folder");

    println!("Snipd capture smoke test");
    println!("Writing to {}\n", output.display());

    report_displays();

    let mut settings = Settings::default();
    settings.save_directory = output.clone();
    settings.format = ImageFormat::Png;
    // Datetime naming avoids touching the persisted counter, so running this
    // example does not disturb a real installation's numbering.
    settings.naming.mode = NamingMode::Datetime;

    let mut failures = 0;

    failures += run(
        "Entire virtual desktop",
        CaptureRequest::FullScreen { monitor_id: None },
        &mut settings,
    );

    // Each connected display individually, which is where mixed-DPI setups
    // tend to reveal coordinate bugs.
    for monitor in win::monitors() {
        failures += run(
            &format!("Single display — {}", monitor.label),
            CaptureRequest::FullScreen {
                monitor_id: Some(monitor.id.clone()),
            },
            &mut settings,
        );
    }

    failures += run("Active window", CaptureRequest::ActiveWindow, &mut settings);

    // Region capture normally crops a frozen frame handed over by the overlay.
    // Here the same path is driven directly with a fixed rectangle.
    let desktop = win::virtual_desktop();
    failures += run(
        "Region (200x150 at desktop origin)",
        CaptureRequest::Region {
            bounds: win::Bounds {
                x: desktop.x + 10,
                y: desktop.y + 10,
                width: 200,
                height: 150,
            },
        },
        &mut settings,
    );

    println!();
    if failures == 0 {
        println!("All captures succeeded.");
    } else {
        println!("{failures} capture(s) failed.");
        std::process::exit(1);
    }
}

fn report_displays() {
    let desktop = win::virtual_desktop();
    println!(
        "Virtual desktop: {} x {} at ({}, {})",
        desktop.width, desktop.height, desktop.x, desktop.y
    );

    let monitors = win::monitors();
    println!("{} display(s) detected:", monitors.len());
    for monitor in &monitors {
        println!(
            "  {:<40} {:>5} x {:<5} at ({:>6}, {:>6})  scale {:.0}%  [{}]",
            monitor.label,
            monitor.width,
            monitor.height,
            monitor.x,
            monitor.y,
            monitor.scale_factor * 100.0,
            monitor.id.trim_start_matches(r"\\.\")
        );
    }

    let scales: Vec<String> = monitors
        .iter()
        .map(|m| format!("{:.0}%", m.scale_factor * 100.0))
        .collect();
    let mixed = monitors
        .windows(2)
        .any(|pair| (pair[0].scale_factor - pair[1].scale_factor).abs() > f64::EPSILON);
    if mixed {
        println!("  -> Mixed DPI detected ({}). Good test case.", scales.join(", "));
    }
    println!();
}

/// Run one capture and print whether it produced a real file.
fn run(label: &str, request: CaptureRequest, settings: &mut Settings) -> u32 {
    match capture::capture_and_save(request, settings) {
        Ok(record) => {
            let on_disk = std::fs::metadata(&record.path).map(|m| m.len()).unwrap_or(0);

            // A capture that "succeeded" but wrote nothing is the failure mode
            // worth catching here, so check the file rather than trusting the
            // return value.
            let ok = on_disk > 0 && record.width > 0 && record.height > 0;

            println!(
                "{} {:<44} {:>5} x {:<5} {:>9} bytes  clipboard: {}",
                if ok { "PASS" } else { "FAIL" },
                label,
                record.width,
                record.height,
                on_disk,
                if record.copied_to_clipboard { "yes" } else { "no" },
            );
            println!("      {}", record.file_name);

            for warning in &record.warnings {
                println!("      warning: {warning}");
            }

            u32::from(!ok)
        }
        Err(err) => {
            println!("FAIL {label:<44} {err}");
            1
        }
    }
}
