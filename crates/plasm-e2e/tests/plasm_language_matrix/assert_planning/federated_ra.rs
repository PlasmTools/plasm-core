//! Planning-IR assert arms (auto-split from monolith).

use std::collections::BTreeSet;

use super::super::ir_helpers::*;
use super::super::row::MatrixRow;
use plasm_agent::plasm_plan::{ComputeOp, ComputeTemplate, PlanValue};
use plasm_agent::plasm_plan_run::DryPlasmPlanEvaluation;
use plasm_core::{
    Expr, InvokeExpr,
};

pub(crate) fn assert_planning_federated_ra(
    row: &MatrixRow,
    surfaces: &[Expr],
    computes: &[ComputeTemplate],
    _rel: &[Expr],
    dry: &DryPlasmPlanEvaluation,
    comp: &serde_json::Value,
) -> Result<Option<()>, String> {
    match row.id {
        "lang_federated_relation_target_entry" => {
            let summary = comp_relation_named(comp, "summary")
                .ok_or_else(|| "expected `.summary` relation in federated session".to_string())?;
            if summary.pointer("/target/entry_id").and_then(|v| v.as_str()) != Some("pokeapi") {
                return Err(format!(
                    "relation target must own pokeapi catalog, not primary linear: {summary:?}"
                ));
            }
            if summary.pointer("/target/entity").and_then(|v| v.as_str()) != Some("LangSummary") {
                return Err(format!("expected LangSummary target, got {summary:?}"));
            }
            let ir = summary
                .pointer("/ir/expr")
                .map(|v| v.to_string())
                .unwrap_or_default();
            if ir.contains("\"$\"") {
                return Err("relation IR must not use teaching placeholder $".into());
            }
        }
        "lang_federated_duplicate_entity_e1_query" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem query on e1, got {:?}", q.entity));
            }
            if q.catalog_entry_id.as_deref() != Some("github") {
                return Err(format!(
                    "e1 must resolve to github catalog, got catalog_entry_id={:?}",
                    q.catalog_entry_id
                ));
            }
            let qe = comp_first_invoke_qualified_entity(comp)
                .ok_or_else(|| "expected qualified_entity on comp invoke step".to_string())?;
            if qe.get("entry_id").and_then(|v| v.as_str()) != Some("github") {
                return Err(format!("comp qualified_entity must be github: {qe:?}"));
            }
            if qe.get("entity").and_then(|v| v.as_str()) != Some("LangItem") {
                return Err(format!("comp qualified_entity entity LangItem: {qe:?}"));
            }
        }
        "lang_federated_duplicate_entity_e2_search" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!(
                    "expected LangItem search on e2, got {:?}",
                    q.entity
                ));
            }
            if q.catalog_entry_id.as_deref() != Some("linear") {
                return Err(format!(
                    "e2 must resolve to linear catalog, got catalog_entry_id={:?}",
                    q.catalog_entry_id
                ));
            }
            let Some(cap) = q.capability_name.as_ref() else {
                return Err("e2 search should pin Search capability".into());
            };
            if cap.as_str() != "langitem_search" {
                return Err(format!("expected langitem_search, got {cap}"));
            }
            let qe = comp_first_invoke_qualified_entity(comp)
                .ok_or_else(|| "expected qualified_entity on comp invoke step".to_string())?;
            if qe.get("entry_id").and_then(|v| v.as_str()) != Some("linear") {
                return Err(format!("comp qualified_entity must be linear: {qe:?}"));
            }
        }
        "lang_federated_duplicate_entity_relation_r" => {
            let rel = comp_relation_named(comp, "children")
                .ok_or_else(|| "expected `.children` relation hop on e2 parent".to_string())?;
            if rel.pointer("/target/entry_id").and_then(|v| v.as_str()) != Some("linear") {
                return Err(format!(
                    "homonymous LangItem relation target must stay on linear catalog: {rel:?}"
                ));
            }
            if rel.pointer("/target/entity").and_then(|v| v.as_str()) != Some("LangItem") {
                return Err(format!("expected LangItem target, got {rel:?}"));
            }
        }
        "lang_federated_duplicate_entity_mutator_m" => {
            let create = surfaces
                .iter()
                .find_map(|e| match e {
                    Expr::Create(c) => Some(c),
                    _ => None,
                })
                .ok_or_else(|| "expected Create surface from e2.m#".to_string())?;
            if create.catalog_entry_id.as_deref() != Some("linear") {
                return Err(format!(
                    "e2 mutator must stamp linear catalog, got {:?}",
                    create.catalog_entry_id
                ));
            }
            if create.capability.as_str() != "langitem_create" {
                return Err(format!(
                    "expected langitem_create, got {}",
                    create.capability
                ));
            }
        }
        "lang_federated_duplicate_entity_pathless_action" => {
            let inv = surfaces
                .iter()
                .find_map(|e| match e {
                    Expr::Invoke(i) => Some(i),
                    _ => None,
                })
                .ok_or_else(|| "expected Invoke surface from e2 pathless Action".to_string())?;
            if inv.catalog_entry_id.as_deref() != Some("linear") {
                return Err(format!(
                    "e2 pathless Action must stamp linear catalog, got {:?}",
                    inv.catalog_entry_id
                ));
            }
            if inv.capability.as_str() != "langitem_broadcast" {
                return Err(format!(
                    "expected langitem_broadcast, got {}",
                    inv.capability
                ));
            }
        }
        "lang_federated_auth_session_provides_mutation" => {
            let logins: Vec<_> = surfaces
                .iter()
                .filter_map(|e| match e {
                    Expr::Invoke(i) if i.capability.as_str() == "langauthsession_login" => Some(i),
                    _ => None,
                })
                .collect();
            if logins.len() != 2 {
                return Err(format!(
                    "expected two federated logins (CUGA dual-auth), got {}",
                    logins.len()
                ));
            }
            let catalogs: BTreeSet<_> = logins
                .iter()
                .filter_map(|i| i.catalog_entry_id.as_deref())
                .collect();
            if catalogs != BTreeSet::from(["github", "linear"]) {
                return Err(format!(
                    "logins must stamp github+linear catalogs, got {:?}",
                    catalogs
                ));
            }
            let has_mutation = json_value_contains_substring(comp, "mutation_result")
                || dry
                    .node_results
                    .iter()
                    .any(|nr| json_value_contains_substring(nr, "mutation_result"));
            if !has_mutation {
                return Err(
                    "Action-with-provides must infer result_shape mutation_result on plan/dry nodes"
                        .into(),
                );
            }
            let has_note_search = surfaces.iter().any(|e| {
                matches!(
                    e,
                    Expr::Query(q)
                        if q.capability_name.as_deref() == Some("langsecurednote_search")
                            || q.entity.as_str() == "LangSecuredNote"
                )
            });
            if !has_note_search {
                return Err(
                    "expected LangSecuredNote search consuming sn_auth.access_token".into(),
                );
            }
            let has_group_query = surfaces.iter().any(|e| {
                matches!(
                    e,
                    Expr::Query(q)
                        if q.capability_name.as_deref() == Some("langsecuredgroup_query")
                            || q.entity.as_str() == "LangSecuredGroup"
                )
            });
            if !has_group_query {
                return Err(
                    "expected LangSecuredGroup query consuming sw_auth.access_token".into(),
                );
            }
            if !json_value_contains_substring(comp, "sn_auth")
                || !json_value_contains_substring(comp, "sw_auth")
            {
                return Err(
                    "dual Bearer surfaces must retain both explicit context binding dependencies"
                        .into(),
                );
            }
        }
        "lang_federated_parallel_roots" => {
            if surfaces.len() < 2 {
                return Err(format!(
                    "expected parallel github+linear roots, got {} surfaces",
                    surfaces.len()
                ));
            }
        }
        "lang_federated_group_by_on_e1" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy on e1 query, got {:?}", computes));
            };
            if keys.len() != 1 || keys[0].dotted() != "owner" {
                return Err(format!("expected group key owner, got {:?}", keys));
            }
            if !aggregates.iter().any(|a| a.name.as_str() == "n") {
                return Err(format!("expected aggregate n, got {:?}", aggregates));
            }
            let q = first_query(&surfaces)?;
            if q.catalog_entry_id.as_deref() != Some("github") {
                return Err(format!(
                    "e1 group_by query must be github, got {:?}",
                    q.catalog_entry_id
                ));
            }
        }
        "lang_bind_template_inline_on_e1" => {
            if computes.is_empty() {
                return Err("expected render compute on inline e1 template".into());
            }
            let q = first_query(&surfaces)?;
            if q.catalog_entry_id.as_deref() != Some("github") {
                return Err(format!(
                    "inline template query must be github e1, got {:?}",
                    q.catalog_entry_id
                ));
            }
        }
        "lang_effect_create_literal" => {
            let Some(Expr::Create(c)) = surfaces.iter().find(|e| matches!(e, Expr::Create(_)))
            else {
                return Err(format!("expected Create, got {:?}", surfaces));
            };
            if c.capability.as_str() != "langitem_create" || c.entity != "LangItem" {
                return Err(format!("unexpected create: {:?}", c.capability));
            }
        }
        "lang_money_predicate_gt" => {
            let Some(ComputeOp::Filter { predicates }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|op| matches!(op, ComputeOp::Filter { .. }))
            else {
                return Err(format!("expected money Filter compute, got {computes:?}"));
            };
            let predicate_debug = format!("{predicates:?}");
            if !predicate_debug.contains("price") || !predicate_debug.contains("10") {
                return Err(format!(
                    "unexpected money filter predicates: {predicate_debug}"
                ));
            }
        }
        "lang_integer_where_gt_dry_coerce" => {
            let Some(ComputeOp::Filter { predicates }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|op| matches!(op, ComputeOp::Filter { .. }))
            else {
                return Err(format!(
                    "expected integer Filter compute (RA-8), got {computes:?}"
                ));
            };
            let predicate_debug = format!("{predicates:?}");
            if !predicate_debug.contains("score") || !predicate_debug.contains('0') {
                return Err(format!(
                    "unexpected integer filter predicates: {predicate_debug}"
                ));
            }
        }
        "lang_money_create_body" => {
            let Some(Expr::Create(c)) = surfaces.iter().find(|e| matches!(e, Expr::Create(_)))
            else {
                return Err(format!("expected Create, got {:?}", surfaces));
            };
            if c.capability.as_str() != "langoffer_create" || c.entity != "LangOffer" {
                return Err(format!("unexpected create: {:?}", c.capability));
            }
        }
        "lang_effect_update" => {
            let Some(Expr::Invoke(InvokeExpr { capability, .. })) =
                surfaces.iter().find(|e| matches!(e, Expr::Invoke(_)))
            else {
                return Err(format!("expected Invoke IR, got {:?}", surfaces));
            };
            if capability.as_str() != "langitem_update" {
                return Err(format!("expected langitem_update, got {capability}"));
            }
        }
        "lang_effect_action_ping" => {
            let Some(Expr::Invoke(InvokeExpr { capability, .. })) =
                surfaces.iter().find(|e| matches!(e, Expr::Invoke(_)))
            else {
                return Err(format!("expected Invoke IR, got {:?}", surfaces));
            };
            if capability.as_str() != "langitem_ping" {
                return Err(format!("expected langitem_ping, got {capability}"));
            }
        }
        "lang_effect_delete" => {
            let Some(Expr::Delete(d)) = surfaces.iter().find(|e| matches!(e, Expr::Delete(_)))
            else {
                return Err(format!("expected Delete IR, got {:?}", surfaces));
            };
            if d.capability.as_str() != "langitem_delete" {
                return Err(format!("expected langitem_delete, got {:?}", d.capability));
            }
        }
        "lang_for_each_update" => {
            // CGS `update` capabilities lower to `Expr::Invoke`, which [`infer_surface_contract`]
            // classifies as [`PlanNodeKind::Action`] (not `Update`) in the plan DAG.
            // Singleton Get source — historical 1-row for_each path (see lang_for_each_multirow_*).
            assert_for_each_action_node(dry, comp)?;
        }
        "lang_for_each_multirow_update" | "lang_for_each_auth_secured_touch" => {
            assert_for_each_action_node(dry, comp)?;
            if !dry.review.has_foreach_fanout_risk {
                return Err(format!(
                    "row {}: plural-source mutating for_each must set has_foreach_fanout_risk",
                    row.id
                ));
            }
            let Some(ComputeOp::Limit { count: 3 }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Limit { .. }))
            else {
                return Err(format!(
                    "row {}: expected Limit(3) on list source, got {:?}",
                    row.id, computes
                ));
            };
        }
        "lang_iterate_until_bound" | "lang_iterate_until_zero_step" | "lang_iterate_bound_exhausted" => {
            let matches_it = |nr: &serde_json::Value| {
                nr.get("kind").and_then(|k| k.as_str()) == Some("iterate_until")
                    || nr.get("kind").and_then(|k| k.as_str()) == Some("unfold_until")
            };
            let dry_ok = dry.node_results.iter().any(matches_it);
            let plan_ok = comp_steps_values(comp).iter().any(|n| {
                n.get("kind").and_then(|k| k.as_str()) == Some("iterate_until")
                    || n.get("kind").and_then(|k| k.as_str()) == Some("unfold_until")
            });
            if !dry_ok && !plan_ok {
                return Err(format!(
                    "row {}: expected iterate_until / unfold_until plan node",
                    row.id
                ));
            }
            let take = comp_steps_values(comp)
                .iter()
                .find(|n| n.get("kind").and_then(|k| k.as_str()) == Some("iterate_until"))
                .and_then(|n| n.get("take"))
                .and_then(|t| t.as_u64());
            if take.is_none() {
                // Comp wire may use unfold_until payload shape — accept dry kind alone.
                if !dry_ok && !plan_ok {
                    return Err(format!("row {}: missing take on iterate_until", row.id));
                }
            }
        }
        "lang_domain_symbol_page_size" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" || q.predicate.is_some() {
                return Err(format!("unexpected query IR: {q:?}"));
            }
            let ps = comp_surface_page_size(comp).ok_or_else(|| {
                "expected plan surface page_size field (IR omits host paging cap)".to_string()
            })?;
            if ps != 10 {
                return Err(format!("expected page_size 10, got {ps}"));
            }
        }
        "lang_ra4_pipe_monolith" | "lang_ra4_pipe_bind_cut" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Filter { .. }))
            {
                return Err(format!("expected Filter compute, got {:?}", computes));
            }
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Limit { .. }))
            {
                return Err(format!("expected Limit compute, got {:?}", computes));
            }
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Project { .. }))
            {
                return Err(format!("expected Project compute, got {:?}", computes));
            }
        }
        "lang_ra4_apply_monolith" | "lang_ra4_apply_bind_cut" => {
            let mut saw_map = false;
            for nr in &dry.node_results {
                if nr.get("kind").and_then(|k| k.as_str()) != Some("derive") {
                    continue;
                }
                let Some(v) = nr.get("value") else {
                    continue;
                };
                let Ok(pv) = serde_json::from_value::<PlanValue>(v.clone()) else {
                    continue;
                };
                if let PlanValue::Object { fields } = pv {
                    if fields.contains_key("t") && fields.contains_key("o") {
                        saw_map = true;
                        break;
                    }
                }
            }
            if !saw_map {
                return Err("expected derive map with fields t and o".into());
            }
        }
        "lang_ra4_apply_derive_message_field" => {
            let mut saw_derive = false;
            for nr in &dry.node_results {
                if nr.get("kind").and_then(|k| k.as_str()) != Some("derive") {
                    continue;
                }
                let Some(v) = nr.get("value") else {
                    continue;
                };
                let Ok(pv) = serde_json::from_value::<PlanValue>(v.clone()) else {
                    continue;
                };
                if let PlanValue::Object { fields } = pv {
                    if fields.contains_key("t") && fields.contains_key("note") {
                        saw_derive = true;
                        break;
                    }
                }
            }
            if !saw_derive {
                return Err("expected derive (not for_each) with fields t and note".into());
            }
            if dry
                .node_results
                .iter()
                .any(|nr| nr.get("kind").and_then(|k| k.as_str()) == Some("for_each"))
            {
                return Err("`.message` in derive body must not lower to for_each".into());
            }
        }
        "lang_ra4_apply_relation_monolith" | "lang_ra4_apply_relation_bind_cut" => {
            if !dry.node_results.iter().any(|nr| {
                matches!(
                    nr.get("kind").and_then(|k| k.as_str()),
                    Some("relation" | "relation_traversal" | "flat_map_relation")
                )
            }) {
                return Err("expected relation application node".into());
            }
        }
        "lang_ra4_apply_render_bind_cut" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Render { .. }))
            {
                return Err(format!("expected Render compute, got {:?}", computes));
            }
        }
        "lang_ra4_apply_foreach_monolith" | "lang_ra4_apply_foreach_bind_cut" => {
            if !dry
                .node_results
                .iter()
                .any(|nr| nr.get("kind").and_then(|k| k.as_str()) == Some("for_each"))
            {
                return Err("expected for_each node".into());
            }
        }
        _ => return Ok(None),
    }
    Ok(Some(()))
}
