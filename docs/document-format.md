# The `.dpaint` project format

## Layout

```
poster.dpaint/
  project.json      canonical document set — the single source of truth
  history.jsonl     append-only op journal with RFC-6902 patches
  assets/
    3f/3f9a1c…png   content-addressed blobs (blake3), two-char shard directories
    7b/7b02de…ttf
  .lock             advisory lock so the GUI and an agent cannot interleave writes
```

`project.json` is written atomically (temp file + rename). No pixel data ever appears in it, so it
stays small, human-readable, and diffable. Two agents editing the same project serialize through
`.lock`; the GUI watches the file and reloads.

## `project.json`

```jsonc
{
  "degenPaint": 1,
  "id": "prj_01J8ZK…",
  "name": "poster",
  "created": "2026-09-19T21:03:00Z",
  "modified": "2026-09-19T21:41:12Z",
  "active": "doc_main",
  "documents": {
    "doc_main":  { "kind": "raster", … },
    "doc_logo":  { "kind": "vector", … },
    "doc_badge": { "kind": "model",  … }
  },
  "palette": { "brand": "#fb8500", "ink": "#1d3557" },
  "fonts":   [ { "family": "Inter", "asset": "blake3:7b02de…", "faces": ["Regular","Bold"] } ]
}
```

Every object anywhere in the tree has a stable `id` and an optional `name`. Ids are never reused,
never renumbered, and are what ops and selectors address.

## Raster document

```jsonc
{
  "kind": "raster",
  "id": "doc_main",
  "name": "poster",
  "size": [2480, 3508],
  "dpi": 300,
  "space": "srgb",
  "depth": 8,
  "background": { "type": "solid", "color": "#ffffff" },
  "layers": [
    {
      "id": "lyr_sky", "name": "sky", "type": "pixel",
      "asset": "blake3:3f9a1c…",
      "offset": [0, 0],
      "opacity": 1.0, "blend": "normal", "visible": true, "locked": false,
      "transform": [1,0,0,1,0,0],
      "mask": { "asset": "blake3:c41e90…", "enabled": true, "inverted": false },
      "clip": false,
      "effects": [ { "type": "drop-shadow", "dx": 0, "dy": 8, "blur": 24, "color": "#00000059" } ]
    },
    { "id": "lyr_curve", "type": "adjustment",
      "adjustment": { "kind": "curves", "channel": "rgb",
                      "points": [[0,0],[0.25,0.18],[0.75,0.82],[1,1]] } },
    { "id": "lyr_title", "type": "text",
      "text": "URBAN EXPLORER", "font": { "family": "Inter", "weight": 700, "size": 96 },
      "fill": "#ffffff", "align": "center", "box": [200, 240, 2080, 400],
      "tracking": 0.02, "leading": 1.1 },
    { "id": "lyr_logo", "type": "linked", "document": "doc_logo",
      "fit": "contain", "box": [1800, 3100, 480, 240] },
    { "id": "grp_fg", "type": "group", "layers": [ … ] }
  ],
  "selection": null,
  "guides": { "bleed": 36, "safe": 120, "columns": 12, "gutter": 24 }
}
```

Non-destructive by default. A pixel layer references an immutable blob; a filter op writes a new
blob and repoints the layer, so the previous state is still on disk and undo is instant. Baking is
explicit (`raster.layer.rasterize`, `raster.doc.flatten`).

## Vector document

