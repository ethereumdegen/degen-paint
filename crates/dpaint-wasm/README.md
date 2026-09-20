# dpaint-wasm

The degen-paint engine compiled into a browser tab: no server, no install, the same op
registry and the same renderer the CLI uses.

```
  browser tab
  ├── boot.js            OPFS / IndexedDB persistence, defines the globals below
  ├── ui/                verbatim copy of crates/dpaint-studio/ui/
  └── pkg/               dpaint_wasm.js + dpaint_wasm_bg.wasm
                         └── dpaint-core · raster · vector · model3d · render · inspect
                             over an in-memory Vfs, plus dpaint-gpu on WebGPU
```

The UI is *unmodified*. It probes for globals and falls back when they are absent, which is
how the identical bytes run in Tauri, against `dpaint serve`, and here:

```js
window.__DPAINT_INVOKE__       = async (method, params) => result
window.__DPAINT_RENDER_URL__   = async ({ doc, scale, max }) => "data:image/png;base64,…"
window.__DPAINT_GPU_VIEWPORT__ = async (canvas) => viewport | null   // optional
```

The first two are the engine bridge; without them the UI talks HTTP. The third is the
interactive viewport, and without it — or when the browser has no WebGPU — the UI keeps its
`<img>` and says `CPU` in the status bar.

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
(`opt-level = 3`, thin LTO). Both columns are from the same tree, the only difference being
whether `dpaint-gpu` and `wgpu`'s WebGPU backend are compiled in:

| File | without the GPU viewport | with it | delta |
|---|---|---|---|
| `dpaint_wasm_bg.wasm`, `wasm-opt -Oz`, raw | 8,236,871 | 8,406,138 | +169,267 (+2.1%) |
| `dpaint_wasm_bg.wasm`, `wasm-opt -Oz`, gzip | 2,852,929 | 2,921,803 | +68,874 (+2.4%) |
| `dpaint_wasm_bg.wasm`, no `wasm-opt`, raw | 9,710,439 | 10,025,647 | +315,208 |
| `dpaint_wasm.js`, raw | 20,445 | 81,041 | +60,596 |
| `dpaint_wasm.js`, gzip | 4,784 | 16,111 | +11,327 |

So WebGPU costs **about 80 KB gzipped end to end, +2.8% of the bundle** — small because the
`webgpu` backend is a thin binding over the browser's own implementation. None of wgpu's
native machinery (the Metal/Vulkan/DX12 HALs, `wgpu-core`'s validator, naga's non-WGSL
frontends) compiles for `wasm32`. Most of the visible growth is in the *JS glue*, which
quadruples: wgpu's web bindings are a lot of small `web-sys` imports, and they gzip well.

`wasm-opt -Oz` buys 16% raw and 8% gzipped — worth having, not transformative. 515 KB of
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

### The GPU viewport

`DpaintViewport` is the interactive path, and only that. `render_png` and everything the
engine calls a render still go through `dpaint_render` on the CPU, so what the tab shows
and what `dpaint render` writes come out of one implementation.

```js
import init, { DpaintEngine, DpaintViewport } from './pkg/dpaint_wasm.js';

const vp = await DpaintViewport.create();   // null when the browser has no adapter
if (vp) {
  vp.attach(canvas);                        // configures a WebGPU surface on this <canvas>
  vp.set_document(engine, 'doc_hero');      // → [w, h] texels; [0, 0] for a model
  vp.mode();                                // "canvas" | "model" | "empty"
  vp.set_view(zoom, panX, panY, pixelated); // device pixels; free, no engine call
  vp.orbit(dYawDeg, dPitchDeg);             // model documents
  vp.resize(w, h);                          // device pixels, after sizing the canvas
  vp.frame();                               // one frame; drive it from requestAnimationFrame
  vp.info();                                // { backend, name, deviceType, samples }
}
```

`set_document` is the only call that re-enters the engine: a raster or vector document is
rasterized once into a pixmap and uploaded as a texture, a model document is turned into
meshes once. After that, pan, zoom and orbit are uniform writes — dragging never
re-rasterizes and never encodes a PNG.

`boot.js` publishes this as `window.__DPAINT_GPU_VIEWPORT__(canvas)`, the seam `studio.js`
probes. The handle it hands over carries no engine, so the shared UI does not learn that
this shell happens to have one in the same address space; the HTTP server and the Tauri
shell simply never publish the seam, and the studio keeps its `<img>`. When the seam exists
but `navigator.gpu` does not, or `create()` resolves to `null`, the status bar says which —
`CPU · no WebGPU in this browser`, `CPU · no WebGPU adapter` — rather than failing.

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
| Threaded or GPU *ops* | The GPU draws the viewport; it does not run ops. `render.image`, `render.turntable`, digests and diffs stay on the CPU renderer, here as everywhere, because that is what makes their output reproducible. |
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

The GPU viewport, same harness, screenshots under `/tmp/gpu-web-shots/`:

- headless Chromium **does** expose WebGPU here (`navigator.gpu` present, an adapter is
  granted, `backend=webgpu`, 4× MSAA), so the GPU path is what runs by default;
- a 1024² raster document draws through `CanvasRenderer`; a drag of 20 pointer moves plus
  15 wheel ticks changes zoom 0.56 → 1.61 and pan 0 → (−287, 173) with the engine-call
  counter frozen at its pre-drag value and `render_png` never called once;
- a model document with a torus and a box draws through `SceneRenderer`; a drag orbits the
  camera 35°/20° → 317°/−2°, again with zero engine calls, and shift-drag pans while the
  wheel dollies;
- `f` and `1` keep working in both modes; a resize of the pane re-configures the swap chain
  through a `ResizeObserver`;
- an op and an undo re-upload exactly once each and the viewport follows;
- with `navigator.gpu` forced absent the probe falls back: the status bar reads
  `CPU · no WebGPU in this browser`, the `<canvas>` stays hidden, the `<img>` carries a
  `data:` URI again, and pan, zoom, undo and OPFS restore all behave as before;
- the HTTP shell (`dpaint serve`), which never publishes the seam, is untouched: status
  `CPU`, `/render.png?…`, and pan/zoom still a CSS transform on `#canvasPan`.
