//! The MCP surface an agent actually drives: discovery generated from the registry, injected
//! loop tools, and failures that arrive as data instead of as a crash.

use dpaint_core::doc::{DocKind, Document, RasterDoc};
use dpaint_core::{schema_for, DocId, Error, Op, OpCx, OpEffect, Project, Registry, Result};
use dpaint_mcp::{handle_message, serve, Handlers};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct BlurArgs {
    /// Layer to blur.
    layer: String,
    /// Blur radius in pixels.
    radius: f64,
}

struct Blur;

impl Op for Blur {
    fn id(&self) -> &'static str {
        "raster.filter.gaussian-blur"
    }
    fn about(&self) -> &'static str {
        "Blur a layer."
    }
    fn schema(&self) -> Value {
        schema_for::<BlurArgs>()
    }
    fn modes(&self) -> &'static [DocKind] {
        &[DocKind::Raster]
    }
    fn apply(&self, _p: &mut Project, _a: Value, _cx: &mut OpCx) -> Result<OpEffect> {
        Ok(OpEffect::default())
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct MeasureArgs {
    /// Object to measure.
    target: String,
}

struct Measure;

impl Op for Measure {
    fn id(&self) -> &'static str {
        "vector.measure.bbox"
    }
    fn about(&self) -> &'static str {
        "Measure a bounding box."
    }
    fn schema(&self) -> Value {
        schema_for::<MeasureArgs>()
    }
    fn is_query(&self) -> bool {
        true
    }
    fn apply(&self, _p: &mut Project, _a: Value, _cx: &mut OpCx) -> Result<OpEffect> {
        Ok(OpEffect::default())
    }
}

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Blur);
    r.register(Measure);
    r
}

fn req(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// Records every op invocation so a test can assert what the server dispatched.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<Value>>,
}

impl Recorder {
    fn handlers(self: &Arc<Self>) -> Handlers {
        let ops = self.clone();
        let batch = self.clone();
        Handlers::new()
            .with_ops(Box::new(move |id, args, doc, dry| {
                ops.calls.lock().push(json!({
                    "kind": "op", "op": id, "args": args, "doc": doc, "dryRun": dry
                }));
                if id == "raster.filter.gaussian-blur" && args["radius"].as_f64() == Some(-1.0) {
                    return Err(Error::SchemaViolation {
                        op: id.to_string(),
                        detail: "radius must be positive".into(),
                    });
                }
                Ok(json!({ "ok": true, "op": id }))
            }))
            .with(
                "dpaint_apply",
                Box::new(move |args| {
                    batch
                        .calls
                        .lock()
                        .push(json!({ "kind": "apply", "args": args }));
                    Ok(json!({ "applied": args["ops"].as_array().map(|o| o.len()).unwrap_or(0) }))
                }),
            )
    }

    fn calls(&self) -> Vec<Value> {
        self.calls.lock().clone()
    }
}

