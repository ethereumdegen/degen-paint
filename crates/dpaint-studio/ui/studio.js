// degen-paint studio — the same module in a browser tab and in the Tauri webview.
//
// One transport indirection is the whole portability story: if the Tauri shell injected
// `window.__DPAINT_INVOKE__` we use it, otherwise we POST to the local server. Everything
// below is transport-agnostic.

const $ = (id) => document.getElementById(id);

const state = {
  project: null,
  documents: [],
  activeDoc: null,
  catalog: [],
  tree: [],
  selection: null,
  revision: -1,
  zoom: 1,
  fitZoom: 1,
  pan: { x: 0, y: 0 },
  renderSize: [0, 0],
  currentOp: null,
  dryRun: false,
  log: [],
  lint: null,
  paletteIndex: 0,
  paletteMatches: [],
};

// ---------------------------------------------------------------- transport

/** Structured error matching the engine's shape, whichever transport produced it. */
class ApiError extends Error {
  constructor(detail) {
    super(detail?.message || "request failed");
    this.code = detail?.code || "error";
    this.candidates = detail?.candidates || [];
    this.suggestion = detail?.suggestion || null;
  }
}

async function call(method, params = {}) {
  const started = performance.now();
  try {
    let result;
    if (window.__DPAINT_INVOKE__) {
      result = await window.__DPAINT_INVOKE__(method, params);
    } else {
      const res = await fetch("/api", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ method, params }),
      });
      const body = await res.json();
      if (!body.ok) throw new ApiError(body.error);
      result = body.result;
    }
    logCall(method, params, result, null, performance.now() - started);
    return result;
  } catch (e) {
    const err = e instanceof ApiError ? e : new ApiError(normalizeError(e));
    logCall(method, params, null, err, performance.now() - started);
    throw err;
  }
}

/** Tauri rejects with a JSON string; the browser path rejects with an ApiError already. */
function normalizeError(e) {
  if (e && typeof e === "object" && e.code) return e;
  if (typeof e === "string") {
    try {
      return JSON.parse(e);
    } catch {
      return { code: "error", message: e };
    }
  }
  return { code: "error", message: String(e?.message || e) };
}

async function renderUrl(doc, max = 1600) {
  if (window.__DPAINT_RENDER_URL__) {
    return await window.__DPAINT_RENDER_URL__({ doc, scale: 1, max });
  }
  const q = new URLSearchParams({ max: String(max), _: String(Date.now()) });
  if (doc) q.set("doc", doc);
  return `/render.png?${q.toString()}`;
}

// ---------------------------------------------------------------- console log

function logCall(method, params, result, error, ms) {
  state.log.unshift({ method, params, result, error, ms, at: new Date() });
  state.log = state.log.slice(0, 200);
  renderConsole();
}

function renderConsole() {
  const errors = state.log.filter((r) => r.error).length;
  const pill = $("conCount");
  pill.hidden = errors === 0;
  pill.textContent = String(errors);
  pill.classList.toggle("err", errors > 0);

  $("tab-console").innerHTML = state.log
    .map((r) => {
      const head = `<span class="op-id">${esc(r.method)}</span> <span class="read">${r.ms.toFixed(0)}ms</span>`;
      if (r.error) {
        const cand = r.error.candidates?.length
          ? `<div class="cerr">available: ${r.error.candidates.slice(0, 12).map(esc).join(", ")}</div>`
          : "";
        const sug = r.error.suggestion
          ? `<div class="cerr">did you mean <b>${esc(r.error.suggestion)}</b></div>`
          : "";
        return `<div class="crow err">${head}
          <div class="cerr"><b>${esc(r.error.code)}</b> ${esc(r.error.message)}</div>${cand}${sug}</div>`;
      }
      const summary = summarize(r.method, r.params, r.result);
      return `<div class="crow">${head}${summary ? `<div class="cerr">${esc(summary)}</div>` : ""}</div>`;
    })
    .join("");
}

function summarize(method, params, result) {
  if (method === "op") {
    const bits = [];
    if (result?.changed?.length) bits.push(`changed ${result.changed.join(", ")}`);
    if (result?.created?.length) bits.push(`created ${result.created.join(", ")}`);
    if (result?.removed?.length) bits.push(`removed ${result.removed.join(", ")}`);
    return `${params.op}${bits.length ? " — " + bits.join("; ") : ""}`;
  }
  if (method === "undo" || method === "redo") return result?.op || "nothing to do";
  return "";
}

