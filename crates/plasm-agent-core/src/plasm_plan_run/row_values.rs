//! Native materialized row access and identity helpers.

use super::*;

pub(crate) fn cached_entity_row_values(entity: &CachedEntity, cgs: &CGS) -> plasm_core::ValueRow {
    plasm_runtime::entity_to_row_values(entity, Some(cgs))
}

pub(crate) fn value_at_segments<'a>(
    row: &'a plasm_core::Value,
    path: &[impl AsRef<str>],
) -> Option<&'a plasm_core::Value> {
    let mut cur = row;
    for segment in path {
        cur = cur.get(segment.as_ref())?;
    }
    Some(cur)
}

pub(crate) fn value_at_dotted<'a>(
    row: &'a plasm_core::Value,
    path: &str,
) -> Option<&'a plasm_core::Value> {
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

pub(crate) fn augment_row_with_identity(
    row: &plasm_core::ValueRow,
    identity: Option<&plasm_core::RowIdentity>,
) -> plasm_core::ValueRow {
    let Some(identity) = identity else {
        return row.clone();
    };
    let mut obj = row.fields().clone();
    let primary = identity.reference.primary_slot_str();
    obj.entry("id".to_string())
        .or_insert_with(|| plasm_core::Value::String(primary.clone()));
    // Homograph-safe primary: when ambient already names the CGS id_field (e.g. access_token),
    // prefer that wire; otherwise still expose `id` for identity bindings.
    for (k, v) in &identity.ambient {
        obj.entry(k.clone())
            .or_insert_with(|| plasm_core::Value::String(v.clone()));
    }
    if let plasm_core::EntityKey::Compound(parts) = &identity.reference.key {
        for (k, v) in parts {
            if let Some(s) = v.as_lit_str() {
                obj.entry(k.clone())
                    .or_insert_with(|| plasm_core::Value::String(s.to_string()));
            }
        }
    }
    // Simple-key identity: also stamp a non-`id` primary when ambient is empty but callers
    // look up catalog id_field via hole path (AuthSession.access_token). Without a CGS here we
    // cannot know id_field; ambient/compound paths above cover stamped sessions. When the
    // decoded row already carries id_field, from_row wins in hole fill.
    plasm_core::ValueRow::from(obj)
}

pub(crate) fn predicate_matches(
    row: &plasm_core::Value,
    pred: &plasm_runtime::row_predicate::BoundRowPredicate,
) -> Result<bool, String> {
    plasm_runtime::row_matches_predicate(row, pred).map_err(|e| e.to_string())
}
