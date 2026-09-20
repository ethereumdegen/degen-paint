# Roadmap

Rule for every phase: it ends with a **rendered artifact and a passing check**, not a claim.
Phases are sequential because each one's proof depends on the previous one's engine.

## Where it stands

| Phase | State | Evidence |
|---|---|---|
| P0 Foundation | done | 61 core tests; apply/undo/redo is byte-identical at every step |
| P1 Raster | done | 84 ops, 106 tests |
| P2 Vector | done | 63 ops, 85 tests |
| P3 Model | done | 37 ops, 49 tests; GLB re-parses and validates |
| P4 Bridges | done | `examples/campaign.sh`: one path to SVG + GLB + PNG |
| P5 Agent surface | done | digest, lint, annotate, diff; 217 MCP tools over stdio |
| P6 AI providers | done | 12 ops, 52 tests against recorded transports |
| P7 Studio | done | Tauri app + browser UI over one `Studio::dispatch`; agent edits surface live in the open page |
| P8 Docs and release | done | WASM engine in the browser, CI on macOS + Linux with fmt/clippy/wasm gates, tagged release workflow, install docs |

Total: **212 ops, 457 tests, 0 failures** (`cargo test --workspace`), with `cargo fmt --check`
and `cargo clippy -D warnings` both clean and blocking in CI.

One deliberate deviation from the original plan, documented where it matters: `wgpu` was
replaced by a CPU rasterizer for headless model previews, because determinism beats throughput
for anything an agent measures — see `docs/architecture.md`. `wgpu` remains the right choice
for an interactive viewport, which is the one thing the Studio does not yet have; today the
viewport displays engine renders rather than driving a GPU surface.

---

## P0 — Foundation

Workspace, document model, and the machinery every later phase derives from.

- cargo workspace, `rustc 1.96`, `wasm32-unknown-unknown` target added, CI on macOS + Linux
- `dpaint-core`: `Project` / `Document` / ids / selectors, serde + `schemars`
- geometry (`kurbo`), color (`palette`, linear f32, ΔE2000), units and DPI
- content-addressed asset store (`blake3`), atomic project writes, `.lock`
- op registry, `OpEffect`, JSON-patch journal, undo/redo, replay
- `dpaint` skeleton: `new`, `doc`, `op --list`, `schema`, `inspect --json`, `undo`, `redo`, `doctor`
- golden-test harness and fixture layout

**Acceptance** — create a project, apply 20 ops, undo all 20, redo all 20, and get a
byte-identical `project.json` at every step; `dpaint schema` emits valid JSON Schema for every
registered op; replaying `history.jsonl` onto an empty project reproduces the same project.

---

## P1 — Raster

- linear-light f32 tiled compositor, `rayon`-parallel, groups and nested groups
- 25+ blend modes, separable and non-separable, per the published formulas
- layer masks, clipping masks, adjustment layers, layer effects
- the full `adjust.*` and `filter.*` catalogs
- selections including magic wand and color range, with feather/grow/shrink, scoping every op
- text layers: shaping, wrapping, tracking, leading, fit-to-box
- encoders: PNG (8/16-bit, palette, `oxipng`), JPG, WebP, AVIF, TIFF, with ICC and DPI metadata

**Acceptance** — a photo-composite poster rendered at 300 DPI with layers, masks, adjustments,
effects, and text; a blend-mode golden grid matching reference values within tolerance; a
selection-scoped filter that provably modifies only the selected region.

---

## P2 — Vector

- path model on `kurbo`, full node editing, SVG-compatible `d` in and out
- booleans via `i_overlay` (union / subtract / intersect / exclude / divide)
- offset path, outline stroke, simplify, round corners
- fills, strokes, dashes, markers, linear and radial gradients, fill rules
- text: `rustybuzz` shaping, `fontdb` fallback, text-on-path, flow-in-shape, to-outlines
- clip paths, masks, groups, symbols
- SVG import and export (tidy, stable ids), PDF export, rasterize at any scale/DPI
- local offline raster→vector trace

**Acceptance** — a logo built entirely from ops using boolean subtraction and text-on-path,
exported to SVG, re-imported, and rendered byte-identically; the same file rasterized at 1× and 4×
with matching geometry; round-trip through an external viewer without visual change.

---

