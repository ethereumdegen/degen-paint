// degen-paint studio.
//
// One document, one journal, one undo stack — shared by the human at this screen, by any
// agent driving the same project through the CLI or MCP, and by a UI-driving agent that can
// only read the accessibility tree. Everything here is a view over `Studio::dispatch`; there
// is no client-side model of the document to drift out of sync.
//
// The accessibility contract is `docs/starkbot.md` §2–§3: the navigator's candidates are
// `a[href] button input textarea select summary [contenteditable]` plus fourteen roles, so
// every capability lives on one of those, named after the thing it acts on. Lists are
// listboxes of options, never custom divs; the canvas is decoration and its information is
// mirrored as text in the Status region.
//
// No bundler, no framework, no npm: this file is served verbatim to a browser tab and
// embedded verbatim in the Tauri webview. The only shell-specific seam is the transport.

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
  // A capability probe that comes back "no such job" has done its job; it is not a fault
  // and must not colour the console panel red.
  if (err && opts.probe) note(`probe ${method}: ${err.message}`);
  else if (err) logCall(method, params, ms, err);
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

// ------------------------------------------------------------------------- capabilities
//
// The Studio's backend grows method by method. A method that is not there yet answers
// `unknown studio method '…'`; the control it would drive is hidden rather than left to
// throw, and the console panel says which one and why. Probing uses read-only methods
// only — `project.close` has no required argument, so calling it to see if it exists
// would close the project.

const caps = {
  // Until the probes answer, the markup's own visibility stands: hiding a control and
  // putting it back a moment later would change the candidate table under a navigator
  // mid-step.
  detected: false,
  overview: false,
  jobs: false,
  quote: false,
  providers: false,
  shortcuts: false,
  projects: false,   // project.new / project.open / project.close / project.recent
  io: false,         // io.import / io.export / io.sendToEditor / io.exportPreview
};

const MISSING = /unknown studio method/i;

/** True when the method answered at all — an argument complaint still proves it is wired. */
async function probe(method, params = {}) {
  try {
    await call(method, params, { quiet: true, probe: true });
    return true;
  } catch (e) {
    if (MISSING.test(e.message)) return false;
    return true;
  }
}

async function detectCapabilities() {
  const [overview, jobs, quote, providers, shortcuts, projects] = await Promise.all([
    probe('overview'),
    probe('job.status', { id: '__probe__' }),
    probe('quote', { op: 'raster.layer.add' }),
    probe('providers.status'),
    probe('shortcuts'),
    probe('project.recent'),
  ]);
  caps.overview = overview;
  caps.jobs = jobs;
  caps.quote = quote;
  caps.providers = providers;
  caps.shortcuts = shortcuts;
  // One agent owns the whole projects-and-files surface, and `project.recent` is the only
  // member of it that is safe to call blind.
  caps.projects = projects;
  caps.io = projects;
  caps.detected = true;
  const off = Object.entries(caps).filter(([k, v]) => k !== 'detected' && !v).map(([k]) => k);
  if (off.length) note(`backend does not answer yet: ${off.join(', ')} — those controls are hidden`);
  updateChrome();
}

// ------------------------------------------------------------------------------- state

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
};

const NOHINTS = new URLSearchParams(location.search).has('nohints');

const S = {
  state: null,
  sig: '',
  catalog: [],
  history: [],
  lint: null,
  digest: null,        // per-object bounds for the active document, id -> NodeDigest
  activeDoc: null,
  sel: null,           // selected object id within activeDoc
  op: null,            // { id, about, schema, modes, query, network }
  maxSeqSeen: 0,
  logs: [],
  logErrors: 0,
  treeFilter: '',
  treeShown: 60,
  collapsed: new Set(),
  lintShown: 40,
  job: null,           // { id, op } while one is in flight
  recent: [],
  shortcuts: [],
};

const TREE_CAP = 60;

let renderSeq = 0;
let renderToken = 0;
let refreshing = null;
let refreshBusy = false;

const view = { zoom: 1, x: 0, y: 0, rw: 0, rh: 0, fitted: false };

// The GPU viewport, when the shell offers one and the browser can honour it. `on === false`
// is the ordinary case — an HTTP or Tauri shell, or a browser without WebGPU — and then
// every path below is exactly the image-and-CSS-transform viewport this file always had.
const gpu = { on: false, vp: null, mode: 'empty', dpr: 1, pending: 0, ro: null, status: 'CPU' };

const activeDocument = () =>
  (S.state && S.state.documents.find((d) => d.id === S.activeDoc)) || null;

const hasProject = () => !!(S.state && S.state.project);

/** What one row of the tree is called, in the language of the document kind. */
const NOUN = { raster: 'layer', vector: 'object', model: 'node' };
const LIST_LABEL = { raster: 'Layers', vector: 'Objects', model: 'Nodes' };
const noun = (kind) => NOUN[kind] || 'object';

/** The ops each document kind uses for the per-object actions in the Selection region. */
const ACTIONS = {
  raster: {
    set: 'raster.layer.set',
    remove: 'raster.layer.remove',
    duplicate: 'raster.layer.duplicate',
    reorder: 'raster.layer.reorder',
    rename: 'raster.layer.rename',
    transform: 'raster.layer.transform',
  },
  vector: {
    set: 'vector.style.opacity',
    remove: 'vector.object.remove',
    duplicate: 'vector.object.duplicate',
    reorder: 'vector.object.reorder',
    rename: 'vector.object.rename',
    translate: 'vector.transform.translate',
    scale: 'vector.transform.scale',
    rotate: 'vector.transform.rotate',
  },
  model: {
    remove: 'model.node.remove',
    rename: 'model.node.rename',
    trs: 'model.node.set-trs',
  },
};

// The one table the `?` overlay and `dpaint skill` both read. The backend's `shortcuts`
// method supersedes it when it answers; this copy keeps the overlay honest on an older
// bridge and is what `?nohints` hides.
const SHORTCUTS = [
  { id: 'palette', label: 'Focus the command palette', keys: '/ or Ctrl-K', scope: 'global' },
  { id: 'edit.undo', label: 'Undo', keys: 'Ctrl-Z', scope: 'global' },
  { id: 'edit.redo', label: 'Redo', keys: 'Shift-Ctrl-Z', scope: 'global' },
  { id: 'view.fit', label: 'Fit document in viewport', keys: 'F', scope: 'viewport' },
  { id: 'view.oneToOne', label: 'Zoom to 100 percent', keys: '1', scope: 'viewport' },
  { id: 'project.new', label: 'New project', keys: 'Ctrl-N', scope: 'global' },
  { id: 'project.open', label: 'Open project', keys: 'Ctrl-O', scope: 'global' },
  { id: 'io.import', label: 'Import file', keys: 'Ctrl-I', scope: 'global' },
  { id: 'io.export', label: 'Export document', keys: 'Ctrl-E', scope: 'global' },
  { id: 'help.shortcuts', label: 'Keyboard shortcuts', keys: '?', scope: 'global' },
  { id: 'dialog.close', label: 'Close the palette or a dialog', keys: 'Esc', scope: 'dialog' },
];

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

/** A line in the console panel that did not come from a call. */
function note(text) {
  S.logs.push({ t: new Date(), method: 'studio', params: text, ms: 0, err: null });
  renderConsole();
}

function renderConsole() {
  const pane = $('tab-console');
  if (!pane) return;
  const stick = pane.scrollTop + pane.clientHeight >= pane.scrollHeight - 24;
  pane.textContent = '';
  if (!S.logs.length) {
    pane.append(el('p', 'empty-note', 'No calls yet. Every request to the engine lands here.'));
    return;
  }
  for (const l of S.logs) {
    const row = el('div', 'crow' + (l.err ? ' err' : ''));
    row.append(
      el('span', 't', l.t.toTimeString().slice(0, 8)),
      el('span', 'm', (l.err ? '✖ ' : '') + l.method),
      el('span', 'p', typeof l.params === 'string' ? l.params : summarize(l.params)),
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
        d.append(el('br'), el('span', 'lbl', `candidates (${l.err.candidates.length}): `),
          el('span', 'cand', l.err.candidates.join(', ')));
      }
      pane.append(d);
    }
  }
  const pill = $('conCount');
  pill.hidden = S.logErrors === 0;
  // A hidden child still contributes to the accessible name, so an empty badge must be
  // empty, not a zero.
  pill.textContent = pill.hidden ? '' : String(S.logErrors);
  pill.className = 'pill err';
  if (stick) pane.scrollTop = pane.scrollHeight;
}

// ------------------------------------------------------------------------------- status
//
// §3.6 and the last line of §3.3: this region is what a Jev `verify` reads. Every op,
// every error and every job outcome becomes a sentence here naming the op and the
// revision, so a step that worked is always visible progress.

function say(text) {
  $('statusLine').textContent = text;
}

function statusSummary() {
  const doc = activeDocument();
  if (!hasProject()) return 'no project open';
  if (!doc) return `project ${S.state.project.name} · no documents`;
  const bits = [doc.kind];
  if (doc.size) bits.push(`${doc.size[0]}×${doc.size[1]}`);
  bits.push(`${doc.objects.length} ${noun(doc.kind)}${doc.objects.length === 1 ? '' : 's'}`);
  const rep = S.lint;
  if (rep) {
    if (rep.errors) bits.push(`${rep.errors} lint error${rep.errors === 1 ? '' : 's'}`);
    else if (rep.warnings) bits.push(`${rep.warnings} lint warning${rep.warnings === 1 ? '' : 's'}`);
    else bits.push('lint clean');
  }
  return `${doc.name} · ${bits.join(' · ')}`;
}

/** The viewport is aria-hidden, so everything it shows is said here instead. */
function statusView() {
  const bits = [`zoom ${Math.round(view.zoom * 100)}%`];
  if (view.rw) bits.push(`render ${view.rw}×${view.rh}`);
  if (view.x || view.y) bits.push(`pan ${Math.round(view.x)},${Math.round(view.y)}`);
  bits.push(`renderer ${gpu.status}`);
  return `viewport: ${bits.join(' · ')}`;
}

function renderStatus() {
  $('statusSummary').textContent = statusSummary();
  $('statusView').textContent = statusView();
  $('viewRead').textContent = `${Math.round(view.zoom * 100)}% · ${view.rw ? `${view.rw}×${view.rh}` : '—'} · ${gpu.status}`;
}

function setBusy(job) {
  S.job = job;
  const pane = $('statusPane');
  const bar = $('jobBar');
  if (job) {
    pane.setAttribute('aria-busy', 'true');
    bar.hidden = false;
    $('jobProgress').setAttribute('aria-label', job.label || `Running ${job.op}`);
    $('btnCancelJob').setAttribute('aria-label', `Cancel job ${job.op}`);
    $('btnCancelJob').hidden = !job.id;
  } else {
    pane.removeAttribute('aria-busy');
    bar.hidden = true;
  }
}

// ------------------------------------------------------------------------------ list ui
//
// Two candidates in one container must never share a name, or the navigator cannot tell
// them apart (§6). Names are built from the document's own words, so a collision is
// possible; it is resolved by the id, which is unique by construction.

function uniqueLabels(rows) {
  const seen = new Map();
  for (const r of rows) seen.set(r.label, (seen.get(r.label) || 0) + 1);
  for (const r of rows) if (seen.get(r.label) > 1) r.label = `${r.label} · #${r.id}`;
  return rows;
}

function option(label, { selected = false, level = null, expanded = null, id = null } = {}) {
  const li = el('li', 'opt');
  li.setAttribute('role', 'option');
  li.setAttribute('aria-label', label);
  li.setAttribute('aria-selected', selected ? 'true' : 'false');
  if (level != null) li.setAttribute('aria-level', String(level));
  if (expanded != null) li.setAttribute('aria-expanded', expanded ? 'true' : 'false');
  if (id != null) li.dataset.id = id;
  li.tabIndex = -1;
  return li;
}

// ---------------------------------------------------------------------------- documents

