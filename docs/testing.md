# Testing strategy

A rendering tool that cannot prove its output is unchanged has no way to refactor safely. The
suite is built around that, not around coverage percentage.

## Layers of the suite

| Layer | What it proves | Tool |
|---|---|---|
| Unit | geometry, color math, blend formulas, selector parsing, patch/undo algebra | `cargo test` |
| Schema | every registered op emits valid JSON Schema; CLI flags and MCP tools round-trip | generated test over the registry |
| Golden render | pixel output has not drifted | render + SSIM/ΔE diff against committed PNGs |
| Round-trip | SVG and glTF export → import → export is stable | byte or structural equality |
| Journal | replay `history.jsonl` onto an empty project reproduces `project.json` exactly | property test |
| Undo algebra | apply *n* ops, undo *n*, redo *n* — identical JSON at every step | property test over random op sequences |
| Conformance | exported GLB passes glTF validation; exported SVG parses in `usvg` and a browser | external validators |
| Lint | each rule fires on a seeded defect fixture and stays silent on the clean one | fixture pairs |
| CLI contract | exit codes, `--json` shape, `--dry-run` writes nothing | integration tests on a temp project |

## Goldens

Goldens live in `tests/golden/<case>/expected.png` with the ops that produce them in
`case.jsonl` — the fixture is a journal, so a golden is *reproducible by replay*, not a mystery
binary.

Comparison is perceptual, not byte-exact: SSIM ≥ 0.999 and max ΔE2000 ≤ 1.0 by default, tightened
per case. A failing case writes `actual.png` and `diff.png` next to the expected file, so the
failure is inspectable rather than a boolean.

`cargo test -- --ignored update-goldens` regenerates, and regeneration is a reviewable diff in
the PR — never something CI does silently.

## Determinism

Golden tests are worthless without it, so determinism is itself tested:

- fonts are embedded in fixtures; no test may depend on a system font
- every stochastic op (`filter.noise-add`, `filter.dither`, `paint.stroke` jitter) takes an
  explicit `seed`
- no wall clock, locale, HashMap iteration order, or thread count reaches the render path —
  `rayon` tiles composite into disjoint regions, so parallelism cannot change results
- the same fixture renders identically on macOS and Linux in CI; a platform diff fails the build

## Blend-mode grid

One fixture renders all 25+ blend modes over a gradient-and-photo backdrop in a labeled grid, with
per-mode numeric assertions against the published formulas at sampled points. Blend math is easy
to get subtly wrong and nearly impossible to eyeball.

## AI providers

Provider tests never hit the network by default. `dpaint-ai` talks to a `Transport` trait; tests
inject a recorded-fixture transport, covering request shaping, SSE parsing, error mapping, cache
keying, and budget enforcement. Live tests are behind `--features live-ai` and are excluded from CI.

## Performance budgets

Tracked as tests, not aspirations, on the reference machine (Apple silicon, 8 cores):

| Operation | Budget |
|---|---|
| Composite 4000×4000, 12 layers, 3 adjustments | < 250 ms |
| Gaussian blur r=32 on 4000×4000 | < 120 ms |
| Boolean op on two 2000-segment paths | < 50 ms |
| SVG parse → display list, 5000 objects | < 80 ms |
| Extrude a 500-segment path with bevel | < 40 ms |
| `dpaint render` cold start to first pixel | < 150 ms |
| `project.json` load, 500 layers | < 15 ms |

A regression beyond 20% fails CI. Cold start matters more than it looks: an agent invokes the CLI
hundreds of times in a session, and a 2-second startup turns a build into a coffee break.