#[test]
fn tools_list_has_one_tool_per_op_plus_the_injected_loop_tools() {
    let rec = Arc::new(Recorder::default());
    let handlers = rec
        .handlers()
        .with("dpaint_render", Box::new(|_| Ok(json!({"png": "…"}))))
        .with("dpaint_lint", Box::new(|_| Ok(json!({"findings": []}))));
    let r = registry();

    let resp = handle_message(&r, &handlers, &req(1, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    assert_eq!(
        tools.len(),
        r.len() + 3,
        "two ops plus three registered loop tools"
    );
    assert!(names.contains(&"raster_filter_gaussian_blur"));
    assert!(names.contains(&"vector_measure_bbox"));
    assert!(names.contains(&"dpaint_render"));
    assert!(names.contains(&"dpaint_lint"));
    assert!(names.contains(&"dpaint_apply"));

    // Every tool carries a usable JSON Schema, and the op tools carry the op's own.
    for t in tools {
        let schema = &t["inputSchema"];
        assert_eq!(schema["type"], "object", "{}: {schema}", t["name"]);
        assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
    }
    let blur = tools
        .iter()
        .find(|t| t["name"] == "raster_filter_gaussian_blur")
        .unwrap();
    let props = &blur["inputSchema"]["properties"];
    assert_eq!(props["radius"]["description"], "Blur radius in pixels.");
    assert_eq!(
        blur["inputSchema"]["required"],
        json!(["layer", "radius"]),
        "the op's own requirements survive"
    );
    // Plus the two the protocol needs to target a document.
    assert_eq!(props["doc"]["type"], "string");
    assert_eq!(props["dryRun"]["type"], "boolean");
    assert!(blur["description"]
        .as_str()
        .unwrap()
        .contains("raster.filter.gaussian-blur"));
}

#[test]
fn calling_an_op_tool_dispatches_the_op_with_its_document_and_dry_run_flag() {
    let rec = Arc::new(Recorder::default());
    let handlers = rec.handlers();

    let resp = handle_message(
        &registry(),
        &handlers,
        &req(
            7,
            "tools/call",
            json!({
                "name": "raster_filter_gaussian_blur",
                "arguments": { "layer": "#sky", "radius": 12, "doc": "poster", "dryRun": true }
            }),
        ),
    )
    .unwrap();

    assert_eq!(resp["id"], 7);
    assert_eq!(resp["result"]["isError"], false);
    assert_eq!(resp["result"]["structuredContent"]["ok"], true);
    // The text block mirrors the structured payload, for clients that only read text.
    let text: Value =
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, resp["result"]["structuredContent"]);

    let call = &rec.calls()[0];
    assert_eq!(call["op"], "raster.filter.gaussian-blur");
    assert_eq!(call["doc"], "poster");
    assert_eq!(call["dryRun"], true);
    assert_eq!(
        call["args"],
        json!({"layer": "#sky", "radius": 12}),
        "steering keys are not passed to the op"
    );
}

#[test]
fn the_dotted_op_id_also_works_as_a_tool_name() {
    let rec = Arc::new(Recorder::default());
    let resp = handle_message(
        &registry(),
        &rec.handlers(),
        &req(
            2,
            "tools/call",
            json!({"name": "raster.filter.gaussian-blur", "arguments": {"layer": "#a", "radius": 1}}),
        ),
    )
    .unwrap();
    assert_eq!(resp["result"]["isError"], false);
    assert_eq!(rec.calls()[0]["op"], "raster.filter.gaussian-blur");
}

#[test]
fn dpaint_apply_forwards_the_whole_batch_to_its_injected_handler() {
    let rec = Arc::new(Recorder::default());
    let handlers = rec.handlers();
    let batch = json!({
        "ops": [
            {"op": "raster.filter.gaussian-blur", "args": {"layer": "#sky", "radius": 4}},
            {"op": "vector.measure.bbox", "args": {"target": "#mark"}, "doc": "logo"}
        ],
        "dryRun": false
    });

    let resp = handle_message(
        &registry(),
        &handlers,
        &req(
            3,
            "tools/call",
            json!({"name": "dpaint_apply", "arguments": batch.clone()}),
        ),
    )
    .unwrap();

    assert_eq!(resp["result"]["isError"], false);
    assert_eq!(resp["result"]["structuredContent"]["applied"], 2);

    let calls = rec.calls();
    assert_eq!(calls.len(), 1, "a batch is one dispatch, not one per op");
    assert_eq!(calls[0]["kind"], "apply");
    assert_eq!(calls[0]["args"], batch, "the batch arrives verbatim");
}

#[test]
fn an_op_failure_comes_back_as_a_tool_error_carrying_the_machine_code() {
    let rec = Arc::new(Recorder::default());
    let resp = handle_message(
        &registry(),
        &rec.handlers(),
        &req(
            4,
            "tools/call",
            json!({"name": "raster_filter_gaussian_blur", "arguments": {"layer": "#sky", "radius": -1}}),
        ),
    )
    .unwrap();

    // A failing tool is a successful JSON-RPC response with isError — the agent reads the
    // reason and retries instead of the transport blowing up.
    assert!(resp.get("error").is_none());
    assert_eq!(resp["result"]["isError"], true);
    let payload = &resp["result"]["structuredContent"];
    assert_eq!(payload["error"]["code"], "schema_violation");
    assert_eq!(payload["error"]["exitCode"], 2);
    assert!(payload["error"]["message"]
        .as_str()
        .unwrap()
        .contains("radius must be positive"));
}

#[test]
fn a_tool_call_with_no_arguments_object_still_works() {
    let handlers = Handlers::new().with("dpaint_lint", Box::new(|args| Ok(json!({"got": args}))));
    let resp = handle_message(
        &registry(),
        &handlers,
        &req(5, "tools/call", json!({"name": "dpaint_lint"})),
    )
    .unwrap();
    assert_eq!(resp["result"]["structuredContent"]["got"], json!({}));
}

#[test]
fn op_tools_are_hidden_when_the_server_cannot_run_them() {
    let handlers = Handlers::new().with("dpaint_render", Box::new(|_| Ok(json!({}))));
    let resp = handle_message(&registry(), &handlers, &req(6, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        1,
        "no project, no op tools — nothing lies to the agent"
    );
    assert_eq!(tools[0]["name"], "dpaint_render");
}

#[test]
fn a_full_session_runs_over_a_stream() {
    let rec = Arc::new(Recorder::default());
    let handlers = rec.handlers();
    let session = [
        req(1, "initialize", json!({"protocolVersion": "2025-06-18"})).to_string(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        req(2, "tools/list", json!({})).to_string(),
        req(
            3,
            "tools/call",
            json!({"name": "vector_measure_bbox", "arguments": {"target": "#mark"}}),
        )
        .to_string(),
    ]
    .join("\n");

    let mut out = Vec::new();
    serve(
        &registry(),
        &handlers,
        std::io::Cursor::new(session),
        &mut out,
    )
    .unwrap();

    let responses: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(responses.len(), 3, "the notification produced no reply");
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "degen-paint");
    assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        responses[2]["result"]["structuredContent"]["op"],
        "vector.measure.bbox"
    );
}

#[test]
fn the_built_in_loop_tools_read_and_write_a_real_project() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project::new(
        "poster",
        Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 16, 16)),
    );
    let ws = dpaint_core::Workspace::create(dir.path(), project).unwrap();

    struct AddLayer;
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct AddArgs {
        /// Name of the layer to add.
        name: String,
    }
    impl Op for AddLayer {
        fn id(&self) -> &'static str {
            "raster.layer.add"
        }
        fn about(&self) -> &'static str {
            "Add a fill layer."
        }
        fn schema(&self) -> Value {
            schema_for::<AddArgs>()
        }
        fn apply(&self, p: &mut Project, a: Value, cx: &mut OpCx) -> Result<OpEffect> {
            let args: AddArgs = dpaint_core::parse_args(self.id(), a)?;
            let doc_id = cx.target_doc(p)?;
            let doc = p.raster_mut(&doc_id)?;
            let id = dpaint_core::LayerId::from_name(&args.name);
            doc.layers.push(dpaint_core::doc::raster::Layer::new(
                id.clone(),
                args.name,
                dpaint_core::doc::raster::LayerKind::Fill {
                    color: dpaint_core::Color::WHITE,
                },
            ));
            Ok(OpEffect::changed(&doc_id).with_created(id.to_string()))
        }
    }

    let mut reg = Registry::new();
    reg.register(AddLayer);
    let engine = Arc::new(Mutex::new(dpaint_core::Engine::new(reg.clone(), ws)));
    let handlers = Handlers::new().backed_by(engine);

    let applied = handle_message(
        &reg,
        &handlers,
        &req(
            1,
            "tools/call",
            json!({
                "name": "dpaint_apply",
                "arguments": {"ops": [
                    {"op": "raster.layer.add", "args": {"name": "sky"}},
                    {"op": "raster.layer.add", "args": {"name": "title"}}
                ]}
            }),
        ),
    )
    .unwrap();
    assert_eq!(applied["result"]["isError"], false, "{applied}");
    assert_eq!(
        applied["result"]["structuredContent"]["applied"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let overview = handle_message(
        &reg,
        &handlers,
        &req(
            2,
            "tools/call",
            json!({"name": "dpaint_overview", "arguments": {}}),
        ),
    )
    .unwrap();
    let doc = &overview["result"]["structuredContent"]["documents"][0];
    assert_eq!(doc["layers"], 2, "the batch was committed to the project");
    assert_eq!(doc["size"], json!([16.0, 16.0]));

    let history = handle_message(
        &reg,
        &handlers,
        &req(
            3,
            "tools/call",
            json!({"name": "dpaint_history", "arguments": {"limit": 5}}),
        ),
    )
    .unwrap();
    let entries = history["result"]["structuredContent"]["entries"]
        .as_array()
        .unwrap();
    assert_eq!(
        entries.len(),
        1,
        "one transactional batch, one journal entry"
    );
    assert!(entries[0]["op"].as_str().unwrap().starts_with("batch["));

    // A failing batch leaves the committed state alone.
    let failed = handle_message(
        &reg,
        &handlers,
        &req(
            4,
            "tools/call",
            json!({"name": "dpaint_apply", "arguments": {"ops": [{"op": "raster.layer.nope", "args": {}}]}}),
        ),
    )
    .unwrap();
    assert_eq!(failed["result"]["isError"], true);
    assert_eq!(
        failed["result"]["structuredContent"]["error"]["code"],
        "unknown_op"
    );

    let after = handle_message(
        &reg,
        &handlers,
        &req(
            5,
            "tools/call",
            json!({"name": "dpaint_overview", "arguments": {}}),
        ),
    )
    .unwrap();
    assert_eq!(
        after["result"]["structuredContent"]["documents"][0]["layers"],
        2
    );
}

#[test]
fn serve_project_discovers_the_workspace_and_answers_over_streams() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().join("poster.dpaint");
    std::fs::create_dir_all(&project_dir).unwrap();
    let project = Project::new(
        "poster",
        Document::Raster(RasterDoc::new(DocId::from("doc_main"), "main", 24, 24)),
    );
    dpaint_core::Workspace::create(&project_dir, project).unwrap();

    // Point at the parent: the server finds the `.dpaint` project below it, the way the CLI
    // does when an agent runs it from the repository root.
    let session = [
        req(1, "initialize", json!({"protocolVersion": "2025-06-18"})).to_string(),
        req(
            2,
            "tools/call",
            json!({"name": "dpaint_overview", "arguments": {}}),
        )
        .to_string(),
    ]
    .join("\n");
    let mut out = Vec::new();
    dpaint_mcp::serve_project(
        registry(),
        Some(dir.path().to_path_buf()),
        Handlers::new(),
        std::io::Cursor::new(session),
        &mut out,
    )
    .unwrap();

    let responses: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    let overview = &responses[1]["result"]["structuredContent"];
    assert_eq!(overview["project"]["name"], "poster");
    assert_eq!(overview["documents"][0]["size"], json!([24.0, 24.0]));
}

#[test]
fn pointing_at_a_directory_with_no_project_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let err = dpaint_mcp::serve_project(
        registry(),
        Some(dir.path().to_path_buf()),
        Handlers::new(),
        std::io::Cursor::new(String::new()),
        Vec::new(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("no degen-paint project"), "{err}");
}
