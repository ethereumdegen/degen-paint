// degen-paint studio.
//
// One document, one journal, one undo stack — shared by the human at this screen and by any
// agent driving the same project through the CLI or MCP. Everything here is a view over
// `Studio::dispatch`; there is no client-side model of the document to drift out of sync.
//
// No bundler, no framework, no npm: this file is served verbatim to a browser tab and embedded
// verbatim in the Tauri webview. The only shell-specific seam is the transport at the top.

// ---------------------------------------------------------------------------- transport

class StudioError extends Error {
  constructor(d) {
    super((d && d.message) || 'request failed');
    this.name = 'StudioError';
    this.code = (d && d.code) || 'error';
    this.candidates = (d && d.candidates) || [];
    this.suggestion = (d && d.suggestion) || null;
  }
}

function asStudioError(e) {
  if (e instanceof StudioError) return e;
  if (e && typeof e === 'object' && (e.code || e.candidates || e.suggestion)) {
    return new StudioError({
      code: e.code, message: e.message || String(e), candidates: e.candidates, suggestion: e.suggestion,
    });
  }
  return new StudioError({ code: 'transport', message: (e && e.message) || String(e) });
}

/** The single door to the engine. Every panel goes through here, so every call is logged. */
async function call(method, params = {}, opts = {}) {
  const t0 = performance.now();
  let result, err;
  try {
    if (typeof window.__DPAINT_INVOKE__ === 'function') {
      // Tauri shell: in-process dispatch, same Studio, same journal.
      const r = await window.__DPAINT_INVOKE__(method, params);
      if (r && typeof r === 'object' && typeof r.ok === 'boolean') {
        if (!r.ok) throw new StudioError(r.error || {});
        result = r.result;
      } else {
        result = r;
      }
    } else {
      const resp = await fetch('/api', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ method, params }),
      });
      const text = await resp.text();
      let body;
      try { body = JSON.parse(text); }
      catch { throw new StudioError({ code: 'bad_response', message: `HTTP ${resp.status}: ${text.slice(0, 200)}` }); }
      if (!body.ok) throw new StudioError(body.error || {});
      result = body.result;
    }
  } catch (e) {
    err = asStudioError(e);
  }
  const ms = Math.round(performance.now() - t0);
  if (err) logCall(method, params, ms, err);
  else if (!opts.quiet) logCall(method, params, ms, null);
  if (err) throw err;
  return result;
}

/** Viewport image source. The Tauri shell hands back a data: URI instead of an HTTP route. */
async function renderUrl(doc, { scale = 1, max = 1600 } = {}) {
  const nonce = Date.now() + '-' + (renderSeq++);
  if (typeof window.__DPAINT_RENDER_URL__ === 'function') {
    return await Promise.resolve(window.__DPAINT_RENDER_URL__({ doc, scale, max, nonce }));
  }
  const q = new URLSearchParams({ scale: String(scale), max: String(max), _: nonce });
  if (doc) q.set('doc', doc);
  return `/render.png?${q.toString()}`;
}

// ------------------------------------------------------------------------------- state

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
};

const S = {
  state: null,
  sig: '',
  catalog: [],
  history: [],
  lint: null,
  activeDoc: null,
  sel: null,           // selected object id within activeDoc
  op: null,            // { id, about, schema, modes, query, network }
  maxSeqSeen: 0,
  logs: [],
  logErrors: 0,
};

let renderSeq = 0;
let renderToken = 0;
let refreshing = null;
let refreshBusy = false;

const view = { zoom: 1, x: 0, y: 0, rw: 0, rh: 0, fitted: false };

const activeDocument = () =>
  (S.state && S.state.documents.find((d) => d.id === S.activeDoc)) || null;

// ------------------------------------------------------------------------------ console

function summarize(v) {
  const s = JSON.stringify(v);
  if (s === undefined) return '';
  return s.length > 160 ? s.slice(0, 157) + '…' : s;
}

function logCall(method, params, ms, err) {
  S.logs.push({ t: new Date(), method, params, ms, err });
  if (S.logs.length > 400) S.logs.splice(0, S.logs.length - 400);
  if (err) S.logErrors++;
  renderConsole();
}

function renderConsole() {
  const pane = $('tab-console');
  const stick = pane.scrollTop + pane.clientHeight >= pane.scrollHeight - 24;
  pane.textContent = '';
  if (!S.logs.length) {
    pane.append(el('div', 'empty-note', 'No calls yet. Every request to the engine lands here.'));
    return;
  }
  for (const l of S.logs) {
    const row = el('div', 'crow' + (l.err ? ' err' : ''));
    row.append(
      el('span', 't', l.t.toTimeString().slice(0, 8)),
      el('span', 'm', (l.err ? '✖ ' : '') + l.method),
      el('span', 'p', summarize(l.params)),
      el('span', 'ms', l.ms + 'ms'),
    );
    pane.append(row);
    if (l.err) {
      const d = el('div', 'cerr');
      d.append(el('span', 'code', l.err.code), document.createTextNode('  '), el('span', 'msg', l.err.message));
      if (l.err.suggestion) {
        d.append(el('br'), el('span', 'lbl', 'suggestion: '), el('span', 'sug', l.err.suggestion));
      }
      if (l.err.candidates && l.err.candidates.length) {
        d.append(el('br'), el('span', 'lbl', `candidates (${l.err.candidates.length}): `));
        for (const c of l.err.candidates) {
          const b = el('span', 'cand', c);
          b.title = 'use this selector in the inspector';
          b.onclick = () => useCandidate(c);
          d.append(b);
        }
      }
      pane.append(d);
    }
  }
  const pill = $('conCount');
  pill.hidden = S.logErrors === 0;
  pill.textContent = String(S.logErrors);
  pill.className = 'pill err';
  if (stick) pane.scrollTop = pane.scrollHeight;
}

