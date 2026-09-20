//! Tool discovery.
//!
//! The op tools are *generated* from the registry: one tool per op, `inputSchema` is the op's
//! own schema, `description` is its `about()`. Nothing is written by hand, so the MCP surface
//! cannot drift from the CLI, the GUI or the docs.

use dpaint_core::Registry;
use serde_json::{json, Value};

/// MCP tool names are identifier-shaped; op ids are dotted. `raster.filter.gaussian-blur`
/// becomes `raster_filter_gaussian_blur`, and `tools/call` accepts either spelling.
pub fn tool_name(op_id: &str) -> String {
    op_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The op id behind a tool name, matching either the sanitized or the dotted spelling.
pub fn op_for_tool(registry: &Registry, name: &str) -> Option<&'static str> {
    registry
        .ids()
        .into_iter()
        .find(|id| *id == name || tool_name(id) == name)
}

/// One op as an MCP tool. The schema gains `doc` and `dryRun`, because a tool call has to be
/// able to say which document it targets and whether it is only asking.
pub fn op_tool(op: &dyn dpaint_core::Op) -> Value {
    let mut schema = op.schema();
    if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        props.insert(
            "doc".into(),
            json!({
                "type": "string",
                "description": "Target document id or name. Defaults to the active document."
            }),
        );
        props.insert(
            "dryRun".into(),
            json!({
                "type": "boolean",
                "description": "Validate and report the effect without writing anything."
            }),
        );
    }
    let mut description = op.about().to_string();
    description.push_str(&format!("\n\nOp id: {}", op.id()));
    if !op.modes().is_empty() {
        let modes: Vec<&str> = op.modes().iter().map(|m| m.as_str()).collect();
        description.push_str(&format!(" · documents: {}", modes.join(", ")));
    }
    if op.is_query() {
        description.push_str(" · read-only");
    }
    if op.is_network() {
        description.push_str(" · reaches a provider network and may cost money");
    }
    json!({
        "name": tool_name(op.id()),
        "description": description,
        "inputSchema": schema,
    })
}

/// The hand-written loop tools. These are not ops: they are the calls an agent's edit-render-
/// inspect loop actually needs, and batching thirty ops into one round trip is the difference
/// between one model turn and thirty.
pub const LOOP_TOOLS: [&str; 5] = [
    "dpaint_overview",
    "dpaint_render",
    "dpaint_lint",
    "dpaint_apply",
    "dpaint_history",
];

pub fn loop_tool(name: &str) -> Option<Value> {
    let spec = match name {
        "dpaint_overview" => json!({
            "name": "dpaint_overview",
            "description": "Project structure: documents, kinds, sizes, layer and object counts, \
                            assets, and what changed recently.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "doc": { "type": "string", "description": "Restrict to one document." }
                },
                "additionalProperties": false
            }
        }),
        "dpaint_render" => json!({
            "name": "dpaint_render",
            "description": "Render a document and return the image and its digest in one call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "doc": { "type": "string", "description": "Document to render; defaults to the active one." },
                    "scale": { "type": "number", "description": "Scale factor, default 1.", "exclusiveMinimum": 0 },
                    "format": { "type": "string", "enum": ["png", "jpeg", "webp"], "description": "Image format, default png." },
                    "annotate": { "type": "boolean", "description": "Overlay numbered bounding boxes with object ids." },
                    "digest": { "type": "boolean", "description": "Include the render digest, default true." },
                    "path": { "type": "string", "description": "Write the image here instead of returning it inline." }
                },
                "additionalProperties": false
            }
        }),
        "dpaint_lint" => json!({
            "name": "dpaint_lint",
            "description": "Run the lint rules and return findings, each with the selector of its offender.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "doc": { "type": "string", "description": "Document to lint; defaults to all." },
                    "severity": { "type": "string", "enum": ["info", "warning", "error"], "description": "Minimum severity to report." }
                },
                "additionalProperties": false
            }
        }),
        "dpaint_apply" => json!({
            "name": "dpaint_apply",
            "description": "Apply a batch of ops transactionally: every op is validated, applied to \
                            an in-memory clone and committed once. Op 17 of 30 failing means nothing \
                            was written.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ops": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Ops to apply, in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "description": "Op id, e.g. raster.filter.gaussian-blur." },
                                "args": { "type": "object", "description": "Arguments for that op." },
                                "doc": { "type": "string", "description": "Target document for that op." }
                            },
                            "required": ["op"],
                            "additionalProperties": false
                        }
                    },
                    "dryRun": { "type": "boolean", "description": "Validate the whole batch without writing." }
                },
                "required": ["ops"],
                "additionalProperties": false
            }
        }),
        "dpaint_history" => json!({
            "name": "dpaint_history",
            "description": "Recent journal entries, including edits a human made in the GUI.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "description": "How many entries, newest last. Default 20." }
                },
                "additionalProperties": false
            }
        }),
        _ => return None,
    };
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_op_ids_become_identifier_shaped_tool_names() {
        assert_eq!(
            tool_name("raster.filter.gaussian-blur"),
            "raster_filter_gaussian_blur"
        );
        assert_eq!(
            tool_name("ai.image.remove-background"),
            "ai_image_remove_background"
        );
    }

    #[test]
    fn every_loop_tool_has_a_spec() {
        for name in LOOP_TOOLS {
            let spec = loop_tool(name).unwrap_or_else(|| panic!("{name} has no spec"));
            assert_eq!(spec["name"], name);
            assert_eq!(spec["inputSchema"]["type"], "object");
        }
        assert!(loop_tool("dpaint_nonsense").is_none());
    }
}