```jsonc
{
  "kind": "vector",
  "id": "doc_logo",
  "units": "px",
  "artboards": [ { "id": "ab_1", "name": "icon", "rect": [0,0,512,512] } ],
  "objects": [
    {
      "id": "obj_mark", "type": "path",
      "d": "M12 2 L22 20 H2 Z",
      "fill":   { "type": "linear", "stops": [[0,"#fb8500"],[1,"#ffb703"]],
                  "from": [0,0], "to": [0,512] },
      "fillRule": "nonzero",
      "stroke": { "paint": "#1d3557", "width": 8, "cap": "round", "join": "miter",
                  "miter": 4, "dash": [24,12], "align": "center" },
      "opacity": 1.0, "blend": "normal",
      "transform": [1,0,0,1,0,0]
    },
    { "id": "obj_word", "type": "text", "text": "DEGEN PAINT",
      "font": { "family": "Inter", "weight": 600, "size": 48 },
      "onPath": { "target": "obj_arc", "offset": 0.1, "side": "left" } },
    { "id": "grp_lockup", "type": "group", "clip": "obj_frame", "objects": [ … ] }
  ],
  "defs": { "symbols": {}, "markers": {}, "gradients": {} }
}
```

Paths are cubic Bézier in `kurbo` terms; `d` is the serialized form and stays SVG-compatible in
both directions. Text carries its font reference *and* can be frozen to outlines with
`vector.text.to-outlines` — the same outlines the 3D extruder consumes.

## Model document

```jsonc
{
  "kind": "model",
  "id": "doc_badge",
  "upAxis": "y",
  "nodes": [
    { "id": "nd_root", "name": "badge", "children": ["nd_body"],
      "translation": [0,0,0], "rotation": [0,0,0,1], "scale": [1,1,1] },
    { "id": "nd_body", "mesh": "msh_body", "material": "mat_gold" }
  ],
  "meshes": [
    { "id": "msh_body", "source": {
        "op": "extrude", "from": { "document": "doc_logo", "object": "obj_mark" },
        "depth": 12, "bevel": { "size": 1.5, "segments": 3 }, "caps": "both",
        "flatten": 0.05 } }
  ],
  "materials": [
    { "id": "mat_gold", "type": "pbr",
      "baseColor": "#d4af37", "metallic": 1.0, "roughness": 0.28,
      "normalTexture":   { "document": "doc_scratches", "scale": 1.0 },
      "emissiveStrength": 0.0 }
  ],
  "lights":  [ { "id": "lgt_key", "type": "directional", "intensity": 3.0, "node": "nd_key" } ],
  "cameras": [ { "id": "cam_hero", "type": "perspective", "yfov": 0.6, "node": "nd_cam" } ],
  "animations": [
    { "id": "anm_spin", "channels": [
        { "node": "nd_root", "path": "rotation", "interpolation": "LINEAR",
          "keys": [ { "t": 0, "v": [0,0,0,1] }, { "t": 4, "v": [0,1,0,0] } ] } ] }
  ]
}
```

Meshes are **procedural by default** — the document stores the recipe (`extrude from doc_logo /
obj_mark, depth 12, bevel 1.5`), not a vertex soup. Edit the vector path and the mesh regenerates.
Imported or baked meshes store a buffer asset instead. This is what makes the 3D mode diffable and
agent-editable rather than an opaque binary.

## `history.jsonl`

One JSON object per line, append-only:

```jsonc
{ "seq": 41, "ts": "2026-09-19T21:41:12Z", "actor": "agent",
  "op": "raster.filter.gaussian-blur",
  "args": { "layer": "lyr_sky", "radius": 12 },
  "patch": [ { "op": "replace", "path": "/documents/doc_main/layers/0/asset",
               "value": "blake3:91aa4e…" } ],
  "effect": { "changed": ["doc_main"], "warnings": [] },
  "cost": null }
```

This single structure gives undo/redo, full replay onto an empty project, an audit trail of who
changed what (`agent` vs `human`), and a diff format an agent can read to understand a human's
edits made in the GUI.

## Schema

`dpaint schema` emits JSON Schema for the project format and for every op, generated from the Rust
types with `schemars`. The schema is the contract for MCP tools, CLI argument parsing, and
editor autocomplete, and it cannot drift from the implementation because it *is* the
implementation.

## Versioning

`"degenPaint": 1` is the format version. Migrations are registered functions from version N to N+1,
run on open, with the pre-migration project preserved in `assets/` so an upgrade is never
destructive.
