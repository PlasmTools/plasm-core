use super::*;

pub(super) fn validate_predicate(
    p: &PlanPredicate,
    node_index: usize,
    pred_index: usize,
) -> Result<(), String> {
    if p.field_path.segments().is_empty()
        || p.field_path.segments().iter().any(|s| s.trim().is_empty())
    {
        return Err(format!(
            "plan.nodes[{node_index}].predicates[{pred_index}].field_path must be non-empty"
        ));
    }
    if let PlanValue::Helper { name, .. } = &p.value {
        return Err(format!(
            "plan.nodes[{node_index}].predicates[{pred_index}].value helper {name:?} is not executable in program-plan predicates; pass a literal/entity_ref_key value or precompute it"
        ));
    }
    validate_plan_value_expr(
        &p.value,
        node_index,
        &format!("predicates[{pred_index}].value"),
    )?;
    Ok(())
}

pub(super) fn validate_plan_value_expr(
    value: &PlanValue,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    match value {
        PlanValue::Literal { value } => {
            validate_json_value_no_js_object_coercion(value, node_index, path)
        }
        PlanValue::Helper { args, .. } => {
            for (i, arg) in args.iter().enumerate() {
                validate_json_value_no_js_object_coercion(
                    arg,
                    node_index,
                    &format!("{path}.args[{i}]"),
                )?;
            }
            Ok(())
        }
        PlanValue::Symbol { path: symbol_path } => {
            validate_no_js_object_coercion(symbol_path, node_index, path)
        }
        PlanValue::BindingSymbol {
            binding,
            path: segments,
        } => {
            validate_no_js_object_coercion(binding, node_index, path)?;
            for (i, segment) in segments.iter().enumerate() {
                validate_no_js_object_coercion(segment, node_index, &format!("{path}.path[{i}]"))?;
            }
            Ok(())
        }
        PlanValue::NodeSymbol {
            node,
            alias,
            path: segments,
        } => {
            validate_no_js_object_coercion(node, node_index, path)?;
            validate_no_js_object_coercion(alias, node_index, path)?;
            for (i, segment) in segments.iter().enumerate() {
                validate_no_js_object_coercion(segment, node_index, &format!("{path}.path[{i}]"))?;
            }
            Ok(())
        }
        PlanValue::Template {
            template,
            input_bindings,
        } => {
            validate_no_js_object_coercion(template.source(), node_index, path)?;
            for b in input_bindings {
                if b.from.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{node_index}].{path} has an empty binding"
                    ));
                }
            }
            Ok(())
        }
        PlanValue::EntityRefKey { api, entity, key } => {
            validate_no_js_object_coercion(api, node_index, path)?;
            validate_no_js_object_coercion(entity, node_index, path)?;
            if api.trim().is_empty() || entity.trim().is_empty() {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} entity_ref_key api and entity must be non-empty"
                ));
            }
            validate_entity_ref_key_value(key, node_index, &format!("{path}.key"))
        }
        PlanValue::Array { items } => {
            for (i, item) in items.iter().enumerate() {
                validate_plan_value_expr(item, node_index, &format!("{path}.items[{i}]"))?;
            }
            Ok(())
        }
        PlanValue::Object { fields } => {
            if looks_like_unnormalized_entity_ref_wrapper(fields) {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} contains an unnormalized entity_ref wrapper; lower {{api, entity, key}} to its key payload before validation"
                ));
            }
            for (k, field) in fields {
                if k.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{node_index}].{path} contains an empty object key"
                    ));
                }
                validate_plan_value_expr(field, node_index, &format!("{path}.{k}"))?;
            }
            Ok(())
        }
    }
}

fn validate_entity_ref_key_value(
    value: &PlanValue,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    match value {
        PlanValue::Literal { .. }
        | PlanValue::BindingSymbol { .. }
        | PlanValue::NodeSymbol { .. }
        | PlanValue::Template { .. } => validate_plan_value_expr(value, node_index, path),
        PlanValue::Object { fields } => {
            if fields.is_empty() {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} compound entity_ref_key object must not be empty"
                ));
            }
            if looks_like_unnormalized_entity_ref_wrapper(fields) {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} contains an unnormalized entity_ref wrapper; entity_ref_key.key must contain only key fields"
                ));
            }
            for (field, child) in fields {
                if field.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{node_index}].{path} compound entity_ref_key object contains an empty key"
                    ));
                }
                match child {
                    PlanValue::Literal { .. }
                    | PlanValue::BindingSymbol { .. }
                    | PlanValue::NodeSymbol { .. }
                    | PlanValue::Template { .. } => {
                        validate_plan_value_expr(child, node_index, &format!("{path}.{field}"))?
                    }
                    other => {
                        return Err(format!(
                            "plan.nodes[{node_index}].{path}.{field} must be a literal, binding symbol, node symbol, or template for compound entity_ref_key (got {other:?})"
                        ));
                    }
                }
            }
            Ok(())
        }
        other => Err(format!(
            "plan.nodes[{node_index}].{path} must be a literal, binding symbol, node symbol, template, or non-empty compound object for entity_ref_key (got {other:?})"
        )),
    }
}

fn looks_like_unnormalized_entity_ref_wrapper(fields: &BTreeMap<String, PlanValue>) -> bool {
    if fields.len() != 3
        || !fields.contains_key("api")
        || !fields.contains_key("entity")
        || !fields.contains_key("key")
    {
        return false;
    }
    matches!(
        fields.get("api"),
        Some(PlanValue::Literal {
            value: serde_json::Value::String(_)
        })
    ) && matches!(
        fields.get("entity"),
        Some(PlanValue::Literal {
            value: serde_json::Value::String(_)
        })
    )
}

fn validate_json_value_no_js_object_coercion(
    value: &serde_json::Value,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(s) => validate_no_js_object_coercion(s, node_index, path),
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                validate_json_value_no_js_object_coercion(
                    item,
                    node_index,
                    &format!("{path}[{i}]"),
                )?;
            }
            Ok(())
        }
        serde_json::Value::Object(fields) => {
            for (k, field) in fields {
                validate_json_value_no_js_object_coercion(
                    field,
                    node_index,
                    &format!("{path}.{k}"),
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(super) fn validate_no_js_object_coercion(
    text: &str,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    if text.contains("[object Object]") {
        return Err(format!(
            "plan.nodes[{node_index}].{path} contains JavaScript object string coercion ([object Object]); use a symbolic field/template value instead"
        ));
    }
    Ok(())
}
