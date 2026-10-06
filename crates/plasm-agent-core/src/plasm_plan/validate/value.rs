use super::*;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ValueValidationKind {
    #[error("predicate field path must be non-empty")]
    EmptyPredicateFieldPath,
    #[error("quantifier binding must be a non-empty root alias")]
    InvalidQuantifierBinding,
    #[error("template requires non-empty root binding aliases")]
    InvalidTemplateBinding,
    #[error("entity_ref_key api and entity must be non-empty")]
    EmptyEntityReferenceOwner,
    #[error("entity_ref_key compound object must not be empty")]
    EmptyEntityReferenceKey,
    #[error("entity_ref_key compound object contains an empty key")]
    EmptyEntityReferenceKeyField,
    #[error("entity_ref_key compound values must be literals, symbols, or templates")]
    InvalidEntityReferenceKeyValue,
    #[error("entity_ref_key must be a literal, symbol, template, or non-empty compound object")]
    InvalidEntityReferenceKey,
    #[error("value contains an unnormalized entity_ref wrapper")]
    UnnormalizedEntityReference,
    #[error("object contains an empty key")]
    EmptyObjectKey,
    #[error("string contains JavaScript object string coercion ([object Object])")]
    JavaScriptObjectCoercion,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("plan node {node_index} {path}: {kind}")]
pub struct ValueValidationError {
    pub node_index: usize,
    pub path: String,
    #[source]
    pub kind: ValueValidationKind,
}

fn invalid(node_index: usize, path: &str, kind: ValueValidationKind) -> ValueValidationError {
    ValueValidationError {
        node_index,
        path: path.to_owned(),
        kind,
    }
}

pub(super) fn validate_predicate(
    p: &PlanPredicate,
    node_index: usize,
    pred_index: usize,
) -> Result<(), ValueValidationError> {
    if p.field_path.segments().is_empty()
        || p.field_path.segments().iter().any(|s| s.trim().is_empty())
    {
        return Err(invalid(
            node_index,
            &format!("predicates[{pred_index}].field_path"),
            ValueValidationKind::EmptyPredicateFieldPath,
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
) -> Result<(), ValueValidationError> {
    match value {
        PlanValue::Quantified {
            binding,
            collection,
            predicate,
            ..
        } => {
            if binding.is_empty() || binding.contains('.') {
                return Err(invalid(
                    node_index,
                    path,
                    ValueValidationKind::InvalidQuantifierBinding,
                ));
            }
            validate_plan_value_expr(collection, node_index, path)?;
            validate_plan_value_expr(predicate, node_index, path)
        }
        PlanValue::Expression { expression } => expression
            .try_map(|v| validate_plan_value_expr(v, node_index, path))
            .map(|_| ()),
        PlanValue::Literal { value } => {
            validate_data_no_js_object_coercion(value.value(), node_index, path)
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
                if b.from.trim().is_empty()
                    || b.to.trim().is_empty()
                    || b.from.contains('.')
                    || b.to.contains('.')
                {
                    return Err(invalid(
                        node_index,
                        path,
                        ValueValidationKind::InvalidTemplateBinding,
                    ));
                }
            }
            Ok(())
        }
        PlanValue::EntityRefKey { api, entity, key } => {
            validate_no_js_object_coercion(api, node_index, path)?;
            validate_no_js_object_coercion(entity, node_index, path)?;
            if api.trim().is_empty() || entity.trim().is_empty() {
                return Err(invalid(
                    node_index,
                    path,
                    ValueValidationKind::EmptyEntityReferenceOwner,
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
                return Err(invalid(
                    node_index,
                    path,
                    ValueValidationKind::UnnormalizedEntityReference,
                ));
            }
            for (k, field) in fields {
                if k.trim().is_empty() {
                    return Err(invalid(
                        node_index,
                        path,
                        ValueValidationKind::EmptyObjectKey,
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
) -> Result<(), ValueValidationError> {
    match value {
        PlanValue::Literal { .. }
        | PlanValue::BindingSymbol { .. }
        | PlanValue::NodeSymbol { .. }
        | PlanValue::Template { .. } => validate_plan_value_expr(value, node_index, path),
        PlanValue::Object { fields } => {
            if fields.is_empty() {
                return Err(invalid(
                    node_index,
                    path,
                    ValueValidationKind::EmptyEntityReferenceKey,
                ));
            }
            if looks_like_unnormalized_entity_ref_wrapper(fields) {
                return Err(invalid(
                    node_index,
                    path,
                    ValueValidationKind::UnnormalizedEntityReference,
                ));
            }
            for (field, child) in fields {
                if field.trim().is_empty() {
                    return Err(invalid(
                        node_index,
                        path,
                        ValueValidationKind::EmptyEntityReferenceKeyField,
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
                        let _ = other;
                        return Err(invalid(
                            node_index,
                            &format!("{path}.{field}"),
                            ValueValidationKind::InvalidEntityReferenceKeyValue,
                        ));
                    }
                }
            }
            Ok(())
        }
        _ => Err(invalid(
            node_index,
            path,
            ValueValidationKind::InvalidEntityReferenceKey,
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
            value
        }) if value.as_str().is_some()
    ) && matches!(
        fields.get("entity"),
        Some(PlanValue::Literal {
            value
        }) if value.as_str().is_some()
    )
}

fn validate_data_no_js_object_coercion(
    value: &plasm_core::Value,
    node_index: usize,
    path: &str,
) -> Result<(), ValueValidationError> {
    match value {
        plasm_core::Value::String(s) => validate_no_js_object_coercion(s, node_index, path),
        plasm_core::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                validate_data_no_js_object_coercion(item, node_index, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        plasm_core::Value::Object(fields) => {
            for (k, field) in fields {
                validate_data_no_js_object_coercion(field, node_index, &format!("{path}.{k}"))?;
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
) -> Result<(), ValueValidationError> {
    if text.contains("[object Object]") {
        return Err(invalid(
            node_index,
            path,
            ValueValidationKind::JavaScriptObjectCoercion,
        ));
    }
    Ok(())
}