## P3 — Model

- glTF scene graph: nodes, hierarchy, TRS, cameras, `KHR_lights_punctual`
- primitives, and extrude / revolve / loft from vector paths with bevels and caps
- normals, `mikktspace` tangents, UV generation, weld, merge, decimate
- PBR metallic-roughness materials, textures sourced from raster documents
- TRS keyframe animation
- `.gltf` + bin and `.glb` export, `meshopt` compression, validation report
- `wgpu` PBR renderer: offscreen turntables for agents, on-surface viewport for the GUI

**Acceptance** — a badge extruded from the P2 logo, exported as a GLB that passes glTF validation
with zero errors, loads in an external viewer, and produces an 8-frame turntable; `model.validate`
catches a deliberately non-manifold mesh.

---

## P4 — Cross-mode bridges

- linked layers: a vector or model document rendered live inside a raster document
- raster documents as PBR texture sources
- turntable renders composited back as raster layers
- cycle detection at op time, render caching keyed by document state

**Acceptance** — one project, one source path, producing a PNG poster, an SVG logo, and a GLB
badge that all stay consistent when the source path is edited and everything is re-rendered.

---

## P5 — Agent surface

- render digest, `inspect.*` queries, annotated previews
- the full lint rule set
- SSIM / ΔE2000 diff with heatmaps
- `dpaint mcp`: one tool per op plus `dpaint_overview`, `dpaint_render`, `dpaint_lint`,
  `dpaint_apply`, `dpaint_history`
- determinism audit: embedded fonts, explicit seeds, no clock or locale in the render path

**Acceptance** — a coding agent, given only the MCP tool list and no human hints, builds the P4
poster end to end; lint catches every one of a set of deliberately seeded defects (off-canvas
badge, low-contrast title, fallback font, non-manifold mesh); the same document renders
byte-identically across 10 runs and on both supported platforms.

---

## P6 — AI providers

- `dpaint-ai`: provider trait, key resolution (env / keychain / config), retry and backoff
- fal.ai: generate, edit, inpaint (selection as mask), outpaint, upscale, remove background, PBR
  texture sets
- QuiverAI: `svgs/generations` and `svgs/vectorizations`, SSE streaming, SVG → editable objects
- provenance records, request cache, budget ceiling, cost accounting, `--dry-run`

**Acceptance** — with keys set, generate a background with fal and a logo with Quiver, then prove
the Quiver output is *editable* by boolean-subtracting a hole in it and extruding it to 3D; with
keys unset, every non-`ai.*` op still works and `ai.*` fails cleanly with exit code 5; a repeated
identical request is served from cache with zero cost.

---

## P7 — Studio

- Tauri v2 shell; Vite + TypeScript frontend; the identical UI against the WASM build in a browser
- viewport with pan/zoom/rulers/guides, layer and object panels, schema-driven inspector
- tool palette that emits ops (never mutates state directly)
- live journal view showing agent and human edits interleaved
- file watching so an agent's writes appear immediately

**Acceptance** — met, and verified by driving the real UI in a browser: a GUI edit is journaled
as `human` and a CLI edit as `agent`; an agent's write appears in the open page within ~2s with
an "updated by agent" indicator; the human's undo button reverts the agent's edits; a bad
selector surfaces its candidate list in the console panel; and a seeded low-contrast defect
appears in the lint panel and selects its layer when clicked.

The Tauri window itself could not be screenshotted — this machine has no display access
(`screencapture` fails) — so the desktop shell is verified by its process staying alive with a
loaded `tauri://localhost` webview and a WebKit content process, plus 13 tests over its command
layer. The pixels are verified through the browser path, which loads byte-identical UI files.

---

## P8 — Documentation and release

- op reference generated from schemas (`dpaint schema --markdown`)
- worked examples: poster, logo, badge, and the cross-mode pipeline, all reproducible from ops
- golden suite green in CI on macOS and Linux
- installers: `cargo install dpaint-cli`, Homebrew tap, signed Tauri builds

---

## Explicitly out of scope for v1

Named so they don't creep in: video and animation timelines beyond glTF TRS tracks; skinned
meshes and morph targets; CMYK and print separations; layer styles beyond the listed effects;
plugin scripting; multi-user realtime collaboration; a node-based compositor graph.
