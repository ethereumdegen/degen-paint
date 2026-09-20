//! Turning `--flag value` into a schema-correct JSON argument object.
//!
//! The CLI does not hand-write a flag table per op — it reads the op's JSON Schema and coerces
//! what it is given. That is what keeps `dpaint op <id>`, the MCP tool and the GUI form in
//! lockstep: all three consume the same schema.

use dpaint_core::{Error, Result};
use serde_json::{Map, Value};

/// Follow a `$ref` into the schema's `$defs`, so `--text '{...}'` knows it wants an object.
fn deref<'a>(root: &'a Value, spec: &'a Value) -> &'a Value {
    let Some(r) = spec.get("$ref").and_then(|r| r.as_str()) else {
        return spec;
    };
    r.strip_prefix("#/$defs/")
        .and_then(|name| root.get("$defs").and_then(|d| d.get(name)))
        .unwrap_or(spec)
}

/// Resolve an optional-or-$ref wrapper down to the schema that carries the real type.
fn effective<'a>(root: &'a Value, spec: &'a Value) -> &'a Value {
    let spec = deref(root, spec);
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(arr) = spec.get(key).and_then(|v| v.as_array()) {
            if let Some(hit) = arr
                .iter()
                .map(|v| deref(root, v))
                .find(|v| v.get("type").and_then(|t| t.as_str()) != Some("null"))
            {
                return hit;
            }
        }
    }
    spec
}

/// Parse `["--radius", "12", "--layer", "#sky", "--invert"]` against a schema.
pub fn parse(op: &str, schema: &Value, argv: &[String]) -> Result<Value> {
    let props = schema.get("properties").and_then(|p| p.as_object());
    let mut out = Map::new();
    let mut i = 0;

    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--args" {
            let raw = argv.get(i + 1).ok_or_else(|| Error::SchemaViolation {
                op: op.into(),
                detail: "--args needs a JSON object".into(),
            })?;
            let v: Value = serde_json::from_str(raw).map_err(|e| Error::SchemaViolation {
                op: op.into(),
                detail: format!("--args is not valid JSON: {e}"),
            })?;
            if let Some(m) = v.as_object() {
                out.extend(m.clone());
            }
            i += 2;
            continue;
        }

        let Some(name) = tok.strip_prefix("--") else {
            return Err(Error::SchemaViolation {
                op: op.into(),
                detail: format!("unexpected argument '{tok}' (flags look like --name value)"),
            });
        };

        // `--flag=value` as well as `--flag value`.
        let (name, inline) = match name.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (name, None),
        };
        // `--text.size 64` sets a field inside an object argument.
        let (root_name, sub) = match name.split_once('.') {
            Some((a, b)) => (a, Some(b.replace('-', "_"))),
            None => (name, None),
        };
        let key = root_name.replace('-', "_");
        let spec = props.and_then(|p| p.get(&key).or_else(|| p.get(root_name)));

        if spec.is_none() {
            let known: Vec<&str> = props
                .map(|p| p.keys().map(|s| s.as_str()).collect())
                .unwrap_or_default();
            return Err(Error::SchemaViolation {
                op: op.into(),
                detail: format!(
                    "unknown argument '--{name}'; this op takes: {}",
                    known.join(", ")
                ),
            });
        }

        let resolved = effective(schema, spec.expect("checked above"));
        let ty = if sub.is_some() {
            sub_type(schema, resolved, sub.as_deref().unwrap_or(""))
        } else {
            type_of(resolved)
        };
        if ty == "boolean" && inline.is_none() {
            // Bare `--invert` means true, but `--invert false` still works.
            let next_is_value = argv
                .get(i + 1)
                .map(|v| matches!(v.as_str(), "true" | "false"))
                .unwrap_or(false);
            if next_is_value {
                out.insert(key, Value::Bool(argv[i + 1] == "true"));
                i += 2;
            } else {
                out.insert(key, Value::Bool(true));
                i += 1;
            }
            continue;
        }

        let raw = match inline {
            Some(v) => v,
            None => argv
                .get(i + 1)
                .cloned()
                .ok_or_else(|| Error::SchemaViolation {
                    op: op.into(),
                    detail: format!("'--{name}' needs a value"),
                })?,
        };
        let consumed = if spec.is_some() && argv.get(i + 1).map(|v| v == &raw).unwrap_or(false) {
            2
        } else {
            1
        };
        match sub {
            Some(field) => {
                let slot = out.entry(key).or_insert_with(|| Value::Object(Map::new()));
                if !slot.is_object() {
                    *slot = Value::Object(Map::new());
                }
                slot.as_object_mut()
                    .expect("just ensured object")
                    .insert(field, coerce(&raw, &ty));
            }
            None => {
                out.insert(key, coerce_for(schema, resolved, &raw, &ty));
            }
        }
        i += consumed;
    }

    Ok(Value::Object(out))
}

