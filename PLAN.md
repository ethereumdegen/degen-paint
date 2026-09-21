# degen-paint — master plan

An agent-native image and 3D asset studio. CLI `dpaint` ·
Rust engine · JSON documents · Tauri v2 shell · browser-first UI.

> This is the design document. Detail lives in [`docs/`](./docs); this file holds the thesis,
> the locked decisions, and the reasoning behind them.

| | |
|---|---|
| [`docs/architecture.md`](./docs/architecture.md) | crate graph, data flow, rendering pipeline, color, assets, WASM |
| [`docs/document-format.md`](./docs/document-format.md) | the `.dpaint` format, all three document kinds, the journal |
| [`docs/op-registry.md`](./docs/op-registry.md) | the op model and the complete v1 op catalog |
| [`docs/agent-interface.md`](./docs/agent-interface.md) | CLI, MCP, digest, lint, annotate, diff, determinism |
| [`docs/ai-providers.md`](./docs/ai-providers.md) | optional fal.ai and QuiverAI integration |
| [`docs/selectors.md`](./docs/selectors.md) | selector grammar and resolution rules |
| [`docs/errors.md`](./docs/errors.md) | structured errors, exit codes, transactional guarantees |
| [`docs/testing.md`](./docs/testing.md) | golden renders, determinism, performance budgets |
| [`docs/roadmap.md`](./docs/roadmap.md) | P0–P8 with acceptance criteria |
| [`docs/starkbot.md`](./docs/starkbot.md) | v2: the operator-app contract for being driven by starkbot-neo — accessible Studio, files hand-off, grounding API, jobs/quotes/keys, `dpaint skill`, P10–P14 |

---

## 1. Thesis