function renderDocs() {
  const box = $('docList');
  box.textContent = '';
  if (!S.state) return;
  $('docsCount').textContent = S.state.documents.length ? `${S.state.documents.length}` : '';
  const rows = S.state.documents.map((d) => ({
    id: d.id,
    d,
    label: [d.name, d.kind, d.size ? `${d.size[0]}×${d.size[1]}` : 'unsized',
      d.id === S.activeDoc ? 'active' : null].filter(Boolean).join(' · '),
  }));
  uniqueLabels(rows);
  for (const r of rows) {
    const li = option(r.label, { selected: r.d.id === S.activeDoc, id: r.d.id });
    li.classList.toggle('active', r.d.id === S.activeDoc);
    li.append(el('span', 'badge ' + r.d.kind, r.d.kind.slice(0, 3)));
    const nm = el('span', 'nm', r.d.name);
    nm.setAttribute('aria-hidden', 'true');
    li.append(nm);
    const sz = el('span', 'sz', r.d.size ? `${r.d.size[0]}×${r.d.size[1]}` : '—');
    sz.setAttribute('aria-hidden', 'true');
    li.append(sz);
    li.onclick = () => setActiveDoc(r.d.id);
    box.append(li);
  }
}

// --------------------------------------------------------------------------------- tree

/** Rows a collapsed group hides: every deeper row until the depth comes back up. */
function visibleObjects(doc) {
  const out = [];
  let hideBelow = null;
  for (const o of doc.objects) {
    if (hideBelow !== null) {
      if (o.depth > hideBelow) continue;
      hideBelow = null;
    }
    out.push(o);
    if (o.type === 'group' && S.collapsed.has(o.id)) hideBelow = o.depth;
  }
  return out;
}

function rowLabel(o, kind) {
  const bits = [o.name || o.id, o.type, o.visible ? 'visible' : 'hidden'];
  if (o.opacity < 0.999) bits.push(`${Math.round(o.opacity * 100)}% opacity`);
  if (o.blend && o.blend !== 'normal') bits.push(o.blend);
  const n = S.digest && S.digest[o.id];
  if (n && n.bbox) {
    const [x, y, w, h] = n.bbox.map((v) => Math.round(v));
    bits.push(`${x},${y} ${w}×${h}`);
  }
  if (kind === 'model' && n && n.text) bits.push(n.text);
  return bits.join(' · ');
}

function renderTree() {
  const box = $('treeList');
  box.textContent = '';
  const doc = activeDocument();
  const kind = doc ? doc.kind : 'raster';
  $('treeTitle').textContent = LIST_LABEL[kind] || 'Tree';
  box.setAttribute('aria-label', LIST_LABEL[kind] || 'Tree');
  const filterBox = $('treeFilter');
  filterBox.setAttribute('aria-label', `Filter ${(LIST_LABEL[kind] || 'rows').toLowerCase()}`);
  $('treeMore').hidden = true;
  if (!doc) {
    $('treeCount').textContent = '';
    box.append(el('li', 'empty-note', 'No document.'));
    return;
  }

  // Engine order is bottom-up; an editor shows the top of the stack first.
  let rows = visibleObjects(doc).slice().reverse();
  const q = S.treeFilter.trim().toLowerCase();
  if (q) rows = rows.filter((o) => (o.name || '').toLowerCase().includes(q) || o.id.toLowerCase().includes(q) || o.type.includes(q));
  const total = rows.length;
  $('treeCount').textContent = q ? `${total} of ${doc.objects.length}` : `${total}`;
  if (!total) {
    box.append(el('li', 'empty-note', q ? 'No row matches the filter.' : 'Empty document.'));
    return;
  }

  const shown = rows.slice(0, S.treeShown);
  const labelled = uniqueLabels(shown.map((o) => ({ id: o.id, o, label: rowLabel(o, kind) })));
  for (const r of labelled) {
    const o = r.o;
    const li = option(r.label, {
      selected: o.id === S.sel,
      level: o.depth + 1,
      expanded: o.type === 'group' ? !S.collapsed.has(o.id) : null,
      id: o.id,
    });
    li.classList.toggle('sel', o.id === S.sel);
    li.classList.toggle('off', !o.visible);
    li.style.paddingLeft = 8 + o.depth * 13 + 'px';
    const body = el('span', 'row-body');
    body.setAttribute('aria-hidden', 'true');
    body.append(
      el('span', 'nm', o.name || o.id),
      el('span', 'ty', o.type),
    );
    if (!o.visible) body.append(el('span', 'meta', 'hidden'));
    li.append(body);
    li.onclick = () => selectObject(o.id);
    box.append(li);
  }

  if (total > shown.length) {
    const more = $('treeMore');
    const rest = total - shown.length;
    more.hidden = false;
    more.textContent = `Show ${rest} more`;
    more.setAttribute('aria-label', `Show ${rest} more ${(LIST_LABEL[kind] || 'rows').toLowerCase()}`);
  }
}

// ---------------------------------------------------------------------------- selection
//
// §3.2: one candidate per row, so everything that acts on an object lives here instead,
// named with the object it acts on.

function selectedObject() {
  const doc = activeDocument();
  if (!doc || !S.sel) return null;
  return doc.objects.find((o) => o.id === S.sel) || null;
}

