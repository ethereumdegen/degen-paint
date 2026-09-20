//! `dpaint` — the command line surface of degen-paint.
//!
//! Every verb speaks `--json`, every exit code means something (see `docs/errors.md`), and
//! `op` subcommands are generated from the op registry rather than hand-maintained.

mod args;

use dpaint_core::{
    doc::{Document, ModelDoc, RasterDoc, VectorDoc},
    journal::Actor,
    AssetStore, DocId, DocKind, Engine, Error, Project, Registry, Result, Workspace,
};
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let json_out = argv.iter().any(|a| a == "--json");
    match run(&argv) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let detail = e.detail();
            if json_out {
                let _ = writeln!(
                    std::io::stderr(),
                    "{}",
                    serde_json::to_string_pretty(&json!({ "ok": false, "error": detail }))
                        .unwrap_or_else(|_| "{\"ok\":false}".into())
                );
            } else {
                eprintln!("error: {e}");
                if !detail.candidates.is_empty() {
                    let shown: Vec<&String> = detail.candidates.iter().take(12).collect();
                    eprintln!(
                        "  available: {}",
                        shown
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(s) = &detail.suggestion {
                    eprintln!("  did you mean: {s}");
                }
            }
            std::process::exit(e.exit_code());
        }
    }
}

fn registry() -> Registry {
    let mut r = Registry::new();
    r.extend(dpaint_core::ops::ops());
    r.extend(dpaint_raster::ops());
    r.extend(dpaint_vector::ops());
    r.extend(dpaint_model3d::ops());
    r.extend(dpaint_render::ops());
    r.extend(dpaint_inspect::ops());
    r.extend(dpaint_ai::ops());
    r
}

const HELP: &str = r#"dpaint — an agent-native image and 3D asset studio

USAGE
  dpaint <command> [options]

COMMANDS
  new <name>            Create a project (--kind raster|vector|model --size WxH --dpi N)
  op <id> [--flag v]    Apply an op; `dpaint op --list` shows every one
  render <path>         Render the active document (png|jpg|webp|tiff|svg|glb|gltf)
  inspect               Measure a render: tree, bounds, colors, contrast
  lint                  Report problems an agent cannot see
  diff <a> <b>          Perceptual diff of two images (SSIM + deltaE2000)
  annotate <path>       Render with numbered bboxes and a selector legend
  undo | redo           Step through history
  history               Recent journal entries, agent and human
  schema [op]           JSON Schema for the project or an op
  gc                    Delete unreferenced asset blobs
  doctor                Report capabilities: fonts, providers, formats
  mcp                   Serve the op registry over MCP on stdio
  serve                 Run the Studio UI on localhost (--addr, --ui-dir)

GLOBAL
  --project <dir>       Project directory (default: discovered from the cwd)
  --doc <id|name>       Target document (default: the project's active document)
                        Note: ops that take their own --document keep it
  --json                Machine-readable output on stdout
  --dry-run             Validate and report without writing
"#;

struct Ctx {
    project_dir: Option<PathBuf>,
    doc: Option<String>,
    json: bool,
    dry_run: bool,
    rest: Vec<String>,
}

fn split_globals(argv: &[String]) -> Ctx {
    let mut ctx = Ctx {
        project_dir: None,
        doc: None,
        json: false,
        dry_run: false,
        rest: Vec::new(),
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--json" => {
                ctx.json = true;
                i += 1;
            }
            "--dry-run" => {
                ctx.dry_run = true;
                i += 1;
            }
            "--project" => {
                ctx.project_dir = argv.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            // Only `--doc` is global: several ops take their own `--document`
            // argument (a linked layer's target, for instance) and it must reach them.
            "--doc" => {
                ctx.doc = argv.get(i + 1).cloned();
                i += 2;
            }
            _ => {
                ctx.rest.push(argv[i].clone());
                i += 1;
            }
        }
    }
    ctx
}

