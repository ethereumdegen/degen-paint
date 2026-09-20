# dpaint-wasm

The degen-paint engine compiled into a browser tab: no server, no install, the same op
registry and the same renderer the CLI uses.

```
  browser tab
  ├── boot.js            OPFS / IndexedDB persistence, defines the two globals
  ├── ui/                verbatim copy of crates/dpaint-studio/ui/
  └── pkg/               dpaint_wasm.js + dpaint_wasm_bg.wasm
                         └── dpaint-core · raster · vector · model3d · render · inspect
                             over an in-memory Vfs
```

The UI is *unmodified*. It probes for two globals and falls back to HTTP when they are
absent, which is how the identical bytes run in Tauri, against `dpaint serve`, and here:

```js
window.__DPAINT_INVOKE__     = async (method, params) => result
window.__DPAINT_RENDER_URL__ = async ({ doc, scale, max }) => "data:image/png;base64,…"
```

`method` is one of `state`, `catalog`, `schema`, `op`, `undo`, `redo`, `digest`, `lint`,
`history`, `select` — exactly `dpaint_studio::api::Studio::dispatch`'s surface. A failure
rejects with the same `{code, message, candidates?, suggestion?}` object the HTTP bridge
puts in its `error` field, so the UI's error handling needs no browser-specific branch.

## Build

```bash
# once: the CLI must match the wasm-bindgen crate version in Cargo.lock exactly
cargo install wasm-bindgen-cli --version 0.2.128 --locked

# optional but worth it: shrinks the module by roughly a third
brew install binaryen          # or: cargo install wasm-opt --locked

# assemble target/wasm-studio/
./crates/dpaint-wasm/web/build.sh

# serve it — a plain static file server, nothing else
python3 -m http.server -d target/wasm-studio 8787
```

`build.sh` is the whole recipe and refuses to run against a mismatched CLI:

```bash
cargo build -p dpaint-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web --no-typescript \
  --out-dir target/wasm-studio/pkg --out-name dpaint_wasm \
  target/wasm32-unknown-unknown/release/dpaint_wasm.wasm
wasm-opt -Oz --enable-bulk-memory …          # when available
cp crates/dpaint-wasm/web/{index.html,boot.js} target/wasm-studio/
cp crates/dpaint-studio/ui/*                  target/wasm-studio/ui/
```

`wasm-pack` also works (`wasm-pack build crates/dpaint-wasm --target web --release`) but
adds an npm-shaped `package.json` this page has no use for, so `build.sh` calls
`wasm-bindgen` directly.

The repo-root `.cargo/config.toml` is required: it sets
`--cfg getrandom_backend="wasm_js"` for `wasm32-unknown-unknown`. Build from the repo root
so cargo picks it up.

### Bundle size

Measured on the assembled directory, `[profile.release]` as the workspace sets it
(`opt-level = 3`, thin LTO):

| File | raw | gzip |
|---|---|---|
| `dpaint_wasm_bg.wasm`, `wasm-opt -Oz` | 8,240,674 (7.9 MiB) | 2,854,643 (2.7 MiB) |
| `dpaint_wasm_bg.wasm`, no `wasm-opt` | 9,710,439 (9.3 MiB) | 3,061,853 (2.9 MiB) |
| `dpaint_wasm.js` | 20,445 | 4,784 |

So `wasm-opt -Oz` buys 15% raw and 7% gzipped — worth having, not transformative. 515 KB of
the module is the embedded Roboto fallback face, which is not optional: a render must not
depend on what fonts the visitor has. The rest is three engines' worth of geometry, image
codecs and SVG parsing in one module; there is no lazy-loading seam today because the op
registry is built eagerly at construction.

## API

