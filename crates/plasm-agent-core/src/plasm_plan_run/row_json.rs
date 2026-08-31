//! Row JSON helpers.

use super::*;

pub(crate) fn cached_entity_row_json(entity: &CachedEntity, cgs: &CGS) -> serde_json::Value {
    entity_to_row_json(entity, Some(cgs))
}

pub(crate) fn value_at_segments<'a>(
    row: &'a serde_json::Value,
    path: &[impl AsRef<str>],
) -> Option<&'a serde_json::Value> {
    let mut cur = row;
    for segment in path {
        cur = cur.get(segment.as_ref())?;
    }
    Some(cur)
}

pub(crate) fn value_at_dotted<'a>(
    row: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    if path.is_empty() {
        return Some(row);
    }
    value_at_segments(
        row,
        &path
            .split('.')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn augment_row_json_with_identity(
    row: &serde_json::Value,
    identity: Option<&plasm_core::RowIdentity>,
) -> serde_json::Value {
    let Some(identity) = identity else {
        return row.clone();
    };
    let mut obj = match row {
        serde_json::Value::Object(map) => map.clone(),
        other => {
            let mut m = serde_json::Map::new();
            m.insert("value".to_string(), other.clone());
            m
        }
    };
    let primary = identity.reference.primary_slot_str();
    obj.entry("id".to_string())
        .or_insert_with(|| serde_json::Value::String(primary.clone()));
    for (k, v) in &identity.ambient {
        obj.entry(k.clone())
            .or_insert_with(|| serde_json::Value::String(v.clone()));
    }
    if let plasm_core::EntityKey::Compound(parts) = &identity.reference.key {
        for (k, v) in parts {
            obj.entry(k.clone())
                .or_insert_with(|| serde_json::Value::String(v.clone()));
        }
    }
    serde_json::Value::Object(obj)
}

pub(crate) fn predicate_matches(
    row: &serde_json::Value,
    pred: &crate::plasm_plan::PlanPredicate,
) -> bool {
    match crate::plan_read_bounds::plan_predicate_to_json(pred) {
        Ok(json_pred) => plasm_runtime::json_matches_predicate(row, &json_pred),
        Err(_) => false,
    }
}
