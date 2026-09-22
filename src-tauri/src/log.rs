//! A small log file.
//!
//! The application is built as a Windows GUI binary, which means it has no
//! console and anything written to stderr goes nowhere at all. That is fine
//! until something goes wrong on a machine that is not this one, at which point
//! there is no way to find out *where* it went wrong — only that it did.
//!
//! So: a plain text file at `%APPDATA%\Snipd\snipd.log`, with a timestamp and a
//! millisecond duration on the things that can be slow. Nothing about the user's
//! machine or their captures beyond file names already visible in the app.
//!
//! Failing to log is never allowed to affect anything. Every write here is
//! best-effort; a full disk or a locked file costs a log line, never a capture.

use std::fmt::Display;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use chrono::Local;

/// Rotate once the file passes this, so it cannot grow without bound on a
/// machine that is left running for months.
const MAX_BYTES: u64 = 512 * 1024;

/// Serialises writes so interleaved lines from several threads stay readable.
static LOCK: Mutex<()> = Mutex::new(());

pub fn path() -> PathBuf {
    crate::config::data_directory().join("snipd.log")
}

/// Write one line. Never panics, never returns an error.
pub fn line(message: impl Display) {
    let Ok(_guard) = LOCK.lock() else {
        return;
    };

    let path = path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Rotate by keeping a single previous file. Two files is enough to cover
    // "it broke, I restarted, then I looked".
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            file,
            "{} {}",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            message
        );
    }
}

/// Time a step and log how long it took.
///
/// Used on the handful of operations that have ever been slow enough to look
/// like a hang — freezing the screen, encoding a capture, building a window.
/// Knowing *which* of them took the time is the whole difference between
/// fixing a stall and guessing at it.
pub fn timed<T>(what: &str, body: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = body();
    let millis = started.elapsed().as_millis();

    // Only the slow ones, so ordinary operation does not bury the interesting
    // lines in noise.
    if millis >= 250 {
        line(format!("[slow] {what} took {millis}ms"));
    }
    result
}

/// Note that the application started, and with what.
pub fn startup(version: &str, autostart: bool) {
    line(format!(
        "--- snipd {version} starting (autostart={autostart}) ---"
    ));
}