/** Point the open form at a selector the engine just named. */
function retarget(sel) {
  const input = document.querySelector('.opform [data-path="target"]');
  if (!input) return;
  input.value = sel;
  delete input.dataset.auto;
}

function useCandidate(sel) {
  retarget(sel);
  const bare = sel.replace(/^[#@]/, '');
  const doc = activeDocument();
  if (doc && doc.objects.some((o) => o.id === bare)) selectObject(bare);
  const input = document.querySelector('.opform [data-path="target"]');
  if (input) input.focus();
}

// ------------------------------------------------------------------------------- panels

function renderDocs() {
  const box = $('docList');
  box.textContent = '';
  if (!S.state) return;
  for (const d of S.state.documents) {
    const row = el('div', 'doc' + (d.id === S.activeDoc ? ' active' : ''));
    row.append(el('span', 'badge ' + d.kind, d.kind.slice(0, 3)));
    const nm = el('span', 'nm', d.name);
    nm.title = `${d.id}${d.dependsOn.length ? '\ndepends on: ' + d.dependsOn.join(', ') : ''}`;
    row.append(nm, el('span', 'sz', d.size ? `${d.size[0]}×${d.size[1]}` : '—'));
    row.onclick = () => setActiveDoc(d.id);
    box.append(row);
  }
}

function renderTree() {
  const box = $('treeList');
  box.textContent = '';
  const doc = activeDocument();
  $('treeCount').textContent = doc ? `${doc.objects.length} objects` : '';
  if (!doc) return;
  if (!doc.objects.length) {
    box.append(el('div', 'empty-note', 'Empty document.'));
    return;
  }
  // Engine order is bottom-up; an editor shows the top of the stack first.
  for (const o of [...doc.objects].reverse()) {
    const row = el('div', 'node' + (o.id === S.sel ? ' sel' : '') + (o.visible ? '' : ' hidden'));
    row.dataset.id = o.id;
    row.style.paddingLeft = 8 + o.depth * 13 + 'px';

    const eye = el('button', 'eye' + (o.visible ? '' : ' off'), o.visible ? '●' : '○');
    const setter = visibilityOp(doc.kind);
    eye.disabled = !setter;
    eye.title = setter ? `toggle visibility (${setter})` : `no visibility op for ${doc.kind} documents`;
    eye.onclick = (ev) => { ev.stopPropagation(); toggleVisible(doc, o); };

    const nm = el('span', 'nm', o.name || o.id);
    nm.title = `${o.id} · ${o.category}`;
    row.append(eye, nm, el('span', 'ty', o.type));
    if (o.opacity < 0.999) row.append(el('span', 'meta', Math.round(o.opacity * 100) + '%'));
    if (o.blend && o.blend !== 'normal') row.append(el('span', 'blend', o.blend));
    row.onclick = () => selectObject(o.id);
    box.append(row);
  }
}

const visibilityOp = (kind) =>
  kind === 'raster' ? 'raster.layer.set' : kind === 'vector' ? 'vector.style.opacity' : null;

async function toggleVisible(doc, o) {
  const op = visibilityOp(doc.kind);
  if (!op) return;
  try {
    await call('op', { op, doc: doc.id, args: { target: '#' + o.id, visible: !o.visible } });
    await refreshAll();
  } catch { /* already surfaced in the console panel */ }
}

function renderHistory() {
  const pane = $('tab-history');
  pane.textContent = '';
  if (!S.history.length) {
    pane.append(el('div', 'empty-note', 'No journal entries yet.'));
    return;
  }
  for (const e of S.history) {
    const row = el('div', 'hrow' + (e.undone ? ' undone' : '') + (e.seq > S.maxSeqSeen ? ' fresh' : ''));
    row.append(
      el('span', 'seq', '#' + e.seq),
      el('span', 'actor ' + e.actor, e.actor),
      el('span', 'op', e.op),
    );
    if (e.undone) row.append(el('span', 'tagx', 'undone'));
    row.append(el('span', 'chg', e.changed.join(', ')), el('span', 'ts', (e.ts || '').replace('T', ' ').replace('Z', '')));
    pane.append(row);
  }
  S.maxSeqSeen = Math.max(S.maxSeqSeen, ...S.history.map((e) => e.seq));
}

function renderLint() {
  const pane = $('tab-lint');
  pane.textContent = '';
  const rep = S.lint;
  const pill = $('lintCount');
  if (!rep) { pill.hidden = true; return; }
  const n = rep.findings.length;
  pill.hidden = n === 0;
  pill.textContent = String(n);
  pill.className = 'pill' + (rep.errors ? ' err' : '');
  if (!n) {
    pane.append(el('div', 'empty-note', 'No findings. Contrast, size and coverage checks all pass.'));
    return;
  }
  for (const f of rep.findings) {
    const row = el('div', 'lrow');
    row.append(el('span', 'sev ' + f.severity, f.severity), el('span', 'rule', f.rule), el('span', 'det', f.detail));
    if (f.value != null) {
      row.append(el('span', 'num', f.required != null ? `${round2(f.value)} / ${round2(f.required)}` : round2(f.value)));
    }
    row.append(el('span', 'tgt', `${f.document} ${f.target}`));
    row.title = `${f.document} ${f.target} — click to select`;
    row.onclick = () => revealFinding(f);
    pane.append(row);
  }
}

const round2 = (v) => (Math.round(v * 100) / 100).toString();

/** Resolve a finding's selector through the engine so one click lands on the offending object. */
async function revealFinding(f) {
  try {
    const matches = await call('select', { doc: f.document, selector: f.target });
    if (!matches.length) return;
    const m = matches[0];
    if (m.document && m.document !== S.activeDoc) await setActiveDoc(m.document);
    selectObject(m.id);
    // Clicking a finding means "fix this one": the open form follows, whatever was typed.
    retarget(f.target);
  } catch { /* surfaced in console */ }
}

function selectObject(id) {
  S.sel = id;
  renderTree();
  const row = $('treeList').querySelector(`.node[data-id="${CSS.escape(id)}"]`);
  if (row) row.scrollIntoView({ block: 'nearest' });
  $('inspectTarget').textContent = id ? '#' + id : '';
  // Follow the selection with the form's `target`, unless the human typed a real query
  // there (`type:text[opacity<0.5]`) rather than an echo of some object's id.
  const doc = activeDocument();
  for (const input of document.querySelectorAll('.opform [data-path="target"]')) {
    const cur = input.value.trim();
    const echo = cur === '' || (cur[0] === '#' && doc && doc.objects.some((o) => '#' + o.id === cur));
    if (input.dataset.auto === '1' || echo) input.value = '#' + id;
  }
}

// ------------------------------------------------------------------------------ refresh

function stateSignature(st) {
  // Covers undo (which flips `undone` without advancing the sequence) as well as new ops.
  return JSON.stringify([st.revision, st.canUndo, st.canRedo, st.project.modified, st.documents, st.palette]);
}

function applyState(st) {
  S.state = st;
  if (!S.activeDoc || !st.documents.some((d) => d.id === S.activeDoc)) {
    S.activeDoc = st.project.active || (st.documents[0] && st.documents[0].id) || null;
  }
  const doc = activeDocument();
  if (S.sel && (!doc || !doc.objects.some((o) => o.id === S.sel))) S.sel = null;
  $('projectName').textContent = st.project.name;
  $('projectName').title = st.project.root;
  $('revBadge').textContent = 'rev ' + st.revision;
  $('btnUndo').disabled = !st.canUndo;
  $('btnRedo').disabled = !st.canRedo;
  $('docRead').textContent = doc ? `${doc.name} · ${doc.kind}` : '—';
  $('inspectTarget').textContent = S.sel ? '#' + S.sel : '';
  renderDocs();
  renderTree();
}

/** Pull everything the panels show. Serialised so a poll cannot interleave with a click. */
function refreshAll(opts = {}) {
  const run = async () => {
    refreshBusy = true;
    try {
      const st = await call('state', {}, { quiet: true });
      S.sig = stateSignature(st);
      applyState(st);
      const [hist, lint] = await Promise.all([
        call('history', { limit: 200 }, { quiet: true }).catch(() => ({ entries: [] })),
        call('lint', {}, { quiet: true }).catch(() => null),
      ]);
      S.history = hist.entries || [];
      S.lint = lint;
      renderHistory();
      renderLint();
      if (!opts.noRender) await refreshRender();
    } finally {
      refreshBusy = false;
    }
  };
  refreshing = (refreshing || Promise.resolve()).then(run, run);
  return refreshing;
}

async function setActiveDoc(id) {
  S.activeDoc = id;
  S.sel = null;
  renderDocs();
  renderTree();
  const doc = activeDocument();
  $('docRead').textContent = doc ? `${doc.name} · ${doc.kind}` : '—';
  view.zoom = 1; view.x = 0; view.y = 0;
  await refreshRender({ fit: true });
}

// ----------------------------------------------------------------------------- viewport

async function refreshRender({ fit = false } = {}) {
  const img = $('canvasImg');
  if (!S.activeDoc) { img.removeAttribute('src'); return; }
  const token = ++renderToken;
  $('busy').hidden = false;
  try {
    const url = await renderUrl(S.activeDoc, { scale: 1, max: 1600 });
    await new Promise((resolve) => {
      const done = (ok) => { if (token === renderToken) { $('canvasEmpty').hidden = ok; } resolve(); };
      img.onload = () => done(true);
      img.onerror = () => done(false);
      img.src = url;
    });
    if (token !== renderToken) return;
    view.rw = img.naturalWidth; view.rh = img.naturalHeight;
    if (fit || !view.fitted) { fitView(); view.fitted = true; } else { layoutView(); }
  } finally {
    if (token === renderToken) $('busy').hidden = true;
  }
}

/** Screen pixels per rendered pixel. `zoom` is document pixels, which is what a user means. */
function screenScale() {
  const doc = activeDocument();
  const dw = (doc && doc.size && doc.size[0]) || view.rw || 1;
  return view.zoom * (dw / (view.rw || 1));
}

function layoutView() {
  const img = $('canvasImg');
  const s = screenScale();
  img.style.width = Math.max(1, Math.round(view.rw * s)) + 'px';
  img.style.height = Math.max(1, Math.round(view.rh * s)) + 'px';
  img.classList.toggle('pixelated', s >= 2);
  $('canvasPan').style.transform = `translate(-50%, -50%) translate(${Math.round(view.x)}px, ${Math.round(view.y)}px)`;
  $('zoomRead').textContent = Math.round(view.zoom * 100) + '%';
  $('sizeRead').textContent = view.rw ? `${view.rw} × ${view.rh} px` : '— × —';
}

function fitView() {
  const area = $('canvasArea').getBoundingClientRect();
  if (!view.rw) return layoutView();
  const s = Math.min((area.width - 40) / view.rw, (area.height - 40) / view.rh);
  const doc = activeDocument();
  const dw = (doc && doc.size && doc.size[0]) || view.rw;
  view.zoom = Math.max(0.02, s * (view.rw / dw));
  view.x = 0; view.y = 0;
  layoutView();
}

function zoomTo(zoom, cx, cy) {
  const s0 = screenScale();
  const z0 = view.zoom;
  view.zoom = Math.min(32, Math.max(0.02, zoom));
  const s1 = screenScale();
  if (cx !== undefined && s0 > 0) {
    // Keep the pixel under the cursor put.
    const u = (cx - view.x) / s0, v = (cy - view.y) / s0;
    view.x = cx - u * s1; view.y = cy - v * s1;
  }
  if (z0 === view.zoom) return;
  layoutView();
}

function initViewport() {
  const area = $('canvasArea');
  area.addEventListener('wheel', (e) => {
    e.preventDefault();
    const r = area.getBoundingClientRect();
    const cx = e.clientX - r.left - r.width / 2;
    const cy = e.clientY - r.top - r.height / 2;
    const k = Math.exp(-e.deltaY * 0.0022);
    zoomTo(view.zoom * k, cx, cy);
  }, { passive: false });

  let drag = null;
  area.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 && e.button !== 1) return;
    drag = { x: e.clientX, y: e.clientY, ox: view.x, oy: view.y };
    area.setPointerCapture(e.pointerId);
    area.classList.add('panning');
  });
  area.addEventListener('pointermove', (e) => {
    if (!drag) return;
    view.x = drag.ox + (e.clientX - drag.x);
    view.y = drag.oy + (e.clientY - drag.y);
    layoutView();
  });
  const end = () => { drag = null; area.classList.remove('panning'); };
  area.addEventListener('pointerup', end);
  area.addEventListener('pointercancel', end);
  window.addEventListener('resize', () => layoutView());

  $('btnFit').onclick = () => fitView();
  $('btnOneToOne').onclick = () => { view.x = 0; view.y = 0; zoomTo(1); layoutView(); };
}

