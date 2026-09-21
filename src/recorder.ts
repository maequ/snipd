/**
 * The floating recorder bar.
 *
 * Polls the recording status rather than being pushed updates: the elapsed time
 * only needs refreshing a few times a second, and a poll keeps the bar correct
 * even if it is opened partway through a recording.
 */

import { invoke } from "@tauri-apps/api/core";

interface RecordingStatus {
  recording: boolean;
  elapsedMs: number;
  frames: number;
  dropped: number;
}

const elapsed = document.getElementById("elapsed") as HTMLSpanElement;
const detail = document.getElementById("detail") as HTMLSpanElement;
const stop = document.getElementById("stop") as HTMLButtonElement;

/** Stops a second click doing anything while the first is still finalising. */
let stopping = false;

function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

async function tick(): Promise<void> {
  if (stopping) return;
  try {
    const status = await invoke<RecordingStatus>("recording_status");
    if (!status.recording) return;

    elapsed.textContent = formatElapsed(status.elapsedMs);

    // Dropped frames are surfaced rather than hidden: it is the honest signal
    // that the machine cannot keep up, and the fix (lower the resolution or
    // frame rate) is something only the user can choose.
    detail.textContent =
      status.dropped > 0 ? `${status.dropped} frames dropped` : "";
  } catch {
    // The bar outliving the recording is not worth reporting.
  }
}

async function finish(): Promise<void> {
  if (stopping) return;
  stopping = true;
  stop.textContent = "Saving…";
  try {
    // Rust closes this window once the file is finalised.
    await invoke("stop_recording");
  } catch (err) {
    console.error("stop_recording failed", err);
    stopping = false;
    stop.textContent = "Stop";
  }
}

stop.addEventListener("click", () => void finish());

window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") void finish();
});

void tick();
window.setInterval(() => void tick(), 250);