fn open(ctx: &Ctx) -> Result<Workspace> {
    match &ctx.project_dir {
        Some(p) => Workspace::open(p),
        None => Workspace::discover("."),
    }
}

fn emit(ctx: &Ctx, human: impl FnOnce(), value: serde_json::Value) {
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
        );
    } else {
        human();
    }
}

fn run(argv: &[String]) -> Result<i32> {
    let ctx = split_globals(argv);
    let Some(cmd) = ctx.rest.first().cloned() else {
        print!("{HELP}");
        return Ok(0);
    };
    let rest = &ctx.rest[1..];

    match cmd.as_str() {
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            Ok(0)
        }
        "version" | "--version" | "-V" => {
            println!("dpaint {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "new" => cmd_new(&ctx, rest),
        "op" => cmd_op(&ctx, rest),
        "render" => cmd_render(&ctx, rest),
        "inspect" => cmd_inspect(&ctx, rest),
        "lint" => cmd_lint(&ctx, rest),
        "diff" => cmd_diff(&ctx, rest),
        "annotate" => cmd_annotate(&ctx, rest),
        "undo" | "redo" => cmd_undo_redo(&ctx, &cmd),
        "history" => cmd_history(&ctx, rest),
        "schema" => cmd_schema(&ctx, rest),
        "gc" => cmd_gc(&ctx),
        "doctor" => cmd_doctor(&ctx),
        "mcp" => cmd_mcp(&ctx),
        "serve" => cmd_serve(&ctx, rest),
        other => Err(Error::Invalid(format!(
            "unknown command '{other}'; run `dpaint help`"
        ))),
    }
}

fn flag<'a>(argv: &'a [String], name: &str) -> Option<&'a String> {
    argv.iter()
        .position(|a| a == name)
        .and_then(|i| argv.get(i + 1))
}

fn cmd_new(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let name = argv
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            Error::Invalid(
                "usage: dpaint new <name> [--kind raster|vector|model] [--size WxH]".into(),
            )
        })?;
    let kind: DocKind = flag(argv, "--kind")
        .map(|s| s.as_str())
        .unwrap_or("raster")
        .parse()?;
    let (w, h) = match flag(argv, "--size") {
        Some(s) => {
            let (a, b) = s
                .split_once(['x', 'X', ','])
                .ok_or_else(|| Error::Invalid(format!("bad --size '{s}', expected WxH")))?;
            (
                a.trim()
                    .parse::<f64>()
                    .map_err(|_| Error::Invalid(format!("bad width in '{s}'")))?,
                b.trim()
                    .parse::<f64>()
                    .map_err(|_| Error::Invalid(format!("bad height in '{s}'")))?,
            )
        }
        None => (1024.0, 1024.0),
    };
    let dpi: f32 = flag(argv, "--dpi")
        .and_then(|d| d.parse().ok())
        .unwrap_or(72.0);

    let dir = ctx
        .project_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{name}.dpaint")));
    if dir.join("project.json").exists() {
        return Err(Error::Invalid(format!(
            "{} already holds a project",
            dir.display()
        )));
    }

    let doc_id = DocId::from_name(&name);
    let first = match kind {
        DocKind::Raster => {
            let mut d = RasterDoc::new(doc_id, &name, w as u32, h as u32);
            d.dpi = dpi;
            Document::Raster(d)
        }
        DocKind::Vector => Document::Vector(VectorDoc::new(doc_id, &name, w, h)),
        DocKind::Model => Document::Model(ModelDoc::new(doc_id, &name)),
    };
    let ws = Workspace::create(&dir, Project::new(&name, first))?;

    emit(
        ctx,
        || println!("created {} ({kind}, {w:.0}x{h:.0})", dir.display()),
        json!({ "ok": true, "project": dir.display().to_string(), "kind": kind.as_str(),
                "document": ws.project.active.as_str(), "size": [w, h] }),
    );
    Ok(0)
}

