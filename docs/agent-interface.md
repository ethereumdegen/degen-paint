# The agent interface

The design constraint that shapes everything here: **an agent cannot see its own work.** A human
using GIMP gets continuous visual feedback at zero cost. An agent gets a return value. So every
capability in degen-paint is paired with a machine-readable channel that answers "what did that
actually do?"

## 1. CLI

```bash
dpaint new poster --kind raster --size 2480x3508 --dpi 300
dpaint doc add logo --kind vector --size 512x512

dpaint op vector.object.add-path --doc logo --d "M12 2 L22 20 H2 Z" --fill "#fb8500" --name mark
dpaint op vector.path.boolean --a "#mark" --b "#cut" --mode subtract
dpaint op raster.layer.add --type linked --document logo --box 1800,3100,480,240 --name badge
dpaint op raster.filter.gaussian-blur --layer "#sky" --radius 12

dpaint render out.png --scale 2 --digest digest.json
dpaint lint --json
dpaint diff out.png golden/out.png --threshold 0.01
dpaint undo
```

Rules that make this usable by a machine:

- **`--json` everywhere.** Every command emits a structured result on stdout; human formatting is
  the fallback, not the contract.
- **Exit codes mean something.** `0` success · `1` op error · `2` bad arguments ·
  `3` selector matched nothing · `4` lint failures present · `5` provider unavailable ·
  `6` budget exceeded.
- **`--dry-run`** validates and reports the intended effect without writing.
- **`dpaint op --list`** and **`dpaint schema --op <id>`** let an agent discover the full surface at
  runtime instead of relying on a memorized manual.
- **Errors carry fixes.** `selector '#sky' matched 0 objects; did you mean '#sky-grad'?
  (layers: #bg, #sky-grad, #title)` — an error that lists the actual candidates saves a round trip.

Two verbs answer questions about the app rather than about a document:

| Verb | Answers |
|---|---|
| `dpaint quote <op> [--flag v]` | `{op, estimateUsd, spentUsd, ceilingUsd, wouldExceed}` for one call — the same four numbers the Studio's `quote` dispatch returns and a paid op's submit button is named with. Ops that reach no provider quote `0`. |
| `dpaint skill [--out <dir>]` | The [starkbot-neo](https://github.com/ethereumdegen/starkbot-neo) `media-apps` pack contribution: vocabulary, native-app hints, the `dp-*` routines, Sol goal templates, grounding probes and the app skill. Written to `<dir>`, or printed as one JSON object. `GET /api/v1/skill` serves the same files under its `files` key, from the same generator. |

## 2. MCP server

```bash
dpaint mcp            # stdio; one MCP tool per op, schemas generated from the registry
```

Plus a small set of hand-written tools that are more useful than raw ops for an agent loop:

| Tool | Purpose |
|---|---|
| `dpaint_overview` | project structure, document kinds, sizes, what changed recently |
| `dpaint_render` | render and return the image **and** the digest in one call |
| `dpaint_lint` | run lint and return findings with the selector of each offender |
| `dpaint_apply` | apply a batch of ops transactionally — all succeed or none are written |
| `dpaint_history` | recent journal entries, including edits a human made in the GUI |

Batching matters: a poster is thirty ops. Thirty MCP round trips is thirty model turns.
`dpaint_apply` takes the list, validates all of them, applies them atomically, and returns one
digest.

## 3. The digest

Every render can emit a digest alongside the pixels:

```jsonc
{
  "document": "doc_main", "size": [2480, 3508], "dpi": 300, "renderMs": 412,
  "tree": [
    { "id": "lyr_sky",   "type": "pixel", "bbox": [0,0,2480,1400], "opacity": 1,
      "blend": "normal", "coverage": 1.0, "meanColor": "#5b7fa6" },
    { "id": "lyr_title", "type": "text",  "bbox": [214,238,2052,392],
      "text": "URBAN EXPLORER", "resolvedFont": "Inter Bold",
      "fontFallback": null, "lines": 1, "overflow": false,
      "contrastVsBackdrop": 7.4 }
  ],
  "histogram": { "r": [ … ], "g": [ … ], "b": [ … ], "l": [ … ] },
  "dominantColors": [ ["#5b7fa6", 0.41], ["#f2e8cf", 0.22], ["#fb8500", 0.07] ],
  "alphaCoverage": 1.0,
  "warnings": [ { "code": "near-edge", "target": "#badge", "detail": "12px from trim, bleed is 36px" } ]
}
```

This is the difference between an agent guessing and an agent knowing. Text overflowed? The digest
says so. Font silently fell back to something ugly? `fontFallback` names it. Layer invisible
because a mask ate it? `coverage: 0.0`.

## 4. Lint

`dpaint lint` encodes the mistakes a blind operator makes:

| Rule | Catches |
|---|---|
| `off-canvas`, `near-edge` | content outside the canvas, or inside the bleed/trim margin |
| `text-overflow`, `text-collision` | text past its box, or overlapping other text |
| `low-contrast` | WCAG AA/AAA failure against the actual rendered backdrop, via ΔE and relative luminance |
| `tiny-type` | type below a readable size **at the output DPI**, not in pixels |
| `invisible-layer` | zero opacity, zero coverage, hidden behind an opaque layer, or masked out entirely |
| `font-fallback` | a requested font was not available and was substituted |
| `upscaled-asset` | a raster asset rendered above its native resolution |
| `non-manifold`, `missing-uv`, `flipped-normals` | geometry that will break in a viewer |
| `oversized-texture`, `npot-texture` | glTF textures that will cost or fail downstream |
| `unreferenced-asset` | blobs no document points at |

Each finding carries a selector, so the fix is directly actionable:
`{ "rule": "low-contrast", "target": "#lyr_title", "value": 2.1, "required": 4.5 }`.

## 5. Annotated preview

`dpaint render --annotate out.png` overlays each object's bounding box with a number and its id.
A vision-capable agent then has stable handles — "#3 is overlapping #7" maps to selectors it can
act on, instead of pixel guesses about "the text near the top".

## 6. Perceptual diff

`dpaint diff a.png b.png` returns SSIM, mean and max ΔE2000, the fraction of pixels changed, the
bounding box of the changed region, and optionally a heatmap image. Two uses:

1. **Intent checking** — "I moved the logo; did anything else change?" A changed-region bbox that
   covers the whole canvas means something went wrong.
2. **Golden tests** — the project's own test suite renders fixtures and diffs against committed
   goldens with a tolerance, so refactors cannot silently change output.

## 7. Determinism

Same document plus same version equals byte-identical output. Fonts are embedded in the project,
random seeds (noise, dither, brush jitter) are explicit op arguments, floating-point paths are
fixed-order, and no wall-clock or locale input reaches the renderer. Without this, golden tests
and perceptual diffs are both worthless.

## 8. Human and agent on the same document

The Tauri app writes through the exact same op registry and the same journal. So:

- an agent can read `history.jsonl` and see, as structured ops, what the human just did;
- a human can undo an agent's op, and vice versa, through one shared history;
- the GUI watches the project directory and reloads when an agent writes;
- the `.lock` file serializes concurrent writes instead of corrupting them.

That shared-document property is what makes the collaboration real rather than two tools pointed
at the same folder.
