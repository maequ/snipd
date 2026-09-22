# Changelog

Every release says what changed and, where it matters, why. The release workflow
reads the section matching the tag and publishes it as the release notes, so
this file is the single place those notes are written.

The format is [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Nothing yet.

## [0.3.2] - 2026-09-22

### Fixed

- **The recorder bar stayed on screen after a recording had been saved.**
  Stopping a recording finalises the MP4, which writes the file's index and
  takes long enough to matter — and it was being done on the event loop, so
  nothing could act on the bar being asked to close until it finished.
- The same fault in three more places, found by looking for it rather than
  waiting for it to be reported: opening the editor **from the library**,
  pinning a capture, and closing a pin all built or closed a window from the
  event loop, which is the freeze fixed in 0.3.1 reached by a different route.
  Capturing from the main window did its screen read and PNG encode there too.

  All of them now run off it. The rule this settles: anything that builds a
  window, closes one, or takes long enough to notice does not belong on a
  synchronous command.

## [0.3.1] - 2026-09-22

### Fixed

- **The application froze when taking a capture.** Releasing a selection could
  leave it unresponsive for minutes, and quitting from the tray then left a
  process behind that made the next launch do nothing at all.

  Tauri runs a synchronous command on the main thread, and the main thread is
  the event loop. Opening the editor from inside one meant asking the event loop
  to build a window before the handler it was waiting on had returned. The
  overlay never hit this only because its command happens to be declared
  `async`. Everything that follows a capture now runs off the command's thread,
  so it does not depend on which commands are async and which are not.
- **Saving a reviewed capture renamed it to "_2".** The name it already had was
  compared against its own canonicalised path, and on Windows those never match
  because canonicalising produces an extended-length path. Every save therefore
  looked like a collision with itself. Both sides are resolved before deciding
  anything has collided now.
- **The editor stayed open after saving.** Closing a window is not covered by
  the default permission set, so the editor asking to close itself did nothing
  at all. The same was true of the recorder bar and pinned captures.
- **Quit now quits.** It asked the event loop to wind down, which it cannot do
  while a thread is wedged — and a process that outlives Quit makes the
  single-instance guard swallow the next launch, which is what "I quit it and
  now it will not open" actually was. It now insists if asking does not work.

### Added

- **A log file**, at `%APPDATA%\Snipd\snipd.log`, reachable from
  Settings → About → Show the log. The application is a windowed binary, so it
  has no console and everything it had to say about a failure went nowhere.
  Operations that have ever been slow enough to look like a hang are timed, so
  a stall names itself instead of having to be guessed at.

## [0.3.0] - 2026-09-21

### Added

- **Recordings have sound.** An AAC track of whatever the machine is playing,
  captured through WASAPI loopback on the default playback device. Nothing extra
  to install and no virtual audio driver. Microphones are *not* captured — that
  is a separate feature. Turn it off in Settings → Recording → Sound.
- A **confirmation dialog** before a capture is deleted. It names the file, says
  plainly that it goes to the Recycle Bin and can be recovered, and offers
  "Don't ask me again" — also a setting under Settings → Saving → Deleting.

### Changed

- Audio is best effort throughout: a machine with no playback device, or one
  whose format the AAC encoder will not accept, still records picture and
  reports that the file is silent rather than failing.
- `Encoder::create` took eight positional arguments, four of them dimensions.
  They are grouped into structs now; transposing two of six bare integers is
  easy to do and hard to see afterwards.

### Notes

Three details in the audio path are what separate audio that works from audio
that drifts. Silence is written as real zeroed samples rather than skipped,
because a recording where ten quiet seconds do not exist leaves the sound ten
seconds ahead of the picture for the rest of the file. Timestamps come from a
running sample count rather than the clock, matching how video frames are timed,
so both describe one timeline. And every write happens on the recording thread,
with the capture thread only queueing — a sink writer is not safe to call from
two threads at once.

## [0.2.1] - 2026-09-21

### Fixed

- **Recordings were upside down.** Uncompressed RGB in Media Foundation is
  bottom-up by convention, so frames handed over without a declared stride were
  read as starting at the bottom row. The capture produces top-down frames; it
  now says so.
- **Dropped frames.** Recording a large area dropped around a third of its
  frames on hardware that should not have struggled. Every frame was creating a
  screen device context, a memory device context, a bitmap and a fresh buffer —
  reasonable once, ruinous thirty times a second — and every frame used
  `CAPTUREBLT`, a flag that exists so layered windows appear in a *still*
  capture and costs far more than a plain blit. Measured afterwards at
  2560x1440 and 30fps: 151 frames written, none dropped.
- **Releasing a selection appeared to freeze the screen.** The overlay covers
  the display with a still image of it, and it was being left up while the
  editor window was built. It now comes down first.
- Toolbar buttons in the capture overlay have real hover labels with their
  shortcut keys. They relied on the `title` attribute, which Windows draws on
  its own schedule and which in a borderless always-on-top window often flashed
  briefly or never appeared.

### Changed

- The encoder asks for H.264 High profile rather than whatever it defaulted to.
  This matters more for screen content than for camera footage, because sharp
  text is exactly what the simpler profiles smear.
- The configured bitrate is treated as a floor and raised to suit the pixel
  count, so a 1440p recording is not handed a figure chosen with a small region
  in mind.

## [0.2.0] - 2026-09-21

### Fixed

- **Saving an annotated capture failed** with a `SecurityError` and no
  explanation. Captures are served over a custom scheme on `snipd.localhost`,
  a different origin from the page, so drawing one onto a canvas tainted it and
  every read of that canvas threw. The editor's entire export path is a canvas
  read. The protocol now sends `Access-Control-Allow-Origin` and the editor
  requests the image in CORS mode; both halves are required.
- Export failures are named. Returning a bare null made every cause — no image,
  a crop of zero size, a tainted canvas — arrive identically, which is why a
  real error was reported as "Nothing to save".
- The release workflow could not build the installer at all:
  `$env:ProgramFiles(x86)` is read by PowerShell as `$env:ProgramFiles`
  followed by a literal `(x86)`.

### Changed

- **The editor is its own window again.** Turning the library window into an
  editor and back meant taking one screenshot appeared to open the whole
  application. It also no longer drags the library up behind it.
- Opening the editor is driven from Rust rather than by the main window
  reacting to an event. That window is deliberately hidden while reviewing, and
  WebView2 suspends a hidden webview, so a handler inside it may never run.
- Keeping a reviewed capture confirms itself by name in the library instead of
  the editor simply vanishing.

## [0.1.0] - 2026-09-21

First release.

### Added

- Capture: rectangle, freeform lasso, window, full screen and record, chosen
  from one overlay, with a delay timer for catching menus.
- Auto-save that cannot lose a capture: pixels are read, the file is written,
  and only then does anything else happen. A locked clipboard produces a
  warning attached to a successful result, never a lost screenshot.
- A searchable, filterable library with lazily loaded thumbnails.
- Annotation: pen, arrow, rectangle, ellipse, text, crop, and redaction that
  pixelates rather than blurs, with undo and redo.
- Screen recording to MP4 using the H.264 encoder built into Windows.
- Pinned always-on-top captures.
- An installer that asks where to save, how to name, which format, what happens
  after a capture, clipboard and startup — and seeds the app's first run.
- Deleting moves captures to the Recycle Bin rather than unlinking them.

[Unreleased]: https://github.com/maequ/snipd/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/maequ/snipd/releases/tag/v0.3.0
[0.2.1]: https://github.com/maequ/snipd/releases/tag/v0.2.1
[0.2.0]: https://github.com/maequ/snipd/releases/tag/v0.2.0
[0.1.0]: https://github.com/maequ/snipd/releases/tag/v0.1.0
