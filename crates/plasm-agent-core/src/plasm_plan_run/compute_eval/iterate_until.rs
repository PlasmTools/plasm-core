//! Live materialization for PLP-8 `iterate … step … until … take N`.

use super::super::*;
use super::eval::{instantiate_expr_template, wire_coercion_by_alias_from_inputs};
use super::for_each::{bound_row_plan_eval_env, cross_uses_excluding_item};
use super::materialized_result_use_inputs;
use crate::plasm_plan::ValidatedIterateUntilNode;
use plasm_core::expr_parser::ParsedExpr;
use thiserror::Error;

#[derive(Debug, Error)]
enum IterateUntilError {
    #[error("iteration row predicate could not be evaluated: {0}")]
    Predicate(#[from] plasm_runtime::RuntimeError),
    #[error("iteration seed produced no rows")]
    EmptySeed,
    #[error("iteration source {0} has not been materialized")]
    SourceNotMaterialized(String),
    #[error("iteration lost its seed row")]
    SeedRowLost,
    #[error("reobserved iteration rows must be inline")]
    ReobservedRowsNotInline,
    #[error("iteration re-observe after step {step} produced no rows")]
    EmptyReobservation { step: usize },
    #[error("iteration bound exhausted: until predicate not satisfied within take {take}")]
    BoundExhausted { take: u32 },
    #[error("iteration is missing seed_ir for re-observe")]
    MissingSeedExpression,
    #[error("iteration seed node is not materialized")]
    MissingSeedNode,
    #[error("iteration seed produced no rows")]
    EmptyPredicateInput,
    #[error("iteration predicate is missing")]
    MissingPredicate,
    #[error(transparent)]
    Collection(#[from] plasm_core::collection_codec::CollectionFault),
    #[error(transparent)]
    Operand(#[from] super::eval::RuntimeOperandError),
    #[error(transparent)]
    WireCoercion(#[from] super::eval::WireCoercionContextError),
}

impl From<IterateUntilError> for ExecutionFailure {
    fn from(error: IterateUntilError) -> Self {
        let code = match &error {
            IterateUntilError::Predicate(_) => "iterate_until_predicate_failed",
            IterateUntilError::EmptySeed => "iterate_until_empty_seed",
            IterateUntilError::SourceNotMaterialized(_) => "iterate_until_source_missing",
            IterateUntilError::SeedRowLost => "iterate_until_seed_row_lost",
            IterateUntilError::ReobservedRowsNotInline => "iterate_until_invalid_reobservation",
            IterateUntilError::EmptyReobservation { .. } => "iterate_until_empty_reobservation",
            IterateUntilError::BoundExhausted { .. } => "iterate_bound_exhausted",
            IterateUntilError::MissingSeedExpression => "iterate_until_seed_expression_missing",
            IterateUntilError::MissingSeedNode => "iterate_until_seed_node_missing",
            IterateUntilError::EmptyPredicateInput => "iterate_until_empty_predicate_input",
            IterateUntilError::MissingPredicate => "iterate_until_predicate_missing",
            IterateUntilError::Collection(_) => "iterate_until_incomplete_collection",
            IterateUntilError::Operand(_) => "iterate_until_operand_binding_failed",
            IterateUntilError::WireCoercion(_) => "iterate_until_operand_binding_failed",
        };
        Self::new(
            plasm_runtime::FailureCause::Program,
            code,
            error.to_string(),
        )
    }
}

fn row_satisfies_until(
    row: &plasm_core::Value,
    preds: &[plasm_runtime::row_predicate::BoundRowPredicate],
) -> Result<bool, IterateUntilError> {
    for pred in preds {
        if !crate::plasm_plan_run::predicate_matches(row, pred)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) async fn materialize_iterate_until_node(
    ctx: &super::super::step_materialize::PlanStepMaterializeCtx<'_>,
    node_index: usize,
    it: &ValidatedIterateUntilNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<MaterializedNode, ExecutionFailure> {
    let (st, es, session_id, trace, sink) = (ctx.st, ctx.es, ctx.session_id, ctx.trace, ctx.sink);
    let plan_shared = Some(Arc::clone(ctx.plan_shared));
    let mut current_rows = materialized_rows(es, st, session_id, materialized, &it.source).await?;
    if current_rows.is_empty() {
        return Err(IterateUntilError::EmptySeed.into());
    }
    let cross = cross_uses_excluding_item(&it.uses_result, &it.item_binding);
    let mut input_rows =
        materialized_result_use_inputs(materialized, &cross, None).map_err(|_| {
            IterateUntilError::Operand(super::eval::RuntimeOperandError::MaterializedInput(
                super::eval::MaterializedInputError,
            ))
        })?;
    let wire_coercion_by_alias =
        wire_coercion_by_alias_from_inputs(es, &mut input_rows).map_err(|error| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Program,
                "iterate_until_wire_coercion_context_invalid",
                error.to_string(),
            )
        })?;
    let scoped_es = entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))
        .map_err(|diagnostic| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Program,
                "iterate_until_catalog_scope_unavailable",
                diagnostic.to_string(),
            )
        })?;

