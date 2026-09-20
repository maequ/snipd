//! Copying captures to the Windows clipboard.
//!
//! # Why this is not a one-liner
//!
//! The Windows clipboard is a single shared, lockable resource. `OpenClipboard`
//! fails outright if another process is holding it — and plenty of software holds
//! it briefly and often: clipboard managers, Office, remote-desktop clients,
//! password managers, even Explorer. A naive single attempt therefore fails
//! occasionally and unpredictably, which is exactly the "sometimes it just
//! doesn't copy" behaviour this app exists to fix.
//!
//! So every copy retries with a short backoff. In practice the lock is released
//! within a few milliseconds and the first or second attempt succeeds; the
//! remaining attempts cover the pathological cases.

use std::borrow::Cow;
use std::thread::sleep;
use std::time::Duration;

use arboard::{Clipboard, ImageData};
use image::RgbaImage;

/// How many times to try before reporting failure.
const MAX_ATTEMPTS: u32 = 6;

/// Base delay between attempts. Doubles each time: 8, 16, 32, 64, 128 ms —
/// about a quarter of a second in total, which stays imperceptible while
/// comfortably outlasting a typical clipboard-manager lock.
const BASE_DELAY: Duration = Duration::from_millis(8);

/// Copy an image to the clipboard, retrying while the clipboard is locked.
///
/// Returns the number of attempts it took, so the caller can log a warning when
/// the clipboard is chronically contended.
pub fn copy_image(image: &RgbaImage) -> Result<u32, String> {
    let data = ImageData {
        width: image.width() as usize,
        height: image.height() as usize,
        bytes: Cow::Borrowed(image.as_raw()),
    };

    let mut last_error = String::from("clipboard was never attempted");

    for attempt in 1..=MAX_ATTEMPTS {
        match try_copy(&data) {
            Ok(()) => return Ok(attempt),
            Err(err) => {
                last_error = err;
                if attempt < MAX_ATTEMPTS {
                    // Exponential backoff, capped implicitly by the attempt count.
                    sleep(BASE_DELAY * 2_u32.pow(attempt - 1));
                }
            }
        }
    }

    Err(format!(
        "clipboard unavailable after {MAX_ATTEMPTS} attempts: {last_error}"
    ))
}

/// One attempt at acquiring the clipboard and writing to it.
///
/// A fresh `Clipboard` is created per attempt on purpose: the handle caches an
/// open connection, and reusing one that failed tends to keep failing.
fn try_copy(data: &ImageData<'_>) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set_image(data.clone())
        .map_err(|e| e.to_string())?;
    Ok(())
}
