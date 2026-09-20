// Boot shell for the WASM build.
//
// Three jobs, in order:
//   1. instantiate the engine and give it whatever the browser managed to persist last time;
//   2. publish `window.__DPAINT_INVOKE__` / `window.__DPAINT_RENDER_URL__`, which is the only
//      seam the studio UI has — with them defined it never touches `fetch`, so there is no
//      server in this picture at all;
//   3. inject the studio's own markup and load its own `studio.js`, both byte-identical
//      copies of `crates/dpaint-studio/ui/`. Nothing in this file edits the UI.
//
// Persistence is the page's job, not the engine's: the engine hands out `project.json`,
// `history.jsonl` and the asset blobs, and this file writes them to OPFS, or IndexedDB, or
// nowhere — saying which, visibly, rather than pretending.

import init, { DpaintEngine } from './pkg/dpaint_wasm.js';

const boot = document.getElementById('dpBoot');
const bootWhy = document.getElementById('dpBootWhy');
const storageBadge = document.getElementById('dpStorage');

const say = (msg) => { if (bootWhy) bootWhy.textContent = msg; };

function fail(stage, err) {
  console.error(`[dpaint] ${stage}:`, err);
  if (boot) {
    boot.classList.add('failed');
    boot.hidden = false;
    say(`${stage} failed\n\n${(err && (err.stack || err.message)) || String(err)}`);
  }
}

// ------------------------------------------------------------------- storage backends
//
// One tiny interface — get / put / remove / keys over byte arrays — and three
// implementations. The engine never sees any of it.

const PREFIX = 'dpaint/';

async function opfsStore() {
  if (!navigator.storage || typeof navigator.storage.getDirectory !== 'function') {
    throw new Error('navigator.storage.getDirectory is unavailable');
  }
  const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('dpaint', { create: true });
  if (typeof FileSystemFileHandle === 'undefined'
      || typeof FileSystemFileHandle.prototype.createWritable !== 'function') {
    throw new Error('FileSystemFileHandle.createWritable is unavailable');
  }
  // Prove it end to end before claiming it works: some engines expose the handles and then
  // refuse the write.
  const probe = await dir.getFileHandle('.probe', { create: true });
  const w = await probe.createWritable();
  await w.write(new Uint8Array([1]));
  await w.close();
  await dir.removeEntry('.probe');

  return {
    kind: 'OPFS',
    async keys() {
      const out = [];
      for await (const name of dir.keys()) out.push(name);
      return out;
    },
    async get(name) {
      try {
        const f = await (await dir.getFileHandle(name)).getFile();
        return new Uint8Array(await f.arrayBuffer());
      } catch { return null; }
    },
    async put(name, bytes) {
      const h = await dir.getFileHandle(name, { create: true });
      const w = await h.createWritable();
      await w.write(bytes);
      await w.close();
    },
    async remove(name) {
      try { await dir.removeEntry(name); } catch { /* already gone */ }
    },
  };
}

function idbStore() {
  if (!self.indexedDB) return Promise.reject(new Error('indexedDB is unavailable'));
  return new Promise((resolve, reject) => {
    const req = indexedDB.open('dpaint', 1);
    req.onupgradeneeded = () => req.result.createObjectStore('files');
    req.onerror = () => reject(req.error || new Error('indexedDB.open failed'));
    req.onsuccess = () => {
      const db = req.result;
      const run = (mode, fn) => new Promise((res, rej) => {
        const tx = db.transaction('files', mode);
        const r = fn(tx.objectStore('files'));
        r.onsuccess = () => res(r.result);
        r.onerror = () => rej(r.error);
      });
      resolve({
        kind: 'IndexedDB',
        keys: () => run('readonly', (s) => s.getAllKeys()).then((k) => k.map(String)),
        get: (name) => run('readonly', (s) => s.get(PREFIX + name))
          .then((v) => (v ? new Uint8Array(v) : null)),
        put: (name, bytes) => run('readwrite', (s) => s.put(bytes.buffer ?? bytes, PREFIX + name)).then(() => {}),
        remove: (name) => run('readwrite', (s) => s.delete(PREFIX + name)).then(() => {}),
      });
    };
  }).then((store) => ({
    ...store,
    // Keys come back prefixed; strip so callers see the same names OPFS reports.
    keys: () => store.keys().then((k) => k.filter((x) => x.startsWith(PREFIX)).map((x) => x.slice(PREFIX.length))),
  }));
}

function memoryStore() {
  const m = new Map();
  return {
    kind: 'memory',
    keys: async () => [...m.keys()],
    get: async (name) => m.get(name) ?? null,
    put: async (name, bytes) => { m.set(name, bytes); },
    remove: async (name) => { m.delete(name); },
  };
}

async function openStorage() {
  try { return await opfsStore(); }
  catch (e) { console.warn('[dpaint] OPFS unavailable, trying IndexedDB:', e.message); }
  try { return await idbStore(); }
  catch (e) { console.warn('[dpaint] IndexedDB unavailable, running memory-only:', e.message); }
  return memoryStore();
}

