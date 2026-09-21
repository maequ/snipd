## What this changes

<!-- What the change does, and why. If it fixes an issue, link it. -->

## Checks

- [ ] `npm run build` passes
- [ ] `cargo fmt --check`, `cargo clippy --all-targets` and `cargo test` pass
- [ ] If this touches capture or recording, the hardware smoke tests were run
      (`cargo run --example smoke_capture`, `cargo run --example smoke_record`)

## Things this project cares about

<!-- Delete any that do not apply. See CONTRIBUTING.md. -->

- [ ] No step was added between reading pixels and writing the file
- [ ] Coordinates stay in virtual-screen physical pixels
- [ ] No coloured accent added to the app's chrome
- [ ] Nothing new reaches the network
