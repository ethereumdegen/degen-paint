//! The `media-apps` pack contribution degen-paint owns.
//!
//! starkbot-neo vendors these bytes (`starkbot-neo/plans/12-media-apps.md` §3), so they are
//! generated from [`crate::contract`] rather than written down twice: a shortcut that moves in
//! the menu bar moves here in the same commit, which is the only way hints stay honest.
//!
//! The format is `starkbot-neo/plans/06-packs.md` §4. Two of its rules shape everything below:
//! a routine step may only use neo's own gated tools, and no part of `desktop/` may carry a
//! selector, a landmark, an element path or a URL to click — the pack describes *workflows* and
//! the navigator works out the screen.

use crate::contract::{CONFIRM_LABEL_PREFIXES, SHORTCUTS};
use serde_json::{json, Map, Value};

/// The pack these files are vendored into.
pub const PACK: &str = "media-apps";

/// macOS bundle id and Wayland `app_id` both, which is what lets one hints file serve both
/// platforms (`docs/starkbot.md` §11: for a GTK/Tauri app the `app_id` *is* the identifier).
pub const APP_ID: &str = "dev.degenpaint.studio";

/// Where the grounding probes of `docs/starkbot.md` §5.1 live, and the env name Starkbot's
/// HTTP runner needs in order to be allowed to reach loopback at all.
pub const BASE_URL_ENV: &str = "DPAINT_BASE_URL";
const BASE_URL_DEFAULT: &str = "http://127.0.0.1:4317";

/// Every file of the contribution, keyed by its path inside the pack.
///
/// Markdown arrives as a string and JSON as a value, so a caller can serve the whole thing as
/// one object (`GET /api/v1/skill`) or write it to a directory (`dpaint skill --out`) without
/// re-parsing anything.
pub fn pack() -> Value {
    let mut files = Map::new();
    files.insert("vocabulary.md".into(), json!(VOCABULARY));
    files.insert("skills/degen-paint.md".into(), json!(SKILL));
    files.insert(format!("desktop/apps/{APP_ID}.json"), app_hints());
    for (name, routine) in routines() {
        files.insert(format!("desktop/routines/{name}.json"), routine);
    }
    files.insert("goals/degen-paint.json".into(), goals());
    files.insert("grounding.json".into(), grounding());
    json!({ "pack": PACK, "app": APP_ID, "files": Value::Object(files) })
}

/// `desktop/apps/<id>.json`: the technical facts the accessibility tree cannot state.
///
/// `ax_strategy` is `none` because both webviews publish their tree unasked — WebKitGTK and
/// WKWebView, unlike Electron, need no flag set before observing.
fn app_hints() -> Value {
    let mut shortcuts = Map::new();
    for s in SHORTCUTS {
        shortcuts.insert(hint_name(s.label), json!(neo_keys(s.keys)));
    }
    json!({
        "bundle_id": APP_ID,
        "app_id": APP_ID,
        "name": "degen-paint Studio",
        "ax_strategy": "none",
        "settle_ms": 300,
        // A render settles in a frame; an export writes a file, and the Status region only
        // names the path once the bytes are on disk.
        "settle_ms_after": { "export": 1500 },
        "shortcuts": Value::Object(shortcuts),
        "confirm_labels": CONFIRM_LABEL_PREFIXES
            .iter()
            .map(|l| json!(l.trim_end()))
            .collect::<Vec<_>>(),
        // The panes are `role=region` wrappers and the lists are capped at 60 rows by the UI
        // itself, so the snapshot only has to drop the furniture between them.
        "snapshot": {
            "drop_roles": ["AXSplitter", "AXScrollBar", "AXImage"],
            "collapse_roles": ["AXGroup"],
            "max_rows": 60,
            "max_depth": 18
        },
        "skills": ["degen-paint"]
    })
}

/// A shortcut's name as the navigator offers it: an action, lower case, no trailing ellipsis.
fn hint_name(label: &str) -> String {
    label.trim_end_matches('…').trim().to_lowercase()
}

