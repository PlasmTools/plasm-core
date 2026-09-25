//! Per-step comp materialization during live plan execute.

use super::*;
use crate::evidence_chain::StepExecutedRecord;
use crate::plan_execute_shared::PlanLineExecuteShared;
use crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node;
use plasm_core::plasm_monad::StepId;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct PlanStepMaterializeOutcome {
    pub(crate) node_id: PlanNodeId,
    pub(crate) mat: MaterializedNode,
    pub(crate) evidence: StepExecutedRecord,
    pub(crate) approval: Option<PlasmPlanApprovalReceipt>,
    pub(crate) scope_instances: Option<serde_json::Value>,
}

/// Shared session/host context for live plan step materialization.
pub(crate) struct PlanStepMaterializeCtx<'a> {
    pub es: &'a ExecuteSession,
    pub st: &'a PlasmHostState,
    pub session_id: &'a str,
    pub plan_shared: &'a Arc<PlanLineExecuteShared>,
    pub approval_policy: &'a PlasmPlanApprovalPolicy,
    pub flow: &'a crate::plan_flow::PlanFlowAnalysis,
    pub trace: Option<&'a PlasmTraceContext>,
    pub sink: Option<&'a McpPlasmTraceSink>,
    pub python_host_calls: bool,
    pub scope_path: Vec<String>,
    pub occurrence_path: Vec<usize>,
    pub rows_progress: Option<plasm_runtime::RowsProgressFn>,
    pub execution_scope: Option<&'a crate::operation::ExecutionScope>,
}

pub(crate) fn apply_step_materialize_outcomes(
    materialized: &mut BTreeMap<PlanNodeId, MaterializedNode>,
    evidence_steps: &mut Vec<StepExecutedRecord>,
    scope_instances: &mut Vec<serde_json::Value>,
    approval_receipts: &mut Vec<PlasmPlanApprovalReceipt>,
    outcomes: impl IntoIterator<Item = PlanStepMaterializeOutcome>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
) {
    for outcome in outcomes {
        if let Some(instances) = outcome.scope_instances {
            scope_instances.push(instances);
        }
        if let Some(scope) = execution_scope {
            scope.sync_rows_materialized(
                outcome
                    .mat
                    .result
                    .count
                    .max(outcome.mat.result.entities.len()),
            );
        }
        if let Some(receipt) = outcome.approval {
            approval_receipts.push(receipt);
        }
        materialized.insert(outcome.node_id, outcome.mat);
        evidence_steps.push(outcome.evidence);
    }
}