function showStorage(store) {
  if (!storageBadge) return;
  storageBadge.hidden = false;
  if (store.kind === 'memory') {
    storageBadge.classList.add('warn');
    storageBadge.textContent = 'memory only — this project is NOT saved and will be lost on reload';
  } else {
    storageBadge.textContent = `persisting to ${store.kind}`;
  }
}

// ----------------------------------------------------------------------- persistence

const PROJECT = 'project.json';
const HISTORY = 'history.jsonl';
const assetKey = (ref) => `asset-${ref.replace(/^blake3:/, '')}`;
const assetExt = (key) => key.slice(key.lastIndexOf('.') + 1);

const enc = new TextEncoder();
const dec = new TextDecoder();

/** Write out everything that changed. Blobs are content-addressed, so a name that already
 *  exists holds the right bytes and is skipped. */
async function persist(engine, store, known) {
  await store.put(PROJECT, enc.encode(engine.project_json()));
  await store.put(HISTORY, enc.encode(engine.history_jsonl()));
  const live = new Set();
  for (const ref of engine.asset_refs()) {
    const key = assetKey(ref);
    live.add(key);
    if (!known.has(key)) {
      await store.put(key, engine.asset_bytes(ref));
      known.add(key);
    }
  }
  // A blob dropped by `asset.gc` should not linger in storage forever.
  for (const key of [...known]) {
    if (!live.has(key)) {
      await store.remove(key);
      known.delete(key);
    }
  }
}

/** Rebuild an engine out of storage, or start a fresh project when there is nothing saved. */
async function restore(store, known) {
  const saved = await store.get(PROJECT);
  if (!saved) return { engine: new DpaintEngine(), restored: false };

  const engine = DpaintEngine.load(dec.decode(saved));
  for (const key of await store.keys()) {
    if (!key.startsWith('asset-')) continue;
    const bytes = await store.get(key);
    if (bytes) {
      // Content addressing makes this exact: the same bytes yield the same ref.
      engine.put_asset(bytes, assetExt(key));
      known.add(key);
    }
  }
  const history = await store.get(HISTORY);
  if (history && history.length) engine.restore_history(dec.decode(history));
  return { engine, restored: true };
}

// ------------------------------------------------------------------------- the bridge

/** Methods that change the document, and therefore need a flush. */
const MUTATES = new Set(['op', 'undo', 'redo']);

function publishBridge(engine, store, known) {
  let queue = Promise.resolve();
  const flush = () => {
    queue = queue.then(() => persist(engine, store, known)).catch((e) => {
      console.error('[dpaint] persist failed:', e);
    });
    return queue;
  };

  // Errors are already `{code, message, candidates?, suggestion?}` — the engine builds the
  // same object the HTTP bridge puts in its `error` field — so they are rethrown untouched.
  window.__DPAINT_INVOKE__ = async (method, params) => {
    const result = engine.dispatch(method, params ?? {});
    if (MUTATES.has(method)) await flush();
    return result;
  };

  window.__DPAINT_RENDER_URL__ = async ({ doc, scale = 1, max = 1600 } = {}) =>
    engine.render_png(doc ?? undefined, scale, max);

  // Importing an image from disk: the page owns the file picker, the engine owns the bytes.
  window.__DPAINT_PUT_ASSET__ = async (bytes, ext) => {
    const ref = engine.put_asset(new Uint8Array(bytes), ext);
    await flush();
    return ref;
  };

  window.__DPAINT_FLUSH__ = flush;
  return flush;
}

// ------------------------------------------------------------------------------- the UI

/** Load the studio's markup and script verbatim from `./ui/`, which is a straight copy of
 *  `crates/dpaint-studio/ui/`. Anything else would be a second frontend to keep in sync. */
async function mountStudio() {
  const html = await fetch('./ui/index.html').then((r) => {
    if (!r.ok) throw new Error(`GET ui/index.html → HTTP ${r.status}`);
    return r.text();
  });
  const parsed = new DOMParser().parseFromString(html, 'text/html');
  for (const script of parsed.body.querySelectorAll('script')) script.remove();
  // Insert before the boot overlay so the overlay keeps covering the UI until it is ready.
  document.body.insertAdjacentHTML('afterbegin', parsed.body.innerHTML);
  await import('./ui/studio.js');
}

// ---------------------------------------------------------------------------- sequence

async function main() {
  say('compiling the engine into this tab…');
  await init();

  say('opening local storage…');
  const store = await openStorage();
  const known = new Set();

  say('restoring your project…');
  const { engine, restored } = await restore(store, known);

  const flush = publishBridge(engine, store, known);
  if (!restored) await flush();
  showStorage(store);

  say('starting the studio…');
  await mountStudio();

  boot.hidden = true;
  console.info(
    `[dpaint] wasm engine ready · storage=${store.kind} · ${restored ? 'restored' : 'new'} project`,
  );
  // A hook the verification harness (and a curious user) can read without guessing.
  window.__DPAINT_WASM__ = { engine, store: store.kind, restored };
  window.dispatchEvent(new CustomEvent('dpaint:wasm-ready', { detail: { storage: store.kind, restored } }));
}

main().catch((e) => fail('boot', e));
