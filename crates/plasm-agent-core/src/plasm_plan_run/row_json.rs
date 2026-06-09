//! Row JSON helpers.

use super::*;

pub(crate) fn cached_entity_row_json(entity: &CachedEntity, cgs: &CGS) -> serde_json::Value {
    entity_to_row_json(entity, Some(cgs))
}

/// Parse one Plasm surface line: strip teaching gloss, expand `e#` / `p#` / `m#` per `pipeline`, then
/// [`plasm_core::expr_parser::parse_with_cgs_layers_program`] (no program-node refs). This is the
pub(crate) fn value_at_segments<'a>(
    row: &'a serde_json::Value,
    path: &[String],
) -> Option<&'a serde_json::Value> {
    let mut cur = row;
    for segment in path {
        cur = cur.get(segment)?;
    }
    Some(cur)
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
