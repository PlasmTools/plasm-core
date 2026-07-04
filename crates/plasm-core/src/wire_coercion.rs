//! Catalog-driven wire value coercion and relation-binding type assignability.

use crate::array_field_policy::{invoke_array_scalar_error, ArrayFieldCoercionPolicy};
use crate::{
    ArrayItemsSchema, EntityDef, FieldType, NamedValueSchema, RelationMaterialization,
    RelationSchema, Value, ValueWireFormat, CGS,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Static witness for a `query_scoped_bindings` / `get_scoped_bindings` materialize map entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationBindingProof {
    pub cap_param: String,
    pub parent_field: String,
}

/// Collect binding proofs from a declared many-relation `query_scoped_bindings` materialization.
pub fn collect_relation_binding_proofs(
    cgs: &CGS,
    entity: &EntityDef,
    relation: &RelationSchema,
) -> Result<Vec<RelationBindingProof>, String> {
    let mat = relation
        .materialize
        .as_ref()
        .ok_or_else(|| format!("relation `{}` has no materialize", relation.name))?;
    let bindings = match mat {
        RelationMaterialization::QueryScopedBindings { bindings, .. }
        | RelationMaterialization::GetScopedBindings { bindings, .. } => bindings,
        _ => {
            return Err(format!(
                "relation `{}` materialize is not query_scoped_bindings",
                relation.name
            ));
        }
    };
    let mut out = Vec::with_capacity(bindings.len());
    for (cap_param, parent_field) in bindings {
        out.push(RelationBindingProof {
            cap_param: cap_param.as_str().to_string(),
            parent_field: parent_field.as_str().to_string(),
        });
    }
    cgs.validate_relation_materialize_bindings(
        entity.name.as_str(),
        relation.name.as_str(),
        entity,
        match mat {
            RelationMaterialization::QueryScopedBindings { capability, .. }
            | RelationMaterialization::GetScopedBindings { capability, .. } => capability,
            _ => unreachable!(),
        },
        bindings,
    )
    .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Coerce a string identity slot into JSON using the parent entity field's catalog type.
pub fn identity_slot_to_json(
    cgs: &CGS,
    entity: &EntityDef,
    field_name: &str,
    slot: &str,
) -> serde_json::Value {
    let raw = serde_json::Value::String(slot.to_string());
    match parent_entity_field_type(cgs, entity, field_name) {
        Ok(ft) => {
            let nv = entity
                .fields
                .get(field_name)
                .and_then(|f| f.named_value(cgs).ok());
            coerce_json_value_for_field_type(
                &ft,
                nv.and_then(|n| n.value_format),
                nv.and_then(|n| n.array_items.as_ref()),
                raw,
            )
        }
        Err(_) => raw,
    }
}

fn identity_slot_needed(existing: Option<&serde_json::Value>) -> bool {
    match existing {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.is_empty(),
        _ => false,
    }
}

/// Merge CGS identity slots into agent row JSON — compound keys never flatten into `id_field`.
pub fn apply_identity_slots_to_row(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    reference: &crate::Ref,
    cgs: Option<&CGS>,
) {
    match &reference.key {
        crate::EntityKey::Simple(id) => {
            if id.is_empty() {
                return;
            }
            let id_field = cgs
                .and_then(|c| c.get_entity(reference.entity_type.as_str()))
                .map(|e| e.id_field.as_str().to_string())
                .unwrap_or_else(|| "id".to_string());
            if identity_slot_needed(obj.get(&id_field)) {
                obj.insert(id_field, serde_json::Value::String(id.to_string()));
            }
        }
        crate::EntityKey::Compound(parts) => {
            if parts.values().all(|v| v.is_empty()) {
                return;
            }
            let ent = cgs.and_then(|c| c.get_entity(reference.entity_type.as_str()));
            for (k, val) in parts {
                if val.is_empty() || !identity_slot_needed(obj.get(k.as_str())) {
                    continue;
                }
                let json = match (cgs, ent) {
                    (Some(cgs), Some(ent)) => {
                        identity_slot_to_json(cgs, ent, k.as_str(), val.as_str())
                    }
                    _ => serde_json::Value::String(val.clone()),
                };
                obj.insert(k.clone(), json);
            }
        }
    }
}

/// Restore `id_field` from compound `_ref` after row JSON omitted it during reload.
pub fn restore_id_field_from_compound_ref(
    fields: &mut indexmap::IndexMap<String, crate::TypedFieldValue>,
    reference: &crate::Ref,
    entity_def: Option<&EntityDef>,
    cgs: &CGS,
) {
    let crate::EntityKey::Compound(parts) = &reference.key else {
        return;
    };
    let Some(ent) = entity_def else {
        return;
    };
    let id_name = ent.id_field.as_str();
    let Some(val) = parts.get(id_name) else {
        return;
    };
    fields.entry(id_name.to_string()).or_insert_with(|| {
        let json = identity_slot_to_json(cgs, ent, id_name, val);
        serde_json::from_value(json)
            .unwrap_or_else(|_| crate::TypedFieldValue::from(crate::Value::String(val.clone())))
    });
}

/// Whether a parent entity field can supply a scoped-query capability parameter after wire coercion.
pub fn field_type_assignable_for_relation_binding(parent: &FieldType, param: &FieldType) -> bool {
    use FieldType::*;
    if parent == param {
        return true;
    }
    match (parent, param) {
        (Integer, Number) | (Number, Integer) => true,
        (String, Integer) | (String, Number) => true,
        (Integer, String) | (Number, String) | (Uuid, String) => true,
        (EntityRef { target: t1 }, EntityRef { target: t2 }) => t1 == t2,
        (Boolean, String) | (String, Boolean) => true,
        (Date, String) | (String, Date) => true,
        _ => false,
    }
}

/// Catalog relation `query_scoped_bindings` / `get_scoped_bindings` assignability (includes identity slots).
pub fn relation_binding_assignable(
    parent_entity: &EntityDef,
    parent_field: &str,
    parent_ty: &FieldType,
    param_ty: &FieldType,
) -> bool {
    if field_type_assignable_for_relation_binding(parent_ty, param_ty) {
        return true;
    }
    let FieldType::EntityRef { target } = param_ty else {
        return false;
    };
    if parent_entity.name != *target {
        return false;
    }
    let identity_slot = parent_field == parent_entity.id_field.as_str()
        || parent_entity
            .key_vars
            .iter()
            .any(|k| k.as_str() == parent_field);
    if identity_slot {
        return matches!(
            parent_ty,
            FieldType::String | FieldType::Integer | FieldType::Number | FieldType::Uuid
        );
    }
    parent_scalar_field_supplies_entity_ref_scope(parent_entity, parent_field, parent_ty)
}

/// True when a single parent row field value can [`normalize_entity_ref_value_for_target`] for this entity.
fn parent_scalar_field_supplies_entity_ref_scope(
    parent_entity: &EntityDef,
    parent_field: &str,
    parent_ty: &FieldType,
) -> bool {
    let leaf = match parent_ty {
        FieldType::String => Value::String("a/b".into()),
        FieldType::Integer => Value::Integer(1),
        FieldType::Number => Value::Float(1.0),
        FieldType::Uuid => Value::String("00000000-0000-0000-0000-000000000001".into()),
        _ => return false,
    };
    let row = Value::Object(IndexMap::from([(parent_field.to_string(), leaf)]));
    crate::entity_ref_value::normalize_entity_ref_value_for_target(&row, parent_entity).is_some()
}

/// Coerce a parsed predicate / env token for typecheck and downstream HTTP binding.
pub fn coerce_value_for_field_type(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    val: Value,
) -> Result<Value, String> {
    coerce_value_for_field_type_with_policy(
        ft,
        value_format,
        array_items,
        val,
        ArrayFieldCoercionPolicy::QueryFilter,
    )
}

/// Coerce with explicit array policy (invoke args use [`ArrayFieldCoercionPolicy::InvokeArg`]).
pub fn coerce_value_for_field_type_with_policy(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    val: Value,
    array_policy: ArrayFieldCoercionPolicy,
) -> Result<Value, String> {
    // Teaching-table bare `$` is a fill-in slot, not a wire token — pass through for every field
    // type (including temporal `Date`) so optional params can appear as `p#=$` in method rows.
    if val.is_domain_example_placeholder() {
        return Ok(val);
    }
    match ft {
        FieldType::Array => {
            let coerce_elem = |v: Value| -> Result<Value, String> {
                match array_items {
                    Some(items) => {
                        coerce_value_for_field_type(&items.field_type, items.value_format, None, v)
                    }
                    None => Ok(v),
                }
            };
            match val {
                Value::Array(elements) => {
                    let mut out = Vec::with_capacity(elements.len());
                    for e in elements {
                        out.push(coerce_elem(e)?);
                    }
                    Ok(Value::Array(out))
                }
                Value::PlasmInputRef(_)
                    if ArrayFieldCoercionPolicy::accepts_deferred_value(&val) =>
                {
                    Ok(val)
                }
                other if array_policy.allows_scalar_wrap() => {
                    Ok(Value::Array(vec![coerce_elem(other)?]))
                }
                other => Err(invoke_array_scalar_error(other.type_name())),
            }
        }
        FieldType::Date => match value_format {
            Some(ValueWireFormat::Temporal(fmt)) => {
                crate::temporal::normalize_temporal_value(val, fmt)
            }
            None => Err("Date field missing value_format in schema".to_string()),
        },
        FieldType::String | FieldType::Uuid | FieldType::Select | FieldType::MultiSelect => {
            Ok(match val {
                Value::Integer(n) => Value::String(n.to_string()),
                Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
                _ => val,
            })
        }
        FieldType::Integer => Ok(match val {
            Value::String(ref s) => s.parse::<i64>().map(Value::Integer).unwrap_or(val),
            Value::Float(f) if f.fract() == 0.0 && f.is_finite() => Value::Integer(f as i64),
            _ => val,
        }),
        FieldType::Number => Ok(match val {
            Value::String(ref s) => s.parse::<f64>().map(Value::Float).unwrap_or(val),
            Value::Integer(n) => Value::Float(n as f64),
            _ => val,
        }),
        FieldType::EntityRef { .. } => Ok(match val {
            Value::Integer(n) => Value::String(n.to_string()),
            Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
            _ => val,
        }),
        FieldType::Boolean => Ok(match val {
            Value::String(s) if s.eq_ignore_ascii_case("true") => Value::Bool(true),
            Value::String(s) if s.eq_ignore_ascii_case("false") => Value::Bool(false),
            _ => val,
        }),
        FieldType::Json => match val {
            Value::String(ref s) if s.as_str() == "$" => Ok(val),
            Value::String(s) => crate::value::parse_json_subtree_str(&s).ok_or_else(|| {
                "Json parameter: string must be valid JSON with a top-level object or array"
                    .to_string()
            }),
            other => Ok(other),
        },
        _ => Ok(val),
    }
}

/// Coerce a JSON wire value toward a catalog field type (plan rows, hole instantiation).
pub fn coerce_json_value_for_field_type(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    value: serde_json::Value,
) -> serde_json::Value {
    let plasm = json_value_to_plasm_value(&value);
    match coerce_value_for_field_type(ft, value_format, array_items, plasm) {
        Ok(v) => plasm_value_to_json(&v),
        Err(_) => value,
    }
}

/// Build a [`Value`] for a relation binding param from row JSON or identity.
pub fn binding_value_as_plasm_value(
    raw: &serde_json::Value,
    target_nv: &NamedValueSchema,
) -> Value {
    let plasm = json_value_to_plasm_value(raw);
    coerce_value_for_field_type(
        &target_nv.field_type,
        target_nv.value_format,
        target_nv.array_items.as_ref(),
        plasm.clone(),
    )
    .unwrap_or(plasm)
}

pub fn json_value_to_plasm_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().map(json_value_to_plasm_value).collect())
        }
        serde_json::Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_value_to_plasm_value(v)))
                .collect(),
        ),
    }
}

