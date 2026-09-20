//! End-to-end proof: one project, three document kinds, cross-mode references, driven
//! exactly the way an agent drives it — through the `dpaint` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    // `cargo test` puts integration binaries in target/<profile>/deps.
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("dpaint")
}

struct Cli {
    dir: tempfile::TempDir,
    project: PathBuf,
}

impl Cli {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("demo.dpaint");
        Self { dir, project }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(bin())
            .args(args)
            .arg("--project")
            .arg(&self.project)
            .current_dir(self.dir.path())
            .output()
            .unwrap_or_else(|e| panic!("could not run {}: {e}", bin().display()));
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn ok(&self, args: &[&str]) -> serde_json::Value {
        let mut a = args.to_vec();
        a.push("--json");
        let (code, stdout, stderr) = self.run(&a);
        assert_eq!(code, 0, "`dpaint {}` failed:\n{stderr}", args.join(" "));
        serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
}

fn exists_nonempty(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}

#[test]
fn one_source_path_drives_a_png_an_svg_and_a_glb() {
    let cli = Cli::new();

    // A vector logo: a triangle with a hole punched out of it by a real boolean op.
    cli.ok(&["new", "campaign", "--kind", "vector", "--size", "512x512"]);
    cli.ok(&[
        "op",
        "vector.object.add-path",
        "--d",
        "M256 32 L480 448 L32 448 Z",
        "--fill",
        "#fb8500",
        "--name",
        "mark",
    ]);
    cli.ok(&[
        "op",
        "vector.object.add-ellipse",
        "--cx",
        "256",
        "--cy",
        "330",
        "--rx",
        "70",
        "--ry",
        "70",
        "--name",
        "hole",
    ]);
    cli.ok(&[
        "op",
        "vector.path.boolean",
        "--target",
        "@mark, @hole",
        "--op",
        "subtract",
    ]);

    let svg = cli.path("out/logo.svg");
    cli.ok(&["render", svg.to_str().unwrap()]);
    assert!(exists_nonempty(&svg), "svg export produced nothing");
    let svg_text = std::fs::read_to_string(&svg).unwrap();
    assert!(
        svg_text.contains("<svg"),
        "not an svg: {}",
        &svg_text[..svg_text.len().min(120)]
    );

    // The same path extruded into a 3D badge.
    cli.ok(&["op", "doc.add", "--name", "badge", "--kind", "model"]);
    cli.ok(&[
        "--doc",
        "badge",
        "op",
        "model.mesh.extrude",
        "--path",
        "campaign:@mark",
        "--depth",
        "24",
        "--node",
        "true",
    ]);
    let glb = cli.path("out/badge.glb");
    cli.ok(&["--doc", "badge", "render", glb.to_str().unwrap()]);
    assert!(exists_nonempty(&glb), "glb export produced nothing");
    let glb_bytes = std::fs::read(&glb).unwrap();
    assert_eq!(&glb_bytes[0..4], b"glTF", "GLB must carry the glTF magic");

    // A raster poster that links the logo document in live.
    cli.ok(&[
        "op", "doc.add", "--name", "poster", "--kind", "raster", "--width", "800", "--height",
        "1000",
    ]);
    cli.ok(&[
        "--doc",
        "poster",
        "op",
        "raster.layer.add",
        "--type",
        "fill",
        "--color",
        "#1d3557",
        "--name",
        "bg",
    ]);
    cli.ok(&[
        "--doc",
        "poster",
        "op",
        "raster.layer.add",
        "--type",
        "linked",
        "--source",
        "campaign",
        "--box",
        "200,240,400,400",
        "--name",
        "badge-mark",
    ]);

    let png = cli.path("out/poster.png");
    cli.ok(&["--doc", "poster", "render", png.to_str().unwrap()]);
    assert!(exists_nonempty(&png), "png export produced nothing");
    let img = image::open(&png)
        .expect("poster is a decodable png")
        .to_rgba8();
    assert_eq!(img.dimensions(), (800, 1000));

    // The linked logo must actually appear in the poster: its orange has to be present.
    let orange = img
        .pixels()
        .filter(|p| p.0[0] > 200 && p.0[1] > 100 && p.0[1] < 190 && p.0[2] < 60)
        .count();
    assert!(
        orange > 500,
        "the linked vector document did not render into the poster ({orange} px)"
    );
}

#[test]
fn editing_the_source_propagates_to_every_consumer() {
    let cli = Cli::new();
    cli.ok(&["new", "campaign", "--kind", "vector", "--size", "256x256"]);
    cli.ok(&[
        "op",
        "vector.object.add-rect",
        "--x",
        "32",
        "--y",
        "32",
        "--width",
        "192",
        "--height",
        "192",
        "--fill",
        "#ff0000",
        "--name",
        "block",
    ]);
    cli.ok(&[
        "op", "doc.add", "--name", "poster", "--kind", "raster", "--width", "256", "--height",
        "256",
    ]);
    cli.ok(&[
        "--doc",
        "poster",
        "op",
        "raster.layer.add",
        "--type",
        "linked",
        "--source",
        "campaign",
        "--box",
        "0,0,256,256",
        "--name",
        "art",
    ]);

    let before = cli.path("before.png");
    cli.ok(&["--doc", "poster", "render", before.to_str().unwrap()]);

    cli.ok(&[
        "op",
        "vector.style.fill",
        "--target",
        "@block",
        "--color",
        "#0000ff",
    ]);

    let after = cli.path("after.png");
    cli.ok(&["--doc", "poster", "render", after.to_str().unwrap()]);

    let (code, stdout, stderr) = cli.run(&[
        "diff",
        before.to_str().unwrap(),
        after.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        code, 4,
        "differing images exit 4 so a loop can branch on it\n{stderr}"
    );
    let d: serde_json::Value = serde_json::from_str(&stdout).expect("diff --json is json");
    let changed = d["diff"]["changed_fraction"].as_f64().unwrap_or(0.0);
    assert!(
        changed > 0.4,
        "recoloring the linked source should repaint most of the poster, changed {changed}"
    );
}

#[test]
fn undo_and_redo_round_trip_the_document_through_the_cli() {
    let cli = Cli::new();
    cli.ok(&["new", "p", "--kind", "raster", "--size", "64x64"]);
    let before = cli.ok(&["inspect", "--fast"]);

    cli.ok(&[
        "op",
        "raster.layer.add",
        "--type",
        "fill",
        "--color",
        "#ffffff",
        "--name",
        "bg",
    ]);
    let with_layer = cli.ok(&["inspect", "--fast"]);
    assert_ne!(
        before["result"]["data"]["tree"],
        with_layer["result"]["data"]["tree"]
    );

    cli.ok(&["undo"]);
    let undone = cli.ok(&["inspect", "--fast"]);
    assert_eq!(
        before["result"]["data"]["tree"], undone["result"]["data"]["tree"],
        "undo must restore the exact prior tree"
    );

    cli.ok(&["redo"]);
    let redone = cli.ok(&["inspect", "--fast"]);
    assert_eq!(
        with_layer["result"]["data"]["tree"],
        redone["result"]["data"]["tree"]
    );
}

#[test]
fn lint_finds_seeded_defects_and_exits_four() {
    let cli = Cli::new();
    cli.ok(&["new", "p", "--kind", "raster", "--size", "200x200"]);
    cli.ok(&[
        "op",
        "raster.layer.add",
        "--type",
        "fill",
        "--color",
        "#ffffff",
        "--name",
        "bg",
    ]);
    // White text on a white background: unreadable, and lint must say so.
    cli.ok(&[
        "op",
        "raster.layer.add",
        "--type",
        "text",
        "--text",
        "INVISIBLE",
        "--text.size",
        "24",
        "--fill",
        "#fefefe",
        "--name",
        "title",
    ]);

    let (code, stdout, stderr) = cli.run(&["lint", "--json"]);
    assert_eq!(
        code, 4,
        "lint with findings must exit 4\nstdout:{stdout}\nstderr:{stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("lint --json is json");
    let rules: Vec<&str> = report["report"]["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["rule"].as_str())
        .collect();
    assert!(
        rules.contains(&"low-contrast"),
        "expected a contrast failure, got {rules:?}"
    );
}

#[test]
fn a_bad_selector_exits_three_and_names_the_real_candidates() {
    let cli = Cli::new();
    cli.ok(&["new", "p", "--kind", "raster", "--size", "32x32"]);
    cli.ok(&[
        "op",
        "raster.layer.add",
        "--type",
        "fill",
        "--color",
        "#000000",
        "--name",
        "sky-grad",
    ]);

    let (code, _out, err) = cli.run(&[
        "op",
        "raster.layer.set",
        "--target",
        "#sky",
        "--opacity",
        "0.5",
    ]);
    assert_eq!(code, 3, "a selector that matches nothing is exit 3");
    assert!(
        err.contains("sky-grad"),
        "the error must list what does exist so the agent can correct in one turn: {err}"
    );
}

#[test]
fn dry_run_reports_the_effect_without_touching_the_project() {
    let cli = Cli::new();
    cli.ok(&["new", "p", "--kind", "raster", "--size", "32x32"]);
    let before = std::fs::read_to_string(cli.project.join("project.json")).unwrap();

    cli.ok(&[
        "--dry-run",
        "op",
        "raster.layer.add",
        "--type",
        "fill",
        "--color",
        "#ff0000",
        "--name",
        "x",
    ]);

    let after = std::fs::read_to_string(cli.project.join("project.json")).unwrap();
    assert_eq!(before, after, "--dry-run must not write");
}

#[test]
fn the_op_catalog_is_discoverable_and_every_schema_is_well_formed() {
    let cli = Cli::new();
    cli.ok(&["new", "p", "--kind", "raster"]);

    let list = cli.ok(&["op", "--list"]);
    let ops = list["ops"].as_array().expect("op list");
    assert!(
        ops.len() > 80,
        "expected a real catalog, got {} ops",
        ops.len()
    );

    let catalog = {
        let (code, stdout, _) = cli.run(&["schema", "--all"]);
        assert_eq!(code, 0);
        serde_json::from_str::<serde_json::Value>(&stdout).expect("schema --all is json")
    };
    for entry in catalog.as_array().expect("catalog array") {
        let id = entry["id"].as_str().expect("op id");
        let schema = &entry["schema"];
        assert!(
            schema.get("type").is_some()
                || schema.get("$ref").is_some()
                || schema.get("properties").is_some(),
            "{id} has no usable schema: {schema}"
        );
        assert!(
            !entry["about"].as_str().unwrap_or("").is_empty(),
            "{id} has no description"
        );
    }
}

#[test]
fn an_explicit_width_is_honored_for_every_document_kind() {
    let cli = Cli::new();
    cli.ok(&["new", "p", "--kind", "raster", "--size", "400x200"]);
    cli.ok(&[
        "op",
        "raster.layer.add",
        "--type",
        "fill",
        "--color",
        "#2a9d8f",
        "--name",
        "bg",
    ]);
    cli.ok(&[
        "op", "doc.add", "--name", "art", "--kind", "vector", "--width", "400", "--height", "200",
    ]);
    cli.ok(&[
        "--doc",
        "art",
        "op",
        "vector.object.add-rect",
        "--x",
        "0",
        "--y",
        "0",
        "--width",
        "400",
        "--height",
        "200",
        "--fill",
        "#e76f51",
    ]);

    for (doc, name) in [("p", "raster.png"), ("art", "vector.png")] {
        let out = cli.path(name);
        cli.ok(&[
            "--doc",
            doc,
            "render",
            out.to_str().unwrap(),
            "--width",
            "120",
        ]);
        let img = image::open(&out).expect("decodes").to_rgba8();
        assert_eq!(
            img.dimensions(),
            (120, 60),
            "{doc}: an explicit --width must resize and keep the aspect ratio"
        );
    }
}

#[test]
fn renders_are_deterministic_across_processes() {
    let cli = Cli::new();
    cli.ok(&["new", "p", "--kind", "raster", "--size", "96x96"]);
    cli.ok(&[
        "op",
        "raster.layer.add",
        "--type",
        "fill",
        "--color",
        "#2a9d8f",
        "--name",
        "bg",
    ]);
    cli.ok(&[
        "op",
        "raster.layer.add",
        "--type",
        "shape",
        "--d",
        "M20 20 H76 V76 H20 Z",
        "--fill",
        "#e76f51",
        "--name",
        "block",
    ]);
    // Filters need pixels, so baking is an explicit step rather than an implicit surprise.
    cli.ok(&["op", "raster.layer.rasterize", "--target", "@block"]);
    cli.ok(&[
        "op",
        "raster.filter.gaussian-blur",
        "--target",
        "@block",
        "--sigma",
        "4",
    ]);

    let a = cli.path("a.png");
    let b = cli.path("b.png");
    cli.ok(&["render", a.to_str().unwrap()]);
    cli.ok(&["render", b.to_str().unwrap()]);
    assert_eq!(
        std::fs::read(&a).unwrap(),
        std::fs::read(&b).unwrap(),
        "identical input must produce byte-identical output, or goldens are worthless"
    );
}