// ------------------------------------------------------------------------------ palette

let pal = { items: [], idx: 0, open: false };

function openPalette() {
  pal.open = true;
  $('paletteWrap').hidden = false;
  const input = $('paletteInput');
  input.value = '';
  filterPalette('');
  input.focus();
  input.select();
}

function closePalette() {
  pal.open = false;
  $('paletteWrap').hidden = true;
}

function filterPalette(q) {
  const needle = q.trim().toLowerCase();
  const doc = activeDocument();
  const kind = doc ? doc.kind : null;
  let items = S.catalog;
  if (needle) {
    const terms = needle.split(/\s+/);
    items = items
      .map((o) => {
        const id = o.id.toLowerCase(), ab = (o.about || '').toLowerCase();
        let score = 0;
        for (const t of terms) {
          const i = id.indexOf(t);
          if (i >= 0) score += 100 - Math.min(i, 40);
          else if (ab.includes(t)) score += 20;
          else return null;
        }
        return { o, score };
      })
      .filter(Boolean)
      .sort((a, b) => b.score - a.score || a.o.id.localeCompare(b.o.id))
      .map((x) => x.o);
  } else if (kind) {
    // With no query, ops that apply to the open document come first.
    items = [...items].sort((a, b) => (b.modes.includes(kind) ? 1 : 0) - (a.modes.includes(kind) ? 1 : 0));
  }
  pal.items = items;
  pal.idx = 0;
  drawPalette(needle);
}

