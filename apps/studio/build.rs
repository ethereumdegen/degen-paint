use std::path::{Path, PathBuf};

fn main() {
    // `tauri_build::build()` emits rerun-if-changed for tauri.conf.json and capabilities/, but
    // not for the files `generate_context!` embeds. Without this, editing the shared UI in
    // crates/dpaint-studio/ui leaves a stale copy baked into the app binary.
    let dist = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"))
        .join("../../crates/dpaint-studio/ui");
    println!("cargo:rerun-if-changed={}", dist.display());
    track(&dist);

    tauri_build::build()
}

fn track(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        panic!("frontendDist {} is missing", dir.display());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            track(&path);
        }
    }
}
