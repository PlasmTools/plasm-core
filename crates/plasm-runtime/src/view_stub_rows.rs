//! Schema-derived stub rows for view DAG preflight (no HTTP).

use std::collections::BTreeMap;

use indexmap::IndexMap;
use plasm_core::schema::{CapabilitySchema, EntityDef, ViewDefinition, ViewOutputBinding};
use plasm_core::{FieldType, Ref, TypedFieldValue, Value, CGS};

use crate::cache::{CachedEntity, EntityCompleteness};
use crate::execution::{current_timestamp, ExecutionResult, ExecutionSource, ExecutionStats};
use crate::RuntimeError;

fn placeholder_value(field_type: &FieldType) -> Value {
    match field_type {
        FieldType::Boolean => Value::Bool(false),
        FieldType::Number | FieldType::Integer => Value::Integer(0),
        FieldType::Uuid
        | FieldType::String
        | FieldType::Blob
        | FieldType::Select
        | FieldType::Date => Value::String(String::new()),
        FieldType::MultiSelect | FieldType::Array => Value::Array(vec![]),
        FieldType::Json => Value::Object(IndexMap::new()),
        FieldType::Money => plasm_core::money::normalize(
            Value::String("0".into()),
            plasm_core::MoneyWireFormat::decimal_string(),
            None,
        )
        .unwrap_or_else(|_| Value::String("0".into())),
        FieldType::EntityRef { target, .. } => Value::String(format!("stub-{target}")),
    }
}

fn field_names_for_stub(cap: &CapabilitySchema, entity: &EntityDef) -> Vec<String> {
    if !cap.provides.is_empty() {
        cap.provides.clone()
    } else {
        entity.fields.keys().map(|k| k.to_string()).collect()
    }
}

/// Build one stub entity row for a query/search node.
///
/// When `node_field_where` is set, the stub includes a row whose `where_field` matches the
/// scope needle so [`ViewOutputBinding::NodeFieldWhere`] preflight gates pass without HTTP.
pub fn stub_query_result(
    cap: &CapabilitySchema,
    cgs: &CGS,
    bound_values: &IndexMap<String, Value>,
    node_field_where: Option<(&ViewDefinition, &str, &IndexMap<String, Value>)>,
) -> Result<ExecutionResult, RuntimeError> {
    let entity =
        cgs.get_entity(cap.domain.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("stub query: unknown entity `{}`", cap.domain),
            })?;
    let mut fields = IndexMap::new();
    for name in field_names_for_stub(cap, entity) {
        let v = bound_values.get(name.as_str()).cloned().unwrap_or_else(|| {
            entity
                .fields
                .get(name.as_str())
                .and_then(|fs| fs.named_value(cgs).ok())
                .map(|nv| placeholder_value(&nv.field_type))
                .unwrap_or_else(|| Value::String(String::new()))
        });
        fields.insert(name, TypedFieldValue::from_value(v));
    }
    let reference = stub_entity_ref(entity, &fields)?;
    let ts = current_timestamp();
    let cached = CachedEntity::from_decoded(
        reference,
        fields.into_iter().map(|(k, v)| (k, v.to_value())).collect(),
        IndexMap::new(),
        ts,
        EntityCompleteness::Summary,
    );
    let mut res = ExecutionResult {
        entities: vec![cached],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: ExecutionStats::default(),
        request_fingerprints: Vec::new(),
    };

    if let Some((view, node_id, scope)) = node_field_where {
        apply_node_field_where_stub(view, node_id, scope, cap, cgs, &mut res)?;
    }

    Ok(res)
}

fn apply_node_field_where_stub(
    view: &ViewDefinition,
    node_id: &str,
    scope: &IndexMap<String, Value>,
    cap: &CapabilitySchema,
    cgs: &CGS,
    res: &mut ExecutionResult,
) -> Result<(), RuntimeError> {
    let entity = cgs
        .get_entity(cap.domain.as_str())
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!(
                "node_field_where stub: unknown entity `{}`",
                cap.domain
            ),
        })?;

    for binding in view.output.values() {
        let ViewOutputBinding::NodeFieldWhere {
            node,
            where_field,
            equals_scope,
            field,
        } = binding
        else {
            continue;
        };
        if node.as_str() != node_id {
            continue;
        }
        let Some(needle) = scope.get(equals_scope.as_str()) else {
            continue;
        };
        let mut fields = IndexMap::new();
        for name in field_names_for_stub(cap, entity) {
            let v = if name == where_field.as_str() {
                needle.clone()
            } else if name == field.as_str() {
                Value::String("stub".into())
            } else {
                entity
                    .fields
                    .get(name.as_str())
                    .and_then(|fs| fs.named_value(cgs).ok())
                    .map(|nv| placeholder_value(&nv.field_type))
                    .unwrap_or_else(|| Value::String(String::new()))
            };
            fields.insert(name, TypedFieldValue::from_value(v));
        }
        let reference = stub_entity_ref(entity, &fields)?;
        let ts = current_timestamp();
        let plain: IndexMap<String, Value> = fields
            .iter()
            .map(|(k, v)| (k.clone(), v.to_value()))
            .collect();
        let cached = CachedEntity::from_decoded(
            reference,
            plain,
            IndexMap::new(),
            ts,
            EntityCompleteness::Summary,
        );
        res.entities = vec![cached];
        res.count = 1;
    }

    Ok(())
}