function selectObject(id) {
  S.sel = id;
  renderTree();
  renderSelection();
  const row = $('treeList').querySelector(`.opt[data-id="${CSS.escape(id)}"]`);
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

function actionButton(label, fn) {
  const b = el('button', 'tb sm', label);
  b.type = 'button';
  b.setAttribute('aria-label', label);
  b.onclick = fn;
  return b;
}

function renderSelection() {
  const text = $('selText');
  const acts = $('selActions');
  const geo = $('selGeometry');
  acts.textContent = '';
  geo.textContent = '';
  const doc = activeDocument();
  const o = selectedObject();
  if (!doc || !o) {
    text.textContent = doc
      ? `No selection. Choose a row in the ${LIST_LABEL[doc.kind] || 'Tree'} list.`
      : 'No selection.';
    return;
  }

  const n = noun(doc.kind);
  const who = `${n} ${o.name || o.id}`;
  const d = S.digest && S.digest[o.id];
  const bits = [`Selected: #${o.id}`, `${o.type} ${n}`, `doc ${doc.name}`];
  if (d && d.bbox) {
    const [x, y, w, h] = d.bbox.map((v) => Math.round(v));
    bits.push(`bbox ${x},${y} ${w}×${h}`);
  }
  bits.push(o.visible ? 'visible' : 'hidden');
  if (o.opacity < 0.999) bits.push(`${Math.round(o.opacity * 100)}% opacity`);
  text.textContent = bits.join(' · ');

  const map = ACTIONS[doc.kind] || {};
  const target = '#' + o.id;

  if (map.set) {
    acts.append(actionButton(o.visible ? `Hide ${who}` : `Show ${who}`,
      () => runOp(map.set, { target, visible: !o.visible })));
    acts.append(actionButton(`Lock ${who}`, () => runOp(map.set, { target, locked: true })));
    acts.append(actionButton(`Unlock ${who}`, () => runOp(map.set, { target, locked: false })));
  }
  if (map.reorder) {
    acts.append(actionButton(`Move ${who} up`, () => runOp(map.reorder, { target, to: 'forward' })));
    acts.append(actionButton(`Move ${who} down`, () => runOp(map.reorder, { target, to: 'backward' })));
  }
  if (map.duplicate) {
    acts.append(actionButton(`Duplicate ${who}`, () => runOp(map.duplicate, { target })));
  }
  if (map.rename) {
    acts.append(actionButton(`Rename ${who}`, () => renameDialog(doc, o, map.rename)));
  }
  if (map.remove) {
    const b = actionButton(`Delete ${who}`, () => deleteDialog(doc, o, map.remove));
    b.classList.add('danger');
    acts.append(b);
  }
  if (o.type === 'group') {
    const open = !S.collapsed.has(o.id);
    acts.append(actionButton(open ? `Collapse group ${o.name || o.id}` : `Expand group ${o.name || o.id}`, () => {
      if (open) S.collapsed.add(o.id); else S.collapsed.delete(o.id);
      renderTree();
      renderSelection();
    }));
  }

  renderGeometry(geo, doc, o, who);
}

/** A spinbutton for one number, named with the object, committing on change. */
function spin(label, value, { step = 1, min = null, max = null, placeholder = null }, commit) {
  const wrap = el('div', 'geo-field');
  const id = 'geo_' + label.replace(/[^a-z0-9]+/gi, '_').toLowerCase();
  const lab = el('label', null, label.split(' ')[0]);
  lab.htmlFor = id;
  const input = el('input');
  input.type = 'number';
  input.id = id;
  input.step = String(step);
  if (min != null) input.min = String(min);
  if (max != null) input.max = String(max);
  if (placeholder != null) input.placeholder = placeholder;
  if (value != null && Number.isFinite(value)) input.value = String(value);
  input.setAttribute('aria-label', label);
  input.onchange = () => {
    const v = input.value.trim() === '' ? null : Number(input.value);
    if (v === null || Number.isNaN(v)) return;
    commit(v, input);
  };
  input.onkeydown = (e) => { if (e.key === 'Enter') { e.preventDefault(); input.blur(); } };
  wrap.append(lab, input);
  return wrap;
}

function renderGeometry(box, doc, o, who) {
  const map = ACTIONS[doc.kind] || {};
  const target = '#' + o.id;
  const d = S.digest && S.digest[o.id];
  const bbox = d && d.bbox;

  if (doc.kind === 'model') {
    // A node's TRS is absolute and is not carried back by `state` or the digest, so the
    // fields say "Set" and start blank rather than claiming a value they cannot read.
    const at = { x: null, y: null, z: null };
    const push = () => runOp(map.trs, { target, translation: [at.x || 0, at.y || 0, at.z || 0] });
    box.append(
      spin(`Set X of ${who}`, null, { step: 0.1, placeholder: '0' }, (v) => { at.x = v; push(); }),
      spin(`Set Y of ${who}`, null, { step: 0.1, placeholder: '0' }, (v) => { at.y = v; push(); }),
      spin(`Set Z of ${who}`, null, { step: 0.1, placeholder: '0' }, (v) => { at.z = v; push(); }),
    );
    return;
  }

  const [bx, by, bw, bh] = bbox ? bbox.map((v) => Math.round(v * 100) / 100) : [null, null, null, null];
  const origin = bbox ? [bbox[0], bbox[1]] : null;

  const move = (dx, dy) => {
    if (map.transform) return runOp(map.transform, { target, translate: [dx, dy] });
    return runOp(map.translate, { target, dx, dy });
  };
  const resize = (sx, sy) => {
    if (map.transform) return runOp(map.transform, { target, scale: [sx, sy], origin });
    return runOp(map.scale, { target, sx, sy, around: origin });
  };
  const turn = (deg) => {
    if (map.transform) return runOp(map.transform, { target, rotate: deg, origin });
    return runOp(map.rotate, { target, degrees: deg, around: origin });
  };

  const geoReady = bbox && bw > 0 && bh > 0;
  box.append(
    spin(`X of ${who}`, bx, { step: 1, placeholder: '—' }, (v) => { if (geoReady) move(v - bx, 0); }),
    spin(`Y of ${who}`, by, { step: 1, placeholder: '—' }, (v) => { if (geoReady) move(0, v - by); }),
    spin(`W of ${who}`, bw, { step: 1, min: 1, placeholder: '—' }, (v) => { if (geoReady && v > 0) resize(v / bw, 1); }),
    spin(`H of ${who}`, bh, { step: 1, min: 1, placeholder: '—' }, (v) => { if (geoReady && v > 0) resize(1, v / bh); }),
    // Absolute orientation is not readable from the engine, so this one is a relative
    // turn: it applies the degrees typed and returns to zero.
    spin(`Rotation of ${who} in degrees`, 0, { step: 1 }, (v, input) => { if (v) { turn(v); input.value = '0'; } }),
  );
  if (map.set) {
    box.append(spin(`Opacity of ${who} in percent`, Math.round(o.opacity * 100), { step: 1, min: 0, max: 100 },
      (v) => runOp(map.set, { target, opacity: Math.min(100, Math.max(0, v)) / 100 })));
  }
  if (!bbox) {
    const p = el('p', 'geo-note', 'Bounds are unavailable until the digest arrives.');
    box.append(p);
  }
}

// ------------------------------------------------------------------------------ history

function renderHistory() {
  const pane = $('tab-history');
  pane.textContent = '';
  if (!S.history.length) {
    pane.append(el('p', 'empty-note', 'No journal entries yet.'));
    return;
  }
  const list = el('ul', 'plain');
  for (const e of S.history) {
    const row = el('li', 'hrow' + (e.undone ? ' undone' : '') + (e.seq > S.maxSeqSeen ? ' fresh' : ''));
    row.append(
      el('span', 'seq', '#' + e.seq),
      el('span', 'actor ' + e.actor, e.actor),
      el('span', 'op', e.op),
    );
    if (e.undone) row.append(el('span', 'tagx', 'undone'));
    row.append(el('span', 'chg', e.changed.join(', ')), el('span', 'ts', (e.ts || '').replace('T', ' ').replace('Z', '')));
    list.append(row);
  }
  pane.append(list);
  S.maxSeqSeen = Math.max(S.maxSeqSeen, ...S.history.map((e) => e.seq));
}

// --------------------------------------------------------------------------------- lint

const round2 = (v) => (Math.round(v * 100) / 100).toString();

function renderLint() {
  const pane = $('tab-lint');
  pane.textContent = '';
  const rep = S.lint;
  const pill = $('lintCount');
  if (!rep) { pill.hidden = true; pill.textContent = ''; return; }
  const n = rep.findings.length;
  pill.hidden = n === 0;
  pill.textContent = pill.hidden ? '' : String(n);
  pill.className = 'pill' + (rep.errors ? ' err' : '');
  if (!n) {
    pane.append(el('p', 'empty-note', 'No findings. Contrast, size and coverage checks all pass.'));
    return;
  }
  const list = el('ul', 'plain');
  const shown = rep.findings.slice(0, S.lintShown);
  const labels = uniqueLabels(shown.map((f, i) => ({
    id: String(i), f, label: `Select ${f.target} in ${f.document} for ${f.rule}`,
  })));
  for (const r of labels) {
    const f = r.f;
    const row = el('li', 'lrow');
    row.append(el('span', 'sev ' + f.severity, f.severity), el('span', 'rule', f.rule), el('span', 'det', f.detail));
    if (f.value != null) {
      row.append(el('span', 'num', f.required != null ? `${round2(f.value)} / ${round2(f.required)}` : round2(f.value)));
    }
    const b = el('button', 'tb sm', f.target);
    b.type = 'button';
    b.setAttribute('aria-label', r.label);
    b.onclick = () => revealFinding(f);
    row.append(b);
    list.append(row);
  }
  pane.append(list);
  if (rep.findings.length > shown.length) {
    const rest = rep.findings.length - shown.length;
    const more = el('button', 'tb sm', `Show ${rest} more`);
    more.type = 'button';
    more.setAttribute('aria-label', `Show ${rest} more lint findings`);
    more.onclick = () => { S.lintShown += 40; renderLint(); };
    pane.append(more);
  }
}

/** Resolve a finding's selector through the engine so one click lands on the offending object. */
async function revealFinding(f) {
  try {
    const matches = await call('select', { doc: f.document, selector: f.target });
    if (!matches.length) return;
    const m = matches[0];
    if (m.document && m.document !== S.activeDoc) await setActiveDoc(m.document);
    selectObject(m.id);
    say(`selected ${f.target} in ${f.document} for lint rule ${f.rule}`);
  } catch (e) {
    say(`failed to resolve ${f.target} · ${asStudioError(e).message}`);
  }
}

// ------------------------------------------------------------------------------ refresh

function stateSignature(st) {
  // Covers undo (which flips `undone` without advancing the sequence) as well as new ops.
  return JSON.stringify([st.revision, st.canUndo, st.canRedo, st.project && st.project.modified,
    st.documents, st.palette, st.busy || null]);
}

function applyState(st) {
  S.state = st;
  const documents = st.documents || [];
  if (!hasProject()) {
    S.activeDoc = null;
    S.sel = null;
  } else if (!S.activeDoc || !documents.some((d) => d.id === S.activeDoc)) {
    S.activeDoc = st.project.active || (documents[0] && documents[0].id) || null;
  }
  const doc = activeDocument();
  if (S.sel && (!doc || !doc.objects.some((o) => o.id === S.sel))) S.sel = null;
  // An editor always has something selected; a navigator that cannot click a canvas needs
  // the per-object actions to be reachable without a first click.
  if (!S.sel && doc && doc.objects.length) S.sel = doc.objects[doc.objects.length - 1].id;

  $('projectName').textContent = hasProject() ? st.project.name : 'no project';
  $('projectName').title = hasProject() ? st.project.root : '';
  $('revBadge').textContent = 'rev ' + st.revision;
  updateUndoRedo();
  $('inspectTarget').textContent = S.sel ? '#' + S.sel : '';
  updateChrome();
  renderDocs();
  renderTree();
  renderSelection();
  renderStatus();
  showWelcome(!hasProject());
  renderJobsFromState();
}

function updateUndoRedo() {
  const st = S.state;
  const u = $('btnUndo');
  const r = $('btnRedo');
  u.disabled = !(st && st.canUndo);
  r.disabled = !(st && st.canRedo);
  const top = S.history.find((e) => !e.undone);
  const redoTop = [...S.history].reverse().find((e) => e.undone);
  u.setAttribute('aria-label', top && !u.disabled ? `Undo ${top.op}` : 'Undo');
  r.setAttribute('aria-label', redoTop && !r.disabled ? `Redo ${redoTop.op}` : 'Redo');
}

/** Hide what the backend cannot do, and name what it can after the thing it acts on. */
function updateChrome() {
  const doc = activeDocument();
  const project = hasProject() ? S.state.project.name : null;
  const has = (cap) => !caps.detected || caps[cap];
  const show = (id, on) => { const n = $(id); if (n) n.hidden = !on; };

  show('btnNewProject', has('projects'));
  show('btnOpenProject', has('projects'));
  show('btnCloseProject', has('projects') && !!project);
  show('btnImport', has('io') && !!project);
  show('btnExport', has('io') && !!doc);
  show('btnSendToEditor', has('io') && !!doc);
  show('btnExportPreview', has('io') && !!doc);
  show('btnProviders', has('providers'));
  show('btnShortcuts', !NOHINTS);

  if (project) $('btnCloseProject').setAttribute('aria-label', `Close project ${project}`);
  if (doc) {
    $('btnExport').setAttribute('aria-label', `Export document ${doc.name}`);
    $('btnExportPreview').setAttribute('aria-label', `Export preview of ${doc.name}`);
    $('btnImport').setAttribute('aria-label', `Import file into project ${project}`);
    $('btnSendToEditor').setAttribute('aria-label', `Send documents to editor`);
  }
  $('orbitGroup').hidden = !(gpu.on && gpu.mode === 'model');
  if (NOHINTS) for (const n of document.querySelectorAll('.hintbar')) n.hidden = true;
}

/** Jobs another shell started show up through `state.busy`. */
function renderJobsFromState() {
  if (S.job) return;
  const busy = (S.state && S.state.busy) || [];
  if (busy.length) setBusy({ id: busy[0].id, op: busy[0].op, label: busy[0].label || `Running ${busy[0].op}` });
  else setBusy(null);
}

/** Pull everything the panels show. Serialised so a poll cannot interleave with a click. */
function refreshAll(opts = {}) {
  const run = async () => {
    refreshBusy = true;
    try {
      const st = await call('state', {}, { quiet: true });
      S.sig = stateSignature(st);
      applyState(st);
      const doc = S.activeDoc;
      const [hist, lint, digest] = await Promise.all([
        hasProject() ? call('history', { limit: 200 }, { quiet: true }).catch(() => ({ entries: [] })) : Promise.resolve({ entries: [] }),
        hasProject() ? call('lint', {}, { quiet: true }).catch(() => null) : Promise.resolve(null),
        doc ? call('digest', { doc }, { quiet: true }).catch(() => null) : Promise.resolve(null),
      ]);
      S.history = hist.entries || [];
      S.lint = lint;
      S.digest = digest && digest.tree
        ? Object.fromEntries(digest.tree.map((n) => [n.id, n]))
        : null;
      updateUndoRedo();
      renderHistory();
      renderLint();
      renderTree();
      renderSelection();
      renderStatus();
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
  S.digest = null;
  S.treeShown = TREE_CAP;
  renderDocs();
  renderTree();
  renderSelection();
  view.zoom = 1; view.x = 0; view.y = 0;
  const doc = activeDocument();
  say(`active document is ${doc ? doc.name : id}`);
  renderStatus();
  await refreshRender({ fit: true });
  await refreshAll({ noRender: true });
}

// ------------------------------------------------------------------------- GPU viewport
//
// A shell that can draw on the GPU publishes `window.__DPAINT_GPU_VIEWPORT__(canvas)`,
// which resolves to a viewport handle or to `null` when the browser has no adapter. The
// handle is deliberately engine-free — the shell closes over whatever engine it owns — so
// this file stays the same file for all three shells.

function gpuSay(text) {
  gpu.status = text;
  renderStatus();
}

async function initGpu() {
  const provider = window.__DPAINT_GPU_VIEWPORT__;
  if (typeof provider !== 'function') { gpuSay('CPU'); return; }
  if (!navigator.gpu) { gpuSay('CPU · no WebGPU in this browser'); return; }
  let vp = null;
  try {
    vp = await provider($('canvasGl'));
  } catch (e) {
    note(`GPU viewport failed to start: ${(e && e.message) || e}`);
    gpuSay('CPU · WebGPU failed to start');
    return;
  }
  if (!vp) { gpuSay('CPU · no WebGPU adapter'); return; }
  gpu.vp = vp;
  gpu.on = true;
  const info = vp.info();
  gpuSay(`GPU · ${info.backend}`);
  // The shader draws its own checkerboard, so the CSS one and the image it framed go away.
  $('canvasPan').hidden = true;
  $('canvasGl').hidden = false;
  gpuResize();
  if (typeof ResizeObserver === 'function') {
    gpu.ro = new ResizeObserver(() => { gpuResize(); layoutView(); });
    gpu.ro.observe($('canvasArea'));
  }
}

/** Keep the canvas backing store matched to the area, in device pixels. */
function gpuResize() {
  if (!gpu.on) return;
  const c = $('canvasGl');
  const r = $('canvasArea').getBoundingClientRect();
  gpu.dpr = Math.min(window.devicePixelRatio || 1, 2);
  const w = Math.max(1, Math.round(r.width * gpu.dpr));
  const h = Math.max(1, Math.round(r.height * gpu.dpr));
  if (c.width === w && c.height === h) return;
  c.width = w;
  c.height = h;
  gpu.vp.resize(w, h);
}

/** One frame per animation frame, however many times the view changed in between. */
function gpuDraw() {
  if (!gpu.on || gpu.pending) return;
  gpu.pending = requestAnimationFrame(() => {
    gpu.pending = 0;
    try { gpu.vp.frame(); } catch (e) { note(`GPU frame failed: ${(e && e.message) || e}`); }
  });
}

/** Hand the current view to the GPU. No engine call: this is why a drag is free. */
function gpuPush() {
  const s = screenScale();
  // A model's zoom is a camera distance, which device pixel ratio has no business scaling;
  // a canvas's is device pixels per texel, which it does.
  const zoom = gpu.mode === 'model' ? view.zoom : s * gpu.dpr;
  gpu.vp.setView(zoom, view.x * gpu.dpr, view.y * gpu.dpr, s >= 2);
  gpuDraw();
}

// ----------------------------------------------------------------------------- viewport

async function refreshRender({ fit = false } = {}) {
  if (gpu.on) return refreshRenderGpu({ fit });
  const img = $('canvasImg');
  if (!S.activeDoc) { img.removeAttribute('src'); view.rw = 0; view.rh = 0; renderStatus(); return; }
  const token = ++renderToken;
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
    if (token === renderToken) renderStatus();
  }
}

/** The GPU equivalent: the engine rasterizes or builds meshes once, here, and never again
 *  until the document changes. Panning and orbiting do not come through this function. */
async function refreshRenderGpu({ fit = false } = {}) {
  const token = ++renderToken;
  if (!S.activeDoc) {
    gpu.vp.clearDocument();
    gpu.mode = 'empty';
    view.rw = 0; view.rh = 0;
    $('canvasEmpty').hidden = false;
    gpuDraw();
    return;
  }
  try {
    const size = await Promise.resolve(gpu.vp.setDocument(S.activeDoc));
    if (token !== renderToken) return;
    gpu.mode = gpu.vp.mode();
    view.rw = size[0] | 0;
    view.rh = size[1] | 0;
    $('canvasEmpty').hidden = true;
    gpuResize();
    if (fit || !view.fitted) { fitView(); view.fitted = true; } else { layoutView(); }
  } catch (e) {
    if (token !== renderToken) return;
    note(`GPU document upload failed: ${(e && e.message) || e}`);
    gpu.mode = 'empty';
    $('canvasEmpty').hidden = false;
    gpuDraw();
  } finally {
    if (token === renderToken) { updateChrome(); renderStatus(); }
  }
}

/** Screen pixels per rendered pixel. `zoom` is document pixels, which is what a user means. */
function screenScale() {
  const doc = activeDocument();
  const dw = (doc && doc.size && doc.size[0]) || view.rw || 1;
  return view.zoom * (dw / (view.rw || 1));
}

function layoutView() {
  if (gpu.on) {
    gpuPush();
    renderStatus();
    return;
  }
  const img = $('canvasImg');
  const s = screenScale();
  img.style.width = Math.max(1, Math.round(view.rw * s)) + 'px';
  img.style.height = Math.max(1, Math.round(view.rh * s)) + 'px';
  img.classList.toggle('pixelated', s >= 2);
  $('canvasPan').style.transform = `translate(-50%, -50%) translate(${Math.round(view.x)}px, ${Math.round(view.y)}px)`;
  renderStatus();
}

function fitView() {
  // A model has no pixel size to fit; `zoom = 1` is the framing the CPU turntable uses.
  if (gpu.on && gpu.mode === 'model') {
    view.zoom = 1; view.x = 0; view.y = 0;
    return layoutView();
  }
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
  // Anchoring to the cursor is a 2D idea; on a model, zoom is a camera dolly and shifting
  // the target with it just makes the subject slide off screen.
  const anchored = !(gpu.on && gpu.mode === 'model');
  if (cx !== undefined && s0 > 0 && anchored) {
    // Keep the pixel under the cursor put.
    const u = (cx - view.x) / s0, v = (cy - view.y) / s0;
    view.x = cx - u * s1; view.y = cy - v * s1;
  }
  if (z0 === view.zoom) return;
  layoutView();
}

function orbit(dx, dy) {
  if (!(gpu.on && gpu.mode === 'model')) return;
  gpu.vp.orbit(dx, dy);
  gpuDraw();
  say(`orbited the viewport by ${dx},${dy} degrees`);
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
    // On a GPU-drawn model, the left button orbits and shift (or the middle button) pans.
    const orbiting = gpu.on && gpu.mode === 'model' && e.button === 0 && !e.shiftKey;
    drag = { x: e.clientX, y: e.clientY, ox: view.x, oy: view.y, orbit: orbiting };
    area.setPointerCapture(e.pointerId);
    area.classList.add(orbiting ? 'orbiting' : 'panning');
  });
  area.addEventListener('pointermove', (e) => {
    if (!drag) return;
    if (drag.orbit) {
      // Relative, so a clamped pitch does not accumulate a hidden debt.
      gpu.vp.orbit((e.clientX - drag.x) * 0.4, -(e.clientY - drag.y) * 0.4);
      drag.x = e.clientX; drag.y = e.clientY;
      gpuDraw();
      return;
    }
    view.x = drag.ox + (e.clientX - drag.x);
    view.y = drag.oy + (e.clientY - drag.y);
    layoutView();
  });
  const end = () => { drag = null; area.classList.remove('panning', 'orbiting'); };
  area.addEventListener('pointerup', end);
  area.addEventListener('pointercancel', end);
  window.addEventListener('resize', () => { gpuResize(); layoutView(); });

  $('btnFit').onclick = () => { fitView(); say(`fitted ${docName()} in the viewport · ${statusView()}`); };
  $('btnOneToOne').onclick = () => { view.x = 0; view.y = 0; zoomTo(1); layoutView(); say(`zoomed ${docName()} to 100% · ${statusView()}`); };
  $('btnZoomIn').onclick = () => { zoomTo(view.zoom * 1.25); say(`zoomed in on ${docName()} · ${statusView()}`); };
  $('btnZoomOut').onclick = () => { zoomTo(view.zoom / 1.25); say(`zoomed out on ${docName()} · ${statusView()}`); };
  $('btnResetView').onclick = () => { view.x = 0; view.y = 0; fitView(); say(`reset the view of ${docName()} · ${statusView()}`); };
  $('btnOrbitLeft').onclick = () => orbit(-15, 0);
  $('btnOrbitRight').onclick = () => orbit(15, 0);
  $('btnOrbitUp').onclick = () => orbit(0, 15);
  $('btnOrbitDown').onclick = () => orbit(0, -15);
}