function drawPalette(needle) {
  const list = $('paletteList');
  list.textContent = '';
  if (!pal.items.length) {
    list.append(el('div', 'empty-note', 'No op matches.'));
    return;
  }
  pal.items.slice(0, 120).forEach((o, i) => {
    const row = el('div', 'pitem' + (i === pal.idx ? ' on' : ''));
    row.append(highlight(o.id, needle, 'pid'), el('span', 'pab', o.about || ''));
    const flags = [o.modes.join('/')];
    if (o.query) flags.push('query');
    if (o.network) flags.push('net');
    row.append(el('span', 'pmode', flags.join(' · ')));
    row.onmouseenter = () => { pal.idx = i; [...list.children].forEach((c, j) => c.classList.toggle('on', j === i)); };
    row.onclick = () => chooseOp(o.id);
    list.append(row);
  });
}

function highlight(text, needle, cls) {
  const span = el('span', cls);
  const i = needle ? text.toLowerCase().indexOf(needle.split(/\s+/)[0]) : -1;
  if (i < 0) { span.textContent = text; return span; }
  const n = needle.split(/\s+/)[0].length;
  span.append(document.createTextNode(text.slice(0, i)), el('mark', null, text.slice(i, i + n)), document.createTextNode(text.slice(i + n)));
  return span;
}