fn cmd_op(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let reg = registry();

    if argv.is_empty() || argv[0] == "--list" {
        let filter = argv.get(1).cloned().unwrap_or_default();
        let listed: Vec<&std::sync::Arc<dyn dpaint_core::Op>> = reg
            .iter()
            .filter(|o| filter.is_empty() || o.id().contains(&filter))
            .collect();
        emit(
            ctx,
            || {
                for o in &listed {
                    println!("{:<34} {}", o.id(), o.about());
                }
                println!("\n{} ops", listed.len());
            },
            json!({ "ok": true, "ops": listed.iter().map(|o| json!({
                "id": o.id(), "about": o.about(),
                "modes": o.modes().iter().map(|m| m.as_str()).collect::<Vec<_>>(),
                "query": o.is_query(), "network": o.is_network(),
            })).collect::<Vec<_>>() }),
        );
        return Ok(0);
    }

    let id = argv[0].clone();
    let op = reg.get(&id)?;

    if argv.iter().any(|a| a == "--help") {
        print!("{}", args::usage(op.id(), op.about(), &op.schema()));
        return Ok(0);
    }

    let parsed = args::parse(op.id(), &op.schema(), &argv[1..])?;
    let mut engine = Engine::new(reg, open(ctx)?);
    let applied = engine.apply(&id, parsed, ctx.doc.clone(), ctx.dry_run)?;

    emit(
        ctx,
        || {
            if let Some(d) = &applied.effect.data {
                println!("{}", serde_json::to_string_pretty(d).unwrap_or_default());
            }
            let mut parts = Vec::new();
            if !applied.effect.changed.is_empty() {
                parts.push(format!(
                    "changed {}",
                    applied
                        .effect
                        .changed
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !applied.effect.created.is_empty() {
                parts.push(format!("created {}", applied.effect.created.join(", ")));
            }
            if !applied.effect.removed.is_empty() {
                parts.push(format!("removed {}", applied.effect.removed.join(", ")));
            }
            if ctx.dry_run {
                parts.push("(dry run, nothing written)".into());
            }
            if !parts.is_empty() {
                println!("{}: {}", id, parts.join("; "));
            }
            for w in &applied.effect.warnings {
                eprintln!("warning [{}] {}: {}", w.code, w.target, w.detail);
            }
        },
        json!({ "ok": true, "result": applied }),
    );
    // Warnings are reported on stderr and carried in --json; they do not make the op a
    // failure. `lint` owns the "ran fine, but the document has problems" signal (exit 4).
    Ok(0)
}

fn cmd_render(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let path = argv
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            Error::Invalid("usage: dpaint render <path> [--scale N] [--width N]".into())
        })?;
    let mut op_args = vec!["--path".to_string(), path];
    for name in [
        "--scale",
        "--width",
        "--height",
        "--background",
        "--quality",
    ] {
        if let Some(v) = flag(argv, name) {
            op_args.push(name.into());
            op_args.push(v.clone());
        }
    }
    if let Some(d) = &ctx.doc {
        op_args.push("--document".into());
        op_args.push(d.clone());
    }
    let mut full = vec!["render.image".to_string()];
    full.extend(op_args);
    cmd_op(ctx, &full)
}

fn cmd_inspect(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let mut full = vec![if argv.iter().any(|a| a == "--select") {
        "inspect.tree".to_string()
    } else {
        "inspect.digest".to_string()
    }];
    for name in ["--select", "--scale"] {
        if let Some(v) = flag(argv, name) {
            full.push(name.into());
            full.push(v.clone());
        }
    }
    if argv.iter().any(|a| a == "--fast") {
        full.push("--fast".into());
    }
    if let Some(d) = &ctx.doc {
        full.push("--document".into());
        full.push(d.clone());
    }
    // Inspection output is only useful as data.
    let ctx = Ctx {
        json: true,
        ..clone_ctx(ctx)
    };
    cmd_op(&ctx, &full)
}

fn clone_ctx(c: &Ctx) -> Ctx {
    Ctx {
        project_dir: c.project_dir.clone(),
        doc: c.doc.clone(),
        json: c.json,
        dry_run: c.dry_run,
        rest: c.rest.clone(),
    }
}

fn cmd_lint(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let ws = open(ctx)?;
    let opts = dpaint_inspect::DigestOptions {
        per_object: !argv.iter().any(|a| a == "--fast"),
        ..Default::default()
    };
    let assets = AssetStore::new(ws.root());
    let report = match &ctx.doc {
        Some(d) => {
            let id = ws.project.resolve_doc(Some(d))?;
            let findings = dpaint_inspect::lint::lint_document(&ws.project, &id, &assets, &opts)?;
            let errors = findings
                .iter()
                .filter(|f| f.severity == dpaint_inspect::Severity::Error)
                .count();
            let warnings = findings
                .iter()
                .filter(|f| f.severity == dpaint_inspect::Severity::Warn)
                .count();
            dpaint_inspect::Report {
                findings,
                errors,
                warnings,
            }
        }
        None => dpaint_inspect::lint::lint_project(&ws.project, &assets, &opts)?,
    };

    let threshold: dpaint_inspect::Severity = flag(argv, "--severity")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(dpaint_inspect::Severity::Error);

    let fail = report.findings.iter().any(|f| f.severity >= threshold);
    emit(
        ctx,
        || {
            for f in &report.findings {
                println!("{:?}\t{}\t{}\t{}", f.severity, f.rule, f.target, f.detail);
            }
            println!(
                "{} finding(s): {} error, {} warning",
                report.findings.len(),
                report.errors,
                report.warnings
            );
        },
        json!({ "ok": true, "report": report }),
    );
    // Exit 4 signals "ran fine, but the document has problems" so a loop can branch on it.
    Ok(if fail { 4 } else { 0 })
}

fn cmd_diff(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let files: Vec<&String> = argv.iter().filter(|a| !a.starts_with("--")).collect();
    if files.len() < 2 {
        return Err(Error::Invalid(
            "usage: dpaint diff <a.png> <b.png> [--heatmap out.png]".into(),
        ));
    }
    let load = |p: &str| -> Result<image::RgbaImage> {
        Ok(image::open(p)
            .map_err(|e| Error::AssetDecode(format!("{p}: {e}")))?
            .to_rgba8())
    };
    let (a, b) = (load(files[0])?, load(files[1])?);
    let d = dpaint_inspect::diff::compare(&a, &b)?;

    if let Some(out) = flag(argv, "--heatmap") {
        let hm = dpaint_inspect::diff::heatmap(&a, &b)?;
        hm.save(out)
            .map_err(|e| Error::Invalid(format!("could not write heatmap: {e}")))?;
    }

    let threshold: f64 = flag(argv, "--threshold")
        .and_then(|t| t.parse().ok())
        .unwrap_or(0.0);
    let fail = d.changed_fraction > threshold;
    emit(
        ctx,
        || {
            println!(
                "ssim {:.6}  meanDE {:.3}  maxDE {:.3}  changed {:.4}%",
                d.ssim,
                d.mean_delta_e,
                d.max_delta_e,
                d.changed_fraction * 100.0
            );
            if let Some(bb) = d.changed_bbox {
                println!("changed region: x{} y{} {}x{}", bb[0], bb[1], bb[2], bb[3]);
            }
        },
        json!({ "ok": true, "diff": d }),
    );
    Ok(if fail { 4 } else { 0 })
}

fn cmd_annotate(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let path = argv
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| Error::Invalid("usage: dpaint annotate <out.png>".into()))?;
    let ws = open(ctx)?;
    let assets = AssetStore::new(ws.root());
    let doc = ws.project.resolve_doc(ctx.doc.as_deref())?;
    let opts = dpaint_inspect::DigestOptions::default();

    let digest = dpaint_inspect::digest::digest(&ws.project, &doc, &assets, &opts)?;
    let base = dpaint_render::render_document(&ws.project, &doc, &assets, &opts.render)?;
    let (marked, legend) = dpaint_inspect::annotate::annotate(&base, &digest)?;
    let bytes = dpaint_render::encode::encode(
        &dpaint_render::encode::to_rgba(&marked),
        dpaint_render::ImageFormat::Png,
        100,
    )?;
    std::fs::write(&path, bytes)?;

    emit(
        ctx,
        || {
            println!("{path}");
            for l in &legend {
                println!(
                    "  {:>2}  {}  {}  [{:.0},{:.0} {:.0}x{:.0}]",
                    l.index, l.selector, l.type_name, l.bbox[0], l.bbox[1], l.bbox[2], l.bbox[3]
                );
            }
        },
        json!({ "ok": true, "path": path, "legend": legend }),
    );
    Ok(0)
}