const docName = () => { const d = activeDocument(); return d ? d.name : 'the document'; };

// ------------------------------------------------------------------------------ palette
//
// §3.3: a real combobox, always present, so the navigator can `fill` it without knowing
// how to open anything first. Options appear on input under `aria-controls`, capped at 12
// so search results never eat the candidate budget.

const PALETTE_MAX = 12;
let pal = { items: [], idx: 0, open: false };

function paletteOpen(on) {
  pal.open = on;
  $('paletteList').hidden = !on;
  $('paletteInput').setAttribute('aria-expanded', on ? 'true' : 'false');
  if (!on) $('paletteInput').removeAttribute('aria-activedescendant');
}

function focusPalette() {
  const input = $('paletteInput');
  input.focus();
  input.select();
  filterPalette(input.value);
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
        if (kind && o.modes.includes(kind)) score += 15;
        return { o, score };
      })
      .filter(Boolean)
      .sort((a, b) => b.score - a.score || a.o.id.localeCompare(b.o.id))
      .map((x) => x.o);
  } else if (kind) {
    // With no query, ops that apply to the open document come first.
    items = [...items].sort((a, b) => (b.modes.includes(kind) ? 1 : 0) - (a.modes.includes(kind) ? 1 : 0));
  }
  pal.items = items.slice(0, PALETTE_MAX);
  pal.idx = 0;
  drawPalette();
  paletteOpen(pal.items.length > 0);
}

function drawPalette() {
  const list = $('paletteList');
  list.textContent = '';
  pal.items.forEach((o, i) => {
    const label = `${o.id} — ${o.about || 'no description'}`;
    const li = option(label, { selected: i === pal.idx, id: o.id });
    li.id = 'palopt_' + i;
    li.classList.toggle('on', i === pal.idx);
    const body = el('span', 'row-body');
    body.setAttribute('aria-hidden', 'true');
    body.append(el('span', 'pid', o.id), el('span', 'pab', o.about || ''));
    const flags = [o.modes.join('/')];
    if (o.query) flags.push('query');
    if (o.network) flags.push('net');
    body.append(el('span', 'pmode', flags.join(' · ')));
    li.append(body);
    li.onmousedown = (e) => { e.preventDefault(); chooseOp(o.id); };
    list.append(li);
  });
  if (pal.items[pal.idx]) $('paletteInput').setAttribute('aria-activedescendant', 'palopt_' + pal.idx);
}

function movePalette(d) {
  const max = pal.items.length;
  if (!max) return;
  pal.idx = (pal.idx + d + max) % max;
  drawPalette();
  const on = $('paletteList').children[pal.idx];
  if (on) on.scrollIntoView({ block: 'nearest' });
}

async function chooseOp(id) {
  paletteOpen(false);
  $('paletteInput').value = '';
  try {
    const meta = S.catalog.find((o) => o.id === id) || {};
    const spec = await call('schema', { op: id });
    S.op = { id: spec.id, about: spec.about, schema: spec.schema, modes: meta.modes || [], query: !!meta.query, network: !!meta.network };
    buildForm();
    say(`opened the form for ${id} in the Inspector`);
  } catch (e) {
    say(`could not open ${id} · ${asStudioError(e).message}`);
  }
}

function initPalette() {
  const input = $('paletteInput');
  input.addEventListener('input', (e) => filterPalette(e.target.value));
  input.addEventListener('focus', () => filterPalette(input.value));
  input.addEventListener('blur', () => setTimeout(() => paletteOpen(false), 120));
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); if (!pal.open) filterPalette(input.value); else movePalette(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); movePalette(-1); }
    else if (e.key === 'Enter') { e.preventDefault(); const o = pal.items[pal.idx]; if (o) chooseOp(o.id); }
    else if (e.key === 'Escape') { e.preventDefault(); paletteOpen(false); input.blur(); }
  });
}

// ------------------------------------------------------------------- schema-driven form

const COLOR_PATTERN = /\[0-9a-fA-F\]\{3,4\}/;
const SELECTOR_FIELDS = new Set(['target', 'source', 'parent', 'layer', 'object', 'node', 'from', 'to_layer']);

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