// ---------------------------------------------------------------- state sync

async function refresh({ viewport = true } = {}) {
  const st = await call("state");
  state.project = st.project;
  state.documents = st.documents;
  if (!state.activeDoc || !state.documents.some((d) => d.id === state.activeDoc)) {
    state.activeDoc = st.project.active;
  }
  state.revision = st.revision;

  $("projectName").textContent = st.project.name;
  $("projectName").title = st.project.root;
  $("revBadge").textContent = `rev ${st.revision}`;
  $("btnUndo").disabled = !st.canUndo;
  $("btnRedo").disabled = !st.canRedo;

  renderDocs();
  renderTree();
  await renderHistory();
  if (viewport) await refreshViewport();
}

function activeDocument() {
  return state.documents.find((d) => d.id === state.activeDoc) || null;
}

function renderDocs() {
  $("docList").innerHTML = state.documents
    .map((d) => {
      const size = d.size ? `${Math.round(d.size[0])}×${Math.round(d.size[1])}` : "scene";
      const deps = d.dependsOn?.length ? `<div class="empty-note">links ${d.dependsOn.map(esc).join(", ")}</div>` : "";
      return `<div class="doc ${d.id === state.activeDoc ? "active" : ""}" data-doc="${esc(d.id)}">
        <span class="badge ${esc(d.kind)}">${esc(d.kind)}</span>
        <b>${esc(d.name)}</b>
        <span class="read">${size}</span>${deps}
      </div>`;
    })
    .join("");
  for (const el of $("docList").querySelectorAll(".doc")) {
    el.onclick = async () => {
      state.activeDoc = el.dataset.doc;
      state.selection = null;
      renderDocs();
      renderTree();
      await refreshViewport({ fit: true });
    };
  }
}

function renderTree() {
  const doc = activeDocument();
  state.tree = doc?.objects || [];
  $("treeCount").textContent = state.tree.length ? `${state.tree.length}` : "";
  $("docRead").textContent = doc ? `${doc.name} · ${doc.kind}` : "—";

  if (!state.tree.length) {
    $("treeList").innerHTML = `<div class="hint">No objects yet. Run an op to add one.</div>`;
    return;
  }
  // Bottom of the stack is the end of the list, matching how a layers panel reads.
  $("treeList").innerHTML = state.tree
    .slice()
    .reverse()
    .map((n) => {
      const hidden = n.visible === false;
      const sel = state.selection === n.id;
      const opacity = n.opacity < 1 ? `<span class="read">${Math.round(n.opacity * 100)}%</span>` : "";
      const blend = n.blend && n.blend !== "normal" ? `<span class="tagx">${esc(n.blend)}</span>` : "";
      return `<div class="node ${hidden ? "hidden" : ""} ${sel ? "sel" : ""} ${n.depth ? "nested" : ""}"
                   data-id="${esc(n.id)}" style="--depth:${n.depth}">
        <button class="tb sm eye" data-eye="${esc(n.id)}" title="Toggle visibility">${hidden ? "○" : "●"}</button>
        <b>${esc(n.name || n.id)}</b>
        <span class="tagx">${esc(n.type)}</span>${blend}${opacity}
      </div>`;
    })
    .join("");

  for (const el of $("treeList").querySelectorAll(".node")) {
    el.onclick = (ev) => {
      if (ev.target.dataset.eye) return;
      selectObject(el.dataset.id);
    };
  }
  for (const btn of $("treeList").querySelectorAll("[data-eye]")) {
    btn.onclick = async (ev) => {
      ev.stopPropagation();
      const id = btn.dataset.eye;
      const node = state.tree.find((n) => n.id === id);
      const doc = activeDocument();
      const op = doc.kind === "raster" ? "raster.layer.set" : "vector.style.opacity";
      const args =
        doc.kind === "raster"
          ? { target: `#${id}`, visible: node.visible === false }
          : { target: `#${id}`, opacity: node.visible === false ? 1 : 0 };
      try {
        await call("op", { op, args, doc: state.activeDoc });
        await refresh();
      } catch (e) {
        showError(e);
      }
    };
  }
}

function selectObject(id) {
  state.selection = id;
  renderTree();
  const field = document.querySelector('[data-field="target"]');
  if (field) field.value = `#${id}`;
  $("inspectTarget").textContent = `#${id}`;
}