[Diffusion Studio](https://diffusion.studio) made video editing agent-native by making the edit
*be a document*: something an agent can read, diff, re-run, and render headlessly, exposed over a
CLI and an MCP server so a coding agent does real work with no GUI in the loop.

There is no equivalent for **still images and 3D assets**. The gap is not "an image library" —
those exist. The gap is:

1. **No document model.** Libraries apply operations to buffers. There is no layer stack, no
   history, no non-destructive edit, no project an agent can reason about across turns.
2. **No feedback channel.** An agent calling an image API cannot tell whether the text overflowed,
   whether the font fell back, whether the logo landed in the bleed, or whether the contrast is
   unreadable. It is operating blind and has no instrument.
3. **No vector or 3D authoring at all.** Agents can generate a raster image. They cannot do a
   boolean subtract on a Bézier path, set a fill rule, or extrude a logo into a validated glTF.

degen-paint closes all three. It is GIMP, Inkscape, and a glTF authoring tool, built so that the
primary operator is a machine and the GUI is the secondary surface.

| | Diffusion Studio | degen-paint |
|---|---|---|
| Unit of work | timeline composition | project of documents (canvas / artboard / scene) |
| Time axis | frames, clips, transitions | none in raster/vector; keyframes in model mode |
| Output | mp4 / webm | PNG JPG WebP AVIF TIFF · SVG PDF · glTF GLB |
| Engine | TypeScript, browser WebCodecs | **Rust**, native + `wasm32` |
| Shell | web | **Tauri v2** with the same UI running in a plain browser |
| Analogy | Premiere for agents | GIMP + Inkscape + glTF authoring, for agents |

## 2. Locked decisions

**Canonical document is JSON.** `project.json` is the single source of truth: stable, diffable,
schema-published, machine-editable. Not code-as-document — pixel work has no clean code
expression, and a JSON canonical form is what makes GUI ↔ agent round-tripping, real undo, and
honest MCP schemas possible. Procedural authoring happens through the op stream, which replays
exactly.

**Engine is Rust.** One engine serving three consumers — the `dpaint` CLI, the MCP server, and the
Tauri app linked in-process with no IPC pixel copies. The same crates compile to
`wasm32-unknown-unknown` for the browser build.

**UI is browser-first, shipped in Tauri.** The frontend is web tech, so the identical UI runs in
the desktop app against the native engine and in a browser tab against the WASM engine.

**3D is `wgpu`.** Metal/Vulkan/DX natively, WebGPU in the browser, offscreen for headless agent
turntables. No Chromium dependency anywhere in the stack.

**One op registry is the spine.** Every mutation is a registered op with a JSON Schema. The CLI
verbs, the MCP tools, the GUI command surface, undo/redo, the replayable journal, and the
generated docs are all derived from it. Adding a feature is adding one op.

**AI is optional and additive.** The editor is complete with no keys configured. When configured,
generated output enters the document as *native editable structure* — fal images become pixel
layers, Quiver SVGs are parsed into real Bézier objects you can boolean and extrude — never as an
opaque result.

## 3. Three modes, one core

| Mode | Analogue | Output |
|---|---|---|
| **raster** | GIMP / Photoshop | PNG, JPG, WebP, AVIF, TIFF |
| **vector** | Inkscape / Illustrator | SVG, PDF, rasterize at any DPI |
| **model** | glTF authoring | `.gltf` / `.glb` + turntable previews |

They share one project, one color system, one font system, one asset store, and reference each
other directly:

- a vector path **extrudes** into a mesh — a logo becomes a 3D badge
- a raster document becomes a **PBR texture** on a material
- a turntable render drops back in as a **raster layer**
- a raster layer **vectorizes** into editable paths

That cross-mode graph is the reason all three belong in one tool rather than three, and it is
where an agent gets leverage a human rarely bothers with: regenerate the whole family of assets
from one edited source path.

## 4. What makes it *for agents*

Detail in [`docs/agent-interface.md`](./docs/agent-interface.md). The short version — an agent
cannot see, so every capability is paired with a machine-readable channel:

- **digest** with every render: resolved tree, world bboxes, dominant colors, alpha coverage,
  histogram, text metrics, fonts that fell back
- **lint**: off-canvas, in-bleed, text overflow and collision, WCAG contrast against the actual
  rendered backdrop, unreadable type at output DPI, invisible layers, non-manifold geometry,
  missing UVs, oversized textures — each finding carrying the selector of the offender
- **annotated previews**: numbered bboxes with stable ids, so a vision model has real handles
- **perceptual diff**: SSIM and ΔE2000, for "did my edit change only what I intended" and for
  golden tests
- **stable ids and selectors** (`#logo`, `layer[type=text]`, `node[name^=bolt]`) — never indices
- **batch apply**: thirty ops in one transactional MCP call instead of thirty model turns
- **determinism**: embedded fonts, explicit seeds, no clock or locale in the render path, so
  identical input gives byte-identical output

## 5. Shared history with humans

The Tauri app writes through the same op registry and the same `history.jsonl`. An agent can read,
as structured ops, what a human just did in the GUI; either party can undo the other's work; the
GUI hot-reloads when an agent writes; a `.lock` file serializes concurrent writes. One document,
one history, two operators.

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Complex-script text shaping | `rustybuzz` covers it; `fontdb` handles fallback; the digest reports which fallback fired instead of failing silently |
| Boolean-op robustness on degenerate geometry | `i_overlay` is integer-robust; flatten tolerance is an explicit op parameter; golden tests cover self-intersection and coincident edges |
| WebGPU availability in browsers | capabilities are probed and reported; the browser build degrades to raster + vector with a clear message rather than an opaque failure |
| Color fidelity | composite in linear f32, tag ICC on export, one golden test per blend mode |
| Scope creep — filters and effects are infinite | the registry makes additions cheap and incremental; v1 ships a named, finite catalog and an explicit out-of-scope list |
| Runaway AI spend from a looping agent | request cache keyed by parameter hash, per-project budget ceiling, cost accounting, `--dry-run` |
| Undo correctness across 150+ ops | JSON-patch diffing instead of per-op inverses — one mechanism to get right, not 150 |

## 7. Verified toolchain and dependencies

Present on the build machine: `rustc 1.96.0`, `cargo-tauri`, `node v20.20.2`, `bun 1.3.14`.

All versions below were verified to exist on crates.io while writing this plan:

`serde 1.0.229` · `serde_json 1.0.151` · `schemars 1.2.2` · `kurbo 0.13.1` · `tiny-skia 0.12.0` ·
`image 0.25.10` · `imageproc 0.27.0` · `fast_image_resize 6.1.0` · `palette 0.7.7` ·
`rayon 1.12.0` · `i_overlay 9.0.0` · `lyon_tessellation 1.0.22` · `usvg`/`resvg 0.48.1` ·
`rustybuzz 0.20.1` · `fontdb 0.24.0` · `ttf-parser 0.25.1` · `svgtypes 0.16.1` ·
`gltf-json 1.4.1` · `mikktspace 0.3.0` · `meshopt 0.6.2` · `wgpu 30.0.1` · `oxipng 10.2.1` ·
`dssim-core 3.5.1` · `image-compare 0.5.0` · `rmcp 3.4.0` · `clap 4.6.7` · `tauri 2.11.5` ·
`blake3 1.8.7` · `notify 8.2.0` · `reqwest 0.13.5` · `tokio 1.53.1` · `keyring 4.2.0` ·
`secrecy 0.10.3` · `vtracer 0.6.5`

## 8. Build order

P0 Foundation → P1 Raster → P2 Vector → P3 Model → P4 Bridges → P5 Agent surface →
P6 AI providers → P7 Studio → P8 Docs and release.

Each phase ends with a rendered artifact and a passing check, never a claim. Acceptance criteria
per phase are in [`docs/roadmap.md`](./docs/roadmap.md).
