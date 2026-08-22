//! Normalize authoring-wire mapping JSON into tagged CML before deserialization.
//!
//! Catalog `mappings.yaml` may attach optional body/query fields as:
//! ```yaml
//! - - status
//!   - name: status
//!     type: var
//!     if:
//!       exists: status
//! ```
//! That shorthand is not valid tagged [`crate::cml::CmlCond`] JSON (`type: exists`).
//! [`super::transport::parse_capability_template`] normalizes these nodes into
//! explicit `type: if` expressions before serde builds [`crate::cml::CmlExpr`].

use serde_json::{json, Value};

/// Walk a capability mapping template and expand optional `if: {exists: …}` field specs.
pub fn normalize_wire_cml_template(template: Value) -> Value {
    normalize_wire_cml_value(template)
}

fn normalize_wire_cml_value(value: Value) -> Value {
    match value {
        Value::Object(mut map) => {
            if map.contains_key("type") && map.contains_key("if") {
                let cond_raw = map
                    .remove("if")
                    .expect("if key present when contains_key true");
                let inner = normalize_wire_cml_value(Value::Object(map));
                return wrap_cml_if(normalize_cml_cond_value(cond_raw), inner);
            }

            if map.get("type").and_then(Value::as_str) == Some("object") {
                if let Some(Value::Array(fields)) = map.get_mut("fields") {
                    for entry in fields.iter_mut() {
                        if let Value::Array(pair) = entry {
                            if pair.len() == 2 {
                                pair[1] = normalize_wire_cml_value(pair[1].take());
                            }
                        }
                    }
                }
            }

            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k, normalize_wire_cml_value(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(normalize_wire_cml_value)
                .collect(),
        ),
        other => other,
    }
}

fn wrap_cml_if(condition: Value, then_expr: Value) -> Value {
    json!({
        "type": "if",
        "condition": condition,
        "then_expr": then_expr,
        "else_expr": { "type": "const", "value": null }
    })
}

fn normalize_cml_cond_value(value: Value) -> Value {
    if let Value::Object(map) = &value {
        if map.len() == 1 {
            if let Some(Value::String(var)) = map.get("exists") {
                return json!({ "type": "exists", "var": var });
            }
        }
    }
    normalize_wire_cml_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compile_operation, parse_capability_template, CmlEnv};
    use plasm_core::Value as PlasmValue;

    #[test]
    fn optional_var_field_if_exists_shorthand_compiles() {
        let v = json!({
            "method": "POST",
            "path": [{"type": "literal", "value": "v1"}],
            "body": {
                "type": "object",
                "fields": [
                    ["credit_card_account_id", {"type": "var", "name": "credit_card_account_id"}],
                    ["status", {"type": "var", "name": "status", "if": {"exists": "status"}}]
                ]
            }
        });
        let t = parse_capability_template(&v).unwrap();
        let mut env = CmlEnv::new();
        env.insert(
            "credit_card_account_id".into(),
            PlasmValue::String("cc".into()),
        );
        let compiled = compile_operation(&t, &env).expect("omit optional status");
        let crate::CompiledOperation::Http(req) = compiled else {
            panic!("expected http");
        };
        let obj = req.body.as_ref().unwrap().as_object().unwrap();
        assert!(!obj.contains_key("status"));
        env.insert("status".into(), PlasmValue::String("PENDING".into()));
        let compiled = compile_operation(&t, &env).expect("include status when bound");
        let crate::CompiledOperation::Http(req) = compiled else {
            panic!("expected http");
        };
        let obj = req.body.as_ref().unwrap().as_object().unwrap();
        assert_eq!(
            obj.get("status"),
            Some(&PlasmValue::String("PENDING".into()))
        );
    }
}