    let seed_mat = materialized
        .get(&it.source)
        .ok_or_else(|| IterateUntilError::SourceNotMaterialized(it.source.as_str().to_owned()))?;
    let seed_qe = seed_mat.qualified_entity.clone();
    let seed_collection = seed_mat.result.collection.clone();

    let resolved_until = super::compute_ops::resolve_filter_predicates_with_materialized(
        &it.until_predicates.clone().into(),
        materialized,
    )
    .map_err(|error| {
        ExecutionFailure::new(
            plasm_runtime::FailureCause::Program,
            "iterate_until_predicate_resolution_failed",
            error.to_string(),
        )
    })?;
    let until_predicates = resolved_until
        .iter()
        .map(crate::plan_read_bounds::bind_row_predicate)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|diagnostic| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Program,
                "iterate_until_predicate_binding_failed",
                diagnostic.to_string(),
            )
        })?;
    let mut current = seed_mat.clone();
    let mut operations = plasm_runtime::OperationLedger::empty();
    let mut iteration_occurrence = ctx.occurrence_path.clone();
    let execution: Result<MaterializedNode, ExecutionFailure> = async {
        let mut request_fingerprints = Vec::new();
        let mut stats = ExecutionStats::default();
        let mut source = ExecutionSource::Cache;
        let mut dependencies = vec![seed_collection.clone()];
        let satisfied =
            evaluate_stop(ctx, it, &current, materialized, &until_predicates, 0).await?;
        request_fingerprints.extend(satisfied.1);
        stats.network_requests += satisfied.2;
        dependencies.push(satisfied.3);
        if satisfied.0 {
            return super::super::materialize::archive_materialize_iterate_until(
                st,
                es,
                session_id,
                &scoped_es,
                it,
                seed_qe,
                current_rows,
                &current,
                operations.clone(),
                request_fingerprints,
                stats,
                ExecutionSource::Cache,
                0,
                &dependencies,
                trace,
            )
            .await;
        }

        for step_idx in 1..=it.take {
            iteration_occurrence = ctx.occurrence_path.clone();
            iteration_occurrence.push(step_idx as usize);
            if let Some(step) = crate::map_body::iteration_step(it)? {
                let mut environment = materialized.clone();
                environment.insert(it.source.clone(), current.clone());
                let mut occurrence_path = ctx.occurrence_path.clone();
                occurrence_path.push(step_idx as usize);
                let mut scope_path = ctx.scope_path.clone();
                scope_path.push(it.id.to_string());
                let child_ctx = super::super::step_materialize::PlanStepMaterializeCtx {
                    es: ctx.es,
                    st: ctx.st,
                    session_id: ctx.session_id,
                    plan_shared: ctx.plan_shared,
                    approval_policy: ctx.approval_policy,
                    flow: ctx.flow,
                    trace: ctx.trace,
                    sink: ctx.sink,
                    python_host_calls: ctx.python_host_calls,
                    scope_path,
                    scope_budget: ctx.scope_budget.clone(),
                    occurrence_path,
                    rows_progress: ctx.rows_progress.clone(),
                    execution_scope: ctx.execution_scope,
                };
                let (result, _) = Box::pin(super::super::map_body::materialize(
                    &child_ctx,
                    &step,
                    &environment,
                ))
                .await?;
                operations.merge(&result.result.operations);
                request_fingerprints.extend(result.result.request_fingerprints.iter().cloned());
                super::super::plan_fanout_parallel::merge_execution_stats(
                    &mut stats,
                    &result.result.stats,
                    super::super::plan_fanout_parallel::ExecutionStatsFold::Telemetry,
                );
                source = super::super::plan_fanout_parallel::combine_execution_source(
                    source,
                    result.result.source,
                );
                dependencies.push(result.result.collection.clone());
            } else {
                let row = current_rows.first().ok_or(IterateUntilError::SeedRowLost)?;
                let env = bound_row_plan_eval_env(
                    &it.item_binding,
                    row,
                    &input_rows,
                    &wire_coercion_by_alias,
                );
                let parsed = instantiate_expr_template(
                    &it.effect_template.ir_template,
                    &env,
                    &scoped_es.cgs,
                )?;
                let expr_label = crate::expr_display::expr_display(&parsed.expr);
                let mut jobs = Vec::new();
                super::super::plan_fanout_parallel::push_row_job(
                    &mut jobs,
                    node_index,
                    (step_idx as usize).saturating_sub(1),
                    expr_label,
                    parsed,
                )
                .map_err(|diagnostic| {
                    ExecutionFailure::new(
                        plasm_runtime::FailureCause::Program,
                        "iterate_until_trace_index_invalid",
                        diagnostic.to_string(),
                    )
                })?;
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
                .await?;
                operations.merge(&fold.operations);
                request_fingerprints.extend(fold.request_fingerprints);
                super::super::plan_fanout_parallel::merge_execution_stats(
                    &mut stats,
                    &fold.stats,
                    super::super::plan_fanout_parallel::ExecutionStatsFold::Telemetry,
                );
                source = super::super::plan_fanout_parallel::combine_execution_source(
                    source,
                    fold.source,
                );
                dependencies.extend(fold.collections.iter().cloned());
            }

            // Always re-Get the seed after the step. Mutator echoes (even with `provides`) are not a
            // substitute for primary_read / composed views — e.g. Player.previous may echo song_id while
            // `is_liked` lives only on player_current. LangCursor.tick remains correct because re-Get
            // reads the updated cursor row.
            current = reobserve_seed(
                st,
                es,
                session_id,
                it,
                materialized,
                plan_shared.as_ref(),
                trace,
            )
            .await?;
            current_rows = current
                .row_source
                .inline_rows()
                .ok_or(IterateUntilError::ReobservedRowsNotInline)?
                .to_vec();
            if current_rows.is_empty() {
                return Err(IterateUntilError::EmptyReobservation {
                    step: step_idx as usize,
                }
                .into());
            }
            let satisfied = evaluate_stop(
                ctx,
                it,
                &current,
                materialized,
                &until_predicates,
                step_idx as usize,
            )
            .await?;
            request_fingerprints.extend(satisfied.1);
            stats.network_requests += satisfied.2;
            dependencies.push(satisfied.3);
            if satisfied.0 {
                return super::super::materialize::archive_materialize_iterate_until(
                    st,
                    es,
                    session_id,
                    &scoped_es,
                    it,
                    seed_qe,
                    current_rows,
                    &current,
                    operations.clone(),
                    request_fingerprints,
                    stats,
                    source,
                    step_idx,
                    &dependencies,
                    trace,
                )
                .await;
            }
        }

        Err(IterateUntilError::BoundExhausted { take: it.take }.into())
    }
    .await;
    execution.map_err(|failure| {
        failure
            .at(it.id.to_string(), iteration_occurrence)
            .with_effects(&operations)
    })
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
) -> Result<MaterializedNode, ExecutionFailure> {
    let seed_ir = it
        .seed_ir
        .as_ref()
        .ok_or(IterateUntilError::MissingSeedExpression)?;
    let parsed = ParsedExpr {
        expr: seed_ir.expr.clone(),
        projection: seed_ir.projection.clone(),
        field_dot_extract: None,
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&it.effect_template.qualified_entity))
        .map_err(|diagnostic| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Program,
                "iterate_until_catalog_scope_unavailable",
                diagnostic.to_string(),
            )
        })?;
    // Bound identity stays template + binding until live re-observe: instantiate holes
    // (`@tok`) before CML/HTTP so bearer never sees a plan-time Binding marker.
    let parsed = super::eval::instantiate_parsed_expr_plan_inputs(
        parsed,
        scoped_es.cgs.as_ref(),
        &seed_replay_uses(&seed_ir.expr),
        materialized,
    )
    .map_err(ExecutionFailure::from)?;
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
    let rows = result
        .entities()
        .iter()
        .map(|e| cached_entity_row_values(e, scoped_es.cgs.as_ref()))
        .collect::<Vec<_>>();
    let mut current = materialized
        .get(&it.source)
        .ok_or(IterateUntilError::MissingSeedNode)?
        .clone();
    current.row_identities = row_identities_from_entities(
        &scoped_es,
        &current.qualified_entity.entity,
        result.entities(),
    );
    current.row_source = inline_row_source_owned(rows);
    current.result = Arc::new(result);
    current.artifact = None;
    Ok(current)
}

