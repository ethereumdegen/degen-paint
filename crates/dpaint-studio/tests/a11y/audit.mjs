#!/usr/bin/env node
// The Studio audit: does this UI look, to something that can only read the accessibility
// tree, like a surface an agent can operate?
//
// It is not a browser test. It runs the *navigator's own* candidate rules (vendored in
// ./candidates.js from starkbot-neo's snapshot.js) against the live page and fails on the
// conditions that make a real run go wrong — an unnamed control, a budget blown past 250,
// a combobox that never produces options, a dialog with no role, a silent op.
//
// Dependencies: Node 22+ (for the global `fetch` and `WebSocket`) and a Chromium-family
// browser. No npm, no Puppeteer: the whole client is the ~80 lines of raw CDP below, which
// is also what keeps this honest — the audit reaches the page the same way the navigator
// does.
//
//   node crates/dpaint-studio/tests/a11y/audit.mjs --url http://127.0.0.1:4317
//   node …/audit.mjs --url … --json          machine-readable
//   node …/audit.mjs --url … --nohints       the `--no-hints` twin: shortcuts overlay off

import { spawn } from 'node:child_process';
import { readFileSync, existsSync, mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const CANDIDATES = readFileSync(join(HERE, 'candidates.js'), 'utf8');
const VENDORED_FROM = 'starkbot-neo snapshot.js @ ae5f815c76f377e8172b7f3a9e28aa4e44f54667';

const BUDGET = 120;          // §6: the default view must stay well under the navigator's 250
const CONTAINER_CAP = 100;   // §6: no single (role, container) group may hoard the budget
const HARD_CAP = 250;        // the navigator's own cap; past this, candidates cannot be chosen

const argv = process.argv.slice(2);
const arg = (k, d) => {
  const i = argv.indexOf(k);
  return i === -1 ? d : argv[i + 1];
};
const flag = (k) => argv.includes(k);

const URL_ = arg('--url', 'http://127.0.0.1:4317');
const JSON_OUT = flag('--json');
const NOHINTS = flag('--nohints');

function browserPath() {
  if (process.env.CHROME && existsSync(process.env.CHROME)) return process.env.CHROME;
  const names = ['chromium', 'chromium-browser', 'google-chrome-stable', 'google-chrome', 'brave-browser'];
  for (const dir of (process.env.PATH || '').split(':')) {
    for (const n of names) {
      const p = join(dir, n);
      if (existsSync(p)) return p;
    }
  }
  for (const p of [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
  ]) if (existsSync(p)) return p;
  throw new Error('no Chromium-family browser found; set $CHROME');
}

// --- the whole CDP client -------------------------------------------------------------

class Cdp {
  constructor(ws) { this.ws = ws; this.id = 0; this.pending = new Map(); this.session = null;
    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(ev.data);
      const p = this.pending.get(msg.id);
      if (p) { this.pending.delete(msg.id); msg.error ? p.reject(new Error(msg.error.message)) : p.resolve(msg.result); }
    });
  }
  call(method, params = {}, useSession = true) {
    const id = ++this.id;
    const payload = { id, method, params };
    if (useSession && this.session) payload.sessionId = this.session;
    this.ws.send(JSON.stringify(payload));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => this.pending.has(id) && (this.pending.delete(id), reject(new Error(`${method} timed out`))), 30000);
    });
  }
  async evaluate(expression) {
    const r = await this.call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || 'evaluate threw');
    return r.result.value;
  }
}

