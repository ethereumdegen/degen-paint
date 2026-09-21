#!/usr/bin/env node
// S8d, with the model taken out.
//
// The real S8d (starkbot-neo plans/12-media-apps.md §5) hands a brief to Sol, which decomposes
// it into navigate() goals, and Jev picks a control per step. That needs a TypeSafe key and an
// inference connection. This script runs the same brief with the *choosing* removed and
// everything else kept: every mutation goes through a control the navigator can actually see —
// resolved by accessible name through the vendored candidate rules, never by CSS selector or
// direct dispatch — and every assertion is read back from the read-only grounding API.
//
// So a pass here does not say "the agent can do it". It says the surface it would have to work
// through is complete, reachable and honest, and that the brief is achievable without touching
// the CLI, the MCP server, or any handle the navigator does not have. A failure here would
// have failed the real S8d too.
//
//   node crates/dpaint-studio/tests/a11y/s8d.mjs --url http://127.0.0.1:4321

import { readFileSync, existsSync, mkdtempSync, rmSync, statSync, readdirSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const CANDIDATES = readFileSync(join(HERE, 'candidates.js'), 'utf8');

const argv = process.argv.slice(2);
const arg = (k, d) => { const i = argv.indexOf(k); return i === -1 ? d : argv[i + 1]; };
const URL_ = arg('--url', 'http://127.0.0.1:4321');
const TAKE = arg('--take', '');
const OUT = arg('--out', '/tmp/s8d');
const JSON_OUT = argv.includes('--json');

const steps = [];
const step = (name, ok, detail) => { steps.push({ name, ok, detail }); console.error(`${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? ' — ' + detail : ''}`); };

function browserPath() {
  if (process.env.CHROME && existsSync(process.env.CHROME)) return process.env.CHROME;
  for (const dir of (process.env.PATH || '').split(':'))
    for (const n of ['chromium', 'chromium-browser', 'google-chrome-stable', 'google-chrome'])
      if (existsSync(join(dir, n))) return join(dir, n);
  throw new Error('no Chromium-family browser found; set $CHROME');
}

class Cdp {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map(); this.session = null;
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data); const p = this.pending.get(m.id);
      if (p) { this.pending.delete(m.id); m.error ? p.reject(new Error(m.error.message)) : p.resolve(m.result); }
    });
  }
  call(method, params = {}, useSession = true) {
    const id = ++this.id; const payload = { id, method, params };
    if (useSession && this.session) payload.sessionId = this.session;
    this.ws.send(JSON.stringify(payload));
    return new Promise((res, rej) => {
      this.pending.set(id, { resolve: res, reject: rej });
      setTimeout(() => this.pending.has(id) && (this.pending.delete(id), rej(new Error(`${method} timed out`))), 60000);
    });
  }
  async evaluate(expression) {
    const r = await this.call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || 'evaluate threw');
    return r.result.value;
  }
}