async fn evaluate_stop(
    ctx: &super::super::step_materialize::PlanStepMaterializeCtx<'_>,
    it: &ValidatedIterateUntilNode,
    current: &MaterializedNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    predicates: &[plasm_runtime::row_predicate::BoundRowPredicate],
    occurrence: usize,
) -> Result<
    (
        bool,
        Vec<String>,
        usize,
        plasm_runtime::execution::ExecutionCollection,
    ),
    ExecutionFailure,
> {
    let Some(_) = &it.until_scope else {
        crate::python_compute::require_complete_collection(&current.result)?;
        let rows = materialized_rows(
            ctx.es,
            ctx.st,
            ctx.session_id,
            &BTreeMap::from([(it.source.clone(), current.clone())]),
            &it.source,
        )
        .await?;
        return Ok((
            row_satisfies_until(
                rows.first().ok_or(IterateUntilError::EmptyPredicateInput)?,
                predicates,
            )?,
            vec![],
            0,
            current.result.collection.clone(),
        ));
    };
    let mut environment = materialized.clone();
    environment.insert(it.source.clone(), current.clone());
    let predicate = crate::map_body::iteration_predicate(it)
        .map_err(ExecutionFailure::from)?
        .ok_or(IterateUntilError::MissingPredicate)?;
    let mut occurrence_path = ctx.occurrence_path.clone();
    occurrence_path.push(occurrence);
    let mut scope_path = ctx.scope_path.clone();
    scope_path.push(it.id.to_string());
    let child_ctx = super::super::step_materialize::PlanStepMaterializeCtx {
        es: ctx.es,
        st: ctx.st,
        session_id: ctx.session_id,
        plan_shared: ctx.plan_shared,
        approval_policy: ctx.approval_policy,
        flow: ctx.flow,
        trace: ctx.trace,
        sink: ctx.sink,
        python_host_calls: ctx.python_host_calls,
        scope_path,
        scope_budget: ctx.scope_budget.clone(),
        occurrence_path,
        rows_progress: ctx.rows_progress.clone(),
        execution_scope: ctx.execution_scope,
    };
    let (result, _) = Box::pin(super::super::map_body::materialize(
        &child_ctx,
        &predicate,
        &environment,
    ))
    .await?;
    crate::python_compute::require_complete_collection(&result.result)?;
    Ok((
        result.result.count() == 1,
        result.result.request_fingerprints.clone(),
        result.result.stats.network_requests,
        result.result.collection.clone(),
    ))
}