/// Type of a named field inside an object-typed argument.
fn sub_type(root: &Value, spec: &Value, field: &str) -> String {
    spec.get("properties")
        .and_then(|p| p.get(field))
        .map(|f| type_of(effective(root, f)))
        .unwrap_or_else(|| "string".into())
}

/// Coerce, with one ergonomic rule: an object argument that has exactly one required
/// field accepts a bare scalar for that field, so `--text "HELLO"` does the obvious thing.
fn coerce_for(root: &Value, spec: &Value, raw: &str, ty: &str) -> Value {
    if ty == "object" && !raw.trim_start().starts_with('{') {
        let required: Vec<&str> = spec
            .get("required")
            .and_then(|r| r.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        if required.len() == 1 {
            let field = required[0];
            let field_ty = sub_type(root, spec, field);
            let mut m = Map::new();
            m.insert(field.to_string(), coerce(raw, &field_ty));
            return Value::Object(m);
        }
    }
    coerce(raw, ty)
}

fn type_of(spec: &Value) -> String {
    // Schemas from `schemars` express optionals as `["string", "null"]` or `anyOf`.
    if let Some(t) = spec.get("type") {
        if let Some(s) = t.as_str() {
            return s.to_string();
        }
        if let Some(arr) = t.as_array() {
            if let Some(first) = arr.iter().find_map(|v| v.as_str()).filter(|s| *s != "null") {
                return first.to_string();
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(arr) = spec.get(key).and_then(|v| v.as_array()) {
            for v in arr {
                let t = type_of(v);
                if t != "null" && !t.is_empty() {
                    return t;
                }
            }
        }
    }
    "string".into()
}

fn coerce(raw: &str, ty: &str) -> Value {
    match ty {
        "integer" => raw
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(raw.into())),
        "number" => raw
            .parse::<f64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(raw.into())),
        "boolean" => Value::Bool(raw == "true" || raw == "1" || raw == "yes"),
        "array" => {
            // `--size 100,200` and `--points [[0,0],[1,1]]` both work.
            if raw.trim_start().starts_with('[') {
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into()))
            } else {
                Value::Array(raw.split(',').map(|p| coerce_scalar(p.trim())).collect())
            }
        }
        "object" => serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into())),
        _ => Value::String(raw.into()),
    }
}

fn coerce_scalar(raw: &str) -> Value {
    if let Ok(i) = raw.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return Value::from(f);
    }
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        other => Value::String(other.into()),
    }
}

