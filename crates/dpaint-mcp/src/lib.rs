//! MCP server over stdio.
//!
//! One tool per op, generated from the registry, plus the five loop tools an agent actually
//! wants: overview, render, lint, apply, history. The protocol is hand-rolled JSON-RPC 2.0
//! over newline-delimited JSON — it is a small surface, and it keeps the server free of a
//! heavyweight SDK and of any dependency on the renderer or the linter.

pub mod handlers;
pub mod tools;

pub use handlers::{Handler, Handlers, OpExec};
pub use tools::{op_for_tool, tool_name, LOOP_TOOLS};

use dpaint_core::{Engine, Error, Registry, Result, Workspace};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The MCP revision this server speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "degen-paint";

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Handle one JSON-RPC message. Returns `None` for notifications, which by definition get no
/// reply. Pure: this is the whole protocol, testable without a process or a pipe.
pub fn handle_message(registry: &Registry, handlers: &Handlers, msg: &Value) -> Option<Value> {
    if !msg.is_object() {
        return Some(error_response(
            Value::Null,
            INVALID_REQUEST,
            "expected a single JSON-RPC request object",
            None,
        ));
    }
    let id = msg.get("id").cloned();
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.is_empty() {
        return Some(error_response(
            id.unwrap_or(Value::Null),
            INVALID_REQUEST,
            "request has no method",
            None,
        ));
    }
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

    // Notifications carry no id and expect no answer.
    let id = id?;

    let result: Value = match method {
        "initialize" => initialize_result(&params),
        "ping" => json!({}),
        "tools/list" => tools_list(registry, handlers),
        "tools/call" => match tools_call(registry, handlers, &params) {
            Ok(v) => v,
            Err(protocol) => {
                return Some(error_response(id, protocol.code, &protocol.message, None))
            }
        },
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),
        other => {
            return Some(error_response(
                id,
                METHOD_NOT_FOUND,
                &format!("unknown method '{other}'"),
                None,
            ))
        }
    };

    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn initialize_result(params: &Value) -> Value {
    // Echo a protocol version we understand: agree with the client when we can.
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = match requested {
        Some(v) if v == PROTOCOL_VERSION => v,
        _ => PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Every mutation is an op; `tools/list` is generated from the op registry. \
                         Batch work through dpaint_apply — it is transactional — then read the \
                         result back with dpaint_render and dpaint_lint instead of guessing."
    })
}

fn tools_list(registry: &Registry, handlers: &Handlers) -> Value {
    let mut list: Vec<Value> = Vec::with_capacity(registry.len() + LOOP_TOOLS.len());
    list.extend(handlers.specs());
    if handlers.can_run_ops() {
        list.extend(registry.iter().map(|op| tools::op_tool(op.as_ref())));
    }
    json!({ "tools": list })
}

struct ProtocolError {
    code: i64,
    message: String,
}

fn tools_call(
    registry: &Registry,
    handlers: &Handlers,
    params: &Value,
) -> std::result::Result<Value, ProtocolError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ProtocolError {
            code: INVALID_PARAMS,
            message: "tools/call requires a 'name'".into(),
        })?;
    let mut args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if args.is_null() {
        args = json!({});
    }
    if !args.is_object() {
        return Err(ProtocolError {
            code: INVALID_PARAMS,
            message: "'arguments' must be an object".into(),
        });
    }

    if let Some(outcome) = handlers.call(name, args.clone()) {
        return Ok(tool_result(outcome));
    }

    let Some(op_id) = op_for_tool(registry, name) else {
        return Err(ProtocolError {
            code: INVALID_PARAMS,
            message: format!(
                "unknown tool '{name}'; call tools/list for the {} tools this server offers",
                registry.len() + handlers.specs().len()
            ),
        });
    };
    if !handlers.can_run_ops() {
        return Err(ProtocolError {
            code: INVALID_PARAMS,
            message: format!("tool '{name}' exists but this server has no project open"),
        });
    }

    // `doc` and `dryRun` steer the call; everything else belongs to the op's own schema.
    let obj = args.as_object_mut().expect("checked above");
    let doc = obj
        .remove("doc")
        .and_then(|v| v.as_str().map(str::to_string));
    let dry = obj
        .remove("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(tool_result(handlers.run_op(op_id, args, doc, dry)))
}

/// MCP reports tool failures inside a successful result, so an agent can read the error and
/// retry without the transport treating it as a protocol fault.
fn tool_result(outcome: Result<Value>) -> Value {
    match outcome {
        Ok(value) => json!({
            "content": [{ "type": "text", "text": compact(&value) }],
            "structuredContent": value,
            "isError": false,
        }),
        Err(e) => {
            let detail = e.detail();
            let payload = json!({
                "ok": false,
                "error": {
                    "code": detail.code,
                    "message": detail.message,
                    "exitCode": e.exit_code(),
                    "candidates": detail.candidates,
                    "suggestion": detail.suggestion,
                }
            });
            json!({
                "content": [{ "type": "text", "text": compact(&payload) }],
                "structuredContent": payload,
                "isError": true,
            })
        }
    }
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"serializationError\":\"{e}\"}}"))
}

fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

/// Serve newline-delimited JSON-RPC until the input ends.
pub fn serve<R: BufRead, W: Write>(
    registry: &Registry,
    handlers: &Handlers,
    input: R,
    mut output: W,
) -> Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle_message(registry, handlers, &msg),
            Err(e) => Some(error_response(
                Value::Null,
                PARSE_ERROR,
                &format!("invalid JSON: {e}"),
                None,
            )),
        };
        if let Some(response) = response {
            writeln!(output, "{}", compact(&response))?;
            output.flush()?;
        }
    }
    Ok(())
}

