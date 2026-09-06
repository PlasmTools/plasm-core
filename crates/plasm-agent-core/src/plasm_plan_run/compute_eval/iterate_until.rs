//! Live materialization for PLP-8 `iterate … step … until … take N`.

use super::super::*;
use super::eval::{instantiate_raw_expr_template, wire_coercion_by_alias_from_inputs};
use super::for_each::{bound_row_plan_eval_env, cross_uses_excluding_item};
use super::materialized_result_use_inputs;
use crate::plasm_plan::{PlanPredicate, ValidatedIterateUntilNode};
use plasm_core::expr_parser::ParsedExpr;

fn row_satisfies_until(row: &serde_json::Value, preds: &[PlanPredicate]) -> bool {
    preds
        .iter()
        .all(|p| crate::plasm_plan_run::predicate_matches(row, p))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_iterate_until_node(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node_index: usize,
    it: &ValidatedIterateUntilNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, String> {
    let mut current_rows =
        materialized_rows(es, st, session_id, materialized, &it.source).await?;
    if current_rows.is_empty() {
        return Err("iterate_until seed produced no rows".into());
    }
    let cross = cross_uses_excluding_item(&it.uses_result, &it.item_binding);
    let mut input_rows = materialized_result_use_inputs(materialized, &cross, None)?;
    let wire_coercion_by_alias = wire_coercion_by_alias_from_inputs(es, &mut input_rows)?;
    let scoped_es =
        entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))?;

    if row_satisfies_until(&current_rows[0], &it.until_predicates) {
        return Ok(final_iterate_node(it, current_rows));
    }

    for step_idx in 1..=it.take {
        let row = current_rows
            .first()
            .ok_or_else(|| "iterate_until lost seed row".to_string())?;
        let env = bound_row_plan_eval_env(
            &it.item_binding,
            row,
            &input_rows,
            &wire_coercion_by_alias,
        );
        let parsed = instantiate_raw_expr_template(&it.effect_template.ir_template, &env)?;
        let expr_label = crate::expr_display::expr_display(&parsed.expr);
        let mut jobs = Vec::new();
        super::super::plan_fanout_parallel::push_row_job(
            &mut jobs,
            node_index,
            (step_idx as usize).saturating_sub(1),
            expr_label,
            parsed,
        );
        let _fold = super::super::plan_fanout_parallel::execute_row_fanout(
            st,
            &scoped_es,
            session_id,
            jobs,
            trace,
            sink,
            plan_shared.clone(),
            super::super::plan_fanout_parallel::RowFanoutPolicy::for_each(false, 1),
        )
        .await
        .map_err(|e| format!("iterate_until step {step_idx}: {e}"))?;

        // Always re-Get the seed after the step. Mutator echoes (even with `provides`) are not a
        // substitute for primary_read / composed views — e.g. Player.previous may echo song_id while
        // `is_liked` lives only on player_current. LangCursor.tick remains correct because re-Get
        // reads the updated cursor row.
        current_rows =
            reobserve_seed(st, es, session_id, it, plan_shared.as_ref(), trace).await?;
        if current_rows.is_empty() {
            return Err(format!(
                "iterate_until re-observe after step {step_idx} produced no rows"
            ));
        }
        if row_satisfies_until(&current_rows[0], &it.until_predicates) {
            return Ok(final_iterate_node(it, current_rows));
        }
    }

    Err(format!(
        "iterate_bound_exhausted: until predicate not satisfied within take {}",
        it.take
    ))
}

fn final_iterate_node(
    it: &ValidatedIterateUntilNode,
    rows: Vec<serde_json::Value>,
) -> MaterializedNode {
    let identities = rows.iter().map(|_| None).collect::<Vec<_>>();
    MaterializedNode::inline_cache(
        it.effect_template.qualified_entity.clone(),
        rows,
        identities,
        String::new(),
        None,
    )
}

async fn reobserve_seed(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    it: &ValidatedIterateUntilNode,
    plan_shared: Option<&Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
    trace: Option<&PlasmTraceContext>,
) -> Result<Vec<serde_json::Value>, String> {
    let seed_ir = it
        .seed_ir
        .as_ref()
        .ok_or_else(|| "iterate_until missing seed_ir for re-observe".to_string())?;
    let parsed = ParsedExpr {
        expr: seed_ir.expr.clone(),
        projection: seed_ir.projection.clone(),
        field_dot_extract: None,
    };
    let scoped_es =
        entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))?;
    let expr_label = seed_ir.display_expr.as_deref().unwrap_or("<iterate-seed>");
    let (_parsed, result, _artifact) = execute_plasm_parsed_expr(
        st,
        &scoped_es,
        session_id,
        expr_label,
        parsed,
        trace,
        0,
        None,
        None,
        None,
        plan_shared.map(|p| p.as_ref()),
    )
    .await?;
    Ok(result
        .entities
        .iter()
        .map(|e| crate::plasm_plan_run::cached_entity_row_json(e, scoped_es.cgs.as_ref()))
        .collect())
}
