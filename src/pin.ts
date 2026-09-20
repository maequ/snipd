/**
 * A pinned capture floating over everything else.
 *
 * The window has no title bar, so the image doubles as the drag handle. Tauri's
 * `data-tauri-drag-region` attribute on the image handles that natively, which
 * behaves far better than tracking mouse deltas in JS — the OS moves the window,
 * so it stays smooth and snaps to screen edges like any other window.
 */

import { invoke } from "@tauri-apps/api/core";

interface PinState {
  imageUrl: string;
  fileName: string;
}

const shot = document.getElementById("shot") as HTMLImageElement;
const closeButton = document.getElementById("close") as HTMLButtonElement;
const error = document.getElementById("error") as HTMLParagraphElement;

function fail(message: string): void {
  shot.style.display = "none";
  error.style.display = "block";
  error.textContent = message;
}

async function start(): Promise<void> {
  try {
    const state = await invoke<PinState>("pin_state");
    shot.src = state.imageUrl;
    shot.alt = state.fileName;
    document.title = state.fileName;
  } catch (err) {
    fail(String(err));
  }

  closeButton.addEventListener("click", () => {
    void invoke("close_pin").catch((err) => console.error("close_pin failed", err));
  });

  // Escape closes the pin under the pointer, matching the overlay and editor.
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      void invoke("close_pin").catch(() => undefined);
    }
  });
}

void start();
