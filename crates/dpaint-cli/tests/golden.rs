//! Golden renders.
//!
//! A fixture is a **journal**, not a mystery binary: `ops.jsonl` is the exact op stream that
//! produces the image, so a golden is reproducible by replay and reviewable as a diff.
//!
//! Comparison is perceptual (SSIM + ΔE2000), not byte-exact, so a one-bit rounding change in a
//! dependency is not a test failure while a real visual regression is.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p dpaint-cli --test golden`. Regeneration is
//! a reviewable diff in the pull request, never something CI does silently.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("dpaint")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Replay a fixture's op stream into a fresh project and render it.
fn replay(case: &str, work: &Path) -> PathBuf {
    let dir = fixtures().join(case);
    let stream = std::fs::read_to_string(dir.join("ops.jsonl"))
        .unwrap_or_else(|e| panic!("fixture {case} has no ops.jsonl: {e}"));
    let project = work.join("case.dpaint");

    for (i, line) in stream.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("{case}:{} bad json: {e}", i + 1));

        let mut cmd = Command::new(bin());
        cmd.current_dir(work).arg("--project").arg(&project);
        if let Some(doc) = entry.get("doc").and_then(|d| d.as_str()) {
            cmd.arg("--doc").arg(doc);
        }
        match entry.get("cmd").and_then(|c| c.as_str()) {
            Some("new") => {
                cmd.arg("new")
                    .arg(entry["name"].as_str().expect("new needs a name"))
                    .arg("--kind")
                    .arg(entry["kind"].as_str().unwrap_or("raster"));
                if let Some(size) = entry.get("size").and_then(|s| s.as_str()) {
                    cmd.arg("--size").arg(size);
                }
            }
            _ => {
                cmd.arg("op")
                    .arg(entry["op"].as_str().expect("op entry needs an op id"))
                    .arg("--args")
                    .arg(entry.get("args").cloned().unwrap_or(serde_json::json!({})).to_string());
            }
        }

        let out = cmd.output().expect("run dpaint");
        assert!(
            out.status.success(),
            "{case}:{} failed: {}",
            i + 1,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let render_doc = stream
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .and_then(|_| None::<String>)
        .unwrap_or_default();
    let _ = render_doc;

    let actual = work.join("actual.png");
    let mut cmd = Command::new(bin());
    cmd.current_dir(work).arg("--project").arg(&project);
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("render.json")).unwrap_or_else(|_| "{}".into()),
    )
    .unwrap_or(serde_json::json!({}));
    if let Some(doc) = meta.get("doc").and_then(|d| d.as_str()) {
        cmd.arg("--doc").arg(doc);
    }
    cmd.arg("render").arg(&actual);
    for (flag, key) in [("--scale", "scale"), ("--width", "width"), ("--background", "background")] {
        if let Some(v) = meta.get(key) {
            cmd.arg(flag).arg(v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()));
        }
    }
    let out = cmd.output().expect("render");
    assert!(out.status.success(), "render failed: {}", String::from_utf8_lossy(&out.stderr));
    actual
}

fn check(case: &str) {
    let work = tempfile::tempdir().expect("tempdir");
    let actual_path = replay(case, work.path());
    let expected_path = fixtures().join(case).join("expected.png");

    if std::env::var("UPDATE_GOLDENS").is_ok() {
        std::fs::copy(&actual_path, &expected_path).expect("write golden");
        eprintln!("updated golden for {case}");
        return;
    }

    let expected = image::open(&expected_path)
        .unwrap_or_else(|e| panic!("missing golden for {case} ({e}); run with UPDATE_GOLDENS=1"))
        .to_rgba8();
    let actual = image::open(&actual_path).expect("actual render decodes").to_rgba8();

    assert_eq!(
        expected.dimensions(),
        actual.dimensions(),
        "{case}: render size changed"
    );

    let d = dpaint_inspect::diff::compare(&expected, &actual).expect("diff");
    if d.ssim < 0.999 || d.max_delta_e > 1.0 {
        // Leave the evidence next to the golden so the failure is inspectable.
        let out = fixtures().join(case);
        let _ = std::fs::copy(&actual_path, out.join("actual.png"));
        if let Ok(hm) = dpaint_inspect::diff::heatmap(&expected, &actual) {
            let _ = hm.save(out.join("diff.png"));
        }
        panic!(
            "{case} drifted: ssim {:.6}, maxDE {:.3}, changed {:.4}% (wrote actual.png and diff.png)",
            d.ssim,
            d.max_delta_e,
            d.changed_fraction * 100.0
        );
    }
}

#[test]
fn campaign_poster_matches_its_golden() {
    check("poster");
}

#[test]
fn blend_mode_grid_matches_its_golden() {
    check("blend-grid");
}