/// Human-readable usage generated from the schema, so `--help` never goes stale.
pub fn usage(id: &str, about: &str, schema: &Value) -> String {
    let mut s = format!("{id}\n  {about}\n\n");
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        if props.is_empty() {
            s.push_str("  (takes no arguments)\n");
        }
        for (name, spec) in props {
            let flag = name.replace('_', "-");
            let resolved = effective(schema, spec);
            let ty = type_of(resolved);
            let req = if required.contains(&name.as_str()) {
                " (required)"
            } else {
                ""
            };
            let desc = spec
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");
            s.push_str(&format!("  --{flag} <{ty}>{req}\n"));
            if !desc.is_empty() {
                s.push_str(&format!("      {desc}\n"));
            }
            if ty == "object" {
                if let Some(fields) = resolved.get("properties").and_then(|p| p.as_object()) {
                    let names: Vec<String> = fields
                        .keys()
                        .map(|f| format!("--{flag}.{}", f.replace('_', "-")))
                        .collect();
                    s.push_str(&format!("      fields: {}\n", names.join(" ")));
                }
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "radius": { "type": "number", "description": "blur radius in px" },
                "layer": { "type": "string" },
                "invert": { "type": "boolean" },
                "size": { "type": "array", "items": { "type": "integer" } },
                "seed": { "type": ["integer", "null"] },
                "fill_rule": { "type": "string" }
            },
            "required": ["radius"]
        })
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scalars_are_coerced_to_their_schema_types() {
        let v = parse(
            "t",
            &schema(),
            &args(&["--radius", "12", "--layer", "#sky"]),
        )
        .unwrap();
        assert_eq!(v["radius"], 12.0);
        assert_eq!(v["layer"], "#sky");
        assert!(
            v["radius"].is_number(),
            "a number flag must not arrive as a string"
        );
    }

    #[test]
    fn bare_boolean_flags_mean_true_and_can_still_be_set_explicitly() {
        let v = parse("t", &schema(), &args(&["--radius", "1", "--invert"])).unwrap();
        assert_eq!(v["invert"], true);
        let v = parse(
            "t",
            &schema(),
            &args(&["--radius", "1", "--invert", "false"]),
        )
        .unwrap();
        assert_eq!(v["invert"], false);
    }

    #[test]
    fn arrays_accept_both_comma_lists_and_json() {
        let v = parse(
            "t",
            &schema(),
            &args(&["--radius", "1", "--size", "100,200"]),
        )
        .unwrap();
        assert_eq!(v["size"], serde_json::json!([100, 200]));
        let v = parse("t", &schema(), &args(&["--radius", "1", "--size", "[3,4]"])).unwrap();
        assert_eq!(v["size"], serde_json::json!([3, 4]));
    }

    #[test]
    fn kebab_flags_map_to_snake_fields_and_inline_values_work() {
        let v = parse(
            "t",
            &schema(),
            &args(&["--radius=2", "--fill-rule=evenodd"]),
        )
        .unwrap();
        assert_eq!(v["radius"], 2.0);
        assert_eq!(v["fill_rule"], "evenodd");
    }

    #[test]
    fn nullable_schema_types_still_coerce_to_the_real_type() {
        let v = parse("t", &schema(), &args(&["--radius", "1", "--seed", "7"])).unwrap();
        assert_eq!(v["seed"], 7);
        assert!(v["seed"].is_i64());
    }

    #[test]
    fn an_unknown_flag_lists_what_the_op_actually_takes() {
        let err = parse("blur", &schema(), &args(&["--radiuss", "1"])).unwrap_err();
        assert_eq!(err.code(), "schema_violation");
        assert!(
            err.to_string().contains("radius"),
            "the error must name the real flags: {err}"
        );
    }

    #[test]
    fn raw_json_can_be_passed_wholesale() {
        let v = parse(
            "t",
            &schema(),
            &args(&["--args", r##"{"radius": 4, "layer": "#a"}"##]),
        )
        .unwrap();
        assert_eq!(v["radius"], 4);
        assert_eq!(v["layer"], "#a");
    }

    fn object_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "anyOf": [{ "$ref": "#/$defs/TextSpec" }, { "type": "null" }],
                          "description": "Text content and typography." }
            },
            "$defs": {
                "TextSpec": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "size": { "type": "number" },
                        "family": { "type": "string" }
                    },
                    "required": ["text"]
                }
            }
        })
    }

    #[test]
    fn a_bare_scalar_fills_an_objects_single_required_field() {
        let v = parse("t", &object_schema(), &args(&["--text", "URBAN EXPLORER"])).unwrap();
        assert_eq!(v["text"]["text"], "URBAN EXPLORER");
    }

    #[test]
    fn dotted_flags_set_fields_inside_an_object_argument() {
        let v = parse(
            "t",
            &object_schema(),
            &args(&[
                "--text",
                "HELLO",
                "--text.size",
                "64",
                "--text.family",
                "Inter",
            ]),
        )
        .unwrap();
        assert_eq!(v["text"]["text"], "HELLO");
        assert_eq!(
            v["text"]["size"], 64.0,
            "the nested field must keep its schema type"
        );
        assert_eq!(v["text"]["family"], "Inter");
    }

    #[test]
    fn a_full_json_object_still_wins_over_the_convenience_rule() {
        let v = parse(
            "t",
            &object_schema(),
            &args(&[r#"--text={"text":"A","size":12}"#]),
        )
        .unwrap();
        assert_eq!(v["text"]["size"], 12);
    }

    #[test]
    fn usage_lists_the_fields_of_an_object_argument() {
        let u = usage("raster.layer.add", "Add a layer", &object_schema());
        assert!(u.contains("--text <object>"), "{u}");
        assert!(
            u.contains("--text.size"),
            "usage must show how to reach nested fields:\n{u}"
        );
    }

    #[test]
    fn usage_is_generated_from_the_schema_including_descriptions() {
        let u = usage("raster.filter.gaussian-blur", "Blur a layer", &schema());
        assert!(u.contains("--radius <number> (required)"));
        assert!(u.contains("blur radius in px"));
        assert!(u.contains("--fill-rule <string>"));
    }
}
