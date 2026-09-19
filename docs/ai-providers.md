# Optional AI providers

Atelier is a complete editor with **no AI configured**. Generation is additive: it produces
ordinary layers, ordinary paths, and ordinary textures that every other op can then manipulate.
Nothing in the core depends on a network.

Two providers are supported in v1.

| Provider | Key | Role |
|---|---|---|
| [fal.ai](https://fal.ai) | `FAL_KEY` | raster: generate, edit, inpaint, outpaint, upscale, background removal, PBR texture sets |
| [QuiverAI](https://quiver.ai) | `QUIVERAI_API_KEY` | vector: prompt → SVG, and raster → SVG vectorization |

## 1. The principle that matters

**Generated output enters the document as native, editable structure — never as an opaque
result.**

- A fal image becomes a **pixel layer** with a normal transform, mask, blend mode, and effects.
- A Quiver SVG is **parsed into vector objects** — real Bézier paths, fills, gradients, groups.
  You can immediately boolean-subtract them, offset them, recolor from the project palette, or
  extrude them into a glTF mesh.

Generation is a source of material, not a black box that produces a finished file. That is the
entire difference between an editor with AI in it and a wrapper around an image API.

## 2. Key management

Resolution order, first hit wins:

1. `--api-key` on the command (discouraged; shows up in shell history)
2. environment: `FAL_KEY`, `QUIVERAI_API_KEY`
3. OS keychain via the `keyring` crate — `atl auth set fal`
4. `~/.config/atelier/config.toml`, mode `0600`

Keys are **never** written to `project.json`, `history.jsonl`, or any asset. The journal records
the provider, model, parameters, and request id, never the credential. `atl doctor` reports which
providers resolved and from which source, without printing the key.

## 3. fal.ai integration

Asynchronous queue API, `Authorization: Key $FAL_KEY`:

```
POST https://queue.fal.run/{model}                       → { request_id, status_url, response_url }
GET  https://queue.fal.run/{model}/requests/{id}/status  → IN_QUEUE | IN_PROGRESS | COMPLETED
GET  https://queue.fal.run/{model}/requests/{id}         → model-specific result
```

Model ids are configuration, not hardcoded constants — the catalog moves fast, and an agent should
be able to point an op at any compatible endpoint:

```toml
# ~/.config/atelier/config.toml
[ai.fal]
generate   = "fal-ai/flux/dev"
edit       = "fal-ai/flux-pro/kontext"
inpaint    = "fal-ai/flux-general/inpainting"
upscale    = "fal-ai/clarity-upscaler"
remove_bg  = "fal-ai/birefnet"
```

Ops:

```bash
atl op ai.image.generate --prompt "storm light over a wheat field, 35mm" \
                         --size 1536x1024 --seed 7 --name sky
atl op ai.image.edit     --layer "#sky" --prompt "make it golden hour"
atl op ai.image.inpaint  --layer "#sky" --prompt "add a distant barn"   # uses the live selection as the mask
atl op ai.image.remove-background --layer "#subject"                    # result becomes a layer mask
atl op ai.image.upscale  --layer "#sky" --factor 2
atl op ai.texture.generate --material "#mat_gold" --prompt "brushed gold, fine scratches" --maps base,normal,roughness
```

Mechanics:
- Inputs are uploaded as data URIs or via fal storage, depending on size.
- **The current selection is the inpaint mask.** Select with `raster.select.wand`, then inpaint —
  the editor's own selection tools drive the model, which is the whole point of having them.
- Results are downloaded into the content-addressed asset store; the layer references the hash.
- Long jobs stream status; `--wait false` returns the request id so an agent can do other work and
  collect later with `atl op ai.job.collect`.

## 4. QuiverAI integration

Bearer auth, `Authorization: Bearer $QUIVERAI_API_KEY`:

```
GET  https://api.quiver.ai/v1/models               → catalog available to the org
POST https://api.quiver.ai/v1/svgs/generations     → text (+ reference images) → SVG
POST https://api.quiver.ai/v1/svgs/vectorizations  → raster image → SVG
```

Default model `arrow-2`; `arrow-2-telos` for detail-sensitive work. Both endpoints accept
`stream: true` for `reasoning` / `draft` / `content` SSE phases, which the GUI shows as a live
preview and the CLI shows as progress.

```bash
atl op ai.vector.generate --doc logo \
      --prompt "heraldic lion crest, ornate medieval detail, gold gradient accents" \
      --instructions "clean geometry, production-ready SVG structure" \
      --model arrow-2-telos --n 3

atl op ai.vector.vectorize --doc logo --from "#lyr_sketch" --auto-crop
```

After the SVG returns, Atelier runs it through the normal vector import path: parse, normalize
transforms, split into named objects, deduplicate gradients, and map colors onto the project
palette where they match. What lands in the document is indistinguishable from hand-authored
geometry.

`--n 3` generates variants as separate artboards so an agent can render all three, run `lint`, and
pick — the feedback loop the rest of the tool exists to provide.

**Offline fallback.** `vector.trace.image` does local raster→vector tracing with no network and no
key. Quiver's vectorization is better; the local path means the capability never simply disappears
when a key is absent.

## 5. Provenance

Every generated object records where it came from:

```jsonc
{
  "id": "lyr_sky", "type": "pixel", "asset": "blake3:91aa4e…",
  "provenance": {
    "provider": "fal", "model": "fal-ai/flux/dev",
    "prompt": "storm light over a wheat field, 35mm",
    "seed": 7, "requestId": "764cabcf-…",
    "at": "2026-09-19T21:12:44Z", "costUsd": 0.025
  }
}
```

This makes generated work auditable, reproducible, and separable — `atl inspect --provenance`
lists everything in a project that came from a model, which matters for licensing, for disclosure,
and for regenerating a piece at higher quality later.

## 6. Cost control

Agents loop. A looping agent with an API key is a billing incident. Therefore:

- **Cache.** Requests are keyed by `blake3(provider ‖ model ‖ canonical_params ‖ input_hashes)`.
  Replaying a journal, re-running a build, or undoing and redoing never re-bills.
- **Budget.** `atl ai budget --set 5.00` sets a ceiling per project. Exceeding it fails with exit
  code `6` and a clear message rather than silently continuing.
- **Accounting.** Every op records `costUsd`; `atl ai budget --status` shows spend by provider,
  model, and op.
- **Dry run.** `--dry-run` reports the request that *would* be sent, with its estimated cost.
- **Explicit opt-in.** No op contacts a network unless its id starts with `ai.`.

## 7. Failure behavior

Missing key, rate limit, content filter, network failure — all surface as structured errors with
the provider's own message preserved, exit code `5`, and **no partial mutation** of the document.
Provider outages degrade the tool to a fully functional offline editor, never to a broken one.