async function renderHistory() {
  const h = await call("history", { limit: 60 });
  const newest = h.entries[0]?.seq;
  $("tab-history").innerHTML = h.entries.length
    ? h.entries
        .map(
          (e) => `<div class="hrow ${e.undone ? "undone" : ""} ${e.seq === newest ? "fresh" : ""}">
            <span class="read">${e.seq}</span>
            <span class="actor ${esc(e.actor)}">${esc(e.actor)}</span>
            <span class="op-id">${esc(e.op)}</span>
            <span class="read">${esc((e.changed || []).join(", "))}</span>
            ${e.undone ? '<span class="tagx">undone</span>' : ""}
          </div>`
        )
        .join("")
    : `<div class="hint">Nothing yet. Every edit — yours or an agent's — lands here.</div>`;
}

// ---------------------------------------------------------------- viewport

async function refreshViewport({ fit = false } = {}) {
  const img = $("canvasImg");
  $("busy").hidden = false;
  try {
    const url = await renderUrl(state.activeDoc, 1600);
    await new Promise((resolve, reject) => {
      img.onload = resolve;
      img.onerror = () => reject(new ApiError({ code: "render_failed", message: "render failed" }));
      img.src = url;
    });
    state.renderSize = [img.naturalWidth, img.naturalHeight];
    $("sizeRead").textContent = `${img.naturalWidth} × ${img.naturalHeight}`;
    $("canvasEmpty").hidden = true;
    if (fit || state.zoom === null) fitView();
    else applyView();
  } catch (e) {
    $("canvasEmpty").hidden = false;
    showError(e);
  } finally {
    $("busy").hidden = true;
  }
}

function fitView() {
  const area = $("canvasArea").getBoundingClientRect();
  const [w, h] = state.renderSize;
  if (!w || !h) return;
  state.fitZoom = Math.min((area.width - 48) / w, (area.height - 48) / h, 1);
  state.zoom = state.fitZoom;
  state.pan = { x: 0, y: 0 };
  applyView();
}

function applyView() {
  const pan = $("canvasPan");
  const [w, h] = state.renderSize;
  // #canvasPan is anchored at the centre of the area, so offset by half the *scaled*
  // size to put the middle of the document under the middle of the viewport.
  const cx = (w * state.zoom) / 2;
  const cy = (h * state.zoom) / 2;
  pan.style.transformOrigin = "0 0";
  pan.style.transform =
    `translate(${state.pan.x - cx}px, ${state.pan.y - cy}px) scale(${state.zoom})`;
  $("canvasImg").classList.toggle("pixelated", state.zoom >= 2);
  $("zoomRead").textContent = `${Math.round(state.zoom * 100)}%`;
}

function installViewportControls() {
  const area = $("canvasArea");
  let dragging = false;
  let last = null;

  area.addEventListener("mousedown", (e) => {
    dragging = true;
    last = { x: e.clientX, y: e.clientY };
    area.classList.add("panning");
  });
  window.addEventListener("mouseup", () => {
    dragging = false;
    area.classList.remove("panning");
  });
  window.addEventListener("mousemove", (e) => {
    if (!dragging) return;
    state.pan.x += e.clientX - last.x;
    state.pan.y += e.clientY - last.y;
    last = { x: e.clientX, y: e.clientY };
    applyView();
  });
  area.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      const k = Math.exp(-e.deltaY / 400);
      state.zoom = Math.min(16, Math.max(0.05, state.zoom * k));
      applyView();
    },
    { passive: false }
  );

  $("btnFit").onclick = fitView;
  $("btnOneToOne").onclick = () => {
    state.zoom = 1;
    state.pan = { x: 0, y: 0 };
    applyView();
  };
}

// ---------------------------------------------------------------- command palette

async function loadCatalog() {
  state.catalog = await call("catalog");
}

function openPalette() {
  $("paletteWrap").hidden = false;
  const input = $("paletteInput");
  input.value = "";
  input.focus();
  filterPalette("");
}

function closePalette() {
  $("paletteWrap").hidden = true;
}

