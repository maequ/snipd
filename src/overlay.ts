/**
 * The capture overlay.
 *
 * Runs in its own always-on-top window that Rust stretches across the entire
 * virtual desktop, showing a still of the screen frozen the instant before it
 * opened. Its job is to turn a gesture into a shape in **virtual-screen
 * physical pixels** and hand that to Rust, which crops it out of the lossless
 * frame still held in memory.
 *
 * ## The coordinate problem
 *
 * The browser thinks in CSS pixels. Windows scales this window by the DPI of
 * whichever display it decides the window belongs to — for a window spanning a
 * 150%-scaled laptop panel and a 100% external monitor, that is one scale factor
 * applied to the whole thing.
 *
 * Rather than reasoning about `devicePixelRatio` (which reports that same single
 * factor and would be wrong to apply per-monitor), the scale is *measured*: the
 * window's physical width is known from Rust, its CSS width from the DOM, and
 * their ratio is the conversion. That is self-correcting on any DPI arrangement.
 */

import { invoke } from "@tauri-apps/api/core";

interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface VirtualDesktop {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface WindowTarget {
  bounds: Bounds;
  title: string;
}

interface MonitorInfo {
  id: string;
  label: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scaleFactor: number;
  isPrimary: boolean;
}

interface OverlayState {
  desktop: VirtualDesktop;
  windows: WindowTarget[];
  monitors: MonitorInfo[];
  backdropUrl: string;
  initialMode: string;
}

type Mode = "rectangle" | "freeform" | "window" | "fullscreen";

/** Ignore drags smaller than this in physical pixels; they are misclicks. */
const MIN_SELECTION_PX = 5;

/** Delay options the timer button cycles through, in seconds. */
const DELAY_STEPS = [0, 3, 5, 10];

/**
 * How long to wait after the last click on the timer button before re-arming.
 * Long enough to cycle 0 -> 3 -> 5 without the overlay vanishing mid-cycle.
 */
const DELAY_COMMIT_MS = 800;

const HINTS: Record<Mode, string> = {
  rectangle: "Drag to select an area",
  freeform: "Draw around what you want to keep",
  window: "Click a window to capture it",
  fullscreen: "Click a display to capture it",
};

const backdrop = document.getElementById("backdrop") as HTMLImageElement;
const bright = document.getElementById("bright") as HTMLImageElement;
const lasso = document.getElementById("lasso") as unknown as SVGSVGElement;
const outline = document.getElementById("outline") as HTMLDivElement;
const readout = document.getElementById("readout") as HTMLDivElement;
const toolbar = document.getElementById("toolbar") as HTMLDivElement;
const hint = document.getElementById("hint") as HTMLDivElement;
const delayButton = document.getElementById("delay") as HTMLButtonElement;
const delayValue = document.getElementById("delay-value") as HTMLSpanElement;
const closeButton = document.getElementById("close") as HTMLButtonElement;

let session: OverlayState = {
  desktop: { x: 0, y: 0, width: 0, height: 0 },
  windows: [],
  monitors: [],
  backdropUrl: "",
  initialMode: "rectangle",
};

let mode: Mode = "rectangle";
let scaleX = 1;
let scaleY = 1;

let dragging = false;
let startX = 0;
let startY = 0;
/** Lasso path in CSS pixels, captured at mouse-move resolution. */
let lassoPoints: Array<{ x: number; y: number }> = [];
/** Whichever window or display the cursor is currently over. */
let hovered: { bounds: Bounds; label: string } | null = null;

let delayIndex = 0;
let delayTimer: number | null = null;
/** Stops a second capture or cancel firing while the first is in flight. */
let settled = false;
/** False until the window has been sized to the desktop and the scale is sound. */
let ready = false;

// ---------------------------------------------------------------------------
// Coordinates
// ---------------------------------------------------------------------------

function measureScale(): void {
  const cssWidth = document.documentElement.clientWidth;
  const cssHeight = document.documentElement.clientHeight;
  scaleX = cssWidth > 0 ? session.desktop.width / cssWidth : 1;
  scaleY = cssHeight > 0 ? session.desktop.height / cssHeight : 1;
}

/**
 * Wait until the window has actually been stretched across the desktop.
 *
 * Rust creates this window and only *then* sets its position and size, so the
 * webview can finish loading while the window is still at its default size. The
 * scale measured at that moment is badly wrong — a drag then maps to a region
 * that is neither the size nor the place the user selected.
 *
 * Resolves once the viewport matches the desktop, or gives up after a second
 * and works with whatever is there rather than hanging on a blank screen.
 */
function waitForGeometry(): Promise<void> {
  return new Promise((resolve) => {
    const deadline = performance.now() + 1000;

    const check = () => {
      const dpr = window.devicePixelRatio || 1;
      const physicalWidth = document.documentElement.clientWidth * dpr;
      const physicalHeight = document.documentElement.clientHeight * dpr;

      // A pixel or two of rounding is expected on fractional scale factors.
      const settled =
        Math.abs(physicalWidth - session.desktop.width) <= 2 &&
        Math.abs(physicalHeight - session.desktop.height) <= 2;

      if (settled || performance.now() > deadline) {
        resolve();
        return;
      }
      requestAnimationFrame(check);
    };

    requestAnimationFrame(check);
  });
}

/** CSS point in this window to a virtual-screen coordinate. */
function toPhysical(cssX: number, cssY: number): { x: number; y: number } {
  return {
    x: Math.round(session.desktop.x + cssX * scaleX),
    y: Math.round(session.desktop.y + cssY * scaleY),
  };
}

/** Virtual-screen rectangle back to CSS pixels, for drawing. */
function toCss(bounds: Bounds): { left: number; top: number; width: number; height: number } {
  return {
    left: (bounds.x - session.desktop.x) / scaleX,
    top: (bounds.y - session.desktop.y) / scaleY,
    width: bounds.width / scaleX,
    height: bounds.height / scaleY,
  };
}

function contains(bounds: Bounds, x: number, y: number): boolean {
  return x >= bounds.x && x < bounds.x + bounds.width && y >= bounds.y && y < bounds.y + bounds.height;
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/** Reveal a rectangle by clipping the undimmed copy of the backdrop to it. */
function showRect(left: number, top: number, width: number, height: number, label: string): void {
  const viewWidth = document.documentElement.clientWidth;
  const viewHeight = document.documentElement.clientHeight;

  bright.style.display = "block";
  bright.style.clipPath = `inset(${top}px ${viewWidth - left - width}px ${
    viewHeight - top - height
  }px ${left}px)`;

  outline.style.display = "block";
  outline.style.left = `${left}px`;
  outline.style.top = `${top}px`;
  outline.style.width = `${width}px`;
  outline.style.height = `${height}px`;

  lasso.style.display = "none";
  placeReadout(left, top, label);
}

/** Reveal a hand-drawn shape, and stroke its outline so the path is visible. */
function showLasso(points: Array<{ x: number; y: number }>): void {
  if (points.length < 2) return;

  const polygon = points.map((p) => `${p.x}px ${p.y}px`).join(", ");
  bright.style.display = "block";
  bright.style.clipPath = `polygon(${polygon})`;

  const d = `M ${points.map((p) => `${p.x} ${p.y}`).join(" L ")} Z`;
  lasso.style.display = "block";
  lasso.innerHTML =
    `<path d="${d}" fill="none" stroke="#ffffff" stroke-width="1.5" ` +
    `stroke-linejoin="round" stroke-linecap="round" />`;

  outline.style.display = "none";

  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const point of points) {
    minX = Math.min(minX, point.x);
    minY = Math.min(minY, point.y);
    maxX = Math.max(maxX, point.x);
    maxY = Math.max(maxY, point.y);
  }
  placeReadout(
    minX,
    minY,
    `${Math.round((maxX - minX) * scaleX)} x ${Math.round((maxY - minY) * scaleY)}`,
  );
}

function clearSelection(): void {
  bright.style.display = "none";
  outline.style.display = "none";
  lasso.style.display = "none";
  readout.style.display = "none";
}

/** Put the size readout beside the selection, kept inside the screen. */
function placeReadout(left: number, top: number, text: string): void {
  readout.textContent = text;
  readout.style.display = "block";

  const gap = 9;
  const viewWidth = document.documentElement.clientWidth;
  const viewHeight = document.documentElement.clientHeight;

  let x = left;
  // Prefer just above the selection; drop inside it when there is no room.
  let y = top - readout.offsetHeight - gap;
  if (y < 0) y = top + gap;
  if (x + readout.offsetWidth > viewWidth) x = viewWidth - readout.offsetWidth - gap;
  if (y + readout.offsetHeight > viewHeight) y = viewHeight - readout.offsetHeight - gap;

  readout.style.left = `${Math.max(0, x)}px`;
  readout.style.top = `${Math.max(0, y)}px`;
}

// ---------------------------------------------------------------------------
// Modes
// ---------------------------------------------------------------------------

function setMode(next: Mode): void {
  mode = next;
  dragging = false;
  hovered = null;
  lassoPoints = [];
  clearSelection();

  for (const button of toolbar.querySelectorAll<HTMLButtonElement>("[data-mode]")) {
    button.setAttribute("aria-pressed", String(button.dataset.mode === next));
  }

  hint.textContent = HINTS[next];
  hint.style.display = "block";
  document.body.style.cursor = next === "window" || next === "fullscreen" ? "pointer" : "crosshair";
}

/** In window and full-screen mode, highlight whatever is under the cursor. */
function updateHover(cssX: number, cssY: number): void {
  const point = toPhysical(cssX, cssY);

  let found: { bounds: Bounds; label: string } | null = null;
  if (mode === "window") {
    // Windows arrive topmost-first, so the first hit is the one on top.
    const match = session.windows.find((w) => contains(w.bounds, point.x, point.y));
    if (match) {
      found = { bounds: match.bounds, label: match.title || "Untitled window" };
    }
  } else {
    const match = session.monitors.find((m) =>
      contains({ x: m.x, y: m.y, width: m.width, height: m.height }, point.x, point.y),
    );
    if (match) {
      found = {
        bounds: { x: match.x, y: match.y, width: match.width, height: match.height },
        label: match.label,
      };
    }
  }

  hovered = found;
  if (!found) {
    clearSelection();
    return;
  }

  const css = toCss(found.bounds);
  showRect(
    css.left,
    css.top,
    css.width,
    css.height,
    `${found.label}  ${found.bounds.width} x ${found.bounds.height}`,
  );
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

async function cancel(): Promise<void> {
  if (settled) return;
  settled = true;
  try {
    await invoke("cancel_capture");
  } catch (err) {
    console.error("cancel_capture failed", err);
  }
}

/**
 * Send a rectangular selection to Rust.
 *
 * The promise is not expected to resolve: Rust saves, announces the result by
 * event, and then closes this window, so the response has nowhere to land.
 */
async function commitRect(bounds: Bounds, kind: string, source: string | null): Promise<void> {
  if (settled) return;
  settled = true;
  try {
    await invoke("capture_rect", { selection: { bounds, kind, source } });
  } catch (err) {
    console.error("capture_rect failed", err);
  }
}

async function commitFreeform(points: Array<{ x: number; y: number }>): Promise<void> {
  if (settled) return;
  settled = true;
  try {
    const physical = points.map((p) => toPhysical(p.x, p.y));
    await invoke("capture_freeform", { points: physical });
  } catch (err) {
    console.error("capture_freeform failed", err);
  }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

function onMouseDown(event: MouseEvent): void {
  if (event.button !== 0 || settled || !ready) return;
  // Clicks on the toolbar are its own business.
  if (toolbar.contains(event.target as Node)) return;

  // Re-measure at the moment of use. Cheap, and it means a display being
  // rescaled or rearranged mid-session cannot leave a stale scale factor behind.
  measureScale();

  if (mode === "window" || mode === "fullscreen") {
    if (!hovered) return;
    const kind = mode === "window" ? "activeWindow" : "fullScreen";
    void commitRect(hovered.bounds, kind, hovered.label);
    return;
  }

  dragging = true;
  startX = event.clientX;
  startY = event.clientY;
  hint.style.display = "none";

  if (mode === "freeform") {
    lassoPoints = [{ x: startX, y: startY }];
  }
}

function onMouseMove(event: MouseEvent): void {
  if (settled || !ready) return;

  if (!dragging) {
    if (mode === "window" || mode === "fullscreen") {
      updateHover(event.clientX, event.clientY);
    }
    return;
  }

  if (mode === "freeform") {
    const last = lassoPoints[lassoPoints.length - 1];
    // Skip sub-pixel moves: they bloat the polygon sent to Rust without
    // changing the shape.
    if (!last || Math.abs(event.clientX - last.x) >= 1 || Math.abs(event.clientY - last.y) >= 1) {
      lassoPoints.push({ x: event.clientX, y: event.clientY });
      showLasso(lassoPoints);
    }
    return;
  }

  const left = Math.min(startX, event.clientX);
  const top = Math.min(startY, event.clientY);
  const width = Math.abs(event.clientX - startX);
  const height = Math.abs(event.clientY - startY);
  showRect(
    left,
    top,
    width,
    height,
    `${Math.round(width * scaleX)} x ${Math.round(height * scaleY)}`,
  );
}

function onMouseUp(event: MouseEvent): void {
  if (!dragging || event.button !== 0) return;
  dragging = false;

  if (mode === "freeform") {
    if (lassoPoints.length < 3) {
      void cancel();
      return;
    }
    void commitFreeform(lassoPoints);
    return;
  }

  const start = toPhysical(Math.min(startX, event.clientX), Math.min(startY, event.clientY));
  const end = toPhysical(Math.max(startX, event.clientX), Math.max(startY, event.clientY));
  const width = end.x - start.x;
  const height = end.y - start.y;

  if (width < MIN_SELECTION_PX || height < MIN_SELECTION_PX) {
    void cancel();
    return;
  }

  void commitRect({ x: start.x, y: start.y, width, height }, "region", null);
}

function onKeyDown(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    event.preventDefault();
    void cancel();
    return;
  }

  // Number keys switch tools, matching the toolbar order.
  const shortcuts: Record<string, Mode> = {
    "1": "rectangle",
    "2": "freeform",
    "3": "window",
    "4": "fullscreen",
  };
  const target = shortcuts[event.key];
  if (target) {
    event.preventDefault();
    setMode(target);
  }
}

/**
 * Cycle the capture delay.
 *
 * A non-zero delay re-arms the whole session: the overlay closes, the wait
 * happens with nothing on screen — which is the entire point, since the user
 * wants to open a menu or produce a hover state — and then the screen is frozen
 * afresh and the overlay returns. The commit is debounced so the button can be
 * clicked through several values first.
 */
function onDelayClick(): void {
  delayIndex = (delayIndex + 1) % DELAY_STEPS.length;
  const seconds = DELAY_STEPS[delayIndex];

  delayValue.textContent = seconds === 0 ? "No delay" : `${seconds}s delay`;
  delayButton.setAttribute("aria-pressed", String(seconds > 0));

  if (delayTimer !== null) window.clearTimeout(delayTimer);
  if (seconds === 0) return;

  hint.textContent = `Re-arming with a ${seconds} second delay…`;
  hint.style.display = "block";

  delayTimer = window.setTimeout(() => {
    if (settled) return;
    settled = true;
    void invoke("begin_capture", { delayMs: seconds * 1000, mode }).catch((err) => {
      console.error("begin_capture failed", err);
    });
  }, DELAY_COMMIT_MS);
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

async function start(): Promise<void> {
  session = await invoke<OverlayState>("overlay_state");

  backdrop.src = session.backdropUrl;
  bright.src = session.backdropUrl;

  // Input stays disabled until the window really covers the desktop, so the
  // very first drag cannot be measured against a half-sized window.
  await waitForGeometry();
  measureScale();
  ready = true;

  for (const button of toolbar.querySelectorAll<HTMLButtonElement>("[data-mode]")) {
    button.addEventListener("click", () => setMode(button.dataset.mode as Mode));
  }
  delayButton.addEventListener("click", onDelayClick);
  closeButton.addEventListener("click", () => void cancel());

  window.addEventListener("mousedown", onMouseDown);
  window.addEventListener("mousemove", onMouseMove);
  window.addEventListener("mouseup", onMouseUp);
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    void cancel();
  });
  // A display added, removed or rescaled while the overlay is open would
  // invalidate the measured scale.
  window.addEventListener("resize", measureScale);

  setMode((session.initialMode as Mode) ?? "rectangle");
}

void start();