async function launch() {
  const profile = mkdtempSync(join(tmpdir(), 's8d-'));
  const proc = spawn(browserPath(), [
    `--user-data-dir=${profile}`, '--headless=new', '--no-first-run', '--no-default-browser-check',
    '--remote-debugging-port=0', '--window-size=1440,900', 'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  const port = await new Promise((res, rej) => {
    let buf = ''; const t = setTimeout(() => rej(new Error('no debugging port')), 20000);
    proc.stderr.on('data', (d) => { buf += d; const m = buf.match(/ws:\/\/127\.0\.0\.1:(\d+)\//); if (m) { clearTimeout(t); res(m[1]); } });
  });
  const v = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
  const ws = new WebSocket(v.webSocketDebuggerUrl);
  await new Promise((r) => ws.addEventListener('open', r, { once: true }));
  const cdp = new Cdp(ws);
  const { targetId } = await cdp.call('Target.createTarget', { url: 'about:blank' }, false);
  const { sessionId } = await cdp.call('Target.attachToTarget', { targetId, flatten: true }, false);
  cdp.session = sessionId;
  await cdp.call('Page.enable'); await cdp.call('Runtime.enable');
  return { cdp, close: () => { try { ws.close(); } catch {} proc.kill('SIGKILL'); rmSync(profile, { recursive: true, force: true }); } };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Every interaction below goes through this: find a candidate by the accessible name the
// navigator would read, and act on it the way `act.js` does. Nothing addresses the DOM.
const byName = (pattern, role) => `(() => {
  const found = (${CANDIDATES.replace(/^\(\(\) => \{/, '(() => {')})();
  return found;
})()`;

async function candidates(cdp) { return (await cdp.evaluate(CANDIDATES)).candidates; }

async function actOn(cdp, kind, namePattern, value, wantRole) {
  // Resolve by accessible name inside the page, using the same name algorithm, then act.
  const script = `(() => {
    const roles=['button','link','checkbox','radio','switch','tab','menuitem','menuitemradio','option','gridcell','combobox','textbox','searchbox','spinbutton'];
    const sel='a[href],button,input,textarea,select,summary,[contenteditable]:not([contenteditable="false"]),'+roles.map(r=>'[role="'+r+'"]').join(',');
    const parent=e=>e?.assignedSlot||e?.parentElement||e?.getRootNode?.().host||null;
    const closest=(e,s)=>{for(let n=e;n;n=parent(n))if(n.matches?.(s))return n;return null;};
    const name=(e,seen=new Set())=>{ if(!e||seen.has(e))return ''; seen.add(e);
      const ref=(e.getAttribute?.('aria-labelledby')||'').split(/\\s+/).map(id=>name(e.ownerDocument.getElementById(id),seen)).filter(Boolean).join(' ');
      return ref||e.getAttribute?.('aria-label')||[...(e.labels||[])].map(l=>name(l,seen)).filter(Boolean).join(' ')||
        (['button','submit','reset'].includes(e.type)?e.value:'')||e.getAttribute?.('alt')||
        (e.tagName==='INPUT'?'':[...(e.childNodes||[])].map(n=>n.nodeType===3?n.textContent:n.nodeType===1&&n.getAttribute('aria-hidden')!=='true'?name(n,seen):'').join(' ').trim())||
        e.getAttribute?.('title')||e.getAttribute?.('placeholder')||''; };
    const re = new RegExp(${JSON.stringify(namePattern)}, 'i');
    // A modal replaces the element table. Both observers enforce this — AT-SPI through
    // State::Modal, the web snapshot through the dialog's own subtree — so a script that
    // reaches past an open dialog to the toolbar behind it is testing something no navigator
    // can do. Scope to the topmost visible modal whenever there is one.
    const modal = [...document.querySelectorAll('[role=dialog][aria-modal="true"], dialog[open]')]
      .filter(d => d.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})).pop();
    const root = modal || document;
    const roleOf = e => e.getAttribute('role') || (e.tagName==='BUTTON'?'button':e.tagName==='SELECT'?'combobox':
      e.type==='checkbox'?'checkbox':e.type==='number'?'spinbutton':e.tagName==='INPUT'||e.tagName==='TEXTAREA'?'textbox':'');
    const want = ${JSON.stringify(wantRole ?? null)};
    const hits = [...root.querySelectorAll(sel)].filter(e => {
      if (closest(e,'[aria-hidden="true"],[inert]') || !e.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})) return false;
      if (want && roleOf(e) !== want) return false;
      return re.test(name(e));
    });
    if (!hits.length) return { ok:false, why:'no visible control named /'+${JSON.stringify(namePattern)}+'/'+(modal?' inside the open dialog':'') };
    const el = hits[0];
    const label = name(el);
    ${kind === 'click' ? `el.click(); return { ok:true, label };`
      : kind === 'fill' ? `el.focus(); el.value = ${JSON.stringify(String(value ?? ''))};
          el.dispatchEvent(new Event('input',{bubbles:true})); el.dispatchEvent(new Event('change',{bubbles:true}));
          return { ok:true, label, value: el.value };`
      : `el.focus(); el.value = ${JSON.stringify(String(value ?? ''))};
          el.dispatchEvent(new Event('change',{bubbles:true})); return { ok:true, label, value: el.value };`}
  })()`;
  const r = await cdp.evaluate(script);
  if (!r.ok) throw new Error(r.why);
  return r;
}

const api = async (path) => (await fetch(new URL(path, URL_))).json();
const status = (cdp) => cdp.evaluate(`document.querySelector('[role=status]')?.innerText.trim() || ''`);

// --- the brief ------------------------------------------------------------------------

const { cdp, close } = await launch();
const report = { url: URL_, brief: "start a project 'acme-promo' 1080x1350, bring in the microphone take, put the title 'Loud on purpose' in Inter Bold across the top, make sure lint is clean, export a PNG and send it to the editor" };

try {
  await cdp.call('Page.navigate', { url: URL_ });
  for (let i = 0; i < 200 && !(await cdp.evaluate('!!document.querySelector("[role=toolbar]")')); i++) await sleep(50);
  await sleep(600);

  // The server keeps whatever project it last opened, and the browser is a fresh tab onto
  // it — so a previous run can leave both a project and an open dialog behind. Clear both
  // through the controls a person would use, so the run starts from the Welcome screen
  // every time instead of only the first time.
  // The same selector `actOn` scopes to, or this clears a dialog that is not the one
  // blocking the next step.
  const openModal = `[...document.querySelectorAll('[role=dialog][aria-modal="true"], dialog[open]')]
      .filter(d => d.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})).length`;
  for (let i = 0; i < 3 && (await cdp.evaluate(openModal)); i++) {
    await actOn(cdp, 'click', '^Cancel', null, 'button').catch(() => {});
    await sleep(400);
  }
  if (await cdp.evaluate(`!!document.querySelector('#btnCloseProject:not([hidden])')`)) {
    // Closing asks first — `Close project` opens a confirmation whose affirmative button
    // repeats the consequence with the project's name on it, so the toolbar button and the
    // confirming button share a prefix and only the second one is inside a dialog.
    await actOn(cdp, 'click', '^Close project', null, 'button');
    await sleep(500);
    await actOn(cdp, 'click', '^Close project ', null, 'button').catch(() => {});
    await sleep(1200);
  }

  // 1 — a project, into a directory that must not already exist. The first draft of this
  // script pointed at $OUT itself, which a previous run had already turned into a project:
  // the create failed with `exists`, the dialog stayed open, and the status assertion passed
  // anyway because it was reading the project the server still had open. An assertion that
  // cannot tell "made it" from "it was already there" is not an assertion.
  const projDir = join(OUT, 'acme-promo');
  if (existsSync(projDir)) throw new Error(`${projDir} already exists; the run needs a clean directory`);
  await actOn(cdp, 'click', '^New project$', null, 'button');
  await sleep(400);
  await actOn(cdp, 'fill', 'folder|directory|location|^path', projDir, 'textbox');
  await actOn(cdp, 'fill', 'project name|^name', 'acme-promo', 'textbox');
  await actOn(cdp, 'fill', 'size', '1080x1350', 'textbox').catch(() => {});
  await actOn(cdp, 'click', '^Create project', null, 'button');
  await sleep(1800);

  const stillOpen = await cdp.evaluate(`[...document.querySelectorAll('[role=dialog][aria-modal="true"]')]
    .filter(d => d.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})).length`);
  step('the dialog closed on success', stillOpen === 0, stillOpen ? `${stillOpen} modal still open: ${await status(cdp)}` : '');

  let st = await api('/api/v1/status');
  step('project created through the New project dialog',
    st.project?.name === 'acme-promo' && String(st.project?.root ?? '').startsWith(projDir) && st.revision === 0,
    `name=${JSON.stringify(st.project?.name)} root=${JSON.stringify(st.project?.root)} rev=${st.revision}`);

  const ov = await api('/api/v1/overview');
  const size = JSON.stringify(ov.documents?.[0]?.size ?? ov.documents?.[0]);
  step('canvas is 1080x1350', /1080/.test(size) && /1350/.test(size), size);

  // 2 — the take, through the Import dialog and its sidecar
  if (TAKE) {
    await actOn(cdp, 'click', '^Import file into');
    await sleep(400);
    await actOn(cdp, 'fill', 'path|file', TAKE);
    await actOn(cdp, 'click', '^Import ');
    await sleep(2500);
    const after = await api('/api/v1/status');
    step('take imported', after.revision > st.revision, `rev ${st.revision} -> ${after.revision}`);
    const dg = await api(`/api/v1/doc/${after.activeDoc}/digest`);
    const prov = JSON.stringify(dg).match(/microphone|chrome mic|prompt/i);
    step('the DMS sidecar became provenance', !!prov, prov ? prov[0] : 'no prompt found in the digest');
    st = after;
  }

  // Every field below is named by its *path* in the generated form — `family`,
  // `fill variant`, `fill.color` — not by the word a person would guess.

  // 3a — the face. The engine's FontSet holds the embedded fallback plus whatever the
  // project registered; it never reads the system's fonts. So "in Inter Bold" is a request
  // to *embed* Inter, and skipping this does not produce Inter, it produces a silent
  // fallback — which is exactly what the digest is checked for below.
  const INTER = '/home/andy/.local/share/fonts/openphase/Inter-700.ttf';
  if (!existsSync(INTER)) throw new Error(`the brief asks for Inter Bold and ${INTER} is missing`);
  await actOn(cdp, 'fill', '^Run op$', 'font register');
  await sleep(500);
  await actOn(cdp, 'click', 'font\\.register');
  await sleep(700);
  await actOn(cdp, 'fill', '^path', INTER);
  await actOn(cdp, 'fill', '^family', 'Inter');
  await actOn(cdp, 'fill', '^weight', '700');
  await actOn(cdp, 'click', '^Run font\\.register');
  await sleep(2000);

  // 3b — the title, through the command palette and the generated form
  await actOn(cdp, 'fill', '^Run op$', 'text add');
  await sleep(500);
  const opts = await cdp.evaluate(`[...document.querySelectorAll('[role=option]')].map(e=>e.textContent.trim()).filter(t=>/text\\.add/.test(t))`);
  step('the palette offers raster.text.add', opts.length > 0, opts[0]?.slice(0, 50));
  await actOn(cdp, 'click', 'raster\\.text\\.add');
  await sleep(700);
  // Deliberately not `^x`/`^y`: those also name the Selection region's transform spinbuttons,
  // and filling one of those emits a `raster.layer.transform` op against whatever is selected.
  // `fill` is a Paint, a tagged union, so its variant is chosen before `fill.color` exists.
  for (const [pat, val] of [['^text', 'Loud on purpose'], ['^name$|^name\\b', 'title'],
                            ['^family', 'Inter'], ['^size', '96'], ['^weight', '700']]) {
    await actOn(cdp, 'fill', pat, val);
  }
  await actOn(cdp, 'select', '^fill variant$', 'solid');
  await sleep(300);
  await actOn(cdp, 'fill', '^fill\\.color', '#ffffff');
  await actOn(cdp, 'click', '^Run raster\\.text\\.add');
  await sleep(2500);
  const sTitle = await status(cdp);
  step('the title op was applied and announced', /applied raster\.text\.add/.test(sTitle), sTitle.split('\n')[0]);

  st = await api('/api/v1/status');
  const dg = await api(`/api/v1/doc/${st.activeDoc}/digest`);
  const title = (dg.tree || dg.data?.tree || []).find((n) => n.name === 'title');
  step('a layer named title exists', !!title);
  // The digest names the face that was actually used, so this can tell "Inter" from "we
  // asked for Inter and silently got the fallback". Before the digest carried it, the only
  // available check was the absence of a key the digest never emitted — which always passed.
  step('the requested font resolved without falling back',
    !!title && !title.fontFallback && /inter/i.test(title.font || ''),
    title ? `font=${title.font} fontFallback=${title.fontFallback ?? 'null'}` : 'no title layer');

  // 4 — lint, and then the thing lint exists for. The brief says "make sure lint is clean",
  // which is not an assertion, it is an instruction: read the finding, act on the selector it
  // names, look again. This is `dp-fix-lint` with the model's choice replaced by one rule.
  let lint = await api(`/api/v1/doc/${st.activeDoc}/lint`);
  step('lint reports what an agent cannot see', Array.isArray(lint.findings),
    `${lint.errors} errors, ${lint.warnings} warnings` +
    (lint.findings?.length ? ': ' + lint.findings.map((f) => `${f.rule} ${f.target ?? ''}`).join(', ') : ''));

  for (let attempt = 0; attempt < 3 && lint.errors > 0; attempt++) {
    const bad = lint.findings.find((f) => f.rule === 'low-contrast');
    if (!bad) break;
    // The finding names the offender, so the fix is directly actionable: the title is
    // white, so give it a dark plate to read against rather than guessing at the title.
    // No `.catch` on these — every one is a real field path, and a swallowed fill is how
    // this step used to "pass" while doing nothing.
    await actOn(cdp, 'fill', '^Run op$', 'layer add');
    await sleep(500);
    await actOn(cdp, 'click', 'raster\\.layer\\.add');
    await sleep(700);
    for (const [pat, val] of [['^type', 'fill'], ['^color', '#101014'], ['^name$|^name\\b', 'title-plate']]) {
      await actOn(cdp, 'fill', pat, val);
    }
    await actOn(cdp, 'click', '^Run raster\\.layer\\.add');
    await sleep(2500);
    // The plate lands on top; move it under the title so the title is what reads.
    await actOn(cdp, 'click', '^Move layer title up').catch(() => {});
    await sleep(1500);
    lint = await api(`/api/v1/doc/${st.activeDoc}/lint`);
  }
  step('lint is clean after acting on the finding', lint.errors === 0,
    `${lint.errors} errors` + (lint.findings?.length ? ': ' + lint.findings.map((f) => `${f.rule} ${f.target ?? ''} ${f.value ?? ''}`).join(', ') : ''));

  // 5 — export. Into the project directory, which step 1 proved did not exist: a stale PNG
  // from a previous run at a shared path makes `existsSync` a lie, and makes the primary
  // button read `Overwrite …` while the export itself is refused.
  const png = join(projDir, 'acme-promo.png');
  await actOn(cdp, 'click', '^Export document', null, 'button');
  await sleep(400);
  await actOn(cdp, 'fill', 'path', png, 'textbox');
  await sleep(300);
  // The dialog's primary button renames itself to the consequence — `Export acme-promo.png` —
  // which is the §3.5 rule doing its job. Match the button, not the "Overwrite an existing
  // file" checkbox that shares the word and comes first in the DOM.
  await actOn(cdp, 'click', '^(Export|Overwrite) ', null, 'button');
  await sleep(4000);
  const exportModal = await cdp.evaluate(`[...document.querySelectorAll('[role=dialog][aria-modal="true"]')]
    .filter(d => d.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})).length`);
  step('the export dialog closed', exportModal === 0, exportModal ? await status(cdp) : '');
  step('the PNG exists', existsSync(png), existsSync(png) ? `${statSync(png).size} bytes` : png);

  // 6 — hand-off. The editor folder is shared across runs by design, so a second run finds
  // the first one's files there and the dialog refuses until overwrite is confirmed. That
  // refusal is the app being careful, not a failure: read it and answer it, which is what
  // an agent has to do too.
  await actOn(cdp, 'click', '^Send documents to editor', null, 'button');
  await sleep(400);
  await actOn(cdp, 'click', '^Send ', null, 'button');
  await sleep(1500);
  if (/already exist/i.test(await status(cdp))) {
    // The refusal ticks "Overwrite existing files" for you and renames the primary button
    // to `Overwrite and send N files to editor` — the §3.5 rule again. So the answer is to
    // press the renamed button; ticking the box here would untick it and refuse twice.
    await actOn(cdp, 'click', '^Overwrite and send ', null, 'button');
  }
  await sleep(4000);
  const sSend = await status(cdp);
  // Do not guess the destination. The plan pins `~/Movies/degen-paint/<project>/` but says to
  // honour `$XDG_VIDEOS_DIR` first, and on this machine that is `~/Videos` — so the folder is
  // read out of the sentence the app put in the status region, which is also the only way the
  // agent learns it.
  const written = [...sSend.matchAll(/(\/[^\s·]+\.(?:png|svg|glb|json))/g)].map((m) => m[1]);
  const sent = written.length ? dirname(written[0]) : null;
  step('files reached the editor folder', !!sent && existsSync(sent) && readdirSync(sent).length > 0,
    sent ? `${sent}: ${readdirSync(sent).join(', ')}` : sSend.split('\n')[0]);
  if (sent) {
    const car = readdirSync(sent).find((f) => f.endsWith('.json'));
    if (car) {
      const j = JSON.parse(readFileSync(join(sent, car), 'utf8'));
      step('the sidecar names the project and revision',
        /acme-promo/.test(JSON.stringify(j.project)) && j.revision !== undefined,
        `project=${JSON.stringify(j.project)} revision=${j.revision}`);
    } else step('the sidecar names the project and revision', false, `no .json sidecar in ${sent}`);
  }
} catch (e) {
  step('run completed', false, e.message);
} finally {
  close();
}

report.steps = steps;
report.failed = steps.filter((s) => !s.ok).length;
if (JSON_OUT) console.log(JSON.stringify(report, null, 2));
console.error(`\n${steps.length - report.failed}/${steps.length} steps passed`);
process.exit(report.failed ? 1 : 0);
