//! The transport-independent half of the desktop shell.
//!
//! Everything here is a plain function over [`Studio`], so the `#[tauri::command]` wrappers in
//! [`crate`] are three lines each and the integration tests exercise the same code the webview
//! reaches. Failures cross the bridge as the *same* JSON the HTTP bridge puts in its `error`
//! field, which is what lets one copy of `studio.js` drive both transports.

use base64::Engine as _;
use dpaint_core::Error;
use dpaint_studio::Studio;
use serde_json::Value;

/// Serialize an engine error the way `POST /api` does: `{code,message,candidates?,suggestion?}`.
///
/// Tauri rejects a command with `Err(String)`, so the structured detail travels as its JSON text
/// and the init script parses it back into an object.
pub fn error_payload(e: &Error) -> String {
    let detail = e.detail();
    serde_json::to_string(&detail).unwrap_or_else(|_| {
        // `code` is a &'static str and `message` a String; this branch is unreachable in
        // practice, but a bridge that can produce un-parseable text is a bridge that can
        // strand the UI with "undefined".
        format!(
            "{{\"code\":{},\"message\":{}}}",
            Value::from(detail.code),
            Value::from(detail.message)
        )
    })
}

/// No project open yet. Shaped exactly like an engine error so the UI needs no special case.
pub fn no_project_payload() -> String {
    error_payload(&Error::Invalid(
        "no project is open — use File > Open Project, or start degen-paint with --project <dir>"
            .into(),
    ))
}

/// `state`, `op`, `undo`, … — the whole GUI surface, unwrapped result on success.
pub fn call(studio: &Studio, method: &str, params: &Value) -> Result<Value, String> {
    studio
        .dispatch(method, params)
        .map_err(|e| error_payload(&e))
}

/// Render for the viewport as a `data:image/png;base64,…` URI.
///
/// A data URI instead of a custom protocol: the webview can put it straight in `img.src`, which
/// keeps the browser build and the desktop build on identical UI code.
pub fn render_data_uri(
    studio: &Studio,
    doc: Option<&str>,
    scale: f64,
    max: u32,
) -> Result<String, String> {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let (png, _size) = studio
        .render_png(doc, scale, max.max(1))
        .map_err(|e| error_payload(&e))?;
    // base64 is 4 bytes per 3, and the prefix is 22: size it once instead of growing.
    let mut uri = String::with_capacity(22 + (png.len() + 2) / 3 * 4);
    uri.push_str("data:image/png;base64,");
    base64::engine::general_purpose::STANDARD.encode_string(&png, &mut uri);
    Ok(uri)
}

/// Injected into every Studio window before the page's own scripts run.
///
/// This is the whole reason the UI files need no knowledge of Tauri: it defines the two globals
/// `studio.js` looks for, and makes a rejected command look exactly like a rejected `fetch`.
pub const INIT_SCRIPT: &str = r#"(function () {
  if (window.__DPAINT_INVOKE__) { return; }

  // Resolved lazily: the Tauri global is itself injected as an init script and the order
  // between the two is not guaranteed.
  function invoke(cmd, args) {
    var t = window.__TAURI__;
    var fn = t && (t.core ? t.core.invoke : t.invoke);
    if (typeof fn !== 'function') {
      return Promise.reject(new Error('degen-paint: Tauri bridge unavailable'));
    }
    return fn(cmd, args);
  }

  // Rebuild the structured error the HTTP bridge throws: an Error carrying
  // {code, message, candidates, suggestion}.
  function structured(raw) {
    var d = null;
    if (raw && typeof raw === 'object' && typeof raw.code === 'string') {
      d = raw;
    } else {
      var text = typeof raw === 'string' ? raw : (raw && raw.message) || String(raw);
      try { d = JSON.parse(text); } catch (_) { d = null; }
      if (!d || typeof d !== 'object' || typeof d.code !== 'string') {
        d = { code: 'invalid', message: text };
      }
    }
    var e = new Error(d.message || 'degen-paint error');
    e.name = 'StudioError';
    e.code = d.code;
    e.candidates = Array.isArray(d.candidates) ? d.candidates : [];
    e.suggestion = d.suggestion === undefined ? null : d.suggestion;
    return e;
  }

  window.__DPAINT_TRANSPORT__ = 'tauri';

  window.__DPAINT_INVOKE__ = function (method, params) {
    return invoke('dpaint_call', { method: method, params: params === undefined ? {} : params })
      .catch(function (raw) { throw structured(raw); });
  };

  window.__DPAINT_RENDER_URL__ = function (req) {
    var r = req || {};
    return invoke('dpaint_render', {
      doc: r.doc === undefined ? null : r.doc,
      scale: typeof r.scale === 'number' ? r.scale : 1,
      max: typeof r.max === 'number' ? r.max : 1600
    }).catch(function (raw) { throw structured(raw); });
  };

  // Change notification deliberately stops here. The native menu and the directory picker
  // write through the engine and then emit the Tauri event `dpaint:changed`; studio.js
  // subscribes to that itself (and keeps a 1s state poll as a backstop), so adding a second
  // listener here would only refresh the same window twice per undo.
})();
"#;

/// Evaluated in each window once its page has finished loading.
///
/// An injected init script that silently fails to land leaves a UI that looks broken for no
/// visible reason, so each window reports back what it actually found and the shell logs it.
/// The answer is `undefined` on the welcome window, which has no bridge and needs none.
pub const BRIDGE_PROBE: &str = "window.__TAURI__.event.emit('dpaint:bridge', {\
 transport: window.__DPAINT_TRANSPORT__ || null,\
 invoke: typeof window.__DPAINT_INVOKE__,\
 render: typeof window.__DPAINT_RENDER_URL__ })";