function movePalette(d) {
  const max = Math.min(pal.items.length, 120);
  if (!max) return;
  pal.idx = (pal.idx + d + max) % max;
  const list = $('paletteList');
  [...list.children].forEach((c, j) => c.classList.toggle('on', j === pal.idx));
  const on = list.children[pal.idx];
  if (on) on.scrollIntoView({ block: 'nearest' });
}

async function chooseOp(id) {
  closePalette();
  try {
    const meta = S.catalog.find((o) => o.id === id) || {};
    const spec = await call('schema', { op: id });
    S.op = { id: spec.id, about: spec.about, schema: spec.schema, modes: meta.modes || [], query: !!meta.query, network: !!meta.network };
    buildForm();
  } catch { /* surfaced in console */ }
}

function initPalette() {
  $('btnPalette').onclick = openPalette;
  $('paletteBackdrop').onclick = closePalette;
  $('paletteInput').addEventListener('input', (e) => filterPalette(e.target.value));
  $('paletteInput').addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); movePalette(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); movePalette(-1); }
    else if (e.key === 'Enter') { e.preventDefault(); const o = pal.items[pal.idx]; if (o) chooseOp(o.id); }
    else if (e.key === 'Escape') { e.preventDefault(); closePalette(); }
  });
}

// ------------------------------------------------------------------- schema-driven form

const COLOR_PATTERN = /\[0-9a-fA-F\]\{3,4\}/;

/** Follow `$ref` and strip the `| null` that optional Rust fields serialise as. */
function resolve(schema, defs, seen = 0) {
  if (!schema || typeof schema !== 'object' || seen > 8) return { s: schema || {}, nullable: false };
  if (schema.$ref) {
    const name = schema.$ref.split('/').pop();
    const target = defs[name];
    if (target) {
      const r = resolve(target, defs, seen + 1);
      return { s: { ...r.s, description: schema.description || r.s.description }, nullable: r.nullable, def: name };
    }
  }
  const branches = schema.anyOf || schema.oneOf;
  if (Array.isArray(branches)) {
    const live = branches.filter((b) => !(b && b.type === 'null'));
    const nullable = live.length !== branches.length;
    if (live.length === 1) {
      const r = resolve(live[0], defs, seen + 1);
      return { s: { ...r.s, description: schema.description || r.s.description, default: schema.default }, nullable: nullable || r.nullable, def: r.def };
    }
    if (live.length > 1) {
      const resolved = live.map((b) => resolve(b, defs, seen + 1));
      // A string enum split across `enum` and per-variant `const` branches (NewLayerKind):
      // one select, with each variant's doc comment as the option's tooltip.
      if (resolved.every((r) => r.s.type === 'string' && (r.s.enum || r.s.const !== undefined))) {
        const values = [];
        const notes = {};
        for (const r of resolved) {
          if (r.s.enum) values.push(...r.s.enum);
          else {
            values.push(r.s.const);
            if (r.s.description) notes[r.s.const] = r.s.description;
          }
        }
        return { s: { type: 'string', enum: values, enumNotes: notes, description: schema.description }, nullable };
      }
      // Tagged unions (Paint = solid | linear | radial, Adjustment, …) get a variant picker.
      if (resolved.every((r) => discriminator(r.s))) {
        return { s: { type: 'variant', variants: resolved.map((r) => r.s), description: schema.description }, nullable };
      }
      // Anything else: offer the simplest branch, with the JSON escape hatch behind it.
      const simple = resolved.find((r) => r.def === 'Color') || resolved.find((r) => r.s.type === 'string');
      return {
        s: { ...(simple ? simple.s : {}), description: schema.description, union: resolved.map((r) => r.def || r.s.title || r.s.type) },
        nullable, def: simple && simple.def,
      };
    }
  }
  if (Array.isArray(schema.type)) {
    const live = schema.type.filter((t) => t !== 'null');
    return { s: { ...schema, type: live[0] }, nullable: live.length !== schema.type.length };
  }
  return { s: schema, nullable: false };
}

/** Name of the `const`-tagged property that picks a union branch, if there is one. */
function discriminator(s) {
  if (!s || s.type !== 'object' || !s.properties) return null;
  for (const [k, v] of Object.entries(s.properties)) if (v && v.const !== undefined) return k;
  return null;
}

function widgetKind(s, def) {
  if (s.type === 'variant') return 'variant';
  if (s.enum) return 'enum';
  if (def === 'Color' || (typeof s.pattern === 'string' && COLOR_PATTERN.test(s.pattern))) return 'color';
  if (s.type === 'boolean') return 'bool';
  if (s.type === 'integer' || s.type === 'number') return 'number';
  if (s.type === 'array') return 'json';
  if (s.type === 'object' && s.properties) return 'object';
  if (s.type === 'object' || !s.type) return 'json';
  return 'text';
}

