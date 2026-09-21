# Snipd

A screenshot tool for Windows that never loses a capture.

Snipd is built as a genuine replacement for the Windows Snipping Tool, not a
reskin of it. Every capture is written to disk the instant it is taken — there is
no save step to forget, no window to close too early, and no capture that exists
only in the clipboard.

---

## Why it exists

Each row below is a specific complaint about the built-in tool, fixed in
behaviour rather than in presentation.

| Problem with Snipping Tool | What Snipd does |
| --- | --- |
| Captures are lost if you forget to save | Every capture is on disk before anything else can fail |
| Meaningless generic filenames | Your naming pattern, with a live preview of the result |
| Saves wherever it feels like | Always the folder you chose |
| No history unless you saved elsewhere | A full, searchable library of every capture ever taken |
| Thin annotation tools | Pen, arrows, shapes, text, crop, and redaction for hiding sensitive details |
| Cannot keep a shot visible while you work | Pin any capture as a floating always-on-top window |
| Clipboard copy is inconsistent | Copying retries while the clipboard is locked instead of silently failing |
| Deleting is unrecoverable | Deleting moves a capture to the Recycle Bin, never unlinks it |
| Poor multi-monitor behaviour | One coordinate space across all displays, including mixed DPI |

## Features

**Capture** — press the shortcut anywhere and the screen freezes with a toolbar:
rectangle, freeform lasso, window, full screen, or record, plus a delay timer
for catching menus and hover states. Every capture saves automatically.

**Two ways to work** — by default a capture opens so you can draw on it and give
it a real name, then keep it. Or switch to instant saving and it is simply on
disk the moment you release the mouse. Either way the file is written
immediately; the choice only changes what happens next.

**Recording** — choose Record in the same overlay, drag an area, and a small bar
shows the elapsed time with a stop button; the tray menu can stop one too, so a
recording is never stuck running. Encoded with the H.264 encoder built into
Windows, so there is nothing extra to install. Frame rate, resolution and
bitrate are all configurable.

**Library** — every capture, searchable by filename and filterable by date, with
lazily loaded thumbnails so a folder of thousands stays fast.

**Editor** — pen, arrow, rectangle, ellipse, text, crop, and a redaction tool
that pixelates rather than blurs. Undo and redo throughout. Reviewing a capture
you just took keeps it, renamed if you like; editing something from the library
later writes a *copy*, so a file you may already have shared is never rewritten
underneath you.

**Pins** — float any capture on top of everything else while you work. Several at
once, each its own window, dragged by the image itself.

**Local only** — no account, no login, no cloud, no telemetry. Captures never
leave your machine.

## Installing

Download `Snipd-Setup-x.y.z.exe` from
[Releases](https://github.com/snipd-app/snipd/releases) and run it. The installer
asks where to save captures, how to name them, which format to use, and whether
to copy to the clipboard and start with Windows — and the app honours all of it
on first launch.

The installer is not code-signed, so Windows SmartScreen will warn on first run.
Choose **More info → Run anyway**.

Everything the installer asks is editable afterwards in Settings.

## Building from source

### Prerequisites

- [Node.js](https://nodejs.org/) 20 or newer
- [Rust](https://rustup.rs/) (stable)
- **Visual Studio Build Tools** with the *Desktop development with C++* workload
- **WebView2 Runtime** — already present on Windows 11 and current Windows 10

With `winget`:

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup -e
```

### Run in development

```bash
npm install
npm run tauri dev
```

### Build a standalone executable

```bash
npm run tauri build -- --no-bundle
```

Produces `src-tauri/target/release/snipd.exe`.

### Build the installer

Needs [Inno Setup 6](https://jrsoftware.org/isinfo.php)
(`winget install --id JRSoftware.InnoSetup -e`), and the executable above:

```bash
"%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer\snipd.iss
```

Output lands in `installer/Output/`.

### Tests

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets
cargo test
```

There are also two hardware smoke tests, which CI cannot run because they need a
real desktop. The first reports your display layout, which is useful for
checking a multi-monitor or mixed-DPI setup:

```bash
cd src-tauri
cargo run --example smoke_capture
cargo run --example smoke_record
```

### The icon

`assets/icon.svg` is the source. Regenerate every size with:

```bash
npm run icon
```

## How it works

The parts worth knowing about:

- **`src-tauri/src/capture/win.rs`** — the capture backend, written against Win32
  directly rather than using a capture crate. It treats *virtual-screen
  coordinates in physical pixels* as the one coordinate space for the whole app.
  A single `BitBlt` of the virtual desktop returns an image whose pixel grid *is*
  that space, so cropping a selection spanning two monitors is plain arithmetic
  rather than a stitching step that can be got wrong.

- **`src-tauri/src/capture/mod.rs`** — enforces the ordering rule the app rests
  on: read pixels, **write the file**, then everything else. Nothing after the
  write may fail the capture. A locked clipboard yields a warning attached to a
  successful result, never a lost screenshot.

- **`src-tauri/src/capture/mask.rs`** — freeform masking by scanline fill rather
  than per-pixel point-in-polygon. The naive form is O(pixels × edges), which on
  a real lasso is hundreds of millions of operations while the user waits.

- **`src-tauri/src/history.rs`** — history is rebuilt by scanning the save folder.
  The sidecar index only adds what the filesystem cannot know, so deleting it
  costs metadata rather than captures.

- **`src/overlay.ts`** — the capture overlay. The screen is frozen in Rust
  *before* the overlay appears, so it can never end up in its own capture and a
  delayed capture can catch an open menu without the overlay closing it.

- **`src-tauri/src/record/`** — recording. Media Foundation supplies the H.264
  encoder and MP4 muxer that ship with Windows, so no ffmpeg is bundled and no
  licensing questions arise. The capture loop paces itself against a fixed
  schedule rather than sleeping between frames, and *drops* frames it cannot
  produce in time rather than letting the timeline drift — a visible stutter is
  better than a video that silently runs slow, and the count is surfaced.

- **`src/editor.tsx`** — annotation. Edits are a list of shapes in *image*
  coordinates and the canvas repaints from scratch on every change, which makes
  undo correct by construction and keeps the export independent of display size.

### Design

A dark/dim monochromatic system with **no coloured accent**. The app sits on top
of whatever you are capturing, so its chrome must never compete with the
screenshot. Active state is pure white on dark and pure black on light, which
reads unambiguously against any content underneath — and the selection border
pairs a white line with a dark outer stroke for the same reason, since a
screenshot tool cannot know what is behind it. Tokens live in `src/tokens.css`.

### Known limitation

The capture backend reads the desktop DWM has composited, which covers normal
applications, browsers and video playback. Content drawn through a hardware
overlay plane or protected by DRM appears black — the same behaviour as most
classic screenshot tools. The backend sits behind a module boundary so a Windows
Graphics Capture path can be added without changing callers.

## Roadmap

- [x] Capture engine — auto-save and auto-naming that cannot lose a capture
- [x] Overlay capture flow — rectangle, freeform, window, full screen, delay timer
- [x] Tray and global shortcuts
- [x] History and library
- [x] Annotation — pen, arrows, shapes, text, redaction, crop, undo/redo
- [x] Pinned always-on-top windows
- [x] Settings, notifications, retention
- [x] Visual design pass
- [x] Inno Setup installer with first-run configuration
- [x] Screen recording, with frame rate and resolution settings

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). It covers the setup, what CI checks, and
the handful of principles the codebase is built around — worth a skim before a
first pull request.

## Licence

[MIT](LICENSE)