pub fn plasm_value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::json!(i),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(plasm_value_to_json).collect())
        }
        Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), plasm_value_to_json(v)))
                .collect(),
        ),
        Value::PlasmInputRef(_) | Value::UnionCtor { .. } => serde_json::Value::Null,
    }
}

fn normalize_numeric_id_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{}", f as i64)
    } else {
        f.to_string()
    }
}

/// Resolve the catalog field type for a parent entity wire field used in `query_scoped_bindings`.
pub fn parent_entity_field_type(
    cgs: &CGS,
    entity: &EntityDef,
    parent_field: &str,
) -> Result<FieldType, String> {
    if parent_field == entity.id_field.as_str() {
        if let Some(fs) = entity.fields.get(parent_field) {
            return Ok(fs
                .named_value(cgs)
                .map_err(|e| e.to_string())?
                .field_type
                .clone());
        }
        return Ok(FieldType::String);
    }
    if let Some(fs) = entity.fields.get(parent_field) {
        return Ok(fs
            .named_value(cgs)
            .map_err(|e| e.to_string())?
            .field_type
            .clone());
    }
    if entity.key_vars.iter().any(|k| k.as_str() == parent_field) {
        if let Some(fs) = entity.fields.get(parent_field) {
            return Ok(fs
                .named_value(cgs)
                .map_err(|e| e.to_string())?
                .field_type
                .clone());
        }
        return Ok(FieldType::String);
    }
    Err(format!(
        "unknown parent field `{parent_field}` on entity `{}`",
        entity.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array_field_policy::ArrayFieldCoercionPolicy;
    use crate::EntityFieldName;
    use crate::FieldValueKind;
    use crate::ValueDomainKey;
    use indexmap::IndexMap;

    #[test]
    fn integer_param_accepts_integer_and_string_parent() {
        assert!(field_type_assignable_for_relation_binding(
            &FieldType::Integer,
            &FieldType::Integer,
        ));
        assert!(field_type_assignable_for_relation_binding(
            &FieldType::String,
            &FieldType::Integer,
        ));
        assert!(!field_type_assignable_for_relation_binding(
            &FieldType::Boolean,
            &FieldType::Integer,
        ));
    }

    #[test]
    fn entity_ref_param_accepts_parent_id_scalar() {
        let zone = EntityDef {
            name: "Zone".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        };
        assert!(relation_binding_assignable(
            &zone,
            "id",
            &FieldType::String,
            &FieldType::EntityRef {
                target: "Zone".into(),
            },
        ));
        assert!(!relation_binding_assignable(
            &zone,
            "name",
            &FieldType::String,
            &FieldType::EntityRef {
                target: "Zone".into(),
            },
        ));
    }

    #[test]
    fn entity_ref_param_accepts_repository_full_name_slug() {
        let repo = EntityDef {
            name: "Repository".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![
                EntityFieldName::from("owner"),
                EntityFieldName::from("repo"),
            ],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        };
        assert!(relation_binding_assignable(
            &repo,
            "full_name",
            &FieldType::String,
            &FieldType::EntityRef {
                target: "Repository".into(),
            },
        ));
        assert!(!relation_binding_assignable(
            &repo,
            "description",
            &FieldType::String,
            &FieldType::EntityRef {
                target: "Repository".into(),
            },
        ));
    }

    #[test]
    fn coerce_json_string_to_integer() {
        let out = coerce_json_value_for_field_type(
            &FieldType::Integer,
            None,
            None,
            serde_json::json!("42"),
        );
        assert_eq!(out, serde_json::json!(42));
    }

    #[test]
    fn teaching_placeholder_passes_through_temporal_date() {
        use crate::TemporalWireFormat;
        let out = coerce_value_for_field_type_with_policy(
            &FieldType::Date,
            Some(ValueWireFormat::Temporal(TemporalWireFormat::Rfc3339)),
            None,
            Value::String("$".into()),
            ArrayFieldCoercionPolicy::InvokeArg,
        )
        .expect("teaching $ must not fail temporal coerce");
        assert!(out.is_domain_example_placeholder());
    }

    #[test]
    fn array_field_rejects_scalar_on_invoke_arg_policy() {
        let err = coerce_value_for_field_type_with_policy(
            &FieldType::Array,
            None,
            Some(&ArrayItemsSchema {
                kind: FieldValueKind::Registry(ValueDomainKey::new("nv_test").expect("key")),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
            }),
            Value::String("a,b".into()),
            ArrayFieldCoercionPolicy::InvokeArg,
        )
        .expect_err("scalar must not auto-wrap for invoke args");
        assert!(err.contains("expected array"), "unexpected error: {err}");
    }

    #[test]
    fn array_field_passes_plasm_input_ref_for_invoke_args() {
        use crate::PlasmInputRef;
        let out = coerce_value_for_field_type_with_policy(
            &FieldType::Array,
            None,
            Some(&ArrayItemsSchema {
                kind: FieldValueKind::Registry(ValueDomainKey::new("nv_test").expect("key")),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
            }),
            Value::PlasmInputRef(PlasmInputRef::node_output("labels", vec!["name".into()])),
            ArrayFieldCoercionPolicy::InvokeArg,
        )
        .expect("column projection ref must pass through");
        assert!(matches!(out, Value::PlasmInputRef(_)));
    }

    #[test]
    fn array_field_wraps_scalar_for_query_predicate_coercion() {
        let out = coerce_value_for_field_type(
            &FieldType::Array,
            None,
            Some(&ArrayItemsSchema {
                kind: FieldValueKind::Registry(ValueDomainKey::new("nv_test").expect("key")),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
            }),
            Value::String("tag".into()),
        )
        .expect("query filter scalar wrap");
        assert_eq!(
            out,
            Value::Array(vec![Value::String("tag".into())]),
            "predicate coercion may still wrap single scalar"
        );
    }
}
