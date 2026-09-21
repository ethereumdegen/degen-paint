// The navigator's candidate rules, vendored verbatim in substance from
// starkbot-neo `crates/jev-nav/js/snapshot.js` @ ae5f815c76f377e8172b7f3a9e28aa4e44f54667
// (2026-09-19). Dropped from the original: the identity cache, guards, page keys, shadow
// and iframe traversal, scroll controls and the text budget — none of which an audit needs.
// Kept exactly: the candidate selector, the fourteen roles, the accessible-name resolution
// order, the visibility and centre-in-viewport filters, and the 250 cap.
//
// This file is the audit's whole claim to authority. If it drifts from upstream the audit
// stops predicting what the real navigator sees, so it carries the commit it came from and
// `audit.mjs` prints it in every run.
(() => {
  const parent = (e) => e?.assignedSlot || e?.parentElement || e?.getRootNode?.().host || null;
  const closest = (e, selector) => {
    for (let n = e; n; n = parent(n)) if (n.matches?.(selector)) return n;
    return null;
  };
  const safe = (e) => !['password', 'hidden'].includes(e.type);
  const visible = (e) =>
    !closest(e, '[aria-hidden="true"],[inert]') &&
    e.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true });
  const byId = (e, id) => {
    const root = e.getRootNode();
    return root.getElementById?.(id) || e.ownerDocument.getElementById(id);
  };
  const name = (e, seen = new Set()) => {
    if (!e || seen.has(e)) return '';
    seen.add(e);
    const referenced = (e.getAttribute?.('aria-labelledby') || '')
      .split(/\s+/)
      .map((id) => name(byId(e, id), seen))
      .filter(Boolean)
      .join(' ');
    return (
      referenced ||
      e.getAttribute?.('aria-label') ||
      [...(e.labels || [])].map((l) => name(l, seen)).filter(Boolean).join(' ') ||
      (['button', 'submit', 'reset'].includes(e.type) ? e.value : '') ||
      e.getAttribute?.('alt') ||
      (e.tagName === 'INPUT'
        ? ''
        : [...(e.childNodes || [])]
            .map((n) =>
              n.nodeType === 3
                ? n.textContent
                : n.nodeType === 1 && n.getAttribute('aria-hidden') !== 'true'
                  ? name(n, seen)
                  : '',
            )
            .join(' ')
            .trim()) ||
      e.getAttribute?.('title') ||
      e.getAttribute?.('placeholder') ||
      ''
    );
  };
  const roles = ['button', 'link', 'checkbox', 'radio', 'switch', 'tab', 'menuitem', 'menuitemradio',
    'option', 'gridcell', 'combobox', 'textbox', 'searchbox', 'spinbutton'];
  const selector =
    'a[href],button,input,textarea,select,summary,[contenteditable]:not([contenteditable="false"]),' +
    roles.map((role) => '[role="' + role + '"]').join(',');
  const role = (e) => {
    const explicit = e.getAttribute('role');
    if (roles.includes(explicit)) return explicit;
    if (e.tagName === 'BUTTON' || e.tagName === 'SUMMARY') return 'button';
    if (e.tagName === 'A') return 'link';
    if (e.tagName === 'SELECT') return 'combobox';
    if (e.tagName === 'TEXTAREA' || e.isContentEditable) return 'textbox';
    if (e.tagName === 'INPUT') {
      if (e.type === 'file') return 'file';
      if (['checkbox', 'radio'].includes(e.type)) return e.type;
      if (['button', 'submit', 'reset', 'image'].includes(e.type)) return 'button';
      if (e.type === 'search') return 'searchbox';
      if (e.type === 'number') return 'spinbutton';
      if (['text', 'email', 'url', 'tel'].includes(e.type)) return 'textbox';
    }
    return null;
  };

  // The container a candidate is grouped under, for the 40%-of-budget diversity rule that
  // `neo-ax` applies per (role, container). On the web path the equivalent grouping is the
  // nearest landmark, because that is what a pane is.
  const container = (e) => {
    const land = closest(e, '[role="region"],[role="toolbar"],[role="dialog"],[role="tablist"],main,nav,header,footer,aside,form');
    if (!land) return 'document';
    return land.getAttribute('aria-label') || land.getAttribute('role') || land.tagName.toLowerCase();
  };

  const out = [];
  for (const e of document.querySelectorAll(selector)) {
    if (!safe(e) || e.matches(':disabled') || closest(e, '[aria-disabled="true"]')) continue;
    const rname = role(e);
    const isFile = e.tagName === 'INPUT' && e.type === 'file';
    if (!rname || (!isFile && !visible(e))) continue;
    const r = e.getBoundingClientRect();
    const x = r.x + r.width / 2;
    const y = r.y + r.height / 2;
    if (!isFile && (r.width <= 0 || r.height <= 0 || x < 0 || y < 0 || x >= innerWidth || y >= innerHeight)) continue;
    if (rname === 'gridcell' && e.querySelector('button,[role="button"]')) continue;
    out.push({
      role: rname,
      label: name(e) || '',
      container: container(e),
      tag: e.tagName.toLowerCase(),
      id: e.id || null,
      describedBy: e.getAttribute('aria-describedby'),
      controls: e.getAttribute('aria-controls'),
    });
  }
  return { candidates: out.slice(0, 250), omitted: Math.max(0, out.length - 250) };
})()