/// Build one stub entity row for a get node (identity from bound params).
pub fn stub_get_result(
    cap: &CapabilitySchema,
    cgs: &CGS,
    bound_param_to_string: &BTreeMap<String, String>,
) -> Result<ExecutionResult, RuntimeError> {
    let entity =
        cgs.get_entity(cap.domain.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("stub get: unknown entity `{}`", cap.domain),
            })?;
    let mut fields = IndexMap::new();
    for name in field_names_for_stub(cap, entity) {
        let v = if let Some(s) = bound_param_to_string.get(name.as_str()) {
            Value::String(s.clone())
        } else if name == entity.id_field.as_str() {
            bound_param_to_string
                .get("id")
                .cloned()
                .map(Value::String)
                .unwrap_or_else(|| placeholder_value(&FieldType::String))
        } else {
            entity
                .fields
                .get(name.as_str())
                .and_then(|fs| fs.named_value(cgs).ok())
                .map(|nv| placeholder_value(&nv.field_type))
                .unwrap_or_else(|| Value::String(String::new()))
        };
        fields.insert(name, TypedFieldValue::from_value(v));
    }
    let reference = stub_entity_ref(entity, &fields)?;
    let ts = current_timestamp();
    let plain: IndexMap<String, Value> = fields
        .iter()
        .map(|(k, v)| (k.clone(), v.to_value()))
        .collect();
    let cached = CachedEntity::from_decoded(
        reference,
        plain,
        IndexMap::new(),
        ts,
        EntityCompleteness::Complete,
    );
    Ok(ExecutionResult {
        entities: vec![cached],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: ExecutionStats::default(),
        request_fingerprints: Vec::new(),
    })
}

fn stub_entity_ref(
    entity: &EntityDef,
    fields: &IndexMap<String, TypedFieldValue>,
) -> Result<Ref, RuntimeError> {
    if !entity.key_vars.is_empty() {
        let mut parts = std::collections::BTreeMap::new();
        for kv in &entity.key_vars {
            let v = fields
                .get(kv.as_str())
                .map(TypedFieldValue::to_value)
                .unwrap_or(Value::String(String::new()));
            let s = match v {
                Value::String(s) => s,
                Value::Integer(i) => i.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Float(f) => f.to_string(),
                _ => String::new(),
            };
            parts.insert(kv.to_string(), s);
        }
        Ok(Ref::compound(entity.name.clone(), parts))
    } else {
        let id = fields
            .get(entity.id_field.as_str())
            .map(TypedFieldValue::to_value)
            .unwrap_or(Value::String(String::new()));
        let id_s = match id {
            Value::String(s) => s,
            Value::Integer(i) => i.to_string(),
            other => format!("{other:?}"),
        };
        Ok(Ref::new(entity.name.clone(), id_s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_test_support::matrix_views_cgs;
    use crate::view_preflight::preflight_view_scoped_with_proof;
    use crate::view_plan::ViewAmbientContext;
    use crate::materialization::SessionMaterialization;

    #[test]
    fn stub_query_uses_provides_fields() {
        let cgs = matrix_views_cgs();
        let cap = cgs.get_capability("langitem_get").expect("cap");
        let res = stub_get_result(cap, &cgs, &BTreeMap::from([("id".into(), "item-1".into())]))
            .expect("stub");
        assert_eq!(res.count, 1);
        assert_eq!(
            res.entities[0]
                .fields
                .get("id")
                .map(TypedFieldValue::to_value),
            Some(Value::String("item-1".into()))
        );
    }

    #[test]
    fn preflight_node_field_where_list_stub_injects_scope_match() {
        let cgs = matrix_views_cgs();
        let scope = indexmap::IndexMap::from([("key".into(), Value::String("item-1".into()))]);
        let mat = SessionMaterialization::new();
        let ambient = ViewAmbientContext::default();
        let proof = preflight_view_scoped_with_proof(
            "lang_key_pick",
            scope,
            &cgs,
            &ambient,
            &mat,
        )
        .expect("preflight");
        assert_eq!(
            proof.output_fields.get("title"),
            Some(&Value::String("stub".into()))
        );
        assert_eq!(
            proof.output_fields.get("key"),
            Some(&Value::String("item-1".into()))
        );
    }
}