async function launch() {
  const profile = mkdtempSync(join(tmpdir(), 'dpaint-audit-'));
  const proc = spawn(browserPath(), [
    `--user-data-dir=${profile}`, '--headless=new', '--no-first-run', '--no-default-browser-check',
    '--remote-debugging-port=0', '--window-size=1440,900', '--ozone-platform-hint=auto', 'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });

  const port = await new Promise((resolve, reject) => {
    let buf = '';
    const t = setTimeout(() => reject(new Error('browser never printed its debugging port')), 20000);
    proc.stderr.on('data', (d) => {
      buf += d;
      const m = buf.match(/ws:\/\/127\.0\.0\.1:(\d+)\//);
      if (m) { clearTimeout(t); resolve(m[1]); }
    });
  });

  const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((r) => ws.addEventListener('open', r, { once: true }));
  const cdp = new Cdp(ws);
  const { targetId } = await cdp.call('Target.createTarget', { url: 'about:blank' }, false);
  const { sessionId } = await cdp.call('Target.attachToTarget', { targetId, flatten: true }, false);
  cdp.session = sessionId;
  await cdp.call('Page.enable');
  await cdp.call('Runtime.enable');
  return { cdp, close: () => { try { ws.close(); } catch {} proc.kill('SIGKILL'); rmSync(profile, { recursive: true, force: true }); } };
}

async function goto(cdp, url) {
  await cdp.call('Page.navigate', { url });
  const deadline = Date.now() + 20000;
  for (;;) {
    if (await cdp.evaluate('document.readyState === "complete"')) break;
    if (Date.now() > deadline) throw new Error(`${url} never finished loading`);
    await new Promise((r) => setTimeout(r, 20));
  }
  // The Studio boots asynchronously: it dispatches `state` before it paints anything.
  for (let i = 0; i < 100; i++) {
    if (await cdp.evaluate('!!document.querySelector("[role=toolbar], #toolbar")')) break;
    await new Promise((r) => setTimeout(r, 50));
  }
}

const snap = (cdp) => cdp.evaluate(CANDIDATES);

// --- the checks -----------------------------------------------------------------------

const findings = [];
const fail = (rule, detail, extra = {}) => findings.push({ rule, detail, severity: 'error', ...extra });
const warn = (rule, detail, extra = {}) => findings.push({ rule, detail, severity: 'warn', ...extra });

function checkNames(view, { candidates }) {
  for (const c of candidates) {
    if (!c.label.trim()) fail('unnamed-candidate', `${view}: a ${c.role} (${c.tag}${c.id ? '#' + c.id : ''}) has no accessible name`);
  }
  const seen = new Map();
  for (const c of candidates) {
    if (!c.label.trim()) continue;
    const key = `${c.container}\u0000${c.label.trim().toLowerCase()}`;
    seen.set(key, (seen.get(key) || 0) + 1);
  }
  for (const [key, n] of seen) {
    if (n > 1) {
      const [container, label] = key.split('\u0000');
      fail('ambiguous-name', `${view}: "${label}" appears ${n} times in ${container}; the navigator cannot tell them apart`);
    }
  }
}

function checkBudget(view, { candidates, omitted }) {
  if (omitted > 0) fail('over-hard-cap', `${view}: ${candidates.length + omitted} candidates, ${omitted} past the navigator's ${HARD_CAP} cap and therefore unselectable`);
  if (candidates.length > BUDGET) fail('over-budget', `${view}: ${candidates.length} candidates, budget is ${BUDGET}`);
  const groups = new Map();
  for (const c of candidates) {
    const k = `${c.role} in ${c.container}`;
    groups.set(k, (groups.get(k) || 0) + 1);
  }
  for (const [k, n] of groups) {
    if (n > CONTAINER_CAP) fail('group-hoards-budget', `${view}: ${n} candidates are "${k}"; cap is ${CONTAINER_CAP}`);
  }
  return { count: candidates.length, groups: [...groups].sort((a, b) => b[1] - a[1]).slice(0, 8) };
}

async function checkLandmarks(cdp) {
  const regions = await cdp.evaluate(`
    [...document.querySelectorAll('[role=region],[role=toolbar],main,aside,section,footer,header')]
      .map(e => ({ role: e.getAttribute('role') || e.tagName.toLowerCase(),
                   label: e.getAttribute('aria-label') || e.getAttribute('aria-labelledby') || null }))`);
  const wanted = ['Documents', 'Viewport', 'Inspector', 'Status', 'History', 'Lint'];
  const labels = regions.map((r) => (r.label || '').toLowerCase());
  for (const w of wanted) {
    if (!labels.some((l) => l.includes(w.toLowerCase()))) warn('missing-landmark', `no labelled region for ${w}`);
  }
  for (const r of regions) {
    if (r.role === 'region' && !r.label) fail('unlabelled-region', 'a role=region carries no aria-label; it is invisible as a container');
  }
  return regions;
}

async function checkStatusRegion(cdp) {
  const live = await cdp.evaluate(`
    [...document.querySelectorAll('[role=status],[aria-live]')]
      .map(e => ({ role: e.getAttribute('role'), live: e.getAttribute('aria-live'), text: (e.innerText||'').slice(0,120) }))`);
  if (!live.length) fail('no-status-region', 'nothing announces op results; a successful step would look like no progress');
  return live;
}

// A combobox the navigator fills must produce a visible [role=option] under aria-controls
// within 200 ms, or the fill is scored as a stale (10-navigator.md).
async function checkPalette(cdp) {
  const box = await cdp.evaluate(`(() => {
    const el = document.querySelector('[role=combobox][aria-controls]');
    if (!el) return null;
    el.focus();
    return { id: el.id || null, controls: el.getAttribute('aria-controls'), name: el.getAttribute('aria-label') || '' };
  })()`);
  if (!box) { fail('no-palette-combobox', 'the command palette is not a role=combobox with aria-controls; the navigator has no way to search ops'); return null; }

  await cdp.call('Input.insertText', { text: 'layer add' });
  const started = Date.now();
  let options = 0;
  while (Date.now() - started < 400) {
    options = await cdp.evaluate(`(() => {
      const root = document.getElementById(${JSON.stringify(box.controls)});
      if (!root) return -1;
      return [...root.querySelectorAll('[role=option]')].filter(o => {
        const r = o.getBoundingClientRect();
        return r.width > 0 && r.height > 0;
      }).length;
    })()`);
    if (options > 0) break;
    await new Promise((r) => setTimeout(r, 20));
  }
  const elapsed = Date.now() - started;
  if (options === -1) fail('palette-controls-dangling', `aria-controls names "${box.controls}", which is not in the document`);
  else if (options === 0) fail('palette-no-options', `no visible [role=option] appeared within 400 ms of typing; the navigator waits 200 ms and then gives up`);
  else if (elapsed > 200) warn('palette-slow', `options appeared after ${elapsed} ms; the navigator's combobox wait is 200 ms`);
  if (options > 12) warn('palette-verbose', `${options} options are live at once; §3.3 caps the list at 12 so the budget is not eaten by search results`);
  return { options, elapsed };
}

async function checkDialogs(cdp) {
  const dialogs = await cdp.evaluate(`
    [...document.querySelectorAll('[role=dialog], dialog')].map(d => ({
      modal: d.getAttribute('aria-modal'),
      labelled: !!(d.getAttribute('aria-label') || d.getAttribute('aria-labelledby')),
      hidden: d.hidden || d.closest('[hidden]') !== null,
      primary: (() => { const b = d.querySelector('button[data-primary], .primary, button[type=submit]'); return b ? (b.innerText||'').trim() : null; })(),
    }))`);
  for (const d of dialogs) {
    if (d.hidden) continue;
    if (d.modal !== 'true') fail('dialog-not-modal', 'an open role=dialog has no aria-modal=true; on the native path it will not replace the element table');
    if (!d.labelled) fail('dialog-unlabelled', 'an open dialog has no accessible name');
  }
  return dialogs;
}

// The decisive one: run an op the way an agent would and prove the page *said* something.
// A control that mutates the document without changing visible text makes three such steps
// look like `Blocked(NoProgress)` to the navigator.
async function checkOpIsAudible(cdp) {
  const before = await cdp.evaluate('document.body.innerText');
  const ran = await cdp.evaluate(`(async () => {
    const call = async (method, params = {}) => {
      const r = await fetch('/api', { method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ method, params }) });
      return r.json();
    };
    const st = await call('state');
    if (!st.ok || !st.result.project) return { skipped: 'no project open' };
    const doc = st.result.documents[0];
    if (!doc || doc.kind !== 'raster') return { skipped: 'active document is not a raster document' };
    const out = await call('op', { op: 'raster.layer.add', args: { type: 'fill', color: '#123456', name: 'audit-probe' } });
    return { ok: out.ok, error: out.error || null };
  })()`);
  if (ran.skipped) { warn('op-audibility-skipped', `could not run the probe op: ${ran.skipped}`); return ran; }
  if (!ran.ok) { fail('op-probe-failed', `the probe op did not apply: ${JSON.stringify(ran.error)}`); return ran; }
  // The UI polls once a second; give it two.
  await new Promise((r) => setTimeout(r, 2200));
  const after = await cdp.evaluate('document.body.innerText');
  if (after === before) fail('silent-op', 'an op changed the document and no visible text changed; the navigator scores this as no progress');
  const names = after.includes('audit-probe');
  if (!names) warn('op-not-reflected', 'the new layer\'s name does not appear anywhere in the page text');
  return { changed: after !== before, names };
}

async function checkOrigin(url) {
  const evil = await fetch(new globalThis.URL('/api', url), {
    method: 'POST',
    headers: { 'content-type': 'application/json', origin: 'http://evil.example' },
    body: JSON.stringify({ method: 'state', params: {} }),
  }).then((r) => r.status).catch(() => 0);
  if (evil !== 403) fail('foreign-origin-accepted', `POST /api with Origin: http://evil.example returned ${evil}, expected 403; any page in any tab can drive this project`);
  const plain = await fetch(new globalThis.URL('/api', url), {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ method: 'state', params: {} }),
  }).then((r) => r.status).catch(() => 0);
  if (plain !== 200) fail('local-request-refused', `POST /api with no Origin returned ${plain}, expected 200`);
  return { evil, plain };
}

// --- run ------------------------------------------------------------------------------

const report = { url: URL_, vendoredFrom: VENDORED_FROM, nohints: NOHINTS, views: {} };
const { cdp, close } = await launch();
try {
  await goto(cdp, NOHINTS ? `${URL_}/?nohints=1` : URL_);

  const def = await snap(cdp);
  checkNames('default view', def);
  report.views.default = checkBudget('default view', def);
  report.landmarks = await checkLandmarks(cdp);
  report.status = await checkStatusRegion(cdp);
  report.origin = await checkOrigin(URL_);
  report.op = await checkOpIsAudible(cdp);

  report.palette = await checkPalette(cdp);
  const withPalette = await snap(cdp);
  checkNames('palette open', withPalette);
  report.views.palette = checkBudget('palette open', withPalette);
  report.dialogs = await checkDialogs(cdp);
} finally {
  close();
}

report.findings = findings;
report.errors = findings.filter((f) => f.severity === 'error').length;
report.warnings = findings.filter((f) => f.severity === 'warn').length;

if (JSON_OUT) {
  console.log(JSON.stringify(report, null, 2));
} else {
  console.log(`audit ${URL_}${NOHINTS ? '  (--nohints)' : ''}`);
  console.log(`rules ${VENDORED_FROM}`);
  for (const [view, v] of Object.entries(report.views)) {
    console.log(`\n${view}: ${v.count} candidates (budget ${BUDGET})`);
    for (const [group, n] of v.groups) console.log(`  ${String(n).padStart(4)}  ${group}`);
  }
  if (report.palette) console.log(`\npalette: ${report.palette.options} options after ${report.palette.elapsed} ms`);
  console.log('');
  for (const f of findings) console.log(`${f.severity === 'error' ? 'FAIL' : 'warn'}  ${f.rule}: ${f.detail}`);
  console.log(`\n${report.errors} errors, ${report.warnings} warnings`);
}

process.exit(report.errors > 0 ? 1 : 0);