```js
import init, { DpaintEngine } from './pkg/dpaint_wasm.js';
await init();

const e = new DpaintEngine();                          // fresh 1024×1024 raster project
const e2 = DpaintEngine.create('poster', 'raster', 2480, 3508);
const e3 = DpaintEngine.load(projectJsonText);         // adopt a persisted project.json

e.dispatch('op', { op: 'raster.layer.add', args: { type: 'fill', color: '#fb8500' } });
await e.render_png(undefined, 1, 1600);                // data:image/png;base64,…

e.project_json();                                      // what to persist
e.history_jsonl();                                     // …and the journal, so undo survives
e.restore_history(jsonl);
e.put_asset(bytes, 'png');                             // → "blake3:….png"
e.asset_refs(); e.asset_bytes(ref);                    // what to persist, and its bytes
```

Persistence is deliberately the page's job. The engine owns a `MemVfs` and hands out bytes;
`boot.js` decides where they go. That keeps the Rust free of storage APIs and lets a host
embed the engine with its own backend.

## Storage

`boot.js` tries three tiers and says in the corner of the window which one it got:

1. **OPFS** (`navigator.storage.getDirectory` + `createWritable`), probed with a real write
   before it is trusted. Needs a secure context — `http://localhost` counts.
2. **IndexedDB**, same key space, bytes as `ArrayBuffer`.
3. **Memory only**, with a visible orange notice reading *"this project is NOT saved and will
   be lost on reload"*. No silent data loss.

Assets are content-addressed, so restoring is just replaying the saved blobs through
`put_asset` — the refs come back identical by construction.

## What is not in this build

| Missing | Why |
|---|---|
| `dpaint-ai` (52 ops) | Needs the network and an OS keychain. Excluded outright, so `reqwest`/`tokio`/`keyring` stay out of the bundle. The op catalog is otherwise byte-identical to the desktop one, pinned by a test. |
| rayon row parallelism | There is no thread to spawn. `dpaint-raster`'s `parallel` feature is on by default natively and off here; filters run sequentially and produce bit-identical output, just slower. Measured in the tab: a σ=6 gaussian blur over a 512×512 pixel layer is ~170 ms for the op and ~280 ms to re-render the viewport. |
| Ops that read a host file | `asset.import`, `model.mesh.import` and `vector.trace.image` take a path on the machine running the engine. There isn't one. They fail with a structured `io_error` rather than crashing — `model.mesh.import --file /tmp/x.glb` returns `io error: operation not supported on this platform`, and `asset.import` reports the path as not found in the in-memory tree. Import bytes through `window.__DPAINT_PUT_ASSET__(bytes, ext)` and pass the resulting `blake3:…` ref as the op's `asset` argument instead. |
| WebGPU 3D preview | Model documents render through the same CPU z-buffer rasterizer the CLI uses. Correct, not fast. |
| Multi-tab editing | Two tabs over one OPFS directory will clobber each other; there is no lock file in a browser. |

`dpaint-core` reaches storage only through `vfs::Vfs`, and the only implementation that
touches `std::fs` is `FsVfs`, which this crate never constructs. A test
(`dpaint_core::vfs::tests::no_direct_filesystem_calls_outside_this_module`) scans the crate's
sources and fails the build if anyone reintroduces a direct call.

## Verified

Against a plain `python3 -m http.server` — no engine process anywhere — in a headless
Chromium at `http://localhost:8787`:

- the UI loads against the WASM engine; the server log shows six static `GET`s and zero
  `/api` or `/render.png` requests;
- the command palette lists the full catalog and builds each op's form from the schema the
  WASM engine returns;
- `doc.add`, `raster.layer.add`, `raster.layer.rasterize`, `raster.filter.noise-add` and
  `raster.filter.gaussian-blur` all run and update the viewport, with the baked pixel blobs
  living in the in-memory asset store;
- undo restores the previous pixels and the journal entry is struck through;
- a reload rebuilds `state`, `history` and the rendered PNG byte-for-byte out of OPFS,
  including the undone-entry flag;
- forcing OPFS off falls through to IndexedDB and round-trips; forcing both off shows the
  memory-only notice.