function typeLabel(s, def, nullable) {
  const base = def
    || (s.type === 'variant' ? s.variants.map((v) => v.properties[discriminator(v)].const).join(' | ')
      : s.enum ? 'enum'
      : s.type === 'array' ? `${(s.items && s.items.type) || 'any'}[]`
      : s.type)
    || 'json';
  return base + (nullable ? '?' : '');
}

function buildForm() {
  const box = $('inspector');
  box.textContent = '';
  const op = S.op;
  if (!op) return;

  const head = el('div', 'op-head');
  head.append(el('div', 'op-id', op.id), el('div', 'op-about', op.about || ''));
  const flags = el('div', 'op-flags');
  flags.append(el('span', 'flag', (op.modes || []).join(' · ') || 'any'));
  if (op.query) flags.append(el('span', 'flag query', 'read-only'));
  if (op.network) flags.append(el('span', 'flag net', 'network'));
  head.append(flags);
  box.append(head);

  const form = el('form', 'opform');
  const defs = op.schema.$defs || {};
  const props = op.schema.properties || {};
  const required = new Set(op.schema.required || []);
  const order = Object.keys(props).sort((a, b) => (required.has(b) ? 1 : 0) - (required.has(a) ? 1 : 0));
  for (const name of order) form.append(buildField(name, props[name], defs, required.has(name), name));

  const actions = el('div', 'form-actions');
  const run = el('button', 'tb accent', 'Run op');
  run.type = 'submit';
  const dryWrap = el('label', 'dry');
  const dry = el('input');
  dry.type = 'checkbox';
  dry.id = 'dryRun';
  dryWrap.append(dry, el('span', null, 'dry run'));
  const clear = el('button', 'tb sm', 'Close');
  clear.type = 'button';
  clear.onclick = () => { S.op = null; box.textContent = ''; box.append(hintNode()); };
  actions.append(run, dryWrap, el('span', 'grow'), clear);
  form.append(actions);

  form.onsubmit = (e) => { e.preventDefault(); submitOp(form, dry.checked); };
  box.append(form);
  const first = form.querySelector('input, select, textarea');
  if (first) first.focus();
}

function hintNode() {
  const d = el('div', 'hint');
  d.innerHTML = 'Pick an op with <kbd>/</kbd> or <kbd>Ctrl-K</kbd> to build its form from the engine schema.';
  return d;
}

function buildField(name, raw, defs, isRequired, path, depth = 0) {
  const { s, nullable, def } = resolve(raw, defs);
  const kind = widgetKind(s, def);

  if (kind === 'variant' && depth < 3) {
    const box = el('div', 'nested');
    box.dataset.group = path;
    const head = el('div', 'nested-head');
    const pick = el('select');
    if (!isRequired) pick.append(new Option('—', ''));
    for (const v of s.variants) pick.append(new Option(v.properties[discriminator(v)].const));
    if (!isRequired) pick.value = '';
    head.append(el('span', null, name), el('span', 'ftype', typeLabel(s, def, nullable)));
    box.append(head);
    if (s.description) box.append(el('p', 'desc', s.description));
    box.append(pick);
    const body = el('div');
    box.append(body);
    const draw = () => {
      body.textContent = '';
      const v = s.variants.find((x) => x.properties[discriminator(x)].const === pick.value);
      if (!v) return;
      const tag = discriminator(v);
      const hidden = el('input');
      hidden.type = 'hidden';
      hidden.dataset.path = `${path}.${tag}`;
      hidden.dataset.disc = '1';
      hidden.dataset.kind = 'text';
      hidden.value = pick.value;
      body.append(hidden);
      const req = new Set(v.required || []);
      for (const [k, sub] of Object.entries(v.properties)) {
        if (k === tag) continue;
        body.append(buildField(k, sub, defs, req.has(k), `${path}.${k}`, depth + 1));
      }
    };
    pick.onchange = draw;
    draw();
    return box;
  }

  if (kind === 'object' && depth < 2) {
    const box = el('div', 'nested');
    box.dataset.group = path;
    const head = el('div', 'nested-head');
    head.append(el('span', null, name), el('span', 'ftype', typeLabel(s, def, nullable)));
    box.append(head);
    if (s.description) box.append(el('p', 'desc', s.description));
    const req = new Set(s.required || []);
    for (const [k, v] of Object.entries(s.properties)) {
      box.append(buildField(k, v, defs, req.has(k), `${path}.${k}`, depth + 1));
    }
    return box;
  }

  const field = el('div', 'field');
  const label = el('label');
  label.append(el('span', 'fname', name));
  if (isRequired) label.append(el('span', 'req', '*'));
  label.append(el('span', 'ftype', typeLabel(s, def, nullable)));
  field.append(label);
  if (s.description) field.append(el('p', 'desc', s.description));

  let input;
  if (kind === 'enum') {
    input = el('select');
    if (!isRequired) input.append(new Option('—', ''));
    for (const v of s.enum) {
      const o = new Option(String(v), String(v));
      if (s.enumNotes && s.enumNotes[v]) o.title = s.enumNotes[v];
      input.append(o);
    }
    if (isRequired && s.default != null) input.value = String(s.default);
  } else if (kind === 'bool') {
    if (isRequired) {
      input = el('input');
      input.type = 'checkbox';
      if (s.default === true) input.checked = true;
    } else {
      input = el('select');
      input.append(new Option('—', ''), new Option('true', 'true'), new Option('false', 'false'));
    }
  } else if (kind === 'number') {
    input = el('input');
    input.type = 'number';
    input.step = s.type === 'integer' ? '1' : 'any';
    if (s.minimum != null) input.min = String(s.minimum);
    if (s.maximum != null) input.max = String(s.maximum);
    if (s.default != null) input.placeholder = String(s.default);
  } else if (kind === 'json') {
    input = el('textarea');
    input.placeholder = s.union ? `JSON — one of ${s.union.join(' | ')}` : 'JSON value';
    input.spellcheck = false;
  } else {
    input = el('input');
    input.type = 'text';
    input.spellcheck = false;
    if (s.default != null && s.default !== '') input.placeholder = String(s.default);
    if (kind === 'color') input.placeholder = '#rrggbb';
  }
  input.dataset.path = path;
  input.dataset.kind = kind;
  if (isRequired) input.dataset.required = '1';

  if (kind === 'color') {
    const row = el('div', 'row');
    const swatch = el('input');
    swatch.type = 'color';
    swatch.value = '#f4a261';
    swatch.oninput = () => { input.value = swatch.value; };
    row.append(input, swatch);
    field.append(row);
  } else {
    field.append(input);
  }

  // Prefill selector fields from the tree selection, until the human types something else.
  if ((path === 'target' || path === 'parent') && input.tagName === 'INPUT' && S.sel && path === 'target') {
    input.value = '#' + S.sel;
    input.dataset.auto = '1';
  }
  input.addEventListener('input', () => { delete input.dataset.auto; });
  return field;
}