function filterPalette(q) {
  const needle = q.trim().toLowerCase();
  const doc = activeDocument();
  const matches = state.catalog
    .filter((op) => {
      if (needle && !(op.id.toLowerCase().includes(needle) || op.about.toLowerCase().includes(needle)))
        return false;
      // Ops declare the document kinds they apply to; hide the ones that cannot run.
      if (doc && op.modes?.length && !op.modes.includes(doc.kind)) return false;
      return true;
    })
    .slice(0, 120);
  state.paletteMatches = matches;
  state.paletteIndex = 0;
  drawPalette();
}

function drawPalette() {
  $("paletteList").innerHTML = state.paletteMatches
    .map(
      (op, i) => `<div class="pitem ${i === state.paletteIndex ? "on" : ""}" data-i="${i}">
        <div class="op-head">
          <span class="op-id">${esc(op.id)}</span>
          <span class="op-flags">
            ${op.query ? '<span class="flag query">read-only</span>' : ""}
            ${op.network ? '<span class="flag net">network</span>' : ""}
          </span>
        </div>
        <div class="op-about">${esc(op.about)}</div>
      </div>`
    )
    .join("");
  for (const el of $("paletteList").querySelectorAll(".pitem")) {
    el.onclick = () => choosePalette(Number(el.dataset.i));
  }
  const on = $("paletteList").querySelector(".pitem.on");
  if (on) on.scrollIntoView({ block: "nearest" });
}

async function choosePalette(i) {
  const op = state.paletteMatches[i];
  if (!op) return;
  closePalette();
  await openInspector(op.id);
}

// ---------------------------------------------------------------- schema-driven inspector

async function openInspector(opId) {
  const spec = await call("schema", { op: opId });
  state.currentOp = spec;
  const schema = spec.schema || {};
  const props = schema.properties || {};
  const required = schema.required || [];

  const fields = Object.entries(props)
    .map(([name, raw]) => field(name, resolveSchema(schema, raw), required.includes(name)))
    .join("");

  $("inspectTarget").textContent = state.selection ? `#${state.selection}` : "";
  $("inspector").innerHTML = `
    <div class="op-head"><span class="op-id">${esc(spec.id)}</span></div>
    <div class="op-about">${esc(spec.about)}</div>
    <form id="opForm">${fields || '<div class="hint">This op takes no arguments.</div>'}
      <div class="form-actions">
        <label class="dry"><input type="checkbox" id="dryRun" ${state.dryRun ? "checked" : ""}> dry run</label>
        <button type="submit" class="tb accent">Apply</button>
      </div>
      <div id="opResult"></div>
    </form>`;

  if (state.selection) {
    const t = document.querySelector('[data-field="target"]');
    if (t && !t.value) t.value = `#${state.selection}`;
  }
  $("opForm").onsubmit = submitOp;
}

/** Follow $ref / anyOf so optional and referenced types show the right control. */
function resolveSchema(root, spec) {
  if (!spec) return {};
  if (spec.$ref) {
    const name = spec.$ref.replace("#/$defs/", "");
    return resolveSchema(root, (root.$defs || {})[name] || {});
  }
  for (const key of ["anyOf", "oneOf", "allOf"]) {
    if (Array.isArray(spec[key])) {
      const hit = spec[key].map((s) => resolveSchema(root, s)).find((s) => s.type !== "null");
      if (hit) return { ...hit, description: spec.description || hit.description };
    }
  }
  return spec;
}

function typeOf(spec) {
  const t = spec.type;
  if (Array.isArray(t)) return t.find((x) => x !== "null") || "string";
  return t || (spec.enum ? "string" : "string");
}

function field(name, spec, isRequired) {
  const label = `${name.replace(/_/g, " ")}${isRequired ? " *" : ""}`;
  const desc = spec.description ? `<div class="hint">${esc(spec.description)}</div>` : "";
  const ty = typeOf(spec);
  let control;

  if (spec.enum) {
    control = `<select data-field="${esc(name)}" data-type="string">
      ${!isRequired ? '<option value=""></option>' : ""}
      ${spec.enum.map((v) => `<option value="${esc(v)}">${esc(v)}</option>`).join("")}</select>`;
  } else if (isColor(spec)) {
    control = `<input type="text" data-field="${esc(name)}" data-type="string" placeholder="#rrggbb">`;
  } else if (ty === "boolean") {
    control = `<input type="checkbox" data-field="${esc(name)}" data-type="boolean">`;
  } else if (ty === "number" || ty === "integer") {
    control = `<input type="number" step="any" data-field="${esc(name)}" data-type="${ty}">`;
  } else if (ty === "array") {
    control = `<input type="text" data-field="${esc(name)}" data-type="array" placeholder="1, 2, 3">`;
  } else if (ty === "object") {
    const sub = Object.keys(spec.properties || {}).join(", ");
    control = `<input type="text" data-field="${esc(name)}" data-type="object"
      placeholder='${esc(sub ? `{ "${Object.keys(spec.properties)[0]}": … }` : "{ }")}'>`;
  } else {
    control = `<input type="text" data-field="${esc(name)}" data-type="string">`;
  }
  return `<label class="field"><span>${esc(label)}</span>${control}${desc}</label>`;
}