fn cmd_undo_redo(ctx: &Ctx, which: &str) -> Result<i32> {
    let mut engine = Engine::new(registry(), open(ctx)?);
    let done = if which == "undo" {
        engine.undo()?
    } else {
        engine.redo()?
    };
    emit(
        ctx,
        || match &done {
            Some(op) => println!("{which}: {op}"),
            None => println!("nothing to {which}"),
        },
        json!({ "ok": true, "op": done }),
    );
    Ok(0)
}

fn cmd_history(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let mut ws = open(ctx)?;
    let limit: usize = flag(argv, "--limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20);
    let entries = ws.journal.load()?;
    let shown: Vec<_> = entries.iter().rev().take(limit).collect();
    emit(
        ctx,
        || {
            for e in shown.iter().rev() {
                println!(
                    "{:>4}  {}  {:?}{}  {}",
                    e.seq,
                    e.ts,
                    e.actor,
                    if e.undone { " (undone)" } else { "" },
                    e.op
                );
            }
        },
        json!({ "ok": true, "entries": shown.iter().map(|e| json!({
            "seq": e.seq, "ts": e.ts, "actor": e.actor, "op": e.op,
            "undone": e.undone, "args": e.args, "effect": e.effect,
        })).collect::<Vec<_>>() }),
    );
    Ok(0)
}

fn cmd_schema(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let reg = registry();
    let target = argv.iter().find(|a| !a.starts_with("--"));
    let value = match target {
        Some(id) => {
            let op = reg.get(id)?;
            json!({ "id": op.id(), "about": op.about(), "schema": op.schema() })
        }
        None if argv.iter().any(|a| a == "--all") => reg.catalog(),
        None => serde_json::to_value(schemars::schema_for!(Project))?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    let _ = ctx;
    Ok(0)
}

fn cmd_gc(ctx: &Ctx) -> Result<i32> {
    let ws = open(ctx)?;
    let assets = AssetStore::new(ws.root());
    let keep = ws.project.referenced_assets();
    let (removed, freed) = assets.gc(&keep)?;
    emit(
        ctx,
        || println!("removed {} blob(s), freed {} bytes", removed.len(), freed),
        json!({ "ok": true, "removed": removed.iter().map(|r| r.to_string()).collect::<Vec<_>>(), "freed": freed }),
    );
    Ok(0)
}

fn cmd_doctor(ctx: &Ctx) -> Result<i32> {
    let reg = registry();
    let providers = dpaint_ai::providers_status();
    let project = open(ctx).ok().map(|w| {
        json!({
            "root": w.root().display().to_string(),
            "documents": w.project.documents.len(),
            "active": w.project.active.as_str(),
        })
    });
    let report = json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "ops": reg.len(),
        "formats": { "raster": ["png", "jpg", "webp", "tiff"], "vector": ["svg"], "model": ["gltf", "glb"] },
        "providers": providers,
        "project": project,
    });
    let summary = match &report["project"] {
        serde_json::Value::Null => "  project: none found from this directory".to_string(),
        p => format!("  project: {} ({} documents)", p["root"], p["documents"]),
    };
    let provider_line = serde_json::to_string(&providers).unwrap_or_default();
    let op_count = reg.len();
    emit(
        ctx,
        || {
            println!("dpaint {}", env!("CARGO_PKG_VERSION"));
            println!("  ops registered: {op_count}");
            println!("  providers: {provider_line}");
            println!("{summary}");
        },
        report,
    );
    Ok(0)
}