/// Tauri accelerator spelling to the pack's: `CmdOrCtrl+Shift+Z` becomes `cmd+shift+z`.
///
/// neo carries one modifier per platform (`docs/starkbot.md` §11: `META` on macOS, `Ctrl` on
/// Linux), so the pack states the logical modifier and neo resolves it.
fn neo_keys(accelerator: &str) -> String {
    accelerator
        .split('+')
        .map(|part| match part {
            "CmdOrCtrl" | "CommandOrControl" | "Cmd" | "Command" | "Super" | "Meta" => {
                "cmd".to_owned()
            }
            "Ctrl" | "Control" => "ctrl".to_owned(),
            "Shift" => "shift".to_owned(),
            "Alt" | "Option" => "alt".to_owned(),
            other => other.to_lowercase(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The five `dp-*` routines of `plans/12-media-apps.md` §3.
///
/// Each one is a fixed step list that runs without Sol on the happy path. Where a dialog has to
/// be filled in, the step is a single `navigate` goal sentence rather than a click script: the
/// dialog is the app's, its layout is not the pack's business, and a sentence survives a
/// redesign that a step list would not.
fn routines() -> Vec<(&'static str, Value)> {
    vec![
        (
            "dp-open-project",
            json!({
                "name": "dp-open-project",
                "description": "Open or create a degen-paint project in degen-paint Studio and leave it as the open project",
                "examples": [
                    "open the acme-promo project in degen-paint",
                    "start a degen-paint project called acme-promo at 1080x1350",
                    "in degen-paint, open ~/art/poster"
                ],
                "params": { "type": "object", "required": ["project"], "properties": {
                    "project": { "type": "string", "ask": "Which degen-paint project should I open?" },
                    "size": { "type": ["string", "null"] }
                }},
                "steps": [
                    { "tool": "launch_app", "args": { "name": "degen-paint Studio" } },
                    { "tool": "wait_for", "args": { "text": "degen-paint", "max_secs": 25 } },
                    { "tool": "navigate", "args": { "goal": "In degen-paint Studio, open the project {project} through the File menu. If no such project exists yet, create it there instead at size {size}. Done when the toolbar names the project {project} and the Documents list has a row." } }
                ],
                "verify": "degen-paint Studio has the project {project} open and the Welcome screen is gone.",
                "on_fail": "handoff",
                "max_secs": 150
            }),
        ),
        (
            "dp-import-file",
            json!({
                "name": "dp-import-file",
                "description": "Import an image, SVG or glTF file into the open degen-paint project as a new layer or a new document",
                "examples": [
                    "bring the microphone take into degen-paint as a layer",
                    "import ~/Movies/Degen Media Studio/acme/t0003.png into degen-paint",
                    "add that SVG to the poster as a new document"
                ],
                "params": { "type": "object", "required": ["path"], "properties": {
                    "path": { "type": "string", "ask": "Which file should I import?" },
                    "destination": { "type": "string", "enum": ["layer", "document"] }
                }},
                "steps": [
                    { "tool": "focus_app", "args": { "name": "degen-paint Studio" } },
                    { "tool": "select_menu", "args": { "path": ["File", "Import…"] } },
                    { "tool": "wait_for", "args": { "text": "Import", "max_secs": 10 } },
                    { "tool": "type_text", "args": { "text": "{path}" } },
                    { "tool": "navigate", "args": { "goal": "In the degen-paint Import dialog, make sure the file path field holds {path}, choose to import it as a new {destination}, and press the import button. Done when the Status region reports the import and a new row is in the tree." } },
                    { "tool": "extract", "args": { "what": "the Status region line naming what was imported" } }
                ],
                "verify": "The imported file has a row in the Layers or Documents list and the Status region names it.",
                "on_fail": "handoff",
                "max_secs": 180
            }),
        ),
        (
            "dp-export-png",
            json!({
                "name": "dp-export-png",
                "description": "Export a document of the open degen-paint project to a PNG file at a given scale, overwriting an existing file when asked to",
                "examples": [
                    "export the poster as a PNG to ~/Pictures/acme.png",
                    "in degen-paint, write out logo at 2x",
                    "save the campaign document as a png"
                ],
                "params": { "type": "object", "required": ["path"], "properties": {
                    "path": { "type": "string", "ask": "Where should the PNG go?" },
                    "scale": { "type": ["number", "null"] },
                    "document": { "type": ["string", "null"] }
                }},
                "steps": [
                    { "tool": "focus_app", "args": { "name": "degen-paint Studio" } },
                    { "tool": "select_menu", "args": { "path": ["File", "Export…"] } },
                    { "tool": "wait_for", "args": { "text": "Export", "max_secs": 10 } },
                    { "tool": "navigate", "args": { "goal": "In the degen-paint Export dialog, pick the document {document} when one is named, set the format to PNG and the scale to {scale}, put {path} in the path field, then press the export button — its label names an overwrite when the file is already there, which is expected. Done when the Status region reports the written path and pixel size." } },
                    { "tool": "wait_for", "args": { "text": "{path}", "max_secs": 120 } },
                    { "tool": "extract", "args": { "what": "the written path and pixel size from the Status region" } }
                ],
                "verify": "The Status region names a written PNG at {path} with its pixel size.",
                "on_fail": "handoff",
                "max_secs": 240
            }),
        ),
        (
            "dp-send-to-editor",
            json!({
                "name": "dp-send-to-editor",
                "description": "Send documents of the open degen-paint project to the editor hand-off folder as PNG, SVG and GLB with their sidecars",
                "examples": [
                    "send the poster to the editor",
                    "hand the acme-promo documents off to Diffusion Studio",
                    "in degen-paint, send title and badge to the editor folder"
                ],
                "params": { "type": "object", "required": [], "properties": {
                    "documents": { "type": ["string", "null"], "ask": "Which documents should I send?" }
                }},
                "steps": [
                    { "tool": "focus_app", "args": { "name": "degen-paint Studio" } },
                    { "tool": "select_menu", "args": { "path": ["File", "Send to Editor…"] } },
                    { "tool": "wait_for", "args": { "text": "Send", "max_secs": 10 } },
                    { "tool": "navigate", "args": { "goal": "In the degen-paint Send to Editor dialog, select the documents {documents}, or the active document when none are named, and press the send button; confirm an overwrite if it asks for one. Done when the Status region lists the written files." } },
                    { "tool": "extract", "args": { "what": "the written file paths listed by the Status region" } }
                ],
                "verify": "The Status region lists one written file per sent document, each beside its sidecar.",
                "on_fail": "handoff",
                "max_secs": 240
            }),
        ),
        (
            "dp-fix-lint",
            json!({
                "name": "dp-fix-lint",
                "description": "Read the Lint pane of the open degen-paint project and fix what it names, one finding at a time, until it is clean",
                "examples": [
                    "make sure lint is clean in degen-paint",
                    "fix the low-contrast finding on the poster",
                    "degen-paint says the title overflows — sort it out"
                ],
                "params": { "type": "object", "required": [], "properties": {
                    "rule": { "type": ["string", "null"] },
                    "document": { "type": ["string", "null"] }
                }},
                "steps": [
                    { "tool": "focus_app", "args": { "name": "degen-paint Studio" } },
                    { "tool": "navigate", "args": { "goal": "In degen-paint Studio, read the Lint pane's findings for document {document}, or for every document when none is named. Fix the finding for rule {rule}, or every finding when no rule is named: each finding names the object it is about and the fix it wants, so select that object and run the op the finding suggests from the command palette. Done when the Lint pane reports no findings." } },
                    { "tool": "extract", "args": { "what": "the findings still listed in the Lint pane" } }
                ],
                "verify": "The Lint pane reports no findings for {document}.",
                "on_fail": "handoff",
                "max_secs": 300
            }),
        ),
    ]
}

/// `goals/`: how a brief becomes `navigate` sentences.
///
/// Sol plans; these are the shapes that have been measured to work, so it does not have to
/// rediscover that one goal per finished artifact beats one goal per click.
fn goals() -> Value {
    json!({
        "app": "degen-paint Studio",
        "rules": [
            "One goal per finished artifact, not one per control: the navigator reads the Status region after every step and knows when an op landed.",
            "Name the document, never the canvas position: every object has a selector-free name in the tree and a bounding box in the Status region.",
            "Ask for lint before export. The Lint pane names the fix, and an export of a document with findings is work thrown away.",
            "Paid ops state their price in the button; leave the confirm card to the user and say in the goal what it is for."
        ],
        "templates": [
            {
                "id": "poster-from-brief",
                "brief": "a poster or social image from a written brief",
                "goals": [
                    "In degen-paint Studio, create the project {project} at {size} and make it the open project.",
                    "In degen-paint, import {source} into {document} as a new layer and place it to fill the canvas.",
                    "In degen-paint, add a text layer named {name} reading {text} in {font} across the {position} of {document}.",
                    "In degen-paint, make lint clean for {document}, fixing each finding the way the Lint pane names.",
                    "In degen-paint, export {document} as a PNG to {path}."
                ]
            },
            {
                "id": "dms-hand-off",
                "brief": "a take from Degen Media Studio finished in degen-paint",
                "goals": [
                    "In degen-paint, import {take} into {document} as a new layer; its sidecar carries the prompt and model, and degen-paint records them as provenance.",
                    "In degen-paint, crop and arrange {document} for {size}, then export it and send it to the editor."
                ]
            },
            {
                "id": "vector-mark",
                "brief": "a logo or mark as vector artwork",
                "goals": [
                    "In degen-paint, add a vector document named {name} at {size} to the open project.",
                    "In degen-paint, draw {description} in {name} using the command palette's vector ops, then export it as an SVG to {path}."
                ]
            },
            {
                "id": "model-turntable",
                "brief": "a glTF model checked and shown off",
                "goals": [
                    "In degen-paint, import {model} as a new document and read its lint findings for geometry and texture problems.",
                    "In degen-paint, export {document} as a turntable of {frames} PNG frames to {path}."
                ]
            }
        ]
    })
}

/// `grounding.json`: the read-only side channel of `docs/starkbot.md` §5.1.
///
/// A step is verified by asking degen-paint, not by looking at it (P10). The endpoints come
/// from [`crate::contract::PROBES`], the same table the Studio serves them from; the checks are
/// the questions worth asking of a step that has just run.
fn grounding() -> Value {
    let probes: Vec<Value> = crate::contract::PROBES
        .iter()
        .map(|(id, path, about)| json!({ "id": id, "path": path, "about": about }))
        .collect();
    json!({
        "base": { "env": BASE_URL_ENV, "default": BASE_URL_DEFAULT },
        "probes": probes,
        "checks": [
            {
                "id": "project-open",
                "question": "Is the expected project open?",
                "probe": "status",
                "expect": "project is not null and names the expected directory"
            },
            {
                "id": "revision-advanced",
                "question": "Did the last step actually change the document?",
                "probe": "status",
                "expect": "revision is higher than it was before the step"
            },
            {
                "id": "idle",
                "question": "Has the render or export finished?",
                "probe": "status",
                "expect": "busy is empty"
            },
            {
                "id": "lint-clean",
                "question": "Is lint clean for this document?",
                "probe": "lint",
                "expect": "no finding of severity error"
            },
            {
                "id": "layer-count",
                "question": "Does the document have the layers it should?",
                "probe": "digest",
                "expect": "tree holds the expected number of entries, and the named layer is one of them"
            },
            {
                "id": "text-layer-present",
                "question": "Is the text layer there, with the font it asked for?",
                "probe": "digest",
                "expect": "a tree entry of type text whose text matches and whose fontFallback is null"
            },
            {
                "id": "step-landed",
                "question": "Which op did the app record last?",
                "probe": "history",
                "expect": "the newest entry names the op the step ran"
            },
            {
                "id": "selector-resolves",
                "question": "Does the object a finding names still exist?",
                "probe": "select",
                "expect": "ids is not empty"
            },
            {
                "id": "job-finished",
                "question": "Has the job finished, and did it succeed?",
                "probe": "job",
                "expect": "state is done"
            },
            {
                "id": "export-exists",
                "question": "Was the file written where the export said?",
                "probe": "history",
                "expect": "the newest entry is the export, and it names the expected path"
            }
        ]
    })
}

const VOCABULARY: &str = r#"# degen-paint vocabulary

What the words in a degen-paint brief mean inside the app. Every one of them names something the
UI states in text, so a step can be verified without looking at the canvas.

| Term | Meaning |
|---|---|
| project | A directory holding documents, assets, fonts and one journal. The Studio has one project open at a time; **Close Project** returns to the Welcome screen. |
| document | One canvas: **raster** (pixels), **vector** (paths) or **model** (glTF geometry). Documents are rows in the Documents list; selecting a row makes it the active document. |
| active document | The document every op applies to when none is named. The Documents row says `active`. |
| layer | A stacked element of a raster document: pixels, text, fill, gradient, or a *linked* layer that draws another document. Rows in the Layers list. |
| group | A layer or object that contains others. Its row is expandable. |
| mask | A layer that hides part of the layer it is attached to. A fully masked layer lints as invisible, not as missing. |
| selection | The object the Selection pane names, and the target of every per-object button (`Hide layer title`, `Delete object mark`). |
| path | A vector outline: `d` in SVG terms. Not a file path, which the dialogs call a *file path*. |
| object | An element of a vector document: path, text, group, image. |
| node | An element of a model document: mesh, material, light, camera. |
| boolean | Combining two vector paths by union, subtract, intersect or exclude. |
| extrude | Turning a vector path into 3D geometry in a model document. |
| op | One named, validated, journalled edit — `raster.filter.gaussian-blur`, `ai.image.generate`. Everything the app can do is an op, run from the command palette. |
| command palette | The `Run op` combobox. Typing filters the ops; choosing one opens its form in the Inspector. |
| dry run | Validate an op and report what it would do, writing nothing. Paid ops report their price. |
| quote | The estimated price of one paid op call, shown in the submit button and in the form's budget line. |
| digest | What a render measured: every object's bounding box, colours, coverage and resolved font. The app's answer to "what does it look like". |
| lint | The findings pane: off-canvas content, text overflow, low contrast, tiny type, invisible layers, font fallbacks, broken geometry. Each finding names the object and the fix. |
| selector | How an object is named in a form or an error: `#title` by id, `title` by name. The Selection pane and `select` both resolve them. |
| revision | The project's edit counter, shown as a badge. It advances on every op that changes something, which is how a step proves it landed. |
| journal | The shared history of ops, human and agent alike. One undo stack: either can undo the other. |
| provenance | Where a generated or imported asset came from: provider, model, prompt, seed, and the sidecar of an imported take. |
| sidecar | A `<name>.json` beside an imported or exported file, carrying prompt, model and lineage. Degen Media Studio writes them; degen-paint reads and writes them. |
| send to editor | Exporting documents to the hand-off folder with their sidecars, for Diffusion Studio or Powermove to import. |
| export preview | One click: render the active document and its annotated twin to a fixed path, and say where. The only thing there is to look at. |
| job | An op that takes longer than a moment. While one runs the Status region is busy, a progress bar names it, and `Cancel job` stops it without a journal entry. |
"#;

const SKILL: &str = r#"---
description: Operating degen-paint Studio — projects, documents, ops, lint, export and the hand-off to an editor.
version: 1.0.0
---

# degen-paint

An image and 3D asset studio built so that something which cannot see its own work can still do
the work. Every capability is an **op**: named, schema-validated, journalled. The canvas is
decoration — what the app knows, it says in text.

## Three kinds of document

- **raster** — pixels, at a size and a DPI. Content is **layers**: pixel, text, fill, gradient,
  or *linked* (another document drawn inside this one).
- **vector** — paths, text and groups, exported as SVG or rasterised at any scale.
- **model** — glTF geometry: meshes, materials, lights, cameras. Exported as glTF/GLB or as a
  turntable of PNG frames.

One project holds any mix of them, and a raster document can draw a vector one through a linked
layer. That is how a poster gets a logo that stays sharp.

## How work happens

Everything runs through the **command palette** (`Run op`): type a few letters, pick the op, fill
its form in the Inspector, press the submit button — which is named with the op, and for a paid op
with its price. The result lands in the **Status** region as one line: what applied, which
document changed, the new revision. That line is the proof a step worked.

Long work becomes a **job**: the Status region goes busy, a progress bar names it, and
`Cancel job` stops it. A cancelled job writes nothing.

## Reading the work back

- **Selection** names the current object: kind, document, bounding box.
- **Tree** lists layers, objects or nodes, one row each, named with their state
  (`sky · pixel · hidden · locked`).
- **Lint** is the pane that matters. It finds what a blind operator gets wrong: content off the
  canvas or inside the bleed, text that overflowed its box, contrast below WCAG AA against the
  *actually rendered* backdrop, type too small at the output DPI, layers that are invisible
  because a mask ate them, fonts that silently fell back, glTF geometry that will break in a
  viewer. Every finding names the object and the fix. **Fix lint before exporting.**
- **History** is the shared journal. A human's edit and an agent's op are the same kind of entry
  and undo the same way.

## Files in and out

- **Import** takes PNG, JPG, WebP, TIFF, SVG, glTF and GLB, as a new layer or a new document. A
  `<name>.json` sidecar beside the file (Degen Media Studio's hand-off format) is read, and its
  prompt, model and lineage become provenance on what gets created.
- **Export** writes PNG, JPG, WebP, TIFF, SVG, glTF, GLB, or a turntable frame sequence. An
  existing file makes the button say `Overwrite <name>`.
- **Send to Editor** exports the chosen documents with their sidecars to the hand-off folder
  Diffusion Studio and Powermove import from.
- **Export Preview** is one click with no dialog: the active document and its annotated twin, at
  a fixed path, named in the Status region. It is the only thing worth looking at.

## Paid ops

`ai.*` ops reach a provider and cost money. The app owns the keys (Settings › Providers, paste
only, never read back) and the budget: every form shows the estimate and what the project has
spent against its ceiling, and an op that would break the ceiling is refused before anything is
sent. `dpaint quote <op>` prints the same numbers.

## Two operators, one document

A coding agent may be editing the same project over the CLI or MCP while the Studio is open. The
Studio polls, the journal serialises, and the Status region says `updated by agent`. A change
appearing that nobody in the UI asked for is normal, not a stale observation.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// 06-packs §4 is a validator on the other side of a vendoring step: a routine that breaks
    /// its rules is not noticed here, it is noticed when `neo pack install` refuses the whole
    /// `media-apps` pack. The closed tool list and the 12-step cap are the two that a plausible
    /// edit to `routines()` gets wrong.
    #[test]
    fn routines_obey_the_pack_format() {
        const TOOLS: &[&str] = &[
            "navigate",
            "open_url",
            "focus_app",
            "launch_app",
            "key",
            "type_text",
            "select_menu",
            "wait_for",
            "extract",
        ];
        for (name, r) in routines() {
            assert_eq!(r["name"], name, "{name}: name must match its file stem");
            for key in ["description", "verify"] {
                assert!(
                    r[key].as_str().is_some_and(|s| !s.is_empty()),
                    "{name}: {key} is required"
                );
            }
            let examples = r["examples"].as_array().expect("examples");
            assert!(
                (1..=8).contains(&examples.len()),
                "{name}: 1..=8 examples, got {}",
                examples.len()
            );
            let steps = r["steps"].as_array().expect("steps");
            assert!(
                (1..=12).contains(&steps.len()),
                "{name}: 1..=12 steps, got {}",
                steps.len()
            );
            for step in steps {
                let tool = step["tool"].as_str().expect("step tool");
                assert!(TOOLS.contains(&tool), "{name}: tool '{tool}' is not gated");
                assert!(step["args"].is_object(), "{name}: {tool} needs args");
            }
            assert!(
                matches!(r["on_fail"].as_str(), Some("handoff" | "fail")),
                "{name}: on_fail is handoff or fail"
            );
            let secs = r["max_secs"].as_u64().expect("max_secs");
            assert!((1..=300).contains(&secs), "{name}: max_secs {secs} > 300");
            let props = r["params"]["properties"].as_object().expect("properties");
            for (field, spec) in props {
                assert!(
                    spec.get("type").is_some() || spec.get("enum").is_some(),
                    "{name}: param '{field}' is untyped, which makes a provider refuse the schema"
                );
            }
        }
    }

    /// P9: no part of `desktop/` may carry page or screen knowledge. The hints schema enforces
    /// it by key name, so a helpful-looking `"selector"` or `"url"` fails the whole pack.
    #[test]
    fn desktop_files_carry_no_screen_knowledge() {
        fn walk(path: &str, v: &Value) {
            match v {
                Value::Object(m) => {
                    for (k, child) in m {
                        let lower = k.to_lowercase();
                        for banned in ["landmark", "selector", "xpath", "css", "url", "origin"] {
                            assert!(
                                !lower.contains(banned),
                                "{path}: key '{k}' matches '{banned}'"
                            );
                        }
                        walk(path, child);
                    }
                }
                Value::Array(a) => a.iter().for_each(|child| walk(path, child)),
                _ => {}
            }
        }
        let files = pack();
        for (path, v) in files["files"].as_object().expect("files") {
            if path.starts_with("desktop/") {
                walk(path, v);
            }
        }
    }

    #[test]
    fn accelerators_become_pack_keys() {
        assert_eq!(neo_keys("CmdOrCtrl+Shift+Z"), "cmd+shift+z");
        assert_eq!(neo_keys("Alt+F4"), "alt+f4");
        assert_eq!(neo_keys("?"), "?");
        assert_eq!(hint_name("New Project…"), "new project");
    }
}