function setPath(obj, path, value) {
  const parts = path.split('.');
  let cur = obj;
  for (const p of parts.slice(0, -1)) cur = cur[p] || (cur[p] = {});
  cur[parts[parts.length - 1]] = value;
}

/** A nested group the human never touched must not be sent: a half-built `{type:"solid"}`
 *  would fail schema validation for no reason. */
function dormant(form) {
  const out = new Set();
  for (const g of form.querySelectorAll('[data-group]')) {
    const live = [...g.querySelectorAll('[data-path]:not([data-disc])')]
      .some((i) => (i.type === 'checkbox' ? i.checked : i.value.trim() !== ''));
    if (!live) out.add(g);
  }
  return out;
}

/** Empty means "not supplied", so the engine's own defaults stay in charge. */
function collect(form) {
  const args = {};
  const dead = dormant(form);
  const buried = (input) => {
    let n = input.parentElement && input.parentElement.closest('[data-group]');
    while (n) {
      if (dead.has(n)) return true;
      n = n.parentElement && n.parentElement.closest('[data-group]');
    }
    return false;
  };
  for (const input of form.querySelectorAll('[data-path]')) {
    if (buried(input)) continue;
    const { path, kind } = input.dataset;
    let v;
    if (kind === 'bool' && input.type === 'checkbox') v = input.checked;
    else {
      const raw = input.value.trim();
      if (raw === '') continue;
      if (kind === 'bool') v = raw === 'true';
      else if (kind === 'number') { v = Number(raw); if (Number.isNaN(v)) throw new StudioError({ code: 'invalid', message: `${path}: '${raw}' is not a number` }); }
      else if (kind === 'json') { try { v = JSON.parse(raw); } catch (e) { throw new StudioError({ code: 'invalid', message: `${path}: ${e.message}` }); } }
      else if (raw[0] === '{' || raw[0] === '[') { try { v = JSON.parse(raw); } catch { v = raw; } }
      else v = raw;
    }
    setPath(args, path, v);
  }
  return args;
}

async function submitOp(form, dryRun) {
  const box = $('inspector');
  for (const old of box.querySelectorAll('.result')) old.remove();
  const show = (cls, text) => { const r = el('div', 'result ' + cls, text); box.append(r); r.scrollIntoView({ block: 'nearest' }); };

  let args;
  try { args = collect(form); }
  catch (e) { const err = asStudioError(e); logCall('op(form)', { op: S.op.id }, 0, err); show('err', `${err.code}: ${err.message}`); return; }

  const params = { op: S.op.id, args, doc: S.activeDoc };
  if (dryRun) params.dryRun = true;
  try {
    const r = await call('op', params);
    const bits = [];
    if (r.seq != null) bits.push(`seq #${r.seq}`);
    if (r.changed && r.changed.length) bits.push(`changed: ${r.changed.join(', ')}`);
    if (r.created && r.created.length) bits.push(`created: ${r.created.join(', ')}`);
    if (r.removed && r.removed.length) bits.push(`removed: ${r.removed.join(', ')}`);
    if (r.warnings && r.warnings.length) bits.push(`warnings: ${r.warnings.join('; ')}`);
    if (r.data !== undefined && r.data !== null) {
      const d = JSON.stringify(r.data, null, 1);
      bits.push('data: ' + (d.length > 2000 ? d.slice(0, 2000) + '\n…' : d));
    }
    if (dryRun) {
      show('dry', `dry run — nothing written\n${r.op}\n${bits.join('\n') || 'no effect reported'}`);
    } else {
      show('ok', `${r.op}\n${bits.join('\n') || 'applied'}`);
      if (r.created && r.created.length) S.sel = r.created[0];
      await refreshAll();
    }
  } catch (e) {
    const err = asStudioError(e);
    const extra = [
      err.suggestion ? `suggestion: ${err.suggestion}` : null,
      err.candidates.length ? `candidates: ${err.candidates.join(', ')}` : null,
    ].filter(Boolean).join('\n');
    show('err', `${err.code}: ${err.message}${extra ? '\n' + extra : ''}`);
  }
}

