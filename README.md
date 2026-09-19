# degen-paint

**An agent-native image and 3D asset studio.** GIMP, Inkscape, and a glTF authoring tool —
designed from the first line to be driven by a coding agent.

> Status: **planning**. This repository currently contains the design. Nothing is implemented yet.
> Read [`PLAN.md`](./PLAN.md) first.

---

## Why

[Diffusion Studio](https://diffusion.studio) made video editing agent-native by making the edit
*be a document* — something an agent can read, diff, re-run, and render headlessly, exposed over a
CLI and an MCP server so a coding agent can do real work without a GUI.

Nothing equivalent exists for **still images and 3D assets**. Agents today produce images by
calling a generation API and hoping. They cannot compose layers, adjust curves, do a boolean
subtract on a Bézier path, set a fill rule, or extrude a logo into a glTF badge — because the
tools for that work are GUIs with no machine surface, and the libraries that do have machine
surfaces have no document model, no history, and no way for an agent to *see what it did*.

degen-paint is that missing tool.

## Three modes, one core

| Mode | Analogue | Native output |
|---|---|---|
| **raster** | GIMP / Photoshop | PNG, JPG, WebP, AVIF, TIFF |
| **vector** | Inkscape / Illustrator | SVG, PDF, + rasterize at any DPI |
| **model** | glTF authoring | `.gltf` / `.glb`, with turntable previews |

They live in one project and reference each other. A vector path extrudes into a mesh. A raster
document becomes a PBR texture. A turntable render drops back in as a raster layer. That
cross-mode graph is the reason all three belong in one tool instead of three.

## What makes it *for agents*

An agent cannot see. So every capability is paired with a machine-readable feedback channel.

- **One op registry is the spine.** Every mutation is a registered op with a JSON Schema. The CLI,
  the MCP tools, the GUI command surface, undo/redo, and the replayable journal are all *derived*
  from it. Adding a feature is adding one op.
- **Render returns a digest, not just pixels** — resolved tree, world-space bounding boxes,
  dominant colors, alpha coverage, histogram, text metrics, font fallbacks that fired.
- **Lint** catches what a blind agent gets wrong: content off-canvas, overlapping text, WCAG
  contrast failures, unreadable sizes at output DPI, non-manifold geometry, missing UVs.
- **Annotated previews** overlay numbered bounding boxes and stable IDs, so a vision model has
  handles that mean something.
- **Perceptual diff** (SSIM / ΔE) answers "did my edit change only what I intended?"
- **Stable IDs and selectors** — `#logo`, `layer[type=text]`, `node[name^=bolt]`. Never an index.

## Optional AI providers

degen-paint is fully functional offline. Generation is **additive and opt-in**:

- **[fal.ai](https://fal.ai)** (`FAL_KEY`) — generate, edit, inpaint, upscale, and
  background-removal directly onto the raster canvas.
- **[QuiverAI](https://quiver.ai)** (`QUIVERAI_API_KEY`) — generate and vectorize **real editable
  SVG geometry**, parsed into the vector document as paths you can boolean, offset, and restyle.

Every generated asset carries provenance (provider, model, prompt, seed, request id, cost) in the
document, results are content-addressed and cached so replays never re-bill, and a budget ceiling
stops a looping agent from spending your money.

See [`docs/ai-providers.md`](./docs/ai-providers.md).

## Stack

Rust engine, JSON documents, Tauri v2 shell, browser-first UI.

```
dpaint              CLI          ─┐
dpaint-mcp      MCP server   ─┼─→ one Rust engine ─→ same renders everywhere
Tauri app / WASM GUI          ─┘
```

The same crates compile natively for the CLI and desktop app and to `wasm32` for the browser
build, so the human's viewport and the agent's headless render come from identical code. 3D is
`wgpu` (Metal/Vulkan/DX natively, WebGPU in browser, offscreen for headless).

## Documentation

| Document | Contents |
|---|---|
| [`PLAN.md`](./PLAN.md) | Master plan: thesis, locked decisions, phases, risks |
| [`docs/architecture.md`](./docs/architecture.md) | Crate graph, data flow, rendering pipeline |
| [`docs/document-format.md`](./docs/document-format.md) | The `.dpaint` project format and JSON schema |
| [`docs/op-registry.md`](./docs/op-registry.md) | The op model and the full v1 op catalog |
| [`docs/agent-interface.md`](./docs/agent-interface.md) | CLI, MCP, digest, lint, diff, annotate |
| [`docs/ai-providers.md`](./docs/ai-providers.md) | fal.ai and QuiverAI integration |
| [`docs/roadmap.md`](./docs/roadmap.md) | Phases with acceptance criteria |

## License

MIT