fn cmd_serve(ctx: &Ctx, argv: &[String]) -> Result<i32> {
    let studio = match &ctx.project_dir {
        Some(p) => dpaint_studio::Studio::open(p)?,
        None => dpaint_studio::Studio::discover(".")?,
    };
    let config = dpaint_studio::ServerConfig {
        addr: flag(argv, "--addr")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1:4317".into()),
        ui_dir: flag(argv, "--ui-dir").map(PathBuf::from),
    };
    dpaint_studio::serve(studio, config)?;
    Ok(0)
}

fn cmd_mcp(ctx: &Ctx) -> Result<i32> {
    let reg = registry();
    let root = ctx.project_dir.clone();

    // The render and lint loop tools live here because they need the engine crates;
    // the MCP crate stays dependent on core alone.
    let render_root = root.clone();
    let lint_root = root.clone();
    let handlers = dpaint_mcp::Handlers::new()
        .with(
            "dpaint_render",
            Box::new(
                move |args: serde_json::Value| -> Result<serde_json::Value> {
                    let ws = match &render_root {
                        Some(p) => Workspace::open(p)?,
                        None => Workspace::discover(".")?,
                    };
                    let assets = AssetStore::new(ws.root());
                    let doc = ws
                        .project
                        .resolve_doc(args.get("document").and_then(|d| d.as_str()))?;
                    let scale = args.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0);
                    let opts = dpaint_inspect::DigestOptions {
                        per_object: !args.get("fast").and_then(|f| f.as_bool()).unwrap_or(false),
                        render: dpaint_render::RenderOptions {
                            scale,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let (digest, findings) =
                        dpaint_inspect::lint::digest_and_lint(&ws.project, &doc, &assets, &opts)?;
                    let path = args.get("path").and_then(|p| p.as_str());
                    let written = match path {
                        Some(p) => Some(dpaint_render::export_document(
                            &ws.project,
                            &doc,
                            &assets,
                            p,
                            &opts.render,
                            90,
                        )?),
                        None => None,
                    };
                    Ok(json!({ "digest": digest, "lint": findings, "written": written }))
                },
            ),
        )
        .with(
            "dpaint_lint",
            Box::new(
                move |args: serde_json::Value| -> Result<serde_json::Value> {
                    let ws = match &lint_root {
                        Some(p) => Workspace::open(p)?,
                        None => Workspace::discover(".")?,
                    };
                    let assets = AssetStore::new(ws.root());
                    let opts = dpaint_inspect::DigestOptions {
                        per_object: !args.get("fast").and_then(|f| f.as_bool()).unwrap_or(false),
                        ..Default::default()
                    };
                    let report = match args.get("document").and_then(|d| d.as_str()) {
                        Some(d) => {
                            let id = ws.project.resolve_doc(Some(d))?;
                            let findings = dpaint_inspect::lint::lint_document(
                                &ws.project,
                                &id,
                                &assets,
                                &opts,
                            )?;
                            let errors = findings
                                .iter()
                                .filter(|f| f.severity == dpaint_inspect::Severity::Error)
                                .count();
                            let warnings = findings
                                .iter()
                                .filter(|f| f.severity == dpaint_inspect::Severity::Warn)
                                .count();
                            dpaint_inspect::Report {
                                findings,
                                errors,
                                warnings,
                            }
                        }
                        None => dpaint_inspect::lint::lint_project(&ws.project, &assets, &opts)?,
                    };
                    Ok(serde_json::to_value(report)?)
                },
            ),
        );

    dpaint_mcp::serve_stdio(reg, root, handlers)?;
    Ok(0)
}

fn _unused(_: Actor) {}