pub(crate) async fn materialize_executable_plan_step(
    ctx: &PlanStepMaterializeCtx<'_>,
    step_idx: usize,
    step_id: &StepId,
    node: ValidatedPlanNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<PlanStepMaterializeOutcome, String> {
    use crate::occurrence_progress::{OccurrenceGuard, OccurrencePhase, OccurrenceProgress};
    let mut occurrence = OccurrenceGuard::new(
        ctx.execution_scope,
        OccurrenceProgress::running(
            ctx.scope_path.clone(),
            step_id.to_string(),
            ctx.occurrence_path.clone(),
        ),
    );
    let source_line = render_node_operation(&node);
    let parsed_evidence = parsed_expr_for_plan_node(&node);
    let approval = ctx
        .flow
        .approval_gate_for_node(node.id().as_str())
        .map(|gate| ctx.approval_policy.review(gate));
    let node_id = node.id().clone();
    // Classify once: pure steps use the shared kernel; runtime steps stay inside this closed
    // execution machine until a compiled request reaches the transport boundary.
    let mut scope_instances = None;
    let execution = async {
        Ok::<_, String>(match ExecStep::classify(node) {
            ExecStep::Io(IoStep::MapBody(map)) => {
                let (mat, instances) =
                    super::map_body::materialize(ctx, &map, materialized).await?;
                scope_instances = Some(instances);
                mat
            }
            ExecStep::Pure(pure) => live_materialize_pure(ctx, pure, materialized).await?,
            ExecStep::Io(io) => {
                let operation = async {
                    if ctx.python_host_calls {
                        Box::pin(super::python_host::materialize(
                            ctx,
                            &io,
                            step_idx,
                            materialized,
                        ))
                        .await
                    } else {
                        Box::pin(live_materialize_io(ctx, &io, step_idx, materialized)).await
                    }
                };
                if io.cancellable_read() {
                    // Cooperatively checking between HTTP batches cannot interrupt
                    // a suspended request. Only effect-free reads may be dropped.
                    crate::python_compute::await_checked(ctx.execution_scope, operation).await?
                } else {
                    operation.await?
                }
            }
        })
    };
    let mat = match Box::pin(execution).await {
        Ok(mat) => mat,
        Err(error) => {
            occurrence.fail(error.clone());
            return Err(error);
        }
    };
    if let Some(scope) = ctx.execution_scope {
        if let Err(error) = scope.check() {
            occurrence.fail(error.clone());
            return Err(error);
        }
    }
    occurrence.finish(
        OccurrencePhase::Done,
        Some(mat.result.count),
        mat.artifact.as_ref().map(|a| a.plasm_uri.clone()),
        mat.result.request_fingerprints.clone(),
        None,
    );
    let step_entry_id = mat.qualified_entity.entry_id.clone();
    let step_fps = mat.result.request_fingerprints.clone();
    Ok(PlanStepMaterializeOutcome {
        node_id,
        mat,
        scope_instances,
        evidence: StepExecutedRecord {
            step_id: step_id.as_str().to_string(),
            step_index: step_idx as u32,
            entry_id: Some(step_entry_id),
            source_line,
            parsed: parsed_evidence,
            request_fingerprints: step_fps,
        },
        approval: approval
            .filter(|receipt| matches!(receipt.decision, PlasmPlanApprovalDecision::Approved)),
    })
}

/// Live materialization of a pure step through the shared [`PureStep::materialize`] kernel.
///
/// `Compute` over a GraphBacked source is the one arm that cannot funnel its rows through the plain
/// kernel: live execute fuses the op with I/O streaming (bounded-RAM early-stop over spilled graph
/// pages). That fusion still evaluates the *same* `eval_compute_from_rows` op semantics — only row
/// *acquisition* differs — so it stays a pure step, just materialized against the live row source.
async fn live_materialize_pure(
    ctx: &PlanStepMaterializeCtx<'_>,
    pure: PureStep,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<MaterializedNode, String> {
    if let PureStep::Compute(compute) = &pure {
        let source_id = PlanNodeId::new(compute.compute.source.clone())?;
        let source_mat = materialized.get(&source_id).ok_or_else(|| {
            format!(
                "source node {:?} has not been materialized",
                source_id.as_str()
            )
        })?;
        let owner_entry_id = source_mat.qualified_entity.entry_id.clone();
        let binding_rows = binding_rows_for_compute(&compute.compute, materialized)?;
        let rows = if matches!(compute.compute.op, ComputeOp::Python { .. }) {
            use crate::occurrence_progress::{ExecutionStage, OccurrenceProgress};
            let report = |stage| {
                if let Some(scope) = ctx.execution_scope {
                    let mut event = OccurrenceProgress::running(
                        ctx.scope_path.clone(),
                        compute.id.to_string(),
                        ctx.occurrence_path.clone(),
                    );
                    event.stage = Some(stage);
                    scope.report_occurrence(event);
                }
            };
            report(ExecutionStage::Materializing);
            let checked = crate::python_compute::check_op(ctx.es, &compute.compute.op)?;
            let owner = plasm_core::symbol_tuning::EntityBinding {
                entry_id: source_mat.qualified_entity.entry_id.clone().into(),
                entity: source_mat.qualified_entity.entity.clone().into(),
            };
            crate::python_compute::require_complete_collection(&source_mat.result)?;
            if source_mat.result.count > crate::python_compute::MAX_INPUT_ROWS {
                return Err("compute input row budget exceeded".into());
            }
            let scoped = entry_scoped_execute_session(ctx.es, Some(&source_mat.qualified_entity))?;
            let input = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
                &scoped,
                ctx.st,
                ctx.session_id,
                &scoped.cgs,
            )
            .resolve_row_source_rows(
                &source_mat.row_source,
                Some(crate::python_compute::MAX_INPUT_ROWS + 1),
            )
            .await?;
            if input.len() != source_mat.result.count {
                return Err(
                    "Python compute materialization does not match the complete source count"
                        .into(),
                );
            }
            crate::python_compute::validate_input_budget(&input)?;
            report(ExecutionStage::Executing);
            let mut rendered = Vec::new();
            if checked.per_row {
                let mut bytes = 0usize;
                for row in input {
                    let content = crate::python_compute::await_checked(
                        ctx.execution_scope,
                        checked.run(
                            &ctx.st.python_pool,
                            &owner,
                            source_mat.result.coverage,
                            &[row],
                        ),
                    )
                    .await?;
                    bytes += content.len();
                    if bytes > 1_048_576 {
                        return Err("Python output byte budget exceeded".into());
                    }
                    rendered.push(serde_json::json!({"content": content}));
                }
            } else {
                let content = crate::python_compute::run_worker(
                    &ctx.st.python_pool,
                    checked,
                    owner,
                    source_mat.result.coverage,
                    input,
                    ctx.execution_scope,
                )
                .await?;
                rendered.push(serde_json::json!({"content": content}));
            }
            report(ExecutionStage::Validating);
            rendered
        } else {
            eval_compute_with_row_source(
                &compute.compute,
                &source_mat.row_source,
                &binding_rows,
                ctx.es,
                ctx.st,
                ctx.session_id,
                ctx.es.cgs.as_ref(),
            )
            .await?
        };
        let row_identities =
            propagate_row_identities(&source_id, &compute.compute.op, materialized, rows.len())?;
        let entity_override = compute.compute.schema.entity.as_deref().map(str::to_string);
        let input_coverage = coverage_from_compute_collections(
            source_mat.result.coverage,
            &compute.compute.op,
            materialized,
        );
        return materialize_synthetic_node(
            ctx.st,
            ctx.es,
            ctx.session_id,
            &pure.into_validated_node(),
            owner_entry_id.as_str(),
            entity_override.as_deref(),
            rows,
            row_identities,
            input_coverage,
            ctx.trace,
        )
        .await;
    }

    // Data / Derive: pure rows over already-materialized source rows (resolved async, which may
    // rehydrate a GraphBacked dependency — that acquisition is the permitted I/O difference).
    let source = pure.source()?;
    let source_rows = match &source {
        Some(src) => materialized_rows(ctx.es, ctx.st, ctx.session_id, materialized, src).await?,
        None => Vec::new(),
    };
    let owner_entry_id = match &source {
        Some(src) => materialized
            .get(src)
            .map(|m| m.qualified_entity.entry_id.clone())
            .ok_or_else(|| format!("source node {:?} has not been materialized", src.as_str()))?,
        None => ctx.es.entry_id.clone(),
    };
    let input_rows = materialized_singleton_inputs(materialized, pure.inputs())?;
    let binding_rows = pure.binding_rows(materialized)?;
    let pm = pure.materialize(
        &PureInputs {
            source_rows: &source_rows,
            input_rows: &input_rows,
            binding_rows: &binding_rows,
        },
        materialized,
    )?;
    let source_coverage = coverage_of_declared_source(source.as_ref(), materialized)?;
    materialize_synthetic_node(
        ctx.st,
        ctx.es,
        ctx.session_id,
        &pure.into_validated_node(),
        owner_entry_id.as_str(),
        pm.entity_override.as_deref(),
        pm.rows,
        pm.row_identities,
        source_coverage,
        ctx.trace,
    )
    .await
}

pub(super) async fn live_materialize_io(
    ctx: &PlanStepMaterializeCtx<'_>,
    step: &IoStep,
    step_idx: usize,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<MaterializedNode, String> {
    match step {
        IoStep::MapBody(_) => Err("map body must execute through scoped materialization".into()),
        IoStep::Capture(_) => Err("capture must be installed by its enclosing scope".into()),
        IoStep::Surface(surface) => {
            let surface = (**surface).clone();
            let scoped_es =
                entry_scoped_execute_session(ctx.es, surface.qualified_entity.as_ref())?;
            let parsed = if let Some(ir) = &surface.ir {
                let pe = ParsedExpr {
                    expr: ir.expr.clone(),
                    projection: ir.projection.clone(),
                    field_dot_extract: None,
                };
                let mut input_rows =
                    materialized_result_use_inputs(materialized, &surface.uses_result, None)?;
                let wire_coercion_by_alias =
                    wire_coercion_by_alias_from_inputs(ctx.es, &mut input_rows)?;
                instantiate_parsed_expr_plan_inputs_with_rows(
                    pe,
                    &scoped_es.cgs,
                    &input_rows,
                    &wire_coercion_by_alias,
                )?
            } else if let Some(template) = &surface.ir_template {
                let mut input_rows = materialized_result_use_inputs(
                    materialized,
                    &surface.uses_result,
                    surface.ir_template.as_ref(),
                )?;
                // Alias-specific coercion: each hole uses its source catalog entity
                // (e.g. AuthSession.access_token), not the surface target or uses_result.first().
                let wire_coercion_by_alias =
                    wire_coercion_by_alias_from_inputs(ctx.es, &mut input_rows)?;
                let scope = EvalScope::Root {
                    row: &serde_json::Value::Null,
                };
                let inputs = InputEnv { rows: &input_rows };
                let env = PlanEvalEnv {
                    scope,
                    inputs,
                    wire_coercion_by_alias: &wire_coercion_by_alias,
                };
                instantiate_expr_template(template, &env, &scoped_es.cgs)?
            } else {
                return Err(format!(
                    "plan node {} has no executable IR",
                    surface.id.as_str()
                ));
            };
            let expr_label = &crate::plan_dry_display::render_executable_expr(
                &parsed.expr,
                parsed.projection.as_deref(),
                Some(&scoped_es),
            );
            let host_page = crate::plan_read_bounds::effective_host_page_size(&surface);
            let (parsed, mut result, artifact) = execute_plasm_parsed_expr(
                ctx.st,
                &scoped_es,
                ctx.session_id,
                expr_label,
                parsed,
                ctx.trace,
                step_idx as i64,
                host_page,
                surface.pushed_read_budget.clone(),
                ctx.rows_progress.clone(),
                Some(ctx.plan_shared.as_ref()),
            )
            .await?;
            let entity_type = surface
                .qualified_entity
                .as_ref()
                .map(|q| q.entity.as_str())
                .unwrap_or_else(|| surface.id.as_str());
            // Backend acquisition is bounded by host_page above. Do not apply that
            // implicit budget a second time to an already-materialized collection:
            // it hides rows from downstream algebra and mislabels a page Complete.
            // Presentation previews belong to the renderer; only an explicit
            // page_size requests a synthetic cursor over these acquired rows.
            if let Some(cap) = surface.page_size {
                crate::plan_read_bounds::cap_execution_result_page(
                    &scoped_es,
                    &mut result,
                    cap,
                    surface.id.as_str(),
                    surface.qualified_entity.as_ref().ok_or_else(|| {
                        format!("pageable step `{}` lacks catalog ownership", surface.id)
                    })?,
                    ctx.trace.and_then(|t| t.logical_session_ref.as_deref()),
                );
            }
            if let Some(scope) = ctx.execution_scope {
                scope.sync_rows_materialized(result.count.max(result.entities.len()));
            }
            let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
                &scoped_es,
                ctx.st,
                ctx.session_id,
                scoped_es.cgs.as_ref(),
            );
            let row_source = rehydrator
                .materialize_surface_rows(entity_type, &result)
                .await;
            let identity_entities = rehydrator
                .resolve_source_parents(entity_type, &result)
                .await;
            let row_identities = row_identities_from_entities(
                &scoped_es,
                parsed.expr.primary_entity(),
                &identity_entities,
            );
            if let Some(sink) = ctx.sink {
                trace_record_plasm_line(sink, step_idx, expr_label, &parsed, &result, &scoped_es)
                    .await;
            }
            Ok(MaterializedNode {
                qualified_entity: surface
                    .qualified_entity
                    .clone()
                    .or_else(|| {
                        crate::catalog_ownership::resolve_qualified_entity_key(
                            &scoped_es,
                            parsed.expr.primary_entity(),
                            None,
                        )
                        .ok()
                    })
                    .unwrap_or_else(|| crate::plasm_plan::QualifiedEntityKey {
                        entry_id: ctx.es.entry_id.clone(),
                        entity: surface.id.as_str().to_string(),
                    }),
                display: crate::expr_display::expr_display(&parsed.expr),
                projection: parsed.projection,
                row_source,
                row_identities,
                result: Arc::new(result),
                artifact,
            })
        }
        // Heap-bound control-flow children keep their futures out of this dispatcher.
        IoStep::Relation(relation) => {
            let relation = (**relation).clone();
            let node = ValidatedPlanNode::RelationTraversal(relation);
            let ValidatedPlanNode::RelationTraversal(relation_ref) = &node else {
                unreachable!("relation traversal node");
            };
            Ok(Box::pin(materialize_validated_relation_traversal(
                ctx.st,
                ctx.es,
                ctx.session_id,
                step_idx,
                &node,
                relation_ref,
                materialized,
                ctx.trace,
                ctx.sink,
                Some(Arc::clone(ctx.plan_shared)),
            ))
            .await?)
        }
        IoStep::ForEach(for_each) => Ok(Box::pin(materialize_for_each_node(
            ctx.st,
            ctx.es,
            ctx.session_id,
            step_idx,
            for_each,
            materialized,
            ctx.trace,
            ctx.sink,
            Some(Arc::clone(ctx.plan_shared)),
        ))
        .await?),
        IoStep::IterateUntil(it) => Ok(Box::pin(materialize_iterate_until_node(
            ctx.st,
            ctx.es,
            ctx.session_id,
            step_idx,
            it,
            materialized,
            ctx.trace,
            ctx.sink,
            Some(Arc::clone(ctx.plan_shared)),
        ))
        .await?),
    }
}
