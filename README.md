# Snipd

A screenshot tool for Windows that never loses a capture.

Snipd is built as a genuine replacement for the Windows Snipping Tool, not a
reskin of it. Every capture is written to disk the instant it is taken — there is
no save step to forget, no window to close too early, and no capture that exists
only in the clipboard.

> **Status:** early development. The capture engine works; history, annotation,
> pinning, tray behaviour, settings and the installer are in progress. See
> [Roadmap](#roadmap).

---

## Why it exists

Each of these is a specific, concrete complaint about the built-in tool, and each
one is fixed in behaviour rather than in presentation:

| Problem with Snipping Tool | What Snipd does |
| --- | --- |
| Captures are lost if you forget to save | Every capture is written to disk before anything else happens |
| Meaningless generic filenames | Names follow the pattern you chose, with a live preview |
| Saves wherever it feels like | Always saves to the folder you configured |
| No history if you did not save elsewhere | A full, browsable history of every capture ever taken |
| Thin annotation tools | Pen, arrows, shapes, text, and a blur tool for redacting sensitive details |
| Cannot keep a shot visible while you work | Pin any capture as a resizable, always-on-top window |
| Clipboard copy is inconsistent | Copying retries while the clipboard is locked, instead of silently failing |
| Poor multi-monitor behaviour | One coordinate space across all displays, including mixed DPI and cross-screen selections |

## Features

- **One way in** — press the shortcut anywhere, or use the tray. The screen
  freezes and a toolbar appears; the mode is chosen there, not beforehand.
- **Four modes** — rectangle, freeform lasso, window, and full screen, plus a
  capture delay for grabbing menus and hover states.
- **Automatic saving** — with the naming pattern, folder and file format you
  configured. Nothing is ever overwritten.
- **Browsable history** — every capture, searchable and filterable, rebuilt by
  scanning the folder so a lost index can never lose a capture.
- **Multi-monitor aware** — selections can span two displays with different DPI
  scaling and still come out pixel-correct.
- **Local only** — no account, no login, no cloud, no telemetry. Your captures
  never leave your machine.

## Building from source

### Prerequisites

- [Node.js](https://nodejs.org/) 20 or newer
- [Rust](https://rustup.rs/) (stable toolchain)
- **Visual Studio Build Tools** with the *Desktop development with C++* workload
- **WebView2 Runtime** — already present on Windows 11 and up-to-date Windows 10

On a machine with `winget`, the two heavier prerequisites are:

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup -e
```

### Run in development

```bash
npm install
npm run tauri dev
```

### Build a release binary

```bash
npm run tauri build
```

### Run the tests

```bash
cd src-tauri
cargo test
```

## How it works

A short tour of the parts worth knowing about:

- **`src-tauri/src/capture/win.rs`** — the capture backend, written directly
  against Win32 rather than using a capture crate. It treats *virtual-screen
  coordinates in physical pixels* as the one coordinate space for the whole app.
  A single `BitBlt` of the virtual desktop returns one image whose pixel grid is
  that coordinate space, so cropping a selection that spans two monitors is plain
  arithmetic rather than a stitching step that can be got wrong.

- **`src-tauri/src/capture/mod.rs`** — enforces the ordering rule the whole app
  rests on: read pixels, **write the file**, then do everything else. Nothing
  after the write is allowed to fail the capture. A locked clipboard produces a
  warning attached to a successful result, never a lost screenshot.

- **`src-tauri/src/naming.rs`** — filename generation. Never returns a path that
  already exists, and never fails.

- **`src/overlay.ts`** — the capture overlay. The screen is frozen in Rust
  *before* the overlay appears, so the overlay can never end up in its own
  capture, nothing moving underneath can change what gets saved mid-drag, and a
  delayed capture can catch an open menu without the overlay closing it.

- **`src-tauri/src/capture/mask.rs`** — freeform lasso masking, via a scanline
  fill rather than a per-pixel point-in-polygon test. The naive version is
  O(pixels x edges), which on a real lasso is hundreds of millions of operations
  while the user waits.

- **`src-tauri/src/history.rs`** — history is rebuilt by scanning the save
  folder. The sidecar index only adds what the filesystem cannot know, so
  deleting it costs metadata rather than captures.

- **`src/editor.tsx`** — annotation. Edits are a list of shapes in *image*
  coordinates, and the canvas is redrawn from scratch on every change. That is
  what makes undo and redo trivially correct — there is no accumulated pixel
  state to unwind, only a shorter list to redraw — and it means the canvas can
  be displayed at any size without affecting the export. Saving always writes a
  **copy**: the original was auto-saved the instant it was taken, and an edit is
  not allowed to destroy the one thing the app promises never to lose.

### Known limitation

The capture backend reads the desktop that DWM has composited, which covers
normal applications, browsers and video playback. Content drawn through a
hardware overlay plane or protected by DRM will appear black — the same
behaviour as most classic screenshot tools. The backend sits behind a module
boundary so a Windows Graphics Capture path can be added without changing
callers.

## Roadmap

- [x] Capture engine — auto-save and auto-naming that cannot lose a capture
- [x] Overlay capture flow — rectangle, freeform, window, full screen, delay timer
- [x] Tray and global shortcuts
- [x] History and gallery
- [x] Annotation — pen, arrows, shapes, text, redaction, crop, undo/redo
- [ ] Pinned always-on-top windows
- [ ] Settings screen
- [ ] Visual design pass
- [ ] Inno Setup installer with first-run configuration

## Licence

[MIT](LICENSE)
