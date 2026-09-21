fn main() {
    // Icons and the Tauri config are embedded into the binary at build time, by
    // this script and by `generate_context!`. Cargo will happily skip re-running
    // a build script whose declared inputs have not changed — and by default it
    // has no idea the icons are inputs at all.
    //
    // The result is a binary that ships a stale icon while the files on disk
    // look perfectly correct, which is as confusing as it sounds. It happened
    // here exactly once: the window icon stayed the framework's default long
    // after the real one had been generated.
    println!("cargo:rerun-if-changed=tauri.conf.json");
    for entry in std::fs::read_dir("icons").into_iter().flatten().flatten() {
        println!("cargo:rerun-if-changed={}", entry.path().display());
    }

    tauri_build::build()
}
