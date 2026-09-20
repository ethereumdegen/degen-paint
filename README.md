<h1 align="center">degen-paint</h1>

<p align="center">
  <b>An agent-native image and 3D asset studio.</b><br>
  GIMP, Inkscape, and a glTF authoring tool — designed from the first line to be driven by a coding agent.
</p>

<p align="center">
  <img alt="status" src="https://img.shields.io/badge/status-planning-orange">
  <img alt="engine" src="https://img.shields.io/badge/engine-Rust-b7410e?logo=rust&logoColor=white">
  <img alt="shell" src="https://img.shields.io/badge/shell-Tauri%20v2-24C8DB?logo=tauri&logoColor=white">
  <img alt="3d" src="https://img.shields.io/badge/3D-wgpu%20%2F%20WebGPU-005A9C">
  <img alt="license" src="https://img.shields.io/badge/license-MIT-blue">
</p>

> **Status: the engine works.** 212 ops across all three modes, 415 tests green, driven by the
> `dpaint` CLI and an MCP server. The Tauri/WASM GUI (P7) is designed but not built — see
> [`docs/roadmap.md`](./docs/roadmap.md) for exactly what is and is not done.

---

## The problem

[Diffusion Studio](https://diffusion.studio) made video editing agent-native by making the edit
*be a document*: something an agent can read, diff, re-run, and render headlessly, exposed over a
CLI and an MCP server so a coding agent does real work with no GUI in the loop.

There is no equivalent for **still images and 3D assets**, and the gap is not "an image library."
Those exist. What's missing is:

1. **No document model.** Libraries apply operations to buffers. No layer stack, no history, no
   non-destructive edits, no project an agent can reason about across turns.
2. **No feedback channel.** An agent that calls an image API cannot tell whether the text
   overflowed its box, whether the font silently fell back, whether the logo landed inside the
   bleed, or whether the title is unreadable against the sky. It is operating blind with no
   instrument.
3. **No vector or 3D authoring at all.** An agent can generate a raster image. It cannot boolean-
   subtract a Bézier path, set a fill rule, offset a stroke, or extrude a logo into a validated
   glTF.

degen-paint is the missing tool.

## Three modes, one core

| Mode | Analogue | Output |
|---|---|---|
| **raster** | GIMP / Photoshop | PNG, JPG, WebP, AVIF, TIFF |
| **vector** | Inkscape / Illustrator | SVG, PDF, rasterize at any DPI |
| **model** | glTF authoring | `.gltf` / `.glb` + turntable previews |

They share one project, one color system, one font system, and one asset store — and they
reference each other:

```
   vector path ──extrude──▶ 3D mesh          raster doc ──▶ PBR texture
   turntable render ──▶ raster layer         raster layer ──vectorize──▶ editable paths
```

That cross-mode graph is why all three belong in one tool instead of three, and it's where an
agent gets leverage a human rarely bothers with: edit one source path, regenerate the whole family
of assets.

## Quickstart

```bash
cargo build --release            # binary at target/release/dpaint
dpaint doctor                    # capabilities: ops, formats, providers
dpaint op --list                 # the whole catalog, or `--list raster.filter`
dpaint op raster.filter.gaussian-blur --help   # generated from the op's JSON Schema
```

[`examples/campaign.sh`](./examples/campaign.sh) is the pipeline below, end to end, verified in
CI by `crates/dpaint-cli/tests/pipeline.rs`.

## What a session looks like

```bash
# one project, three documents of different kinds
dpaint new campaign --kind raster --size 2480x3508 --dpi 300
dpaint doc add logo  --kind vector --size 512x512
dpaint doc add badge --kind model

# vector: real geometry, real boolean ops
dpaint op vector.object.add-path --doc logo --d "M256 32 L480 448 H32 Z" --fill "#fb8500" --name mark
dpaint op vector.object.add-ellipse --doc logo --cx 256 --cy 320 --rx 64 --ry 64 --name hole
dpaint op vector.path.boolean --a "#mark" --b "#hole" --mode subtract
dpaint op vector.text.on-path --doc logo --text "DEGEN" --target "#mark" --offset 0.1

# 3D: extrude that same path, no export/import dance
dpaint op model.mesh.extrude --doc badge --from "logo:#mark" --depth 12 --bevel 1.5 --caps both
dpaint op model.material.set-pbr --material "#mat" --base-color "#d4af37" --metallic 1 --roughness 0.28

# raster: compose, with the other documents linked in live
dpaint op raster.layer.add --type linked --document logo --box 1800,3100,480,240 --name badge-mark
dpaint op raster.layer.add --type text --text "URBAN EXPLORER" --font Inter:700 --size 96 --name title
dpaint --doc campaign op raster.select.wand --at 100,100 --tolerance 24
dpaint --doc campaign op raster.filter.gaussian-blur --target "@sky" --sigma 12   # scoped to the selection

dpaint --doc poster render out/poster.png --scale 2
dpaint --doc badge  render out/badge.glb
dpaint --doc poster inspect          # the digest
dpaint lint --json                   # exit 4 when the document has problems
```

Two flag conventions worth knowing, because they are uniform across all 212 ops: the global
`--doc` picks the document being edited, and an op's own `--source` names a document it *reads*
(the target of a linked layer, the vector path an extrusion consumes, the raster document behind
a texture).

Every one of those commands is a registered op. Every one is journaled, undoable, and replayable.

## What makes it *for agents*

An agent cannot see. So every capability is paired with a machine-readable feedback channel.

**One op registry is the spine.** Every mutation is a registered op with a JSON Schema. The CLI
verbs, the MCP tools, the GUI command surface, undo/redo, the replayable journal, and the
generated reference docs are all *derived* from that single definition. Adding a feature is adding
one op — it shows up everywhere automatically, and the surfaces cannot drift.

**Renders return a digest, not just pixels.**

```jsonc
{ "id": "title", "type": "text", "bbox": [214,238,2052,392],
  "resolvedFont": "Inter Bold", "fontFallback": null,
  "lines": 1, "overflow": false, "contrastVsBackdrop": 2.1 }
```

That last field is the difference between an agent guessing and an agent knowing.

**Lint encodes the mistakes a blind operator makes** — and each finding carries the selector of
the offender, so the fix is directly actionable:

```jsonc
{ "rule": "low-contrast",  "target": "#title",      "value": 2.1, "required": 4.5 }
{ "rule": "near-edge",     "target": "#badge-mark", "detail": "12px from trim, bleed is 36px" }
{ "rule": "font-fallback", "target": "#caption",    "requested": "Inter", "used": "DejaVu Sans" }
{ "rule": "non-manifold",  "target": "badge:#msh_body", "edges": 14 }
```

- **Annotated previews** overlay numbered bounding boxes and stable IDs, so a vision model has
  handles that mean something instead of pixel guesses.
- **Perceptual diff** (SSIM / ΔE2000 + changed-region bbox) answers *"did my edit change only what
  I intended?"* — and powers the golden test suite.
- **Stable IDs and selectors** — `#logo`, `layer[type=text]`, `node[name^=bolt]`. Never an index.
  A selector matching nothing is an error, not a silent no-op.
- **Batch apply** — thirty ops in one transactional MCP call instead of thirty model turns.
- **Determinism** — embedded fonts, explicit seeds, no clock or locale in the render path. Same
  input, byte-identical output.

## MCP

```bash
dpaint mcp        # stdio; one MCP tool per op, schemas generated from the registry
```

Plus a handful of tools shaped for an agent loop rather than for a GUI: `dpaint_overview`,
`dpaint_render` (image **and** digest in one call), `dpaint_lint`, `dpaint_apply` (transactional
batch), `dpaint_history`.

## Human and agent, one document

The Tauri app writes through the same op registry and the same `history.jsonl`. So an agent can
read, as structured ops, what a human just did in the GUI; either party can undo the other's work;
the GUI hot-reloads when an agent writes; and a lock file serializes concurrent writes instead of
corrupting them. One document, one history, two operators.

## Optional AI providers

degen-paint is a complete editor with **no AI configured**. Generation is additive and opt-in:

| Provider | Key | Role |
|---|---|---|
| [fal.ai](https://fal.ai) | `FAL_KEY` | generate, edit, inpaint, outpaint, upscale, remove background, PBR texture sets |
| [QuiverAI](https://quiver.ai) | `QUIVERAI_API_KEY` | prompt → SVG, and raster → SVG vectorization |

The principle: **generated output enters the document as native editable structure, never as an
opaque result.** A fal image becomes a pixel layer with a normal mask, blend mode, and effects. A
Quiver SVG is parsed into real Bézier objects you can immediately boolean-subtract, offset,
recolor from the project palette, or extrude into 3D.

```bash
dpaint op ai.image.generate --prompt "storm light over a wheat field, 35mm" --size 1536x1024 --seed 7
dpaint op raster.select.wand --layer "#sky" --at 100,100 --tolerance 24
dpaint op ai.image.inpaint --layer "#sky" --prompt "add a distant barn"   # the selection IS the mask
dpaint op ai.vector.generate --doc logo --prompt "heraldic lion crest, gold gradient" --model arrow-2-telos --n 3
```

Keys resolve from env, OS keychain, or a `0600` config file and never touch the project. Every
generated object records provenance (provider, model, prompt, seed, request id, cost). Requests
are cached by parameter hash, so replaying a journal or undoing and redoing never re-bills, and a
per-project budget ceiling stops a looping agent from spending your money.

Details: [`docs/ai-providers.md`](./docs/ai-providers.md).

## Stack

```
  dpaint          (CLI)    ─┐
  dpaint-mcp   (MCP stdio) ─┼─▶  one Rust engine  ─▶  identical renders everywhere
  Tauri app / WASM  (GUI)  ─┘
```

Rust engine · JSON canonical documents · Tauri v2 shell · browser-first UI. The same crates
compile natively for the CLI and the desktop app and to `wasm32` for the browser build, so the
human's viewport and the agent's headless render come from the same code path — no "preview looks
different from export." 3D is `wgpu`: Metal/Vulkan/DX natively, WebGPU in the browser, offscreen
for headless turntables. No Chromium anywhere in the stack.

Raster compositing is linear-light f32, tiled and `rayon`-parallel. Pixels live in a
`blake3` content-addressed store and never in the JSON, which keeps `project.json` small and
diffable, makes undo snapshots cheap, and gives free deduplication.

## Roadmap

Every phase ends with a rendered artifact and a passing check, never a claim.

| | Phase | Gate |
|---|---|---|
| P0 | Foundation — doc model, op registry, journal, undo | **done** — apply/undo/redo restores byte-identical JSON at every step |
| P1 | Raster engine — 84 ops | **done** — 106 tests: blur reduces variance, group opacity differs from per-child, selections scope every filter |
| P2 | Vector engine — 63 ops | **done** — 85 tests: boolean areas verified numerically, SVG round-trips byte-stable |
| P3 | Model engine — 37 ops | **done** — 49 tests: GLB re-parses, extrude volume = s²d, holes survive into the caps |
| P4 | Cross-mode bridges | **done** — one path drives `logo.svg` + `badge.glb` + `poster.png`; recoloring the source repaints the poster |
| P5 | Agent surface | **done** — digest, lint, annotate, SSIM/ΔE diff, 217 MCP tools over stdio |
| P6 | AI providers | **done** — 52 tests against recorded transports; cache, budget, provenance |
| P7 | Studio (Tauri + browser) | not started |
| P8 | Docs and release | in progress |

Full acceptance criteria: [`docs/roadmap.md`](./docs/roadmap.md).

## Not in v1

Named so they don't creep in: video and animation timelines beyond glTF TRS tracks; skinned meshes
and morph targets; CMYK and print separations; plugin scripting; multi-user realtime
collaboration; a node-based compositor graph.

## Documentation

| Document | Contents |
|---|---|
| [`PLAN.md`](./PLAN.md) | Master plan: thesis, locked decisions, risks, build order |
| [`docs/architecture.md`](./docs/architecture.md) | Crate graph, data flow, render pipeline, color, assets, WASM |
| [`docs/document-format.md`](./docs/document-format.md) | The `.dpaint` format, all three document kinds, the journal |
| [`docs/op-registry.md`](./docs/op-registry.md) | The op model and the complete v1 op catalog |
| [`docs/agent-interface.md`](./docs/agent-interface.md) | CLI, MCP, digest, lint, annotate, diff, determinism |
| [`docs/ai-providers.md`](./docs/ai-providers.md) | Optional fal.ai and QuiverAI integration |
| [`docs/selectors.md`](./docs/selectors.md) | Selector grammar and resolution rules |
| [`docs/errors.md`](./docs/errors.md) | Structured errors, exit codes, transactional guarantees |
| [`docs/testing.md`](./docs/testing.md) | Golden renders, determinism, performance budgets |
| [`docs/roadmap.md`](./docs/roadmap.md) | P0–P8 with acceptance criteria |

## License

MIT