function widgetKind(s, def, path) {
  if (s.type === 'variant') return 'variant';
  if (s.enum) return 'enum';
  if (def === 'Color' || (typeof s.pattern === 'string' && COLOR_PATTERN.test(s.pattern))) return 'color';
  if (s.type === 'boolean') return 'bool';
  if (s.type === 'integer' || s.type === 'number') return 'number';
  if (s.type === 'string' && SELECTOR_FIELDS.has(path)) return 'selector';
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

let fieldSeq = 0;

function buildForm() {
  const box = $('inspector');
  box.textContent = '';
  const op = S.op;
  if (!op) { box.append(hintNode()); return; }

  const head = el('div', 'op-head');
  const title = el('div', 'op-id', op.id);
  title.id = 'opFormTitle';
  head.append(title, el('div', 'op-about', op.about || ''));
  const flags = el('div', 'op-flags');
  flags.append(el('span', 'flag', (op.modes || []).join(' · ') || 'any'));
  if (op.query) flags.append(el('span', 'flag query', 'read-only'));
  if (op.network) flags.append(el('span', 'flag net', 'network'));
  head.append(flags);
  box.append(head);

  const form = el('form', 'opform');
  form.setAttribute('aria-labelledby', 'opFormTitle');
  const defs = op.schema.$defs || {};
  const props = op.schema.properties || {};
  const required = new Set(op.schema.required || []);
  const order = Object.keys(props).sort((a, b) => (required.has(b) ? 1 : 0) - (required.has(a) ? 1 : 0));
  for (const name of order) form.append(buildField(name, props[name], defs, required.has(name), name));

  const budget = el('p', 'budget');
  budget.id = 'formBudget';
  budget.hidden = true;
  form.append(budget);

  const actions = el('div', 'form-actions');
  const run = el('button', 'tb accent', `Run ${op.id}`);
  run.type = 'submit';
  run.id = 'formRun';
  run.setAttribute('aria-label', `Run ${op.id}`);
  const dryWrap = el('label', 'dry');
  const dry = el('input');
  dry.type = 'checkbox';
  dry.id = 'dryRun';
  dryWrap.htmlFor = 'dryRun';
  dryWrap.append(dry, el('span', null, 'Dry run'));
  const clear = el('button', 'tb sm', 'Close form');
  clear.type = 'button';
  clear.setAttribute('aria-label', `Close the ${op.id} form`);
  clear.onclick = () => { S.op = null; box.textContent = ''; box.append(hintNode()); say(`closed the ${op.id} form`); };
  actions.append(run, dryWrap, el('span', 'grow'), clear);
  form.append(actions);

  form.onsubmit = (e) => { e.preventDefault(); submitOp(form, dry.checked); };
  form.addEventListener('input', () => scheduleQuote(form));
  box.append(form);
  scheduleQuote(form);
  const first = form.querySelector('input, select, textarea');
  if (first) first.focus();
}

function hintNode() {
  const d = el('div', 'hint');
  d.append(el('p', null, 'Search an op in the command palette to build its form from the engine schema.'));
  const bar = el('p', 'hintbar');
  bar.textContent = 'Press / or Ctrl-K to focus the palette, ? for the shortcut list.';
  if (NOHINTS) bar.hidden = true;
  d.append(bar);
  return d;
}

function buildField(name, raw, defs, isRequired, path, depth = 0) {
  const { s, nullable, def } = resolve(raw, defs);
  const kind = widgetKind(s, def, path);

  if (kind === 'variant' && depth < 3) {
    const box = el('div', 'nested');
    box.dataset.group = path;
    const head = el('div', 'nested-head');
    const pick = el('select');
    pick.setAttribute('aria-label', `${path} variant`);
    if (!isRequired) pick.append(new Option('—', ''));
    for (const v of s.variants) pick.append(new Option(v.properties[discriminator(v)].const));
    if (!isRequired) pick.value = '';
    head.append(el('span', null, path), el('span', 'ftype', typeLabel(s, def, nullable)));
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
    head.append(el('span', null, path), el('span', 'ftype', typeLabel(s, def, nullable)));
    box.append(head);
    if (s.description) box.append(el('p', 'desc', s.description));
    const req = new Set(s.required || []);
    for (const [k, v] of Object.entries(s.properties)) {
      box.append(buildField(k, v, defs, req.has(k), `${path}.${k}`, depth + 1));
    }
    return box;
  }

  const uid = 'f' + (++fieldSeq);
  const field = el('div', 'field');
  const label = el('label');
  label.htmlFor = uid;
  // The path, not the bare property name: `fill.color` and `stroke.color` are two
  // candidates in one form and must not share a name.
  label.append(el('span', 'fname', path));
  if (isRequired) label.append(el('span', 'req', '*'));
  label.append(el('span', 'ftype', typeLabel(s, def, nullable)));
  field.append(label);

  const describedBy = [];
  if (s.description) {
    const p = el('p', 'desc', s.description);
    p.id = uid + '_d';
    describedBy.push(p.id);
    field.append(p);
  }
  const err = el('p', 'ferr');
  err.id = uid + '_e';
  err.hidden = true;
  describedBy.push(err.id);

  let input;
  let control = null;         // what goes into the DOM, when it is not the input itself
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
  } else if (kind === 'selector') {
    input = el('input');
    input.type = 'text';
    input.spellcheck = false;
    input.setAttribute('role', 'combobox');
    input.setAttribute('aria-autocomplete', 'list');
    input.setAttribute('aria-expanded', 'false');
    input.setAttribute('aria-controls', uid + '_l');
    input.placeholder = '#id or a selector';
    const list = el('ul', 'listbox seloptions');
    list.id = uid + '_l';
    list.setAttribute('role', 'listbox');
    list.setAttribute('aria-label', `${path} candidates`);
    list.hidden = true;
    control = el('div', 'combo');
    control.append(input, list);
    attachSelectorCombobox(input, list, path);
  } else {
    input = el('input');
    input.type = 'text';
    input.spellcheck = false;
    if (s.default != null && s.default !== '') input.placeholder = String(s.default);
    if (kind === 'color') input.placeholder = '#rrggbb';
  }
  input.id = uid;
  input.dataset.path = path;
  input.dataset.kind = kind;
  if (isRequired) {
    input.dataset.required = '1';
    // Only a top-level field is unconditionally required: one inside an optional group is
    // required *if that group is used*, and claiming otherwise would send the navigator
    // filling a `stroke` nobody asked for.
    if (depth === 0) input.setAttribute('aria-required', 'true');
  }
  if (describedBy.length) input.setAttribute('aria-describedby', describedBy.join(' '));

  if (kind === 'color') {
    const row = el('div', 'row');
    const swatch = el('input');
    swatch.type = 'color';
    swatch.value = '#f4a261';
    swatch.id = uid + '_s';
    swatch.setAttribute('aria-label', `${path} colour picker`);
    swatch.oninput = () => { input.value = swatch.value; input.dispatchEvent(new Event('input', { bubbles: true })); };
    row.append(input, swatch);
    field.append(row);
  } else {
    field.append(control || input);
  }
  field.append(err);

  // Prefill selector fields from the tree selection, until the human types something else.
  if (path === 'target' && S.sel) {
    input.value = '#' + S.sel;
    input.dataset.auto = '1';
  }
  input.addEventListener('input', () => {
    delete input.dataset.auto;
    if (input.getAttribute('aria-invalid') === 'true') {
      input.removeAttribute('aria-invalid');
      err.hidden = true;
    }
  });
  return field;
}

/** Selector-typed fields offer the engine's own resolved candidates, so a bad selector is
 *  corrected before the op runs rather than after it fails. */
