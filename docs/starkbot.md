# Driven by Starkbot Neo — the operator-app contract

Plan for the v2 upgrade. degen-paint becomes the fourth media app that
[starkbot-neo](https://github.com/ethereumdegen/starkbot-neo) operates — beside Diffusion Studio,
Powermove and Degen Media Studio — and it is driven **the way Diffusion Studio is driven**: through
its UI, by the Jev navigator, over the accessibility tree. Not through the CLI, not through MCP.

Starkbot's constitution fixes the shape of this (`starkbot-neo/plans/00-decisions.md`, `12-media-apps.md`):

| Rule | Consequence for degen-paint |
|---|---|
| P3 — no shell, no general file access | `dpaint` and `dpaint mcp` are **never** a mutation path for Starkbot. Every edit is a click or a keystroke in the Studio. |
| P9 — no per-app profiles, no selectors | The UI must be self-describing: accessible names, roles, states. Hints may make Starkbot faster, never possible. Everything below must work with the `media-apps` pack disabled. |
| P10 — no vision for automation | Starkbot cannot see the viewport. The canvas is decoration; **every capability needs a DOM/AX control that names what it does.** Vision is used only to *judge* renders the app produced. |
| K4 — the app owns its keys and credits | fal/Quiver keys stay in degen-paint (already true). The Studio gets a Settings screen where the user types them; Starkbot's enablement flow stops there. |
| A19 — read-only grounding side channel | `dapi` for Diffusion Studio; for degen-paint, GET endpoints on `dpaint serve`. Grounding verifies a step; it never mutates. |
| Confirm cards for paid / destructive / outward actions | Paid controls state their price in their accessible name; destructive controls carry explicit labels ("Overwrite poster.png", "Delete document logo"). |

The existing agent surface (CLI, MCP, digest, lint) is untouched and stays the surface for coding
agents. This plan adds a second operator: a UI-driving agent.

## 1. How Starkbot sees degen-paint

| | degen-paint |
|---|---|
| Observes via | **macOS app** (`dpaint-studio-app`, bundle `dev.degenpaint.studio`, Tauri + WKWebView): `AxObserver`. **Linux app** (the same Tauri build on webkit2gtk, Wayland `app_id` `dev.degenpaint.studio`): the AT-SPI observer of §11. **Web UI** on both: `dpaint serve` at `127.0.0.1:4317` in Starkbot's managed Chrome/Chromium, CDP snapshot. Same UI bytes, so one DOM contract serves every path. |
| Typical work | open or create a project · import a DMS take or footage still · add and arrange layers, text, vector objects · run ops from the command palette with schema forms · read lint, fix what it names · export PNG/SVG/GLB · send to an editor |
| Credentials | `FAL_KEY`, `QUIVERAI_API_KEY` in degen-paint's own keychain / config (existing `dpaint-ai` key resolution); none in Starkbot |
| Read-only side channel | `GET http://127.0.0.1:4317/api/v1/…` (§4) |
| Hand-off in | files from DMS's send-to-editor folder (`~/Movies/Degen Media Studio/<studio>/`, take + `<take>.json` sidecar), through the Import dialog |
| Hand-off out | files to `~/Movies/degen-paint/<project>/` with a sidecar, through Export; Diffusion Studio / Powermove import from there |

## 2. What the navigator actually reads — and what that forces

These are measured facts from `starkbot-neo/crates/jev-nav/js/snapshot.js`,
`plans/10-navigator.md` and `plans/01-accessibility.md`. The UI design follows from them.

| Navigator fact | Design consequence |
|---|---|
| Web candidates are only: `a[href] button input textarea select summary [contenteditable]` and roles `button link checkbox radio switch tab menuitem menuitemradio option gridcell combobox textbox searchbox spinbutton`. **`treeitem`, `listitem`, `row` are not candidates.** | Layer/object/node lists are `role=listbox` of `role=option` rows. No custom `div` controls anywhere. |
| Accessible name = `aria-labelledby` → `aria-label` → `<label>` → button value → `alt` → text → `title` → `placeholder`. | Every control gets a name that says *what it acts on*: `Hide layer title`, not `Hide`. `title` alone is not a name. Icon glyphs (`↶`) never stand alone. |
| 250-candidate cap; no `(role, container)` group may take more than 40 %; surplus round-robin across groups; truncated candidates **cannot be selected**. | Each pane is its own labelled container. Lists are capped at 60 visible rows with a filter textbox. Default view ≤ 120 candidates, verified by an audit test (§6). |
| A candidate must be visible, enabled and have its centre inside the viewport. | No control lives only off-screen. Panes scroll independently; the important controls (Run, Export, Import, Undo) never scroll away. |
| After `fill` into a `role=combobox`, the navigator waits for a visible `[role=option]` under `aria-controls`. | The command palette is a real combobox: `input role=combobox aria-controls=paletteList` → `role=listbox` → `role=option`. Results appear on input, ≤ 12 rows. |
| Freshness guard = the innerText of the nearest `form, dialog, [role=dialog], article, li, tr, [role=row]`. | Each list row is an `li`; each form is a `form`; dialogs are `role=dialog`. A change to one row does not stale every click on the page. |
| `pageKey` includes every form control's value; "no progress" = 3 steps with no page change. | Every op result changes visible text (the status region, the revision badge, the history row), so a successful step is always "progress". |
| Native: `AXProgressIndicator` / `AXBusyIndicator` = busy; `AXSheet` / `AXDialog` subroles = modal and replace the element table. | `role=progressbar` while a job runs; `role=dialog aria-modal=true` for every dialog. |
| P10: Sol may `look` only at renders the app produced. | Stable render and annotated-preview URLs on the web path; an **Export preview** one-click action writing a deterministic path for the native path. |

## 3. The Studio, as a surface an agent can operate

Rebuild `crates/dpaint-studio/ui/` around this contract. Same three files, no bundler, no
framework — the constraint that lets the identical bytes ship in Tauri, `dpaint serve` and WASM.

### 3.1 Landmarks

Every pane is a labelled container: `<section role=region aria-label="…">` for
**Toolbar · Documents · Viewport · Tree · Inspector · Selection · Status · History · Lint · Console**.
Toolbar is `role=toolbar`. Dock tabs stay `role=tablist`/`tab` with `aria-controls` and
`aria-selected`.

### 3.2 Documents and the tree

- Documents: `role=listbox aria-label="Documents"`; each row `role=option` named
  `"campaign · raster · 2480×3508 · active"`. Selecting a row switches the active document.
- Tree (layers / objects / nodes by kind): `role=listbox aria-label="Layers"` (or Objects / Nodes);
  rows `role=option aria-selected aria-level`, named
  `"title · text · visible · 214,238 2052×392"` / `"sky · pixel · hidden · locked"` /
  `"msh_body · mesh · 1,204 tris"`. Groups are rows with `aria-expanded`.
- One candidate per row. Per-object actions (Hide/Show, Lock, Delete, Move up/down, Duplicate,
  Rename) live in the **Selection** region as buttons named with the object:
  `"Hide layer title"`, `"Delete object mark"`. The region also states the selection as text:
  `Selected: #title · text layer · doc campaign · bbox 214,238 2052×392`.
- A filter textbox `"Filter layers"` above the list; ≥ 60 rows → the list shows the first 60 and
  a `"Show 48 more"` button.

### 3.3 Command palette and schema forms

- `input role=combobox aria-label="Run op" aria-controls=paletteList aria-expanded aria-autocomplete=list`,
  results `role=listbox` → `role=option` named `"raster.filter.gaussian-blur — Blur a layer…"`,
  ≤ 12 rows, ranked by active document kind. `/` or `Ctrl/Cmd-K` opens it; `Esc` closes.
- Choosing an op opens its form in the Inspector as a real `<form aria-labelledby>`. Fields keep
  the generated schema widgets but gain: `<label for>` binding (already), `aria-describedby` →
  the description, `aria-required`, `aria-invalid` + an inline error on failed validation,
  `<select>` for enums, `spinbutton` semantics for numbers. Selector-typed fields (`layer`,
  `target`, `source`) are comboboxes whose options are the resolved candidates
  (`#title`, `#sky`, …) from `dispatch("select")`, so a bad selector is corrected before the op runs.
- The submit button is named with the op and, for paid ops, the quote:
  `"Run raster.filter.gaussian-blur"` · `"Run ai.image.generate · est. $0.03"` (§5). A `Dry run`
  checkbox stays.
- The result lands in the **Status** region (`role=status aria-live=polite`):
  `applied raster.filter.gaussian-blur · changed campaign · rev 42`, or the structured error with
  its candidate list and suggestion. That text is what a Jev `verify` reads.

### 3.4 Viewport

The `<img>`/`<canvas>` stays for humans. It is `aria-hidden` for the navigator; its information is
carried by text instead: an alt-text summary of the digest (`raster 2480×3508 · 5 layers · 1 lint
error`) in the Status region, and the tree. Pan/zoom/orbit buttons (`Fit`, `100 %`, `Zoom in`,
`Zoom out`, `Orbit left/right/up/down`, `Reset view`) are real buttons, so the human viewport
state is reachable without a mouse.

Direct-manipulation tools (move, scale, draw) never exist *only* on the canvas. The
transform/geometry of the selected object is editable as spinbuttons in the Inspector's Selection
section (`X`, `Y`, `W`, `H`, `Rotation`, `Opacity`), and each emits the existing op.

### 3.5 Dialogs and destructive actions

Every dialog is `role=dialog aria-modal=true aria-labelledby`, traps focus, closes on `Esc`, and
names its primary button with the consequence: `"Create project acme"`, `"Import 3 files"`,
`"Overwrite poster.png"`, `"Delete document logo"`, `"Send 2 files to editor"`. Those labels are
also what `dpaint skill` emits as `confirm_labels` (§7).

### 3.6 Busy state and jobs

Any op, render or AI call that can exceed ~300 ms runs as a **job** (§5). While one runs, the
Status region has `aria-busy=true` and a `role=progressbar aria-label="Rendering campaign"`
(indeterminate or with `aria-valuenow`), plus a `"Cancel job"` button. WebKit maps this to
`AXProgressIndicator`, which `neo-ax` reads as busy; the navigator treats WAIT as progress.

### 3.7 Native menus, shortcuts, keyboard

The Tauri app gets a real menu bar (`tauri::menu`): **File** New Project… · Open Project… · Open
Recent · Import… · Export… · Export Preview · Send to Editor… · Close Project; **Edit** Undo · Redo
· Delete · Select All; **View** Fit · 100 % · panes; **Help** Doctor. macOS exposes the menu bar
to `AxObserver` (`neo-ax` emits `menuitem` rows with shortcuts), which gives Jev `select_menu`
without any UI knowledge. The web build renders the same menu as a `role=menubar`.

Every action has a shortcut, documented in one table in `studio.js` (`SHORTCUTS`) that the
`?` overlay and `dpaint skill` both read. Full Tab order through every pane; no keyboard trap
except modal dialogs.

### 3.8 Multi-project Studio

Today a shell is bound to one project at launch. The Studio gains an **open project** state:
launch with none → a Welcome screen (`New project`, `Open project`, recent list as buttons);
`File › Open Project…` swaps it; `Close Project` returns to Welcome. `Studio` holds
`Option<Workspace>`; `dispatch("state")` reports `project: null` when nothing is open.

## 4. Files in, files out

Starkbot never moves a file (P3). Hand-off is the app's own import/export dialogs.

- **Import dialog** (`File › Import…`): a `"File path"` textbox **and** a `"Browse…"` button that
  opens the OS panel (`tauri-plugin-dialog`; on the web path `<input type=file>`). The typed path
  works on both paths today — the navigator cannot drive file choosers over CDP yet. Accepts
  PNG/JPG/WebP/TIFF/SVG/glTF/GLB. Destination radio: `"Into document campaign as a new layer"` /
  `"As a new document"`. If a `<name>.json` sidecar sits next to the file (DMS's format), the
  prompt, model and lineage are recorded as provenance on the created layer/object, the way
  `ai.*` ops already do.
- **Export dialog** (`File › Export…`): document, format, scale/DPI preset, path textbox + Browse,
  and for model docs `turntable frames` (PNG sequence + contact sheet). An existing file makes the
  primary button `"Overwrite <name>"`. Writes through `dpaint-render`; the Status region reports
  the written path and size.
- **Send to Editor** (`File › Send to Editor…`): exports the selected documents to
  `~/Movies/degen-paint/<project>/` as PNG (raster and rasterised vector at a chosen size), SVG and
  GLB, each with `<name>.json` `{ project, document, revision, digest summary, provenance }` —
  the mirror image of DMS's hand-off, so Diffusion Studio and Powermove import from a folder they
  already know the shape of. Overwrites confirm.
- **Export Preview** (one click, no dialog): renders the active document to
  `~/Pictures/degen-paint/previews/<project>-<doc>-r<rev>.png` and its annotated twin, and states
  the path in Status. This is what Sol `look`s at on the native path.

## 5. Grounding API, jobs, quotes, keys

### 5.1 Grounding: read-only GET endpoints on `dpaint serve`

The `dapi` analogue. Same JSON as the CLI's `--json` output — one serializer, no second shape.

```
GET /api/v1/status              { project, revision, busy: [jobs], activeDoc }
GET /api/v1/overview            = dpaint_overview
GET /api/v1/doc/:id/digest      = dpaint inspect --json
GET /api/v1/doc/:id/lint        = dpaint lint --json
GET /api/v1/history?limit=      = dpaint history --json
GET /api/v1/select?q=&doc=      resolved ids for a selector, or the candidate list
GET /api/v1/jobs/:id            job state
GET /render.png?doc=&scale=     (exists)
GET /annotate.png?doc=&scale=   annotated preview
GET /api/v1/skill               the pack data of §7, as JSON
```

Rules: loopback bind only (existing); `Host`/`Origin` must be `127.0.0.1:<port>` or absent, on
**every** route including the existing `POST /api` — a page in any browser tab can currently
POST ops to the bridge, and it must not. Starkbot's pack declares the origin as its one
`requires_env` (`DPAINT_BASE_URL`, a URL, not a key), which is how its HTTP runner is allowed to
reach loopback. GETs are ungated in Starkbot by construction.

### 5.2 Jobs

`dispatch("op")` stays synchronous for the CLI/MCP path. The Studio adds
`dispatch("job.start", {op,args,doc})` → `{ id }`, `dispatch("job.status", {id})`, and
`dispatch("job.cancel", {id})`. The UI runs every op through jobs; the server runs them on a
thread; `state` carries `busy`. Journal semantics are unchanged: a job commits one entry when it
finishes, nothing on cancel.

### 5.3 Quotes

`ai.*` ops already price themselves (`AiConfig::cost_of`, `--dry-run` reports the estimate, the
budget refuses before sending). The Studio surfaces it: when an `ai.*` form is open, a dry run
is issued on every field change and the estimate is written into the submit button's name and a
`"Budget: $0.42 of $5.00 spent"` line in the form. `dpaint quote <op> [args]` exposes the same
number on the CLI. Starkbot's `spends` head plus `confirm_all` on those labels turns each into a
confirm card quoting degen-paint's own price.

### 5.4 Keys in the app

`Settings › Providers` in the Studio: one paste-only field per provider (`fal.ai key`,
`QuiverAI key`), stored through the existing `dpaint-ai` key resolution (keychain first),
never read back, status rendered as `configured`/`missing` with the doctor output. Starkbot's
enablement flow offers a navigate goal that stops on this screen.

## 6. Verification: an audit that thinks like the navigator

`crates/dpaint-studio/tests/a11y/` — a Bun script driving headless Chromium over CDP against
`dpaint serve`, running the **same candidate rules as `snapshot.js`** (roles, visibility, name
resolution), a dev-time dependency only: no Chromium enters the product. It fails when:

- any candidate has an empty accessible name, or two candidates in one container share a name;
- the default view exceeds 120 candidates, or a `(role, container)` group exceeds 100;
- a control changes the document without changing visible text (silent op);
- the palette does not produce `[role=option]` within 200 ms of a `fill`;
- a dialog lacks `role=dialog aria-modal aria-labelledby`, or its primary button name does not
  contain the object it acts on;
- a route on the bridge accepts a foreign `Origin`.

The same script runs every scenario twice: normally and with `?nohints` (the Studio's own
shortcut table hidden), mirroring Starkbot's `--no-hints` gate.

The Tauri/WKWebView path cannot be audited on Linux. **Spike (macOS):** record what
`dpaint-studio-app` exposes to `AxObserver` — whether `role=option` rows, `role=dialog` and
`role=progressbar` arrive as `AXStaticText`/`AXRow`, `AXGroup:AXApplicationDialog` (not in
`neo-ax`'s `MODAL_SUBROLES` today) and `AXProgressIndicator` — before M6′'s S8d. Findings go to
`starkbot-neo/plans/spikes.md`; mapping gaps are fixed in `neo-ax`, not worked around in the UI.

On Linux the audit is not a stand-in: once §11 L1 lands, the **real navigator** runs against
`dpaint serve` on this machine (`neo nav http://127.0.0.1:4317 "…"`), and once L3 lands, against
the Tauri Linux build over AT-SPI (`neo app dev.degenpaint.studio "…"`). The audit stays as the
fast CI gate; `neo eval` is the truth.

## 7. `dpaint skill`: the pack data degen-paint owns

Like `dms skill`. One command emits the `media-apps` pack contribution, generated from the same
sources the UI uses, so it cannot drift:

```
dpaint skill --out <dir>
  vocabulary.md                     glossary for Jev's rules: project, document, layer, group,
                                    mask, selection, path, node, boolean, extrude, digest, lint,
                                    selector, revision, send to editor …
  desktop/apps/dev.degenpaint.studio.json
                                    bundle id, ax_strategy "none", settle_ms 300 (render) /
                                    1500 (export), shortcuts = SHORTCUTS, confirm_labels = §3.5,
                                    snapshot pruning hints
  desktop/routines/dp-open-project.json   ·  dp-import-file.json  ·  dp-export-png.json
  desktop/routines/dp-send-to-editor.json ·  dp-fix-lint.json
  goals/                            Sol goal templates: a brief → navigate(goal) sentences
  grounding.json                    the §5.1 probes: "lint is clean", "export exists at path",
                                    "revision advanced", "document has N layers"
  skills/degen-paint.md             what the app is, the three modes, how ops and lint work
```

Routines carry goal sentences and neo tools only (`launch_app`, `select_menu`, `type_text`,
`navigate`, `wait_for`) — no selectors, per the pack format. starkbot-neo vendors the output into
its embedded `media-apps` pack.

## 8. Phases

Same rule as the roadmap: each phase ends with an artifact and a passing check.

| Phase | Scope | Gate |
|---|---|---|
| **P10 Accessible Studio** | §3.1–3.7 on the existing single-project Studio; the audit of §6 | audit green on the default view, palette, one schema form, one dialog; `neo eval`-style transcript: a scripted navigator (the audit's own walker) opens the palette, runs `raster.layer.add`, sees the new row and the status text, with zero selectors |
| **P11 Projects and files** | §3.8, §4: Welcome, Open/New/Close, Import, Export, Send to Editor, Export Preview, sidecars, native menu | from Welcome: create a project, import a DMS-shaped take + sidecar, export PNG, send to editor — every step by menu/dialog only; the sidecar's prompt appears as provenance in `dpaint inspect --json`; overwrite prompts once |
| **P12 Grounding API** | §5.1, origin checks, `annotate.png` | every endpoint's JSON byte-equals the CLI's `--json`; a foreign-`Origin` POST is refused; `curl` from the pack's probe definitions returns the documented shapes |
| **P13 Jobs, quotes, keys** | §5.2–5.4, §3.6 | a 20 s render shows a progressbar and can be cancelled without a journal entry; an `ai.image.generate` form's button reads the same estimate as `dpaint quote`; a key typed in Settings makes `doctor` report `configured` and never appears in any response |
| **P14 Pack and smoke test** | §7; `App::DegenPaint` + cases in starkbot-neo's `neo-eval`; the macOS AX spike; S8d | `dpaint skill` output validates against `neo-packs`; S8d passes 4 of 5 |

P10 first; P11 and P12 are independent of each other and of P13; P14 last. The Linux milestones
of §11 run in parallel in starkbot-neo: L1 is needed for S8d-web, L3 for S8d-native on Linux.

### S8d — the smoke test (runs in starkbot-neo, only its two keys)

- Fixtures: one DMS-style still + sidecar in `~/Movies/Degen Media Studio/acme/`, Inter installed.
- Brief: *"In degen-paint, start a project 'acme-promo' 1080×1350, bring in the microphone take,
  put the title 'Loud on purpose' in Inter Bold across the top, make sure lint is clean, export a
  PNG and send it to the editor."*
- **Pass:** the PNG exists at 1080×1350; grounding reports zero lint findings and a text layer
  named title; the sidecar in `~/Movies/degen-paint/acme-promo/` names the project and revision;
  Sol vision confirms the title on the export; no confirm card other than the overwrite/send
  ones; 4 of 5 runs; median step latency and `BLOCKED` count recorded in `spikes.md`.

## 9. What changes in starkbot-neo (applied there, listed here for the contract)

- `plans/12-media-apps.md`: a fourth column for degen-paint in §2; `dp-*` routines and
  `dev.degenpaint.studio.json` in §3's tree; S8d in §5.
- `plans/00-decisions.md`: amendment naming degen-paint a fourth media app under A12′/A19/S8.
- `crates/neo-eval/src/apps.rs`: `App::DegenPaint` (`"degen-paint"`, bundles
  `["degen-paint.app"]`), plus an `open-degen-paint` case in `cases.rs`.
- `crates/neo-ax/src/mapping.rs`: whatever the macOS spike shows is missing
  (`AXApplicationDialog` as modal is the likely one).
- `media-apps` pack: vendor `dpaint skill` output; optional `DPAINT_BASE_URL` for grounding.
- The Linux port, §11: `neo-ax` AT-SPI backend, `neo-cdp` Chrome discovery + modifiers,
  `neo-keys` Secret Service, XDG paths, doctor, eval, CI.

## 10. Risks

| Risk | Control |
|---|---|
| WKWebView flattens `role=option`/`listbox` into something `neo-ax` ignores | the P14 spike measures first; fallback is buttons per row (`"Select layer title"`), which costs budget but is unambiguous |
| 212 ops swamp the 250-candidate budget | ops are never listed as buttons; only the combobox's ≤ 12 live options are candidates |
| Two operators on one document (a human or a coding agent writes while Jev clicks) | already handled by the journal and the 1 s poll; the Status region announces `updated by agent · <op>` so the navigator sees the change as progress, not as a stale |
| Long renders look like a hang | jobs + progressbar + cancel; pack `settle_ms` per action |
| Silent AI spend | quotes in button names, budget line in the form, the existing per-project ceiling |
| The audit passes but the real navigator fails | the audit reuses `snapshot.js`'s rules verbatim, vendored with its commit hash; S8d is the real gate |
| Hints become a dependency | the audit's `?nohints` pass; routines are goal sentences |

## 11. Starkbot Neo on Linux

starkbot-neo is "a local-first macOS agent" today (`README.md:8`); this machine is Arch/Omarchy
on Hyprland (Wayland). The port is smaller than it looks because the architecture already
separates the portable half from the OS half, and because the OS half on Linux has a direct
equivalent for every macOS primitive. Inventory (from the workspace, exact cites):

| Concern | macOS today | Linux |
|---|---|---|
| Native accessibility | `neo-ax`: `AXUIElement*` via `objc2-application-services`, `CGEvent` input (`crates/neo-ax/src/{sys,input,apps,perm}.rs`, all `#[cfg(target_os = "macos")]`) | **AT-SPI2 over D-Bus** (`atspi` crate 0.30, pure Rust, zbus). The a11y bus is running here (`org.a11y.Bus` → `/run/user/1000/at-spi/bus_0`, registry answers). |
| Managed browser | `DEFAULT_CHROME = "/Applications/Google Chrome.app/…"` (`neo-cdp/src/lib.rs:23`) | `google-chrome-stable` / `chromium` / `brave` on `PATH` (`/usr/bin/chromium` here). `--remote-debugging-pipe` + `command-fds` is Linux-native. |
| Select-all in `TYPE_TEXT` | `modifiers: META` (`neo-cdp/src/lib.rs:684`) | `Ctrl` (`modifiers: 2`); one platform constant. |
| Secrets | Login Keychain via `security-framework` (`neo-keys/src/keychain.rs:140-227`); a `Backend::File` already exists behind `cfg(not(macos))` | **Secret Service** (`org.freedesktop.secrets`, `secret-service` crate 5.x; gnome-keyring portal is present here), same service name `com.starkbot.neo`; the `0600` file backend only when no Secret Service answers. |
| Data dirs | `~/Library/Application Support/com.starkbot.neo`, `~/Library/Logs/…`, `~/Library/Caches/…` | XDG via `directories::ProjectDirs`: `~/.local/share/starkbot-neo`, `~/.local/state/starkbot-neo` (logs), `~/.cache/starkbot-neo`, `~/.config/starkbot-neo`. |
| Permissions | TCC: `AXIsProcessTrusted`, mic, speech, dictation (`neo-ax/src/perm.rs`, `neo-voice/src/permission.rs`) | None to request. Doctor checks facts instead: a11y bus reachable, `org.a11y.Status.IsEnabled` true (Chromium/Electron only publish AT-SPI when it is), compositor offers `zwp_virtual_keyboard_v1` + `zwlr_virtual_pointer_v1`, a Chrome binary exists. |
| Voice | on-device `SFSpeechRecognizer` (`neo-voice/src/stt/apple.rs`), `cpal` capture | `cpal` on PipeWire works as is; on-device STT is `Unsupported` (already the `cfg(not(macos))` arm), so `gpt-transcribe` is the only STT — doctor says so instead of "denied". |
| Desktop shell | Tauri with `macos-private-api`, `tauri-nspanel`, `PanelController` (`src-tauri`) | `neo tui` is the acceptance surface (P12) and needs nothing. The Tauri shell builds on webkit2gtk with `panels` cfg-gated to macOS; no layer-shell pill in v1. |
| App identity | bundle id (`AppSel::BundleId`, `deny.rs`, `neo-eval` `*.app` bundles) | Wayland **`app_id`** — for GTK/Tauri apps it *is* the identifier (`dev.degenpaint.studio`), so pack hints and `AppSel::BundleId` strings carry over unchanged *(verify on the Tauri Linux build)*. Desktop-entry id for launching. |
| Launch / focus / windows | `NSWorkspace`, `NSRunningApplication::ownsMenuBar` (`neo-ax/src/apps.rs:30-156`) | Launch: parse the `.desktop` entry's `Exec` (`freedesktop-desktop-entry`), spawn. Enumerate + focus: **Hyprland IPC** (`$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock`: `j/clients`, `dispatch focuswindow pid:N`), behind a `WindowManager` trait so a second compositor is one impl. |
| Deny list | 45 bundle ids (`neo-ax/src/deny.rs`) | the same list keyed by `app_id`: `Alacritty`, `kitty`, `foot`, `com.mitchellh.ghostty`, `org.wezfurlong.wezterm`, `org.gnome.Terminal`, `org.kde.konsole`, `code`, `dev.zed.Zed`, `org.gnome.seahorse.Application`, `org.kde.kwalletmanager5`, plus `jetbrains-*`. |
| Toolchain / CI | `rust-toolchain.toml` targets darwin only; CI Linux lane is a crate subset (`ci.yml:21-50`) | add `x86_64-unknown-linux-gnu`; Linux lane becomes `cargo test --workspace`. |

### 11.1 The AT-SPI backend for `neo-ax`

`AxHandle`'s public surface (`crates/neo-ax/src/actor.rs:95-300`: `spawn`, `apps`, `activate`,
`table`, `table_for_goal`, `guard`, `act`) is the seam. It becomes a trait-shaped module pair:
`backend::mac` (today's code, `cfg(target_os = "macos")`) and `backend::atspi`
(`cfg(target_os = "linux")`), both producing the same `ElementTable`, `Guard`, `Freshness` and
`ActOutcome` types, so `jev-nav`, `neo-agent`, `neo-eval` and the packs do not change. Mapping:

| macOS | AT-SPI2 |
|---|---|
| `AXUIElementCopyMultipleAttributeValues` (role, title, value, position, size, children) | `Accessible.GetRole`/`Name`/`Description`, `Value.CurrentValue`, `Text.GetText`, `Component.GetExtents`, `Accessible.GetChildren`; one D-Bus round trip per node, batched with `zbus` futures — measure against the 250-budget walk. |
| `AXUIElementCopyActionNames` → `AXPress`/`AXConfirm`/`AXPick`/`AXShowMenu` | `Action.GetActions` → `Action.DoAction(i)` (`click`, `press`, `activate`, `toggle`, `expand`) |
| `AXUIElementSetAttributeValue(AXValue)` | `EditableText.SetTextContents`, `Value.CurrentValue` (set) |
| `AXFocused` | `Component.GrabFocus` |
| `AXUIElementCopyElementAtPosition` | `Component.GetAccessibleAtPoint` |
| `AXSheet`/`AXDialog` subroles = modal | `Role::Dialog`/`Alert` + `State::Modal` |
| `AXProgressIndicator`/`AXBusyIndicator` = busy | `Role::ProgressBar`, `State::Busy` |
| `AXMenuBar` → `menuitem` rows with shortcuts | `Role::MenuBar`/`Menu`/`MenuItem`; the accelerator from `Action.GetKeyBinding` |
| `ax_strategy: manual_accessibility` (Electron) | `force_renderer_accessibility`: Chromium/Electron apps need `--force-renderer-accessibility` or `org.a11y.Status.IsEnabled=true`; Qt needs `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`. Same hint slot, Linux values. |
| `CGEvent` key/mouse fallback | AT-SPI actions first (same preference order as today). Fallback input on Wayland: `zwp_virtual_keyboard_v1` (keys) and `zwlr_virtual_pointer_v1` (clicks) through `wayland-client`; Hyprland supports both. No `xdotool`/`ydotool` (needs uinput/root; a subprocess is not P3). |

Two apps that matter here publish AT-SPI natively: **GTK 3/4**, and **WebKitGTK** — which is what
the Tauri Linux build of degen-paint's Studio runs on. So the §3 DOM contract reaches the Linux
observer through the same role names it uses for the web path (`role=option` → `ListItem`,
`role=dialog` → `Dialog`+`Modal`, `role=progressbar` → `ProgressBar`), with less mapping doubt
than WKWebView. The macOS spike of §6 has a Linux twin that is runnable now: record the AT-SPI
tree of `dpaint-studio-app` with `accerciser` or `busctl`, before L3 starts.

### 11.2 Milestones

| L | Scope | Gate |
|---|---|---|
| **L0 Builds on Linux** | add the Linux target; cfg-gate `neo-ax`'s backend, `src-tauri`'s panels, `neo-voice`'s Apple STT; XDG `ProjectDirs`; `rustls` → `ring` if `aws-lc-sys` bites | `cargo test --workspace` green on this machine and in a Linux CI job; `neo doctor` runs and reports the Linux facts above |
| **L1 Web path** | `neo-cdp`: Chrome discovery on `PATH`, `Ctrl` select-all, `--ozone-platform-hint=auto`; Chrome profile under XDG | `neo nav https://example.com "click the More information link"` passes; **S8d-web**: the S8d brief against `dpaint serve` in managed Chromium passes 4 of 5 on this machine |
| **L2 Keys + TUI** | `neo-keys` Secret Service backend; `neo account … login` and `neo keys set` land in the keyring; `neo tui` end to end | keys round-trip through `secret-tool lookup service com.starkbot.neo`; a full `neo tui` turn with the Claude subscription |
| **L3 Native path** | §11.1 backend; Hyprland `WindowManager`; desktop-entry launch; Linux deny list; `neo-eval` `App::installed()` via desktop entries, cases for Chromium, LibreOffice Calc and degen-paint | `neo app dev.degenpaint.studio "open the command palette and run raster.layer.add"` passes; `neo eval` for the three apps passes; **S8d-native** on the Tauri Linux build 4 of 5 |
| **L4 Desktop shell** *(optional)* | Tauri shell on webkit2gtk with plain windows | the Connections shell opens and completes onboarding; no NSPanel features claimed |

L0 → L1 → L2 are each a day-scale change with no design risk. L3 is the real work and is where
the `WindowManager` trait and the AT-SPI batching cost need measuring first — the same lesson as
`01-accessibility.md`'s Numbers/Calc findings: probe, record in `spikes.md`, then build.

### 11.3 What does not change

P3, P9, P10, the gates, the pack format, `snapshot.js`, Jev, the routines, and every plan that
says "app" instead of "macOS app". A Linux user gets the same product with `neo tui` as the front
end; the media apps it operates are whichever of the four are installed — on Linux today that is
degen-paint (native or web) and DMS (web), since Diffusion Studio and Powermove ship no Linux build.
