# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — unreleased

First release: the engine, the three surfaces an agent drives it through, and the GUI a human
drives it through. The date is filled in when the `v0.1.0` tag is pushed.

### Added

**Document model and op registry** (`dpaint-core`)
- `Project` / `Document` JSON model for raster, vector and model documents, with `schemars`
  JSON Schema for every type.
- **212 registered ops** — 84 `raster.*`, 63 `vector.*`, 37 `model.*`, 12 `ai.*` and 16 across
  `asset`, `doc`, `font`, `inspect`, `lint`, `palette`, `project` and `render`. Every op has a
  JSON Schema, and the CLI flags, MCP tools and GUI inspector forms are all generated from it.
- RFC-6902 journal (`history.jsonl`), undo/redo by inverse patch, and replay of a journal onto
  an empty project.
- Stable ids and a selector grammar (`#id`, `layer[type=text]`, `node[name^=bolt]`,
  `doc:@selector`); a selector that matches nothing is an error carrying the candidate list.
- `blake3` content-addressed asset store, atomic project writes and a lock file, so an agent
  and a human writing at once serialise instead of corrupting.

**Raster** (`dpaint-raster`)
- Linear-light f32 tiled compositor, `rayon`-parallel, with groups, nested groups, clipping
  masks, layer masks and adjustment layers.
- Separable and non-separable blend modes against the published formulas.
- The `adjust.*` and `filter.*` catalogs, selections (including magic wand and colour range,
  with feather/grow/shrink) that scope every filter, and text layers with shaping, wrapping
  and fit-to-box.

**Vector** (`dpaint-vector`)
- `kurbo` path model with SVG-compatible `d` in and out, boolean ops via `i_overlay`, offset,
  outline, simplify and round-corners.
- Fills, strokes, dashes, gradients and fill rules; text shaping through `rustybuzz` with
  `fontdb` fallback, text-on-path and text-to-outlines.
- SVG import and export with stable ids, rasterisation at any scale, and a local offline
  raster→vector trace that needs no network.

**3D** (`dpaint-model3d`)
- glTF scene graph, primitives, and extrude / revolve / loft from vector paths with bevels and
  caps; normals, tangents, UVs, weld, merge and decimate.
- PBR metallic-roughness materials, TRS keyframe animation, `.gltf` and `.glb` export, and a
  validation report that catches non-manifold geometry.

**Cross-mode**
- Linked layers: a vector or model document rendered live inside a raster document, with cycle
  detection at op time and render caching keyed by document state.
- Raster documents as PBR texture sources, and turntable renders composited back as layers.

**Rendering** (`dpaint-render`)
- One render path for every surface: PNG, JPG, WebP, TIFF, SVG, `.gltf` and `.glb`.
- A CPU z-buffer PBR preview rasterizer for headless turntables — deliberately not `wgpu`, so
  an agent's render is byte-identical on a laptop, in CI and in a container with no display.

**Agent feedback** (`dpaint-inspect`)
- Render digest: per-object bboxes, resolved fonts, fallback detection, overflow and contrast.
- Lint rules for the mistakes a blind operator makes, each finding carrying the selector of the
  offender; `dpaint lint` exits 4 when a document has problems.
- SSIM / ΔE2000 perceptual diff with heatmaps, and annotated previews with numbered bboxes.

**CLI** (`dpaint-cli`)
- `dpaint` with `new`, `doc`, `op`, `render`, `inspect`, `lint`, `diff`, `annotate`, `undo`,
  `redo`, `history`, `schema`, `gc`, `doctor`, `mcp` and `serve`.
- Uniform conventions across all 212 ops: global `--doc`, `--project`, `--json`, `--dry-run`;
  per-op `--source` and `--target`. `dpaint op <id> --help` is generated from the op's schema.

**MCP** (`dpaint-mcp`)
- `dpaint mcp`: MCP over stdio, protocol `2025-06-18`, **217 tools** — one per op plus
  `dpaint_overview`, `dpaint_render` (image and digest in one call), `dpaint_lint`,
  `dpaint_apply` (transactional batch) and `dpaint_history`.

**AI providers, optional** (`dpaint-ai`)
- fal.ai (generate, edit, inpaint with the selection as mask, outpaint, upscale, background
  removal, PBR texture sets) and QuiverAI (prompt→SVG, raster→SVG), both landing as native
  editable structure rather than an opaque result.
- Key resolution from an explicit argument, environment, OS keychain or a `0600` config file;
  request cache keyed by parameter hash, per-project budget ceiling, cost accounting,
  provenance records and `--dry-run`. Model ids are configuration, not constants.

**Studio** (`dpaint-studio`, `apps/studio`)
- One `Studio::dispatch` API behind both shells, so the Tauri desktop app and the browser tab
  cannot drift.
- `dpaint serve` hosts the UI on localhost with no bundler and no npm — the frontend is plain
  ES modules and CSS.
- Tauri v2 desktop app writing through the same op registry and the same journal: either party
  can undo the other's work, and an agent's write appears in the open page within ~2s.
- Schema-driven inspector: pick any op from the command palette and its form is built from the
  same JSON Schema the CLI and MCP use.

**Release engineering**
- CI on `ubuntu-latest` and `macos-latest`: workspace build and test, `cargo fmt --check` and
  `cargo clippy -D warnings`, a `wasm32-unknown-unknown` build of the engine crates, and
  golden-render `actual.png` / `diff.png` uploaded as artifacts when a render drifts.
- Tagged releases build stripped `dpaint` tarballs for macOS (aarch64 and x86_64) and Linux
  x86_64 with a `SHA256SUMS` file, plus unsigned macOS Studio bundles. Nothing is published to
  crates.io automatically; the workflow runs `cargo publish --dry-run --workspace` instead.
- `docs/installing.md` covers installing the CLI, building the desktop app, registering the
  MCP server with a coding agent and supplying the optional provider keys.

### Notes

- Internal dependencies carry both `path` and an explicit `version`, which is what makes the
  publish dry-run meaningful. A version bump therefore has to touch `workspace.package.version`
  **and** the `version = "…"` on each internal entry in `[workspace.dependencies]`.
- The desktop app (`dpaint-studio-app`) is `publish = false`: it ships as a bundle, not as a
  crate.
- Release binaries and bundles are unsigned; no Developer ID or notarisation is configured.

[0.1.0]: https://github.com/ethereumdegen/degen-paint/releases/tag/v0.1.0
