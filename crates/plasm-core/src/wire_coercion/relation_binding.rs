//! Relation-binding assignability + identity-slot helpers.

use super::{
    coerce_json_value_for_field_type, coerce_value_for_field_type, json_to_plasm_for_field,
};
use crate::{
    EntityDef, FieldType, NamedValueSchema, RelationMaterialization, RelationSchema, Value, CGS,
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
        crate::EntityKey::Simple(slot) => {
            let Some(id) = slot.as_lit_str() else {
                return;
            };
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
            if parts.values().all(|v| v.is_empty_lit()) {
                return;
            }
            let ent = cgs.and_then(|c| c.get_entity(reference.entity_type.as_str()));
            for (k, val) in parts {
                let Some(s) = val.as_lit_str() else {
                    continue;
                };
                if s.is_empty() || !identity_slot_needed(obj.get(k.as_str())) {
                    continue;
                }
                let json = match (cgs, ent) {
                    (Some(cgs), Some(ent)) => identity_slot_to_json(cgs, ent, k.as_str(), s),
                    _ => serde_json::Value::String(s.to_string()),
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
    let Some(val) = parts.get(id_name).and_then(|s| s.as_lit_str()) else {
        return;
    };
    fields.entry(id_name.to_string()).or_insert_with(|| {
        let json = identity_slot_to_json(cgs, ent, id_name, val);
        serde_json::from_value(json)
            .unwrap_or_else(|_| crate::TypedFieldValue::from(crate::Value::String(val.to_string())))
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
        (EntityRef { target: t1, .. }, EntityRef { target: t2, .. }) => t1 == t2,
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
    let FieldType::EntityRef { target, .. } = param_ty else {
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

/// Build a [`Value`] for a relation binding param from row JSON or identity.
pub fn binding_value_as_plasm_value(
    raw: &serde_json::Value,
    target_nv: &NamedValueSchema,
) -> Value {
    let plasm = json_to_plasm_for_field(&target_nv.field_type, raw);
    coerce_value_for_field_type(
        &target_nv.field_type,
        target_nv.value_format,
        target_nv.array_items.as_ref(),
        plasm.clone(),
    )
    .unwrap_or(plasm)
}

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