/// The CLI entry point: open the project, back the handlers with its engine, and serve on
/// stdio until the client disconnects.
pub fn serve_stdio(
    registry: Registry,
    project_dir: Option<PathBuf>,
    handlers: Handlers,
) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve_project(registry, project_dir, handlers, stdin.lock(), stdout.lock())
}

/// The same thing over any pair of streams: open or discover the project, give the handlers
/// its engine, and run the protocol.
pub fn serve_project<R: BufRead, W: Write>(
    registry: Registry,
    project_dir: Option<PathBuf>,
    handlers: Handlers,
    input: R,
    output: W,
) -> Result<()> {
    let workspace = open_workspace(project_dir.as_deref())?;
    let engine = Arc::new(Mutex::new(Engine::new(registry.clone(), workspace)));
    let handlers = handlers.backed_by(engine);
    serve(&registry, &handlers, input, output)
}

fn open_workspace(dir: Option<&Path>) -> Result<Workspace> {
    match dir {
        Some(p) if p.join("project.json").exists() => Workspace::open(p),
        Some(p) => Workspace::discover(p),
        None => Workspace::discover(std::env::current_dir().map_err(Error::Io)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpaint_core::doc::{DocKind, Document, RasterDoc};
    use dpaint_core::{schema_for, Op, OpCx, OpEffect, Project};

    /// A minimal op so the registry under test is a real one.
    struct Noop;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct NoopArgs {
        /// How loudly to do nothing.
        #[serde(default)]
        volume: Option<u32>,
    }

    impl Op for Noop {
        fn id(&self) -> &'static str {
            "test.thing.do-nothing"
        }
        fn about(&self) -> &'static str {
            "Do nothing, observably."
        }
        fn schema(&self) -> Value {
            schema_for::<NoopArgs>()
        }
        fn modes(&self) -> &'static [DocKind] {
            &[DocKind::Raster]
        }
        fn apply(&self, _p: &mut Project, _a: Value, _cx: &mut OpCx) -> Result<OpEffect> {
            Ok(OpEffect::default())
        }
    }

    fn registry() -> Registry {
        let mut r = Registry::new();
        r.register(Noop);
        r
    }

    fn request(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    }

    #[test]
    fn initialize_reports_the_protocol_and_the_server() {
        let r = registry();
        let h = Handlers::new();
        let resp = handle_message(
            &r,
            &h,
            &request("initialize", json!({"protocolVersion": PROTOCOL_VERSION})),
        )
        .expect("a request gets a response");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], "degen-paint");
        assert_eq!(
            resp["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
    }

    #[test]
    fn notifications_get_no_response() {
        let r = registry();
        let h = Handlers::new();
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_message(&r, &h, &note).is_none());
    }

    #[test]
    fn an_unknown_method_is_a_jsonrpc_error_not_a_panic() {
        let r = registry();
        let h = Handlers::new();
        let resp = handle_message(&r, &h, &request("does/not/exist", json!({}))).unwrap();
        assert_eq!(resp["error"]["code"], METHOD_NOT_FOUND);
        assert!(resp.get("result").is_none());
    }

    #[test]
    fn an_unknown_tool_is_a_jsonrpc_error_not_a_panic() {
        let r = registry();
        let h = Handlers::new().with_ops(Box::new(|_, _, _, _| Ok(json!({}))));
        let resp = handle_message(
            &r,
            &h,
            &request(
                "tools/call",
                json!({"name": "raster_filter_nonsense", "arguments": {}}),
            ),
        )
        .unwrap();
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    #[test]
    fn a_malformed_line_is_answered_with_a_parse_error_and_the_loop_continues() {
        let r = registry();
        let h = Handlers::new();
        let input =
            std::io::Cursor::new("not json\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
        let mut out = Vec::new();
        serve(&r, &h, input, &mut out).unwrap();

        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], PARSE_ERROR);
        assert_eq!(lines[1]["id"], 2);
        assert_eq!(lines[1]["result"], json!({}));
    }

    #[test]
    fn workspace_backed_handlers_expose_the_loop_tools_and_run_ops() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::new(
            "t",
            Document::Raster(RasterDoc::new(
                dpaint_core::DocId::from("doc_main"),
                "main",
                8,
                8,
            )),
        );
        let ws = Workspace::create(dir.path(), project).unwrap();
        let engine = Arc::new(Mutex::new(Engine::new(registry(), ws)));
        let h = Handlers::new().backed_by(engine);

        let list = handle_message(&registry(), &h, &request("tools/list", json!({}))).unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"dpaint_overview"));
        assert!(names.contains(&"dpaint_apply"));
        assert!(names.contains(&"dpaint_history"));
        assert!(names.contains(&"test_thing_do_nothing"));

        let overview = handle_message(
            &registry(),
            &h,
            &request(
                "tools/call",
                json!({"name": "dpaint_overview", "arguments": {}}),
            ),
        )
        .unwrap();
        assert_eq!(overview["result"]["isError"], false);
        assert_eq!(
            overview["result"]["structuredContent"]["documents"][0]["id"],
            "doc_main"
        );
        assert_eq!(
            overview["result"]["structuredContent"]["documents"][0]["kind"],
            "raster"
        );
    }
}