function isColor(spec) {
  return typeof spec.pattern === "string" && spec.pattern.includes("0-9a-fA-F");
}

function collectArgs(schema) {
  const args = {};
  for (const el of document.querySelectorAll("#opForm [data-field]")) {
    const name = el.dataset.field;
    const ty = el.dataset.type;
    if (ty === "boolean") {
      if (el.checked) args[name] = true;
      continue;
    }
    const raw = el.value.trim();
    if (raw === "") continue;
    if (ty === "number") args[name] = Number(raw);
    else if (ty === "integer") args[name] = parseInt(raw, 10);
    else if (ty === "array")
      args[name] = raw.startsWith("[") ? JSON.parse(raw) : raw.split(",").map((s) => coerce(s.trim()));
    else if (ty === "object")
      args[name] = raw.startsWith("{") ? JSON.parse(raw) : singleRequired(schema, name, raw);
    else args[name] = raw;
  }
  return args;
}

/** `--text "HELLO"` ergonomics, mirrored from the CLI: one required field takes a scalar. */
function singleRequired(schema, name, raw) {
  const spec = resolveSchema(schema, (schema.properties || {})[name] || {});
  const req = spec.required || [];
  if (req.length === 1) return { [req[0]]: coerce(raw) };
  return raw;
}

function coerce(s) {
  if (s === "true") return true;
  if (s === "false") return false;
  const n = Number(s);
  return Number.isNaN(n) || s === "" ? s : n;
}

async function submitOp(ev) {
  ev.preventDefault();
  const spec = state.currentOp;
  if (!spec) return;
  state.dryRun = $("dryRun").checked;
  const out = $("opResult");
  try {
    const args = collectArgs(spec.schema || {});
    const res = await call("op", {
      op: spec.id,
      args,
      doc: state.activeDoc,
      dryRun: state.dryRun,
    });
    const bits = [];
    if (res.changed?.length) bits.push(`changed ${res.changed.join(", ")}`);
    if (res.created?.length) bits.push(`created ${res.created.join(", ")}`);
    if (res.removed?.length) bits.push(`removed ${res.removed.join(", ")}`);
    for (const w of res.warnings || []) bits.push(`⚠ ${w.code} ${w.target}: ${w.detail}`);
    out.className = `result ${state.dryRun ? "dry" : "ok"}`;
    out.textContent = state.dryRun
      ? `would ${bits.join("; ") || "do nothing"} (nothing written)`
      : bits.join("; ") || "applied";
    if (res.data) out.textContent += `\n${JSON.stringify(res.data, null, 1).slice(0, 2000)}`;
    if (!state.dryRun) await refresh();
  } catch (e) {
    out.className = "result err";
    out.textContent = `${e.code}: ${e.message}${e.suggestion ? `\ndid you mean ${e.suggestion}` : ""}`;
    showError(e);
  }
}

function showError(e) {
  selectTab("console");
}

// ---------------------------------------------------------------- lint

