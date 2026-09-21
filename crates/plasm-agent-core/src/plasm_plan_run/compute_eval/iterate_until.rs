//! Live materialization for PLP-8 `iterate … step … until … take N`.

use super::super::*;
use super::eval::{instantiate_expr_template, wire_coercion_by_alias_from_inputs};
use super::for_each::{bound_row_plan_eval_env, cross_uses_excluding_item};
use super::materialized_result_use_inputs;
use crate::plasm_plan::ValidatedIterateUntilNode;
use plasm_core::expr_parser::ParsedExpr;

fn row_satisfies_until(
    row: &serde_json::Value,
    preds: &[plasm_runtime::row_predicate::BoundRowPredicate],
) -> Result<bool, String> {
    for pred in preds {
        if !crate::plasm_plan_run::predicate_matches(row, pred)? {
            return Ok(false);
        }
    }
    Ok(true)
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
    let mut current_rows = materialized_rows(es, st, session_id, materialized, &it.source).await?;
    if current_rows.is_empty() {
        return Err("iterate_until seed produced no rows".into());
    }
    let cross = cross_uses_excluding_item(&it.uses_result, &it.item_binding);
    let mut input_rows = materialized_result_use_inputs(materialized, &cross, None)?;
    let wire_coercion_by_alias = wire_coercion_by_alias_from_inputs(es, &mut input_rows)?;
    let scoped_es = entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))?;

    let seed_mat = materialized.get(&it.source).ok_or_else(|| {
        format!(
            "iterate_until source {} has not been materialized",
            it.source.as_str()
        )
    })?;
    let seed_qe = seed_mat.qualified_entity.clone();
    let seed_coverage = seed_mat.result.coverage;

    let resolved_until = super::compute_ops::resolve_filter_predicates_with_materialized(
        &it.until_predicates.clone().into(),
        materialized,
    )?;
    let until_predicates = resolved_until
        .iter()
        .map(crate::plan_read_bounds::bind_row_predicate)
        .collect::<Result<Vec<_>, _>>()?;
    if row_satisfies_until(&current_rows[0], &until_predicates)? {
        return super::super::materialize::archive_materialize_iterate_until(
            st,
            es,
            session_id,
            &scoped_es,
            it,
            seed_qe,
            current_rows,
            plasm_runtime::OperationLedger::empty(),
            Vec::new(),
            ExecutionStats::default(),
            ExecutionSource::Cache,
            0,
            coverage_for_iterate_until(seed_coverage, []),
            trace,
        )
        .await;
    }

    let mut operations = plasm_runtime::OperationLedger::empty();
    let mut request_fingerprints = Vec::new();
    let mut stats = ExecutionStats::default();
    let mut source = ExecutionSource::Cache;
    let mut step_coverages = Vec::new();

    for step_idx in 1..=it.take {
        let row = current_rows
            .first()
            .ok_or_else(|| "iterate_until lost seed row".to_string())?;
        let env =
            bound_row_plan_eval_env(&it.item_binding, row, &input_rows, &wire_coercion_by_alias);
        let parsed =
            instantiate_expr_template(&it.effect_template.ir_template, &env, &scoped_es.cgs)?;
        let expr_label = crate::expr_display::expr_display(&parsed.expr);
        let mut jobs = Vec::new();
        super::super::plan_fanout_parallel::push_row_job(
            &mut jobs,
            node_index,
            (step_idx as usize).saturating_sub(1),
            expr_label,
            parsed,
        )?;
        let fold = super::super::plan_fanout_parallel::execute_row_fanout(
            st,
            &scoped_es,
            session_id,
            jobs,
            trace,
            sink,
            plan_shared.clone(),
            super::super::plan_fanout_parallel::RowFanoutPolicy::state_step(),
        )
        .await
        .map_err(|e| format!("iterate_until step {step_idx}: {e}"))?;
        operations.merge(&fold.operations);
        request_fingerprints.extend(fold.request_fingerprints);
        super::super::plan_fanout_parallel::merge_execution_stats(
            &mut stats,
            &fold.stats,
            super::super::plan_fanout_parallel::ExecutionStatsFold::Telemetry,
        );
        source = super::super::plan_fanout_parallel::combine_execution_source(source, fold.source);
        step_coverages.push(fold.coverage);

        // Always re-Get the seed after the step. Mutator echoes (even with `provides`) are not a
        // substitute for primary_read / composed views — e.g. Player.previous may echo song_id while
        // `is_liked` lives only on player_current. LangCursor.tick remains correct because re-Get
        // reads the updated cursor row.
        current_rows = reobserve_seed(
            st,
            es,
            session_id,
            it,
            materialized,
            plan_shared.as_ref(),
            trace,
        )
        .await?;
        if current_rows.is_empty() {
            return Err(format!(
                "iterate_until re-observe after step {step_idx} produced no rows"
            ));
        }
        if row_satisfies_until(&current_rows[0], &until_predicates)? {
            return super::super::materialize::archive_materialize_iterate_until(
                st,
                es,
                session_id,
                &scoped_es,
                it,
                seed_qe,
                current_rows,
                operations,
                request_fingerprints,
                stats,
                source,
                step_idx,
                coverage_for_iterate_until(seed_coverage, step_coverages.iter().copied()),
                trace,
            )
            .await;
        }
    }

    Err(format!(
        "iterate_bound_exhausted: until predicate not satisfied within take {}",
        it.take
    ))
}

fn seed_replay_uses(expr: &plasm_core::Expr) -> Vec<crate::plasm_plan::PlanResultUse> {
    let mut seen = std::collections::BTreeSet::new();
    let mut uses = Vec::new();
    for reference in plasm_core::operand_binding::input_references(expr) {
        let plasm_core::PlasmInputRef::NodeInput { node, .. } = reference else {
            continue;
        };
        if !seen.insert(node.clone()) {
            continue;
        }
        uses.push(crate::plasm_plan::PlanResultUse {
            node: node.clone(),
            r#as: node,
            qualified_entity: None,
        });
    }
    uses
}

async fn reobserve_seed(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    it: &ValidatedIterateUntilNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
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
    let scoped_es = entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))?;
    // Bound identity stays template + binding until live re-observe: instantiate holes
    // (`@tok`) before CML/HTTP so bearer never sees a plan-time Binding marker.
    let parsed = super::eval::instantiate_parsed_expr_plan_inputs(
        parsed,
        scoped_es.cgs.as_ref(),
        &seed_replay_uses(&seed_ir.expr),
        materialized,
    )?;
    let expr_label = &crate::plan_dry_display::render_executable_expr(
        &seed_ir.expr,
        seed_ir.projection.as_deref(),
        Some(es),
    );
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
