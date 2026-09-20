/**
 * Region-selection overlay.
 *
 * This runs in its own transparent, always-on-top window that Rust stretches
 * across the entire virtual desktop. Its only job is to turn a mouse drag into a
 * rectangle in **virtual-screen physical pixels** and hand that to Rust, which
 * crops it out of a frame frozen before this window ever appeared.
 *
 * ## The coordinate problem
 *
 * The browser thinks in CSS pixels. Windows scales this window by the DPI of
 * whichever display it considers the window to be on — which, for a window
 * spanning a 150%-scaled laptop screen and a 100% external monitor, is a single
 * scale factor applied to the whole thing.
 *
 * Rather than trying to reason about `devicePixelRatio` (which reports that same
 * single factor and would be wrong to apply per-monitor), the scale is derived
 * by measuring: the window's physical width is known from Rust, and its CSS
 * width is known from the DOM, so their ratio is the conversion. That is
 * self-correcting on any DPI arrangement, including ones that change mid-session.
 */

import { invoke } from "@tauri-apps/api/core";

/** A rectangle in virtual-screen coordinates, physical pixels. */
interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Bounding box of every display combined. Origin may be negative. */
interface VirtualDesktop {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Ignore drags smaller than this (in physical pixels) and treat them as a
 * misclick. Without it, a stray click produces a 1x2px screenshot.
 */
const MIN_SELECTION_PX = 4;

const dim = document.getElementById("dim") as HTMLDivElement;
const selection = document.getElementById("selection") as HTMLDivElement;
const readout = document.getElementById("readout") as HTMLDivElement;
const hint = document.getElementById("hint") as HTMLDivElement;

let desktop: VirtualDesktop = { x: 0, y: 0, width: 0, height: 0 };
let scaleX = 1;
let scaleY = 1;

let dragging = false;
let startX = 0;
let startY = 0;
/** Guards against a second finish/cancel firing while the first is in flight. */
let settled = false;

/**
 * Work out how many physical pixels one CSS pixel covers, by comparing the
 * window size Rust set against the size the DOM reports.
 */
function measureScale(): void {
  const cssWidth = document.documentElement.clientWidth;
  const cssHeight = document.documentElement.clientHeight;

  // Guard against a zero-sized layout during the first frame.
  scaleX = cssWidth > 0 ? desktop.width / cssWidth : 1;
  scaleY = cssHeight > 0 ? desktop.height / cssHeight : 1;
}

/** Convert a CSS-pixel point in this window to virtual-screen coordinates. */
function toPhysical(cssX: number, cssY: number): { x: number; y: number } {
  return {
    x: Math.round(desktop.x + cssX * scaleX),
    y: Math.round(desktop.y + cssY * scaleY),
  };
}

/** Position the hint bar near the top of wherever the pointer currently is. */
function placeHint(cssY: number): void {
  hint.style.top = `${Math.max(24, cssY - 80)}px`;
}

function updateSelectionVisual(currentX: number, currentY: number): void {
  const left = Math.min(startX, currentX);
  const top = Math.min(startY, currentY);
  const width = Math.abs(currentX - startX);
  const height = Math.abs(currentY - startY);

  selection.style.left = `${left}px`;
  selection.style.top = `${top}px`;
  selection.style.width = `${width}px`;
  selection.style.height = `${height}px`;

  // Report the size in physical pixels, because that is what the saved file
  // will actually be — showing CSS pixels would understate it on a scaled display.
  const physicalWidth = Math.round(width * scaleX);
  const physicalHeight = Math.round(height * scaleY);
  readout.textContent = `${physicalWidth} x ${physicalHeight}`;

  // Keep the readout on screen when the selection runs near an edge.
  const offset = 10;
  const readoutWidth = readout.offsetWidth;
  const readoutHeight = readout.offsetHeight;
  const viewportWidth = document.documentElement.clientWidth;
  const viewportHeight = document.documentElement.clientHeight;

  let readoutLeft = left;
  let readoutTop = top - readoutHeight - offset;

  if (readoutTop < 0) {
    // Not enough room above the selection, so sit just inside its top edge.
    readoutTop = top + offset;
  }
  if (readoutLeft + readoutWidth > viewportWidth) {
    readoutLeft = viewportWidth - readoutWidth - offset;
  }
  if (readoutTop + readoutHeight > viewportHeight) {
    readoutTop = viewportHeight - readoutHeight - offset;
  }

  readout.style.left = `${Math.max(0, readoutLeft)}px`;
  readout.style.top = `${Math.max(0, readoutTop)}px`;
}

async function cancel(): Promise<void> {
  if (settled) return;
  settled = true;
  try {
    await invoke("cancel_region_capture");
  } catch (err) {
    // The window is being torn down either way; there is nothing useful to
    // show the user in an overlay that is about to vanish.
    console.error("cancel_region_capture failed", err);
  }
}

async function finish(bounds: Bounds): Promise<void> {
  if (settled) return;
  settled = true;
  try {
    // Rust saves the crop, announces the result to the main window, and closes
    // this one. The promise below may therefore never resolve — this window is
    // destroyed first — which is why the result is delivered by event instead
    // of being awaited here.
    await invoke("finish_region_capture", { bounds });
  } catch (err) {
    console.error("finish_region_capture failed", err);
  }
}

function onMouseDown(event: MouseEvent): void {
  if (event.button !== 0 || settled) return;

  dragging = true;
  startX = event.clientX;
  startY = event.clientY;

  dim.style.display = "none";
  hint.style.display = "none";
  selection.style.display = "block";
  readout.style.display = "block";
  updateSelectionVisual(startX, startY);
}

function onMouseMove(event: MouseEvent): void {
  if (!dragging) {
    placeHint(event.clientY);
    return;
  }
  updateSelectionVisual(event.clientX, event.clientY);
}

function onMouseUp(event: MouseEvent): void {
  if (!dragging || event.button !== 0) return;
  dragging = false;

  const start = toPhysical(Math.min(startX, event.clientX), Math.min(startY, event.clientY));
  const end = toPhysical(Math.max(startX, event.clientX), Math.max(startY, event.clientY));

  const width = end.x - start.x;
  const height = end.y - start.y;

  if (width < MIN_SELECTION_PX || height < MIN_SELECTION_PX) {
    void cancel();
    return;
  }

  void finish({ x: start.x, y: start.y, width, height });
}

function onKeyDown(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    event.preventDefault();
    void cancel();
  }
}

async function start(): Promise<void> {
  desktop = await invoke<VirtualDesktop>("get_virtual_desktop");
  measureScale();

  // A display can be added, removed or rescaled while the overlay is open.
  window.addEventListener("resize", () => {
    void (async () => {
      desktop = await invoke<VirtualDesktop>("get_virtual_desktop");
      measureScale();
    })();
  });

  window.addEventListener("mousedown", onMouseDown);
  window.addEventListener("mousemove", onMouseMove);
  window.addEventListener("mouseup", onMouseUp);
  window.addEventListener("keydown", onKeyDown);

  // Right-click cancels, matching the behaviour of every other snipping tool.
  window.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    void cancel();
  });

  placeHint(document.documentElement.clientHeight / 2);
  hint.style.display = "block";
}

void start();
