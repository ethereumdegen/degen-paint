# Build contracts (internal, for parallel implementation)

`dpaint-core` is **done and frozen** for this round: document model, ids, selectors, colors,
asset store, op registry, journal, transactional engine. 50 tests pass. Do not modify it; if
you genuinely need a change there, message `Main` over hub instead of editing.

Read these first — they are the spec, and they are accurate:
- `crates/dpaint-core/src/doc/{common,raster,vector,model}.rs` — the document types you operate on
- `crates/dpaint-core/src/op.rs` — the `Op` trait, `OpEffect`, `OpCx`, `parse_args`, `schema_for`
- `crates/dpaint-core/src/selector.rs` — `resolve`, `resolve_one`
- `docs/op-registry.md` — the op catalog you are implementing
- `docs/errors.md` — error codes and the validate-then-mutate rule

## Universal rules

1. **Every crate exposes `pub fn ops() -> Vec<Box<dyn dpaint_core::Op>>`.** That is how the CLI
   and MCP server pick up your work. One struct per op, `id()` exactly as in
   `docs/op-registry.md` (prefixed with its domain, e.g. `raster.filter.gaussian-blur`).
2. **Args types** derive `serde::Deserialize` + `schemars::JsonSchema`, with `#[serde(default)]`
   on anything optional and doc comments on every field — those comments become the CLI help and
   the MCP tool schema an agent reads.
3. **Validate fully, then mutate.** Resolve selectors and check ranges before touching the
   project. `dpaint-core`'s engine clones before applying, but an op that half-mutates then
   errors is still a bug.
4. **Targets are selectors**, never indices: `dpaint_core::selector::resolve_one(project, sel,
   Some(&doc_id))`. Never `layers[0]`.
5. **Pixels go in the asset store**, never in the JSON: `cx.assets.put(&png_bytes, "png")?`
   returns the `AssetRef` you store on the layer.
6. **Return a truthful `OpEffect`** — `changed`, `created`, `removed`, and `warnings` for
   anything an agent should know but that is not fatal (font fallback, clamped parameter,
   empty result).
7. **Tests must assert observable behavior**, not plumbing. Test that a blur actually blurs
   (variance drops, edges soften), that a boolean subtract removes area, that an exported GLB
   parses. Do not test that a field was copied. Delete nothing from other crates' tests.
8. **Do not run workspace-wide builds, formatters, linters, or other crates' tests.** Run only
   `cargo test -p <your-crate>`. Main runs the full suite at the end.
9. Determinism: no wall clock, no HashMap iteration order, no system fonts without an explicit
   fallback report, explicit `seed` args on anything stochastic.

## Shared interchange types

- Raster interchange is `tiny_skia::Pixmap` (premultiplied sRGB u8 RGBA). Both the raster and
  vector crates already depend on `tiny-skia`; use it at crate boundaries.
- Geometry interchange is `kurbo` (re-exported as `dpaint_core::kurbo`) so there is one
  version across the workspace.
- Fonts: `fontdb` with an embedded fallback. Report `font-fallback` as an `OpEffect` warning
  whenever the requested family is not found.

## Crate APIs other crates depend on

```rust
// dpaint-raster
pub fn render_doc(
    project: &dpaint_core::Project,
    doc: &dpaint_core::DocId,
    assets: &dpaint_core::AssetStore,
    scale: f64,
    link: &LinkResolver<'_>,
) -> dpaint_core::Result<tiny_skia::Pixmap>;

/// Renders another document in the project (vector or model) at a requested pixel size.
/// dpaint-render supplies this; raster must not depend on the other engines.
pub type LinkResolver<'a> =
    dyn Fn(&dpaint_core::DocId, u32, u32) -> dpaint_core::Result<tiny_skia::Pixmap> + 'a;

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>>;
```

```rust
// dpaint-vector
pub fn render_doc(project, doc, assets, scale) -> Result<tiny_skia::Pixmap>;
pub fn to_svg(project, doc) -> Result<String>;
pub fn import_svg(svg: &str, doc_id: DocId, name: &str) -> Result<dpaint_core::VectorDoc>;

/// Resolved outline of one object in document space, with its transform applied.
/// Shapes, text (as outlines) and paths all resolve through this; the 3D extruder
/// consumes exactly this.
pub fn path_of(project, doc: &DocId, object: &ObjectId) -> Result<dpaint_core::kurbo::BezPath>;

pub fn ops() -> Vec<Box<dyn dpaint_core::Op>>;
```

```rust
// dpaint-model3d
pub struct GltfOut { pub json: String, pub bin: Vec<u8>, pub glb: Vec<u8> }
pub fn export(project, doc, assets) -> Result<GltfOut>;
pub fn build_mesh(project, doc, mesh_id) -> Result<MeshData>;   // positions, normals, uvs, indices
pub fn ops() -> Vec<Box<dyn dpaint_core::Op>>;
```

```rust
// dpaint-ai
pub trait Transport: Send + Sync {                  // injected, so tests never hit the network
    fn request(&self, req: HttpRequest) -> Result<HttpResponse>;
}
pub fn ops() -> Vec<Box<dyn dpaint_core::Op>>;
```

## Ownership for this round

| Crate | Owner | Non-goals |
|---|---|---|
| `dpaint-raster` | RasterEngine | vector paths, glTF, CLI wiring |
| `dpaint-vector` | VectorEngine | raster compositing, glTF, CLI wiring |
| `dpaint-model3d` | ModelEngine | raster, SVG parsing, CLI wiring |
| `dpaint-ai` + `dpaint-mcp` | AgentSurface | engine internals |
| `dpaint-core`, `dpaint-render`, `dpaint-inspect`, `dpaint-cli` | Main | — |

Nobody edits another owner's crate. Coordinate through hub if a contract needs to change.