function attachSelectorCombobox(input, list, path) {
  const open = (on) => {
    list.hidden = !on;
    input.setAttribute('aria-expanded', on ? 'true' : 'false');
  };
  // Focus lists every candidate the engine resolved: the field arrives prefilled with the
  // tree's selection, and filtering by that would hide every alternative. Typing filters.
  const draw = async (filtered) => {
    const options = await selectorCandidates();
    const q = filtered ? input.value.trim().replace(/^#/, '').toLowerCase() : '';
    const rows = options
      .filter((o) => !q || o.id.toLowerCase().includes(q) || (o.name || '').toLowerCase().includes(q))
      .slice(0, PALETTE_MAX);
    list.textContent = '';
    for (const o of rows) {
      const label = `#${o.id} · ${o.name || o.id} · ${o.type}`;
      const li = option(label, { id: o.id, selected: input.value.trim() === '#' + o.id });
      li.onmousedown = (e) => {
        e.preventDefault();
        input.value = '#' + o.id;
        delete input.dataset.auto;
        open(false);
        input.dispatchEvent(new Event('input', { bubbles: true }));
      };
      list.append(li);
    }
    open(rows.length > 0);
  };
  input.addEventListener('focus', () => draw(false));
  input.addEventListener('input', () => draw(true));
  input.addEventListener('blur', () => setTimeout(() => open(false), 120));
  input.addEventListener('keydown', (e) => { if (e.key === 'Escape') { e.preventDefault(); open(false); } });
  input.dataset.selectorField = path;
}

let selectorCache = { key: '', rows: null, at: 0 };

/** `select` takes the category keyword for the kind ("layer", "object", "node"); when the
 *  engine refuses it, the error it raises carries the real candidate list, and failing that
 *  the tree in `state` already holds every id. */
async function selectorCandidates() {
  const doc = activeDocument();
  if (!doc) return [];
  const key = `${doc.id}:${S.state.revision}`;
  if (selectorCache.key === key && selectorCache.rows) return selectorCache.rows;
  let rows = null;
  for (const selector of [noun(doc.kind), '*']) {
    try {
      const matches = await call('select', { doc: doc.id, selector }, { quiet: true });
      if (Array.isArray(matches) && matches.length) {
        rows = matches.map((m) => ({ id: m.id, name: m.name, type: m.type }));
        break;
      }
    } catch (e) {
      const err = asStudioError(e);
      if (err.candidates && err.candidates.length) {
        rows = err.candidates.map((c) => ({ id: c.replace(/^[#@]/, ''), name: '', type: 'candidate' }));
        break;
      }
    }
  }
  if (!rows) rows = doc.objects.map((o) => ({ id: o.id, name: o.name, type: o.type }));
  selectorCache = { key, rows, at: Date.now() };
  return rows;
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

/** Inputs under a group nobody filled in. Shared by `collect` and `validate`, because a
 *  dormant `stroke` must neither be sent nor be demanded. */
function buriedTest(form) {
  const dead = dormant(form);
  return (input) => {
    let n = input.parentElement && input.parentElement.closest('[data-group]');
    while (n) {
      if (dead.has(n)) return true;
      n = n.parentElement && n.parentElement.closest('[data-group]');
    }
    return false;
  };
}

/** Empty means "not supplied", so the engine's own defaults stay in charge. */
function collect(form) {
  const args = {};
  const buried = buriedTest(form);
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

/** Required fields, checked in the page so the error lands beside the field that caused it. */
function validate(form) {
  let first = null;
  const buried = buriedTest(form);
  for (const input of form.querySelectorAll('[data-required="1"]')) {
    if (buried(input)) { input.removeAttribute('aria-invalid'); continue; }
    const errNode = $(input.id + '_e');
    const empty = input.type === 'checkbox' ? false : input.value.trim() === '';
    if (empty) {
      input.setAttribute('aria-invalid', 'true');
      if (errNode) { errNode.textContent = `${input.dataset.path} is required.`; errNode.hidden = false; }
      if (!first) first = input;
    } else {
      input.removeAttribute('aria-invalid');
      if (errNode) errNode.hidden = true;
    }
  }
  return first;
}

// ------------------------------------------------------------------------------- quotes

let quoteTimer = null;

function scheduleQuote(form) {
  if (!caps.quote || !S.op) return;
  clearTimeout(quoteTimer);
  quoteTimer = setTimeout(() => refreshQuote(form), 250);
}

async function refreshQuote(form) {
  const run = $('formRun');
  const budget = $('formBudget');
  if (!run || !S.op) return;
  let args;
  try { args = collect(form); } catch { return; }
  let q;
  try {
    q = await call('quote', { op: S.op.id, args, doc: S.activeDoc }, { quiet: true });
  } catch {
    return;      // an op with no price is not an error; the plain name stands
  }
  const est = Number(q.estimateUsd || 0);
  const name = est > 0 ? `Run ${S.op.id} · est. $${est.toFixed(2)}` : `Run ${S.op.id}`;
  run.textContent = name;
  run.setAttribute('aria-label', name);
  if (est > 0 || q.spentUsd) {
    budget.hidden = false;
    budget.textContent = q.ceilingUsd != null
      ? `Budget: $${Number(q.spentUsd || 0).toFixed(2)} of $${Number(q.ceilingUsd).toFixed(2)} spent`
      : `Budget: $${Number(q.spentUsd || 0).toFixed(2)} spent, no ceiling`;
    if (q.wouldExceed) budget.textContent += ' — this run would exceed it';
  } else {
    budget.hidden = true;
  }
}

// --------------------------------------------------------------------------------- jobs
//
// §3.6: every op goes through a job when the bridge has them, so a render that takes
// twenty seconds shows a progressbar and can be cancelled instead of looking like a hang.

async function startJob(op, args, doc, dryRun) {
  const started = await call('job.start', { op, args, doc, dryRun: !!dryRun });
  const id = started.id;
  setBusy({ id, op, label: `Running ${op}` });
  try {
    for (;;) {
      await new Promise((r) => setTimeout(r, 220));
      const st = await call('job.status', { id }, { quiet: true });
      if (st.state === 'running') continue;
      if (st.state === 'cancelled') {
        say(`cancelled ${op} · nothing written · rev ${S.state ? S.state.revision : '?'}`);
        return null;
      }
      if (st.state === 'error') throw new StudioError(st.error || { message: `job ${id} failed` });
      return st.result || {};
    }
  } finally {
    setBusy(null);
  }
}

async function cancelJob() {
  if (!S.job || !S.job.id) return;
  const { id, op } = S.job;
  try {
    await call('job.cancel', { id });
    say(`cancelled job ${id} running ${op}`);
  } catch (e) {
    say(`could not cancel job ${id} · ${asStudioError(e).message}`);
  }
}

/** One door for every mutation this UI performs: palette forms, selection buttons and the
 *  geometry spinbuttons all land here, so all of them announce themselves the same way. */
async function runOp(op, args, opts = {}) {
  const doc = opts.doc || S.activeDoc;
  const dryRun = !!opts.dryRun;
  if (!caps.jobs) setBusy({ id: null, op, label: `Running ${op}` });
  try {
    const r = caps.jobs
      ? await startJob(op, args, doc, dryRun)
      : await call('op', { op, args, doc, ...(dryRun ? { dryRun: true } : {}) });
    if (!caps.jobs) setBusy(null);
    if (!r) return null;                     // cancelled
    if (r.created && r.created.length) S.sel = r.created[0];
    await refreshAll();
    sayResult(r, dryRun);
    return r;
  } catch (e) {
    if (!caps.jobs) setBusy(null);
    sayError(op, asStudioError(e));
    return null;
  }
}

function sayResult(r, dryRun) {
  const rev = S.state ? S.state.revision : '?';
  const bits = [];
  if (r.changed && r.changed.length) bits.push(`changed ${r.changed.join(', ')}`);
  if (r.created && r.created.length) bits.push(`created ${r.created.join(', ')}`);
  if (r.removed && r.removed.length) bits.push(`removed ${r.removed.join(', ')}`);
  if (r.warnings && r.warnings.length) bits.push(`warnings: ${r.warnings.join('; ')}`);
  const head = dryRun ? `dry run ${r.op} · nothing written` : `applied ${r.op}`;
  say([head, ...bits, `rev ${rev}`].join(' · '));
}

function sayError(op, err) {
  const bits = [`failed ${op}`, `${err.code}: ${err.message}`];
  if (err.suggestion) bits.push(`suggestion: ${err.suggestion}`);
  if (err.candidates && err.candidates.length) bits.push(`candidates: ${err.candidates.join(', ')}`);
  say(bits.join(' · '));
}

async function submitOp(form, dryRun) {
  const bad = validate(form);
  if (bad) {
    say(`cannot run ${S.op.id} · ${bad.dataset.path} is required`);
    bad.focus();
    return;
  }
  let args;
  try { args = collect(form); }
  catch (e) { sayError(S.op.id, asStudioError(e)); return; }
  await runOp(S.op.id, args, { dryRun });
}

// ------------------------------------------------------------------------------ actions

async function doUndo() {
  try {
    const r = await call('undo');
    await refreshAll();
    say(r.op ? `undid ${r.op} · rev ${S.state.revision}` : 'nothing to undo');
    flash(r.op ? `undid ${r.op}` : 'nothing to undo', 'human');
  } catch (e) { sayError('undo', asStudioError(e)); }
}

async function doRedo() {
  try {
    const r = await call('redo');
    await refreshAll();
    say(r.op ? `redid ${r.op} · rev ${S.state.revision}` : 'nothing to redo');
    flash(r.op ? `redid ${r.op}` : 'nothing to redo', 'human');
  } catch (e) { sayError('redo', asStudioError(e)); }
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
  for (const t of document.querySelectorAll('.tab')) {
    const on = t.dataset.tab === name;
    t.classList.toggle('active', on);
    t.setAttribute('aria-selected', on ? 'true' : 'false');
  }
  for (const p of document.querySelectorAll('.tabpane')) p.classList.toggle('active', p.id === 'tab-' + name);
  // A hidden pane cannot be scrolled, so the newest line is only reachable once it is shown.
  if (name === 'console') { const p = $('tab-console'); p.scrollTop = p.scrollHeight; }
}

// ------------------------------------------------------------------------------ dialogs
//
// §3.5: role=dialog, aria-modal, a name, a focus trap, Esc, and a primary button that
// states the consequence. While one is open the rest of the page is `inert` and
// `aria-hidden`, which is how the navigator's "a modal replaces the element table" rule
// reaches the web path.

let openDialogState = null;

function closeDialog(reason) {
  if (!openDialogState) return;
  const { node, restore } = openDialogState;
  openDialogState = null;
  node.remove();
  for (const n of [$('app'), $('welcome')]) {
    if (!n) continue;
    n.removeAttribute('inert');
    n.removeAttribute('aria-hidden');
  }
  if (restore && restore.isConnected) restore.focus();
  if (reason) say(reason);
}

const focusables = (root) => [...root.querySelectorAll(
  'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])',
)].filter((n) => !n.hidden && n.offsetParent !== null);

/**
 * `fields` is the whole dialog body: each one becomes a labelled control inside the form.
 * `primary(values)` names the button after what pressing it will do, recomputed on input,
 * which is what makes "Overwrite poster.png" appear only when it is true.
 */
function openDialog({ id, title, intro, fields = [], primary, onSubmit, extras }) {
  closeDialog(null);
  const restore = document.activeElement;
  const wrap = el('div', 'modal-wrap');
  const node = el('div', 'modal');
  node.setAttribute('role', 'dialog');
  node.setAttribute('aria-modal', 'true');
  const titleId = `dlg_${id}_title`;
  node.setAttribute('aria-labelledby', titleId);
  const h = el('h2', 'dlg-title', title);
  h.id = titleId;
  node.append(h);
  if (intro) node.append(el('p', 'dlg-intro', intro));

  const form = el('form', 'dlgform');
  const inputs = new Map();
  for (const f of fields) {
    const uid = `dlg_${id}_${f.name}`;
    const row = el('div', 'field');
    if (f.type === 'radio') {
      const group = el('fieldset', 'radios');
      group.append(el('legend', null, f.label));
      for (const opt of f.options) {
        const rid = `${uid}_${opt.value}`;
        const lab = el('label', 'radio');
        const r = el('input');
        r.type = 'radio';
        r.name = uid;
        r.id = rid;
        r.value = opt.value;
        if (opt.value === f.value) r.checked = true;
        r.setAttribute('aria-label', opt.label);
        lab.htmlFor = rid;
        lab.append(r, el('span', null, opt.label));
        group.append(lab);
        inputs.set(f.name + ':' + opt.value, r);
      }
      row.append(group);
    } else if (f.type === 'checkbox') {
      const lab = el('label', 'check');
      const c = el('input');
      c.type = 'checkbox';
      c.id = uid;
      c.checked = !!f.value;
      c.setAttribute('aria-label', f.label);
      lab.htmlFor = uid;
      lab.append(c, el('span', null, f.label));
      row.append(lab);
      inputs.set(f.name, c);
    } else if (f.type === 'select') {
      const lab = el('label', null, f.label);
      lab.htmlFor = uid;
      const sel = el('select');
      sel.id = uid;
      for (const opt of f.options) sel.append(new Option(opt.label, opt.value));
      if (f.value != null) sel.value = String(f.value);
      row.append(lab, sel);
      inputs.set(f.name, sel);
    } else {
      const lab = el('label', null, f.label);
      lab.htmlFor = uid;
      const input = el('input');
      input.type = f.type === 'number' ? 'number' : 'text';
      input.id = uid;
      input.autocomplete = 'off';
      input.spellcheck = false;
      if (f.value != null) input.value = String(f.value);
      if (f.placeholder) input.placeholder = f.placeholder;
      if (f.step) input.step = String(f.step);
      row.append(lab, input);
      inputs.set(f.name, input);
    }
    if (f.describe) {
      const d = el('p', 'desc', f.describe);
      d.id = uid + '_d';
      row.append(d);
      const target = inputs.get(f.name);
      if (target) target.setAttribute('aria-describedby', d.id);
    }
    form.append(row);
  }

  const values = () => {
    const out = {};
    for (const f of fields) {
      if (f.type === 'radio') {
        const hit = f.options.find((o) => inputs.get(f.name + ':' + o.value).checked);
        out[f.name] = hit ? hit.value : null;
      } else {
        const n = inputs.get(f.name);
        out[f.name] = f.type === 'checkbox' ? n.checked : n.value.trim();
      }
    }
    return out;
  };

  const extraBox = el('div', 'dlg-extra');
  node.append(form);
  if (extras) extras(extraBox, { values, close: closeDialog, inputs });
  node.append(extraBox);

  const actions = el('div', 'dlg-actions');
  const go = el('button', 'tb accent');
  go.type = 'submit';
  go.dataset.primary = '1';
  const cancel = el('button', 'tb', 'Cancel');
  cancel.type = 'button';
  cancel.setAttribute('aria-label', `Cancel ${title.toLowerCase()}`);
  cancel.onclick = () => closeDialog(`closed the ${title} dialog`);
  actions.append(go, el('span', 'grow'), cancel);
  form.append(actions);

  const rename = () => {
    const name = primary(values());
    go.textContent = name;
    go.setAttribute('aria-label', name);
  };
  form.addEventListener('input', rename);
  form.addEventListener('change', rename);
  rename();
  // A refusal the engine can describe — "that file exists" — should leave the dialog open
  // with the field that fixes it already set and the button renamed to say so.
  const set = (field, value) => {
    const n = inputs.get(field);
    if (!n) return;
    if (n.type === 'checkbox') n.checked = !!value;
    else n.value = String(value);
    rename();
    n.focus();
  };

  form.onsubmit = async (e) => {
    e.preventDefault();
    go.disabled = true;
    try { await onSubmit(values(), { close: () => closeDialog(null), name: go.textContent, set }); }
    finally { if (go.isConnected) go.disabled = false; }
  };

  node.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') { e.preventDefault(); closeDialog(`closed the ${title} dialog`); return; }
    if (e.key !== 'Tab') return;
    const f = focusables(node);
    if (!f.length) return;
    const first = f[0], last = f[f.length - 1];
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  });

  wrap.append(node);
  $('modalRoot').append(wrap);
  for (const n of [$('app'), $('welcome')]) {
    if (!n || n.hidden) continue;
    n.setAttribute('inert', '');
    n.setAttribute('aria-hidden', 'true');
  }
  openDialogState = { node: wrap, restore };
  const f = focusables(node);
  if (f.length) f[0].focus();
  say(`opened the ${title} dialog`);
  return { values, close: closeDialog, node };
}

const baseName = (p) => (p || '').split(/[\\/]/).filter(Boolean).pop() || (p || '');
/** A project folder is `<name>.dpaint`; the suffix is plumbing, not part of the name. */
const projectNameOf = (p) => baseName(p).replace(/\.dpaint$/i, '');

// --- the dialogs themselves ------------------------------------------------------------

function newProjectDialog() {
  openDialog({
    id: 'newproject',
    title: 'New project',
    fields: [
      { name: 'path', label: 'Project folder to create', value: '', placeholder: '/home/you/work/acme-promo.dpaint',
        describe: 'The project directory itself, not the folder it sits in.' },
      { name: 'name', label: 'Project name', value: '', placeholder: 'acme-promo' },
      { name: 'kind', label: 'First document kind', type: 'select', value: 'raster',
        options: [{ value: 'raster', label: 'raster' }, { value: 'vector', label: 'vector' }, { value: 'model', label: 'model' }] },
      { name: 'size', label: 'Document size', value: '1080x1350', placeholder: '1080x1350' },
      { name: 'dpi', label: 'Document DPI', type: 'number', value: '', placeholder: '72' },
    ],
    primary: (v) => `Create project ${v.name || projectNameOf(v.path) || '…'}`,
    onSubmit: async (v, ctx) => {
      if (!v.path) { say('cannot create a project without a path'); return; }
      const m = /^(\d+)\s*[x×]\s*(\d+)$/i.exec(v.size || '');
      const params = { path: v.path, name: v.name || projectNameOf(v.path), kind: v.kind };
      if (m) params.size = [Number(m[1]), Number(m[2])];
      if (v.dpi) params.dpi = Number(v.dpi);
      try {
        const st = await call('project.new', params);
        ctx.close();
        applyState(st);
        await refreshAll();
        say(`created project ${params.name} at ${params.path} · rev ${S.state.revision}`);
      } catch (e) { sayError('project.new', asStudioError(e)); }
    },
  });
}

function openProjectDialog() {
  openDialog({
    id: 'openproject',
    title: 'Open project',
    fields: [{ name: 'path', label: 'Project folder', value: '', placeholder: '/home/you/work/acme-promo.dpaint' }],
    primary: (v) => `Open project ${projectNameOf(v.path) || '…'}`,
    onSubmit: async (v, ctx) => {
      if (!v.path) { say('cannot open a project without a path'); return; }
      await doOpenProject(v.path, ctx.close);
    },
    extras: (box) => {
      if (!S.recent.length) return;
      box.append(el('h3', 'dlg-sub', 'Recent projects'));
      const list = el('div', 'recent-list');
      for (const r of S.recent.slice(0, 8)) {
        const b = el('button', 'tb sm', `${r.name} — ${r.path}`);
        b.type = 'button';
        b.setAttribute('aria-label', `Open project ${r.name} at ${r.path}`);
        b.onclick = () => doOpenProject(r.path, () => closeDialog(null));
        list.append(b);
      }
      box.append(list);
    },
  });
}

async function doOpenProject(path, close) {
  try {
    const st = await call('project.open', { path });
    if (close) close();
    applyState(st);
    await refreshAll();
    say(`opened project ${st.project ? st.project.name : projectNameOf(path)} · rev ${S.state.revision}`);
    await loadRecent();
  } catch (e) { sayError('project.open', asStudioError(e)); }
}

function closeProjectDialog() {
  const name = hasProject() ? S.state.project.name : '';
  openDialog({
    id: 'closeproject',
    title: 'Close project',
    intro: `Unsaved edits are already on disk; the journal stays with the project.`,
    fields: [],
    primary: () => `Close project ${name}`,
    onSubmit: async (_v, ctx) => {
      try {
        const st = await call('project.close', {});
        ctx.close();
        applyState(st);
        say(`closed project ${name}`);
      } catch (e) { sayError('project.close', asStudioError(e)); }
    },
  });
}

function importDialog() {
  const doc = activeDocument();
  openDialog({
    id: 'import',
    title: 'Import',
    intro: 'PNG, JPG, WebP, TIFF, SVG, glTF or GLB. A <name>.json sidecar beside the file is read as provenance.',
    fields: [
      { name: 'path', label: 'File path', value: '', placeholder: '/home/you/Movies/Degen Media Studio/acme/take.png' },
      { name: 'mode', label: 'Destination', type: 'radio', value: 'layer', options: [
        { value: 'layer', label: doc ? `Into document ${doc.name} as a new layer` : 'Into the active document as a new layer' },
        { value: 'document', label: 'As a new document' },
      ] },
    ],
    primary: (v) => `Import ${baseName(v.path) || 'file'}`,
    onSubmit: async (v, ctx) => {
      if (!v.path) { say('cannot import without a file path'); return; }
      try {
        const r = await call('io.import', { path: v.path, mode: v.mode, doc: S.activeDoc });
        ctx.close();
        await refreshAll();
        const made = (r.created || []).join(', ');
        say(`imported ${baseName(v.path)} as ${v.mode} ${made || '(no id reported)'}`
          + (r.sidecar ? ' with sidecar provenance' : '')
          + ` · rev ${S.state.revision}`);
      } catch (e) { sayError('io.import', asStudioError(e)); }
    },
  });
}

function exportDialog() {
  const doc = activeDocument();
  if (!doc) return;
  openDialog({
    id: 'export',
    title: 'Export',
    fields: [
      { name: 'doc', label: 'Document', type: 'select', value: doc.id,
        options: S.state.documents.map((d) => ({ value: d.id, label: `${d.name} (${d.kind})` })) },
      { name: 'path', label: 'Output path', value: '', placeholder: `/home/you/Pictures/${baseName(doc.name)}.png` },
      { name: 'scale', label: 'Scale', type: 'number', value: '1', step: '0.1' },
      { name: 'dpi', label: 'DPI', type: 'number', value: '', placeholder: '72' },
      ...(doc.kind === 'model' ? [{ name: 'frames', label: 'Turntable frames', type: 'number', value: '', placeholder: '24' }] : []),
      { name: 'overwrite', label: 'Overwrite an existing file', type: 'checkbox', value: false },
    ],
    primary: (v) => (v.overwrite && v.path)
      ? `Overwrite ${baseName(v.path)}`
      : `Export ${baseName(v.path) || 'document'}`,
    onSubmit: async (v, ctx) => {
      if (!v.path) { say('cannot export without an output path'); return; }
      const params = { doc: v.doc, path: v.path, overwrite: !!v.overwrite };
      if (v.scale) params.scale = Number(v.scale);
      if (v.dpi) params.dpi = Number(v.dpi);
      if (v.frames) params.frames = Number(v.frames);
      try {
        const r = await call('io.export', params);
        ctx.close();
        await refreshAll();
        say(`exported ${v.doc} to ${r.path} · ${r.bytes} bytes · ${(r.size || []).join('×')} · rev ${S.state.revision}`);
      } catch (e) {
        const err = asStudioError(e);
        if (err.code === 'exists') {
          ctx.set('overwrite', true);
          say(`${baseName(v.path)} already exists · press Overwrite ${baseName(v.path)} to replace it`);
          return;
        }
        sayError('io.export', err);
      }
    },
  });
}

function sendToEditorDialog() {
  const docs = (S.state && S.state.documents) || [];
  openDialog({
    id: 'sendtoeditor',
    title: 'Send to editor',
    intro: 'Writes PNG, SVG and GLB with a <name>.json sidecar naming the project, document and revision.',
    fields: [
      ...docs.map((d) => ({ name: 'doc_' + d.id, label: `Send document ${d.name}`, type: 'checkbox', value: d.id === S.activeDoc })),
      { name: 'dir', label: 'Destination folder', value: '', placeholder: '~/Movies/degen-paint/<project>' },
      { name: 'overwrite', label: 'Overwrite existing files', type: 'checkbox', value: false },
    ],
    primary: (v) => {
      const n = docs.filter((d) => v['doc_' + d.id]).length;
      return v.overwrite ? `Overwrite and send ${n} file${n === 1 ? '' : 's'} to editor`
        : `Send ${n} file${n === 1 ? '' : 's'} to editor`;
    },
    onSubmit: async (v, ctx) => {
      const chosen = docs.filter((d) => v['doc_' + d.id]).map((d) => d.id);
      if (!chosen.length) { say('cannot send to editor without choosing a document'); return; }
      const params = { docs: chosen, overwrite: !!v.overwrite };
      if (v.dir) params.dir = v.dir;
      try {
        const r = await call('io.sendToEditor', params);
        ctx.close();
        await refreshAll();
        const files = (r.files || []).map((f) => f.path);
        say(`sent ${files.length} file${files.length === 1 ? '' : 's'} to editor: ${files.join(', ')} · rev ${S.state.revision}`);
      } catch (e) {
        const err = asStudioError(e);
        if (err.code === 'exists') {
          ctx.set('overwrite', true);
          say(`some of those files already exist · press the overwrite button to replace them`);
          return;
        }
        sayError('io.sendToEditor', err);
      }
    },
  });
}

async function exportPreview() {
  const doc = activeDocument();
  if (!doc) return;
  setBusy({ id: null, op: 'io.exportPreview', label: `Rendering preview of ${doc.name}` });
  try {
    const r = await call('io.exportPreview', { doc: doc.id });
    say(`exported a preview of ${doc.name} to ${r.png} and an annotated twin at ${r.annotated}`);
  } catch (e) {
    sayError('io.exportPreview', asStudioError(e));
  } finally {
    setBusy(null);
  }
}

function renameDialog(doc, o, op) {
  const who = `${noun(doc.kind)} ${o.name || o.id}`;
  openDialog({
    id: 'rename',
    title: 'Rename',
    fields: [{ name: 'name', label: `New name for ${who}`, value: o.name || o.id }],
    primary: (v) => `Rename ${who} to ${v.name || '…'}`,
    onSubmit: async (v, ctx) => {
      if (!v.name) { say(`cannot rename ${who} to an empty name`); return; }
      ctx.close();
      await runOp(op, { target: '#' + o.id, name: v.name });
    },
  });
}

function deleteDialog(doc, o, op) {
  const who = `${noun(doc.kind)} ${o.name || o.id}`;
  openDialog({
    id: 'delete',
    title: 'Delete',
    intro: 'This lands in the journal and can be undone.',
    fields: [],
    primary: () => `Delete ${who}`,
    onSubmit: async (_v, ctx) => {
      ctx.close();
      await runOp(op, { target: '#' + o.id });
    },
  });
}

async function providersDialog() {
  let status = null;
  try { status = await call('providers.status', {}); }
  catch (e) { sayError('providers.status', asStudioError(e)); return; }
  const render = (box, st) => {
    box.textContent = '';
    box.append(el('h3', 'dlg-sub', 'Status'));
    for (const [k, v] of Object.entries(st || {})) {
      box.append(el('p', 'prov-line',
        `${k}: ${v.configured ? `configured (${v.source || 'unknown source'})` : 'missing'}`));
    }
  };
  openDialog({
    id: 'providers',
    title: 'Providers',
    intro: 'Keys are stored through this app (keychain first) and are never read back.',
    fields: [
      { name: 'fal', label: 'fal.ai key', value: '', placeholder: 'paste to replace' },
      { name: 'quiver', label: 'QuiverAI key', value: '', placeholder: 'paste to replace' },
    ],
    primary: (v) => {
      const which = [v.fal ? 'fal.ai' : null, v.quiver ? 'QuiverAI' : null].filter(Boolean);
      return which.length ? `Save ${which.join(' and ')} key${which.length > 1 ? 's' : ''}` : 'Save keys';
    },
    onSubmit: async (v, ctx) => {
      const jobs = [];
      if (v.fal) jobs.push(['fal', v.fal]);
      if (v.quiver) jobs.push(['quiver', v.quiver]);
      if (!jobs.length) { say('no key was typed, so nothing was saved'); return; }
      try {
        let st = null;
        for (const [provider, key] of jobs) st = await call('providers.set', { provider, key });
        ctx.close();
        const names = jobs.map(([p]) => p).join(' and ');
        const conf = Object.entries(st || {}).filter(([, x]) => x.configured).map(([k]) => k);
        say(`saved the ${names} key; configured providers: ${conf.join(', ') || 'none'}`);
      } catch (e) { sayError('providers.set', asStudioError(e)); }
    },
    extras: (box) => render(box, status),
  });
}

/** One table drives the overlay and the key handler, so the page cannot advertise a
 *  shortcut it does not honour. The backend's `contract::SHORTCUTS` wins when it answers;
 *  the copy in this file is what an older bridge gets. */
async function loadShortcuts() {
  let rows = SHORTCUTS;
  if (caps.shortcuts) {
    try {
      const r = await call('shortcuts', {}, { quiet: true });
      if (r && Array.isArray(r.shortcuts) && r.shortcuts.length) rows = r.shortcuts;
    } catch { /* the table in this file stands in */ }
  }
  S.shortcuts = rows;
  keyMap.clear();
  for (const s of rows) if (s.keys) keyMap.set(normalizeCombo(s.keys), s.id);
}

async function shortcutsDialog() {
  const rows = S.shortcuts.length ? S.shortcuts : SHORTCUTS;
  openDialog({
    id: 'shortcuts',
    title: 'Keyboard shortcuts',
    fields: [],
    primary: () => 'Close the shortcut list',
    onSubmit: async (_v, ctx) => ctx.close(),
    extras: (box) => {
      const table = el('table', 'shortcut-table');
      const body = el('tbody');
      for (const s of rows) {
        const tr = el('tr');
        tr.append(el('td', 'sc-keys', s.keys), el('td', null, s.label), el('td', 'sc-scope', s.scope || ''));
        body.append(tr);
      }
      table.append(body);
      box.append(table);
    },
  });
}

// ------------------------------------------------------------------------------ welcome

function showWelcome(on) {
  $('bootNote').hidden = true;
  $('welcome').hidden = !on;
  $('app').hidden = on;
  if (on) renderWelcome();
}

function renderWelcome() {
  const box = $('welcomeRecent');
  box.textContent = '';
  $('welcomeNote').textContent = caps.projects
    ? 'No project is open. Create one or open an existing one.'
    : 'No project is open, and this bridge cannot open one yet.';
  $('wNew').hidden = !caps.projects;
  $('wOpen').hidden = !caps.projects;
  if (!S.recent.length) {
    box.append(el('p', 'empty-note', 'No recent projects.'));
    return;
  }
  for (const r of S.recent.slice(0, 8)) {
    const b = el('button', 'tb', `${r.name} — ${r.path}`);
    b.type = 'button';
    b.setAttribute('aria-label', `Open project ${r.name} at ${r.path}`);
    b.onclick = () => doOpenProject(r.path, null);
    box.append(b);
  }
}

async function loadRecent() {
  if (!caps.projects) { S.recent = []; return; }
  try {
    const r = await call('project.recent', {}, { quiet: true });
    S.recent = (r && r.entries) || [];
  } catch { S.recent = []; }
  if (!$('welcome').hidden) renderWelcome();
}

// -------------------------------------------------------------------------- live sync

/** The shared-journal claim, made visible: an agent's write shows up here on its own. */
async function poll() {
  if (refreshBusy || openDialogState) return;   // a refresh in flight will pick the change up anyway
  try {
    const st = await call('state', {}, { quiet: true });
    const sig = stateSignature(st);
    if (sig === S.sig) return;
    const before = S.state ? S.state.revision : 0;
    // Say so before the refresh, not after: a re-render plus a project lint takes long
    // enough that a silent second would look like the UI had missed the write.
    const peek = st.project
      ? await call('history', { limit: 1 }, { quiet: true }).catch(() => ({ entries: [] }))
      : { entries: [] };
    const top = peek.entries[0];
    const actor = top && top.actor === 'human' ? 'human' : 'agent';
    const what = st.revision > before ? (top ? top.op : 'change') : top && top.undone ? 'undo' : 'redo';
    flash(`updated by ${actor} · ${what}`, actor);
    say(`updated by ${actor} · ${what} · rev ${st.revision}`);
    await refreshAll();
  } catch { /* transport hiccup; the next tick retries */ }
}

// ------------------------------------------------------------------------------- keys

const MENU = {
  'project.new': () => caps.projects && newProjectDialog(),
  'project.open': () => caps.projects && openProjectDialog(),
  'project.close': () => caps.projects && hasProject() && closeProjectDialog(),
  'io.import': () => caps.io && importDialog(),
  'io.export': () => caps.io && exportDialog(),
  'io.exportPreview': () => caps.io && exportPreview(),
  'io.sendToEditor': () => caps.io && sendToEditorDialog(),
  'edit.undo': () => doUndo(),
  'edit.redo': () => doRedo(),
  'edit.selectAll': () => selectAll(),
  'object.delete': () => withSelection((doc, o, map) => map.remove && deleteDialog(doc, o, map.remove)),
  'object.rename': () => withSelection((doc, o, map) => map.rename && renameDialog(doc, o, map.rename)),
  'object.duplicate': () => withSelection((doc, o, map) => map.duplicate && runOp(map.duplicate, { target: '#' + o.id })),
  'view.fit': () => { fitView(); say(`fitted ${docName()} in the viewport · ${statusView()}`); },
  'view.oneToOne': () => { view.x = 0; view.y = 0; zoomTo(1); layoutView(); say(`zoomed ${docName()} to 100% · ${statusView()}`); },
  'view.zoomIn': () => { zoomTo(view.zoom * 1.25); say(`zoomed in on ${docName()} · ${statusView()}`); },
  'view.zoomOut': () => { zoomTo(view.zoom / 1.25); say(`zoomed out on ${docName()} · ${statusView()}`); },
  'view.orbitLeft': () => orbit(-15, 0),
  'view.orbitRight': () => orbit(15, 0),
  'view.orbitUp': () => orbit(0, 15),
  'view.orbitDown': () => orbit(0, -15),
  'palette.open': () => focusPalette(),
  'palette.close': () => { paletteOpen(false); if (document.activeElement) document.activeElement.blur(); },
  'filter.focus': () => { $('treeFilter').focus(); $('treeFilter').select(); say('focused the layer filter'); },
  'pane.focus': () => focusNextPane(),
  'help.doctor': () => (caps.providers ? providersDialog() : say('this bridge has no provider status to report yet')),
  'help.shortcuts': () => shortcutsDialog(),
  // The shell builds View › panes from `contract::SHORTCUTS`; whichever of these ids it
  // defines, the dock follows.
  'view.pane.history': () => { showTab('history'); say('showed the History panel'); },
  'view.pane.lint': () => { showTab('lint'); say('showed the Lint panel'); },
  'view.pane.console': () => { showTab('console'); say('showed the Console panel'); },
  'view.pane.tree': () => { $('treeFilter').focus(); say('focused the Tree panel'); },
  'view.pane.inspector': () => {
    const first = $('inspector').querySelector('input, select, textarea, button');
    if (first) first.focus();
    say('focused the Inspector panel');
  },
};

function withSelection(fn) {
  const doc = activeDocument();
  const o = selectedObject();
  if (!doc || !o) { say('nothing is selected'); return; }
  fn(doc, o, ACTIONS[doc.kind] || {});
}

/** Edit › Select All means the raster selection mask; the other kinds have no equivalent. */
function selectAll() {
  const doc = activeDocument();
  if (!doc) { say('no document is open'); return; }
  if (doc.kind !== 'raster') { say(`select all applies to raster documents, and ${doc.name} is ${doc.kind}`); return; }
  runOp('raster.select.all', {});
}

const PANE_STOPS = ['paletteInput', 'docList', 'btnFit', 'treeFilter', 'selActions', 'inspector', 'tabbtn-history'];
let paneStop = 0;

function focusNextPane() {
  for (let i = 0; i < PANE_STOPS.length; i++) {
    paneStop = (paneStop + 1) % PANE_STOPS.length;
    const root = $(PANE_STOPS[paneStop]);
    if (!root || root.hidden) continue;
    const target = root.matches('input, button, select, textarea')
      ? root
      : root.querySelector('input, button, select, textarea, [role=option]');
    if (!target) continue;
    if (target.tabIndex < 0) target.tabIndex = -1;
    target.focus();
    const region = target.closest('[role=region], [role=toolbar]');
    say(`focused the ${region ? region.getAttribute('aria-label') : 'next'} pane`);
    return;
  }
}

function fireMenu(id) {
  const fn = MENU[id];
  if (!fn) { note(`menu action '${id}' has no handler`); return; }
  fn();
}

const keyMap = new Map();

/** `CmdOrCtrl+Shift+W` and `Alt+Left` reduced to one comparable spelling. */
function normalizeCombo(spec) {
  const parts = String(spec).split('+').map((p) => p.trim().toLowerCase());
  const key = parts.pop() || '+';
  const mods = parts
    .filter(Boolean)
    .map((p) => (['cmd', 'ctrl', 'command', 'control', 'cmdorctrl', 'meta'].includes(p) ? 'cmdorctrl' : p))
    .sort();
  return [...mods, key].join('+');
}

function comboOf(e) {
  let k = e.key;
  if (k.startsWith('Arrow')) k = k.slice(5);
  k = k.toLowerCase();
  const mods = [];
  if (e.ctrlKey || e.metaKey) mods.push('cmdorctrl');
  if (e.altKey) mods.push('alt');
  // Shift is only a modifier when it did not already change the character: `?` is its own
  // key in the table, `Shift+W` is not.
  if (e.shiftKey && (k.length > 1 || /^[a-z0-9]$/.test(k))) mods.push('shift');
  mods.sort();
  return [...mods, k].join('+');
}

/** What the caret keeps for itself: select-all, undo/redo and the clipboard. */
const TEXT_CHORDS = new Set([
  'cmdorctrl+a', 'cmdorctrl+z', 'cmdorctrl+shift+z', 'cmdorctrl+y',
  'cmdorctrl+c', 'cmdorctrl+v', 'cmdorctrl+x',
]);

function reachesThroughTyping(e, combo) {
  if (TEXT_CHORDS.has(combo)) return false;
  return e.ctrlKey || e.metaKey || e.altKey || /^f\d{1,2}$/.test(e.key.toLowerCase());
}

function initKeys() {
  window.addEventListener('keydown', (e) => {
    const t = e.target;
    const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT');
    const combo = comboOf(e);

    if (e.key === 'Escape') {
      if (openDialogState) return;           // the dialog handles its own Esc
      e.preventDefault();
      fireMenu('palette.close');
      return;
    }
    if (openDialogState) return;

    // Inside a text field the caret's own bindings win: a bare key types, and the six
    // editing chords stay the browser's. Everything else — F6, Ctrl-K, Ctrl-Alt-4 — still
    // reaches the Studio, because those are exactly the keys you want while typing.
    if (typing && !reachesThroughTyping(e, combo)) return;

    const id = keyMap.get(combo);
    if (id && MENU[id]) { e.preventDefault(); fireMenu(id); return; }
    if (combo === '/') { e.preventDefault(); focusPalette(); }
  });

  // The Tauri shell's native menu bar reaches the page through this one event.
  window.addEventListener('dpaint:menu', (e) => fireMenu(e.detail && e.detail.id));
}

// -------------------------------------------------------------------------------- boot

async function boot() {
  initViewport();
  initPalette();
  initKeys();
  $('btnUndo').onclick = doUndo;
  $('btnRedo').onclick = doRedo;
  $('btnNewProject').onclick = () => fireMenu('project.new');
  $('btnOpenProject').onclick = () => fireMenu('project.open');
  $('btnCloseProject').onclick = () => fireMenu('project.close');
  $('btnImport').onclick = () => fireMenu('io.import');
  $('btnExport').onclick = () => fireMenu('io.export');
  $('btnSendToEditor').onclick = () => fireMenu('io.sendToEditor');
  $('btnExportPreview').onclick = () => fireMenu('io.exportPreview');
  $('btnProviders').onclick = () => fireMenu('help.doctor');
  $('btnShortcuts').onclick = () => fireMenu('help.shortcuts');
  $('wNew').onclick = () => fireMenu('project.new');
  $('wOpen').onclick = () => fireMenu('project.open');
  $('btnCancelJob').onclick = cancelJob;
  $('btnClearConsole').onclick = () => { S.logs = []; S.logErrors = 0; renderConsole(); say('cleared the console log'); };
  $('btnLint').onclick = async () => {
    showTab('lint');
    try {
      S.lint = await call('lint', { doc: S.activeDoc });
      S.lintShown = 40;
      renderLint();
      renderStatus();
      const n = S.lint.findings.length;
      say(`re-linted ${docName()} · ${n} finding${n === 1 ? '' : 's'} · ${S.lint.errors} error${S.lint.errors === 1 ? '' : 's'}`);
    } catch (e) { sayError('lint', asStudioError(e)); }
  };
  for (const t of document.querySelectorAll('.tab')) {
    t.onclick = () => fireMenu('view.pane.' + t.dataset.tab);
  }
  $('treeFilter').addEventListener('input', (e) => {
    S.treeFilter = e.target.value;
    S.treeShown = TREE_CAP;
    renderTree();
  });
  $('treeMore').onclick = () => { S.treeShown += TREE_CAP; renderTree(); };

  $('inspector').append(hintNode());
  renderConsole();
  // Before the first render, so the first frame already goes wherever it is going to go.
  await initGpu();
  // The catalogue and the capability probes have nothing to do with the first paint, so
  // they run beside it rather than in front of it.
  const detecting = detectCapabilities();
  const catalogue = call('catalog', {}, { quiet: true }).catch(() => []);
  await refreshAll({ noRender: true });
  await detecting;
  S.catalog = await catalogue;
  await Promise.all([loadRecent(), loadShortcuts()]);
  await refreshRender({ fit: true });
  S.maxSeqSeen = Math.max(0, ...S.history.map((e) => e.seq));
  renderHistory();
  updateChrome();
  renderStatus();
  say(hasProject()
    ? `ready · ${statusSummary()} · rev ${S.state.revision}`
    : 'ready · no project open');

  setInterval(poll, 1000);

  // The Tauri shell drives native menu items through these; the poll is the backstop.
  window.__DPAINT_ON_CHANGE__ = () => { refreshAll().then(() => flash('updated', 'human')); };
  window.addEventListener('dpaint:changed', () => window.__DPAINT_ON_CHANGE__());
  const tauriEvent = window.__TAURI__ && window.__TAURI__.event;
  if (tauriEvent && typeof tauriEvent.listen === 'function') {
    tauriEvent.listen('dpaint:changed', () => window.__DPAINT_ON_CHANGE__());
    tauriEvent.listen('dpaint:menu', (ev) => fireMenu(ev && ev.payload && (ev.payload.id || ev.payload)));
  }

  // What the verification harness reads instead of guessing from pixels.
  window.__DPAINT_VIEWPORT__ = {
    get renderer() { return gpu.on ? 'gpu' : 'cpu'; },
    get mode() { return gpu.on ? gpu.mode : 'image'; },
    get status() { return gpu.status; },
    get view() { return { zoom: view.zoom, x: view.x, y: view.y, rw: view.rw, rh: view.rh }; },
    get orbit() { return gpu.on && gpu.vp.orbitAngles ? gpu.vp.orbitAngles() : null; },
  };
  window.__DPAINT_CAPS__ = caps;
}

window.addEventListener('error', (e) => note(`uncaught: ${e.message}`));
window.addEventListener('unhandledrejection', (e) => note(`unhandled rejection: ${(e.reason && e.reason.message) || e.reason}`));

// If the first `state` never arrives there is no project and no document to name controls
// after, so the shell stays behind the boot line and the reason goes to the console panel
// and the Status region rather than to a blank page.
try {
  await boot();
} catch (e) {
  const err = asStudioError(e);
  note(`boot failed: ${err.message}`);
  $('bootNote').textContent = `The Studio could not reach the engine: ${err.message}`;
}