async function runLint() {
  try {
    const report = await call("lint", { doc: state.activeDoc });
    state.lint = report;
    const pill = $("lintCount");
    pill.hidden = report.findings.length === 0;
    pill.textContent = String(report.findings.length);
    pill.classList.toggle("err", report.errors > 0);

    $("tab-lint").innerHTML = report.findings.length
      ? report.findings
          .map(
            (f) => `<div class="lrow" data-target="${esc(f.target)}">
              <span class="sev ${esc(f.severity)}">${esc(f.severity)}</span>
              <span class="op-id">${esc(f.rule)}</span>
              <span class="tagx">${esc(f.target)}</span>
              <span class="read">${esc(f.detail)}</span>
              ${f.value != null ? `<span class="read">${fmt(f.value)}${f.required != null ? ` / need ${fmt(f.required)}` : ""}</span>` : ""}
            </div>`
          )
          .join("")
      : `<div class="hint">No findings. Lint checks contrast, overflow, off-canvas content and broken geometry.</div>`;

    for (const row of $("tab-lint").querySelectorAll(".lrow")) {
      row.onclick = async () => {
        const target = row.dataset.target;
        try {
          const matches = await call("select", { doc: state.activeDoc, selector: target });
          if (matches[0]) {
            selectObject(matches[0].id);
            selectTab("history");
            selectTab("lint");
          }
        } catch (e) {
          showError(e);
        }
      };
    }
  } catch (e) {
    showError(e);
  }
}

function fmt(v) {
  return typeof v === "number" ? (Math.abs(v) >= 100 ? v.toFixed(0) : v.toFixed(2)) : String(v);
}

// ---------------------------------------------------------------- tabs, keys, polling

function selectTab(name) {
  for (const t of document.querySelectorAll(".tab")) t.classList.toggle("active", t.dataset.tab === name);
  for (const p of document.querySelectorAll(".tabpane"))
    p.classList.toggle("active", p.id === `tab-${name}`);
}

function installChrome() {
  for (const t of document.querySelectorAll(".tab")) t.onclick = () => selectTab(t.dataset.tab);
  $("btnClearConsole").onclick = () => {
    state.log = [];
    renderConsole();
  };
  $("btnLint").onclick = runLint;
  $("btnPalette").onclick = openPalette;
  $("btnUndo").onclick = async () => {
    await call("undo");
    await refresh();
  };
  $("btnRedo").onclick = async () => {
    await call("redo");
    await refresh();
  };
  $("paletteBackdrop").onclick = closePalette;
  $("paletteInput").oninput = (e) => filterPalette(e.target.value);
  $("paletteInput").onkeydown = (e) => {
    if (e.key === "ArrowDown") {
      state.paletteIndex = Math.min(state.paletteMatches.length - 1, state.paletteIndex + 1);
      drawPalette();
      e.preventDefault();
    } else if (e.key === "ArrowUp") {
      state.paletteIndex = Math.max(0, state.paletteIndex - 1);
      drawPalette();
      e.preventDefault();
    } else if (e.key === "Enter") {
      choosePalette(state.paletteIndex);
      e.preventDefault();
    } else if (e.key === "Escape") {
      closePalette();
    }
  };

  window.addEventListener("keydown", (e) => {
    const typing = /input|textarea|select/i.test(document.activeElement?.tagName || "");
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "z") {
      e.preventDefault();
      (e.shiftKey ? $("btnRedo") : $("btnUndo")).click();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      openPalette();
      return;
    }
    if (typing) return;
    if (e.key === "/") {
      e.preventDefault();
      openPalette();
    } else if (e.key === "f") fitView();
    else if (e.key === "1") $("btnOneToOne").click();
  });

  window.addEventListener("resize", () => applyView());
}

/** Poll for writes from an agent, so the shared journal is visible on screen. */
function installSync() {
  setInterval(async () => {
    if (!$("paletteWrap").hidden) return;
    try {
      const st = await callQuiet("state");
      if (st && st.revision !== state.revision) {
        const flash = $("agentFlash");
        flash.hidden = false;
        flash.classList.remove("human");
        setTimeout(() => (flash.hidden = true), 2200);
        await refresh();
        await runLint();
      }
    } catch {
      /* the server may be restarting; the next tick retries */
    }
  }, 1000);
}

/** Polling must not fill the console with a line every second. */
async function callQuiet(method, params = {}) {
  if (window.__DPAINT_INVOKE__) return await window.__DPAINT_INVOKE__(method, params);
  const res = await fetch("/api", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ method, params }),
  });
  const body = await res.json();
  if (!body.ok) throw new ApiError(body.error);
  return body.result;
}

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// ---------------------------------------------------------------- boot

(async function main() {
  installChrome();
  installViewportControls();
  try {
    await loadCatalog();
    await refresh();
    fitView();
    await runLint();
    installSync();
  } catch (e) {
    $("inspector").innerHTML = `<div class="result err">${esc(e.code)}: ${esc(e.message)}</div>`;
    showError(e);
  }
})();
