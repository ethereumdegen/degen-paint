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
| P9 GPU viewport | done | `dpaint-gpu` + `dpaint-view` + WebGPU in the browser; 60 fps at 2560×1600, CPU/GPU parity measured |
| P8 Docs and release | done | WASM engine in the browser, CI on macOS + Linux with fmt/clippy/wasm gates, tagged release workflow, install docs |

Total: **212 ops, 533 tests, 0 failures** (`cargo test --workspace`), with `cargo fmt --check`
and `cargo clippy -D warnings` both clean and blocking in CI.

The plan's one deviation is now settled rather than outstanding. `wgpu` was replaced by a CPU
rasterizer for headless previews, because determinism beats throughput for anything an agent
measures — and `wgpu` has since landed where it always belonged, driving the interactive
viewport in `dpaint-view` and in the browser build. Both renderers ship, the split is a
documented policy, and a parity test keeps them honest. See `docs/gpu-viewport.md`.

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

## v2 — Driven by Starkbot Neo

degen-paint becomes the fourth media app starkbot-neo operates, the way it operates Diffusion
Studio: through the Studio's UI over the accessibility tree, with a read-only grounding API and
file hand-off. Full contract, navigator constraints, and S8d: [`docs/starkbot.md`](./starkbot.md).

| Phase | State | Evidence |
|---|---|---|
| P10 Accessible Studio | done | the navigator-rule audit (`crates/dpaint-studio/tests/a11y/`, rules vendored from `snapshot.js` @ ae5f815c) reports **0 errors, 0 warnings**, normally and with `?nohints`; 22 candidates on an empty project, 42 with a populated tree, 44 with the palette open, largest group 11 — against a 120 budget and the navigator's hard cap of 250. Baseline before the rewrite: 8 candidates and three hard failures. Driven for real in Chromium: the palette is a `combobox` whose `aria-controls` listbox produces 6 options in 0 ms, the generated form carries `aria-required`/`aria-describedby` and a submit named `Run raster.layer.add`, and the status region announces `applied raster.layer.add · changed doc_… · created lyr_… · rev 2` |
| P11 Projects and files | done | `project.new/open/close/recent` and `io.import/export/sendToEditor/exportPreview` on dispatch; `io.import` is one op, so one Import is one journal entry and one undo; a DMS `<take>.json` sidecar lands as provenance (`Provenance.parents` is new and `skip_serializing_if`, so existing `project.json` round-trips byte-identically); export against an existing path is `Error::Exists` with a message naming the flag that unblocks it |
| P12 Grounding API | done | `GET /api/v1/{status,overview,history,select,skill}`, `/doc/:id/{digest,lint}`, `/jobs/:id`, `/annotate.png`; measured live: `Origin: http://evil.example` → **403** and the op does not run, no `Origin` → 200, own origin → 200. `dpaint_overview` now has one implementation (`dpaint_core::overview`) that MCP and the Studio both call |
| P13 Jobs, quotes, keys | done | `job.start/status/cancel` on a worker thread, `state.busy`, a cancelled job leaves the journal untouched; `dpaint quote ai.image.generate` and the Studio's `quote` return the same `estimateUsd` 0.025 from one `AiConfig::cost_of`; `providers.set` writes through `dpaint_ai::keys::store` (keychain, else a 0600 config file) and no response ever carries key material |
| P14 Pack and smoke test | partial | `dpaint skill --out` emits all ten files; every routine validates against [`06-packs`](https://github.com/ethereumdegen/starkbot-neo/blob/main/plans/06-packs.md) §4 — required fields, ≤ 12 steps, the closed tool list, every param typed, no selector-shaped key. S8d itself waits on starkbot-neo L3 |

The desktop shell runs on this Wayland session: `hyprctl clients` reports
`class: dev.degenpaint.studio`, `xwayland: false`, which is the `app_id` the pack hints and
`AppSel::BundleId` are keyed on, and the window publishes an AT-SPI tree — so the same DOM
contract reaches the native path.

Still open: the **macOS** AX spike (§6 of `docs/starkbot.md`) cannot run here, and **S8d**
needs starkbot-neo's Linux port. L0–L2 of that port are done and green
([`docs/starkbot.md` §11](./starkbot.md#11-starkbot-neo-on-linux)): the workspace builds and
tests on Linux, `chrome_path()` finds `/usr/bin/chromium`, and secrets live in the Secret
Service. L3 — the AT-SPI `neo-ax` backend with Hyprland window management — is what S8d-native
waits on; S8d-web needs nothing further.

---

## Explicitly out of scope for v1

Named so they don't creep in: video and animation timelines beyond glTF TRS tracks; skinned
meshes and morph targets; CMYK and print separations; layer styles beyond the listed effects;
plugin scripting; multi-user realtime collaboration; a node-based compositor graph.
