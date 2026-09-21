# Contributing

Thanks for looking. This is a small, focused project and contributions are
welcome.

## Getting set up

See [Building from source](README.md#building-from-source) in the README. In
short: Node 20+, a stable Rust toolchain, and the Visual Studio C++ build tools.

```bash
npm install
npm run tauri dev
```

## Before opening a pull request

```bash
npm run build                   # type-checks the frontend
cd src-tauri
cargo fmt
cargo clippy --all-targets
cargo test
```

CI runs all of these on Windows with warnings treated as errors.

There are also two hardware smoke tests that CI cannot run, because they need a
real desktop. Run them yourself if you touch capture or recording:

```bash
cd src-tauri
cargo run --example smoke_capture     # reports your display layout too
cargo run --example smoke_record
```

## The things this project cares about

A few principles run through the codebase. Changes that cut against them will
get pushback, so they are worth knowing up front.

**A capture is never lost.** Pixels are read, the file is written, and only then
does anything else happen. Nothing after the write may fail a capture — a locked
clipboard produces a warning attached to a successful result, never an error
instead of one. If you add a step, it goes after the write.

**One coordinate space.** Virtual-screen coordinates in physical pixels, from the
capture backend through to the overlay. Mixing in logical pixels or per-monitor
coordinates is how multi-monitor bugs get in. The overlay *measures* its scale
rather than trusting `devicePixelRatio`.

**The filesystem is the source of truth.** History is rebuilt by scanning the
save folder. The index only adds what the filesystem cannot know. Deleting it
must never lose a capture from view.

**No coloured accent.** The app sits on top of whatever is being captured, so its
chrome must not compete with the screenshot. Active state is pure white on dark
and pure black on light. Colour appears in exactly two places: the user's own
annotation ink, and error text.

**Motion is for meaning.** Three moments have it — something arriving, something
being pointed at, switching view — and everything respects
`prefers-reduced-motion`. Decorative fade-and-slide on every element is
explicitly not wanted.

**Local only.** No accounts, no uploads, no telemetry, ever. A pull request that
adds a network call to the app needs a very good reason.

## Style

Comments explain *why*, not *what*. If a piece of code looks odd, say what would
go wrong if it were written the obvious way — several of the trickier parts of
this codebase exist because the obvious version was wrong, and the comment is
what stops someone helpfully "fixing" it back.

## Releasing

Releases are cut by tagging:

```bash
git tag v0.4.0
git push --tags
```

That builds the executable and the installer on a clean Windows runner and
publishes them, with the notes taken from the matching section of
[CHANGELOG.md](CHANGELOG.md). **Add that section before tagging** — the workflow
fails if the version has none, deliberately, so that a release can never go out
without saying what changed.

## Reporting a bug

Include your Windows version, whether you have more than one display, and
whether any of them use display scaling. Most of the hard bugs in a screenshot
tool are multi-monitor or DPI bugs.
