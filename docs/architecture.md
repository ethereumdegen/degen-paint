# Architecture

## 1. Shape of the system

```
                       ┌──────────────────────────────────────────┐
   agent ──── stdio ──▶│ dpaint-mcp   (MCP tools from registry)   │
                       ├──────────────────────────────────────────┤
   agent ──── argv ───▶│ dpaint-cli   (`dpaint`, verbs from registry)│
                       ├──────────────────────────────────────────┤
   human ──── webview ▶│ apps/studio   (Tauri v2 + Vite/TS UI)     │
                       └────────────────────┬─────────────────────┘
                                            │ every surface calls the same ops
                       ┌────────────────────▼─────────────────────┐
                       │ dpaint-core                             │
                       │  Project · Documents · Assets            │
                       │  Op registry · Journal · Undo            │
                       └──┬──────────┬──────────┬─────────────┬───┘
                          │          │          │             │
                  ┌───────▼──┐ ┌─────▼────┐ ┌───▼──────┐ ┌────▼─────┐
                  │  raster  │ │  vector  │ │ model3d  │ │    ai    │
                  └───────┬──┘ └─────┬────┘ └───┬──────┘ └────┬─────┘
                          └──────────┴─────┬────┴─────────────┘
                              ┌────────────▼───────────┐
                              │ dpaint-render         │  PNG JPG WebP AVIF TIFF
                              │ dpaint-inspect        │  SVG PDF GLB
                              └────────────────────────┘  + digest / lint / diff
```

**The invariant that matters:** there is exactly one engine. The pixels a human sees in the Tauri
viewport and the pixels an agent gets from `dpaint render` come from the same Rust code path. No
second renderer, no "preview looks different from export".

## 2. Crates

| Crate | Responsibility | Key dependencies |
|---|---|---|
| `dpaint-core` | Document model (serde), ids & selectors, geometry, color, units/DPI, content-addressed asset store, op registry, JSON-patch journal, undo/redo, project load/save, JSON Schema export | `serde`, `serde_json`, `schemars`, `kurbo`, `palette`, `blake3`, `indexmap` |
| `dpaint-raster` | Layer compositor, blend modes, masks, clipping, adjustment layers, filters, selections, brush/stroke replay | `tiny-skia`, `image`, `imageproc`, `fast_image_resize`, `rayon` |
| `dpaint-vector` | Path model, boolean ops, offset/outline/simplify, gradients, text shaping → outlines, SVG parse/serialize | `kurbo`, `i_overlay`, `rustybuzz`, `fontdb`, `ttf-parser`, `usvg`, `svgtypes` |
| `dpaint-model3d` | glTF authoring: primitives, extrude/revolve/loft, PBR materials, node graph, TRS animation, validation | `gltf-json`, `lyon_tessellation`, `mikktspace`, `meshopt` |
| `dpaint-ai` | Optional provider layer: fal.ai, QuiverAI; key resolution, caching, provenance, budget | `reqwest`, `tokio`, `keyring`, `secrecy`, `eventsource-stream` |
| `dpaint-render` | Unified render targets, scale/DPI, thumbnails, wgpu PBR renderer (offscreen + on-surface) | `wgpu`, `oxipng`, `zune-jpeg`, `resvg` |
| `dpaint-inspect` | Digest, measurement, lint rules, SSIM/ΔE diff, annotate overlay | `dssim-core`, `image-compare` |
| `dpaint-cli` | `dpaint` binary; subcommands generated from the registry | `clap`, `indicatif` |
| `dpaint-mcp` | MCP server over stdio; tools generated from the registry | `rmcp` |
| `dpaint-wasm` | `wasm-bindgen` surface for the browser build | `wasm-bindgen`, `js-sys` |
| `apps/studio` | Tauri v2 desktop app; Vite + TypeScript frontend | `tauri` |

Dependency direction is strictly downward. `core` knows nothing about the mode crates; the mode
crates register their ops into `core`'s registry at startup through an inventory pattern.

## 3. Data flow of a single edit

```
  dpaint op raster.filter.gaussian-blur --layer '#sky' --radius 12
        │
        ├─ 1. parse argv against the op's JSON Schema        (dpaint-cli)
        ├─ 2. load project.json + open asset store           (dpaint-core)
        ├─ 3. snapshot the affected document                 (dpaint-core)
        ├─ 4. resolve the selector '#sky' → LayerId          (dpaint-core)
        ├─ 5. apply(&mut Project, args)                      (dpaint-raster)
        │       └─ mutates document JSON only; pixel results
        │          are written to the asset store by hash
        ├─ 6. diff snapshot → RFC-6902 patch                 (dpaint-core)
        ├─ 7. append {op, args, patch, ts} to history.jsonl  (dpaint-core)
        ├─ 8. write project.json atomically                  (dpaint-core)
        └─ 9. emit OpEffect as JSON on stdout                (dpaint-cli)
                { "changed": ["doc:main"], "created": [], "warnings": [] }
```

Undo is replaying the inverse patch. This gives correct undo for every op with no per-op inverse
code — a per-op `undo()` for 150+ ops would be a permanent bug farm.

## 4. Rendering pipeline

### Raster
Documents composite in **linear-light f32 RGBA**, premultiplied, tiled, `rayon`-parallel. The
layer tree is walked depth-first; groups composite into their own buffer so group opacity and
group blend modes are correct; clipping masks intersect coverage; adjustment layers apply to the
accumulated backdrop beneath them. Conversion to the output color space and bit depth happens once
at encode time.

### Vector
The object tree lowers to a `kurbo`-based display list, which either serializes to SVG/PDF or
rasterizes through `tiny-skia`. Text shapes with `rustybuzz` against `fontdb`, then converts to
outlines — so a rendered file never depends on the viewer having the font, and the same outlines
feed the 3D extruder.

### Model
`wgpu` with a PBR metallic-roughness shader, IBL from a bundled environment map, rendering
offscreen to a texture for headless turntables and to a surface inside Tauri for the live
viewport. Same shader both ways.

### Cross-mode
A **linked layer** is a document reference plus a transform. Rendering it recursively renders the
referenced document at the required resolution and caches the result under
`blake3(doc_state ‖ render_params)`. Cycles are rejected at op time, not render time.

## 5. Color

One color type end to end: `palette`-backed, with explicit space tagging. Internally linear f32.
Blend modes are specified against the published separable/non-separable formulas and golden-tested
individually. Exports tag ICC. ΔE2000 is available to ops and to lint so "is this text readable on
this background" has a real numeric answer instead of a guess.

## 6. Assets

`assets/` is content-addressed by `blake3`. Imported photos, baked pixel layers, fonts, AI
generations, and cached cross-mode renders all live there under their hash.

Consequences worth naming:
- `project.json` stays small and diffable — it never contains pixels.
- Undo snapshots are cheap; they copy JSON, not images.
- Deduplication is automatic; re-importing the same photo costs nothing.
- Re-running an identical AI request hits the cache and does not re-bill.
- `dpaint gc` prunes blobs unreachable from the document set and the journal.

## 7. WASM and Tauri

`core`, `raster`, `vector`, and `model3d` are `no_std`-friendly in spirit: no filesystem or process
assumptions in library code. I/O lives behind a `Vfs` trait with a native implementation
(`std::fs`) and a browser implementation (OPFS). That is what lets the identical UI run against a
native engine in Tauri and a WASM engine in a plain browser tab.

Capabilities are reported, never assumed: `dpaint doctor` and the GUI's capability probe both report
whether WebGPU, the AI providers, and the system font sources are available, so a missing feature
produces a clear message rather than a mysterious failure.
