# Error model

An agent's recovery path is only as good as the error it gets back. Every failure in degen-paint
is structured, carries the context needed to fix it, and never leaves a half-written project.

## Guarantees

1. **Validate fully, then mutate.** An op resolves selectors, deserializes and range-checks
   arguments, and verifies preconditions *before* touching the project. A failed op leaves
   `project.json`, the journal, and the asset store exactly as they were.
2. **Atomic writes.** `project.json` is written to a temp file and renamed. A crash mid-write
   cannot corrupt a project.
3. **Batches are transactional.** `dpaint_apply` and `dpaint op --batch` validate every op, apply
   to an in-memory clone, and commit once. Op 17 of 30 failing means nothing was written.
4. **No partial network mutation.** An `ai.*` op that fails after billing still records the
   request id and cost in the journal, so the spend is visible and the result is recoverable from
   cache on retry.

## Shape

```jsonc
{
  "ok": false,
  "error": {
    "code": "selector_no_match",
    "message": "selector '#sky' matched 0 objects in doc_main",
    "op": "raster.filter.gaussian-blur",
    "target": "#sky",
    "candidates": ["#bg", "#sky-grad", "#title", "#badge-mark"],
    "suggestion": "#sky-grad",
    "docs": "dpaint schema --op raster.filter.gaussian-blur"
  }
}
```

`candidates` and `suggestion` exist specifically to collapse a two-turn failure into one.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | op failed (precondition, invalid geometry, I/O) |
| `2` | bad arguments (schema violation, unknown op, unparseable selector) |
| `3` | selector matched nothing, or matched many where one was required |
| `4` | completed, but lint findings exist at or above the configured severity |
| `5` | provider unavailable (missing key, network, rate limit, content filter) |
| `6` | budget exceeded |
| `7` | project locked by another writer |

Exit code `4` is deliberate: `dpaint lint` succeeding at *running* while the document has problems
is not success, and an agent loop should be able to branch on that without parsing text.

## Warnings

Non-fatal problems ride along on a successful result rather than being printed and lost:

```jsonc
{ "ok": true,
  "effect": { "changed": ["doc_main"], "created": ["lyr_9"],
              "warnings": [ { "code": "font-fallback", "target": "#title",
                              "requested": "Inter", "used": "DejaVu Sans" } ] } }
```

## Catalog

`selector_no_match` · `selector_ambiguous` · `unknown_op` · `schema_violation` ·
`wrong_document_kind` · `cyclic_link` · `asset_missing` · `asset_decode_failed` ·
`font_unavailable` · `degenerate_geometry` · `non_manifold` · `unsupported_format` ·
`project_locked` · `migration_required` · `provider_unconfigured` · `provider_error` ·
`budget_exceeded` · `exists` · `io_error`

Each maps to exactly one exit code and is stable across versions — agents branch on `code`, never
on message text.
