//! The shell's own surface: the bundled frontend.

#[test]
fn the_shared_frontend_is_bundled_into_the_app_not_read_from_a_developers_disk() {
    // `frontendDist` in tauri.conf.json is a path relative to this crate; if it stops resolving
    // the app ships without a UI, so assert it from the manifest directory the same way the
    // Tauri codegen does.
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/dpaint-studio/ui")
        .canonicalize()
        .expect("frontendDist must resolve from apps/studio");
    for f in ["index.html", "studio.css", "studio.js"] {
        assert!(dist.join(f).is_file(), "frontendDist is missing {f}");
    }

    // And the codegen really embedded them: this is the same context `run()` passes to Tauri.
    // Byte equality also proves build.rs re-embeds when StudioUI edits a file, instead of
    // shipping a stale copy.
    let ctx: tauri::Context<tauri::Wry> = tauri::generate_context!();
    for f in ["index.html", "studio.css", "studio.js"] {
        let embedded = tauri::Assets::get(ctx.assets(), &f.into())
            .unwrap_or_else(|| panic!("{f} must be compiled into the binary"));
        assert_eq!(
            embedded.as_ref(),
            std::fs::read(dist.join(f)).unwrap().as_slice(),
            "the embedded {f} is not the shared UI file"
        );
    }
}