// ------------------------------------------------------------------------------ actions

async function doUndo() {
  try {
    const r = await call('undo');
    await refreshAll();
    flash(r.op ? `undid ${r.op}` : 'nothing to undo', 'human');
  } catch { /* surfaced in console */ }
}

async function doRedo() {
  try {
    const r = await call('redo');
    await refreshAll();
    flash(r.op ? `redid ${r.op}` : 'nothing to redo', 'human');
  } catch { /* surfaced in console */ }
}

let flashTimer = null;
function flash(text, cls) {
  const f = $('agentFlash');
  f.textContent = text;
  f.className = 'flash ' + (cls || '');
  f.hidden = false;
  clearTimeout(flashTimer);
  flashTimer = setTimeout(() => { f.hidden = true; }, 3200);
}

function showTab(name) {
  for (const t of document.querySelectorAll('.tab')) t.classList.toggle('active', t.dataset.tab === name);
  for (const p of document.querySelectorAll('.tabpane')) p.classList.toggle('active', p.id === 'tab-' + name);
  // A hidden pane cannot be scrolled, so the newest line is only reachable once it is shown.
  if (name === 'console') { const p = $('tab-console'); p.scrollTop = p.scrollHeight; }
}

// -------------------------------------------------------------------------- live sync

/** The shared-journal claim, made visible: an agent's write shows up here on its own. */
async function poll() {
  if (refreshBusy) return;   // a refresh in flight will pick the change up anyway
  try {
    const st = await call('state', {}, { quiet: true });
    const sig = stateSignature(st);
    if (sig === S.sig) return;
    const before = S.state ? S.state.revision : 0;
    // Say so before the refresh, not after: a re-render plus a project lint takes long
    // enough that a silent second would look like the UI had missed the write.
    const peek = await call('history', { limit: 1 }, { quiet: true }).catch(() => ({ entries: [] }));
    const top = peek.entries[0];
    const actor = top && top.actor === 'human' ? 'human' : 'agent';
    const what = st.revision > before ? (top ? top.op : 'change') : top && top.undone ? 'undo' : 'redo';
    flash(`updated by ${actor} · ${what}`, actor);
    await refreshAll();
  } catch { /* transport hiccup; the next tick retries */ }
}

// ------------------------------------------------------------------------------- keys

function initKeys() {
  window.addEventListener('keydown', (e) => {
    const t = e.target;
    const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT');
    const mod = e.metaKey || e.ctrlKey;

    if (mod && e.key.toLowerCase() === 'z') {
      e.preventDefault();
      if (e.shiftKey) doRedo(); else doUndo();
      return;
    }
    if (mod && e.key.toLowerCase() === 'k') { e.preventDefault(); pal.open ? closePalette() : openPalette(); return; }
    if (e.key === 'Escape' && pal.open) { e.preventDefault(); closePalette(); return; }
    if (typing || pal.open) return;

    if (e.key === '/') { e.preventDefault(); openPalette(); }
    else if (e.key === 'f') { e.preventDefault(); fitView(); }
    else if (e.key === '1') { e.preventDefault(); view.x = 0; view.y = 0; zoomTo(1); layoutView(); }
  });
}

// -------------------------------------------------------------------------------- boot

async function boot() {
  initViewport();
  initPalette();
  initKeys();
  $('btnUndo').onclick = doUndo;
  $('btnRedo').onclick = doRedo;
  $('btnClearConsole').onclick = () => { S.logs = []; S.logErrors = 0; renderConsole(); };
  $('btnLint').onclick = async () => {
    showTab('lint');
    try { S.lint = await call('lint', { doc: S.activeDoc }); renderLint(); } catch { /* logged */ }
  };
  for (const t of document.querySelectorAll('.tab')) t.onclick = () => showTab(t.dataset.tab);

  renderConsole();
  try {
    S.catalog = await call('catalog', {}, { quiet: true });
  } catch { S.catalog = []; }

  await refreshAll({ noRender: true });
  await refreshRender({ fit: true });
  S.maxSeqSeen = Math.max(0, ...S.history.map((e) => e.seq));
  renderHistory();

  setInterval(poll, 1000);

  // The Tauri shell drives native menu items through these; the poll is the backstop.
  window.__DPAINT_ON_CHANGE__ = () => { refreshAll().then(() => flash('updated', 'human')); };
  window.addEventListener('dpaint:changed', () => window.__DPAINT_ON_CHANGE__());
  const tauriEvent = window.__TAURI__ && window.__TAURI__.event;
  if (tauriEvent && typeof tauriEvent.listen === 'function') {
    tauriEvent.listen('dpaint:changed', () => window.__DPAINT_ON_CHANGE__());
  }
}

boot();
