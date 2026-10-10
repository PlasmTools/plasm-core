//! Per-step comp materialization during live plan execute.

use super::*;
use crate::evidence_chain::StepExecutedRecord;
use crate::plan_execute_shared::PlanLineExecuteShared;
use crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node;
use crate::plasm_plan_run::executable_plan::PureStepError;
use plasm_core::plasm_monad::StepId;
use std::collections::BTreeMap;
use std::sync::Arc;

fn step_failure(code: &'static str, diagnostic: impl Into<String>) -> ExecutionFailure {
    ExecutionFailure::new(plasm_runtime::FailureCause::Program, code, diagnostic)
}

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
    pub(super) approval_policy: &'a PlasmPlanApprovalPolicy,
    pub flow: &'a crate::plan_flow::PlanFlowAnalysis,
    pub trace: Option<&'a PlasmTraceContext>,
    pub sink: Option<&'a McpPlasmTraceSink>,
    pub python_host_calls: bool,
    pub scope_path: Vec<String>,
    pub(super) scope_budget: Arc<super::map_body::ScopeBudget>,
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
                    .count()
                    .max(outcome.mat.result.entities().len()),
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
    schema_nodes: &[ValidatedPlanNode],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<PlanStepMaterializeOutcome, ExecutionFailure> {
    use crate::occurrence_progress::{OccurrenceGuard, OccurrencePhase, OccurrenceProgress};
    let mut occurrence = OccurrenceGuard::new(
        ctx.execution_scope,
        OccurrenceProgress::running(
            ctx.scope_path.clone(),
            step_id.to_string(),
            ctx.occurrence_path.clone(),
        ),
    );
    // Nested maps expose receipts at the executing child address, not twice
    // through both the child and the aggregate container.
    let owns_operations = !matches!(&node, ValidatedPlanNode::MapBody(_));
    let source_line = render_node_operation(&node);
    let parsed_evidence = parsed_expr_for_plan_node(&node);
    let qualified_step = ctx
        .scope_path
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(node.id().as_str()))
        .collect::<Vec<_>>()
        .join("/");
    let approval = ctx
        .flow
        .approval_gate_for_node(&qualified_step)
        .map(|gate| ctx.approval_policy.review(gate));
    let node_id = node.id().clone();
    // Classify once: pure steps use the shared kernel; runtime steps stay inside this closed
    // execution machine until a compiled request reaches the transport boundary.
    let mut scope_instances = None;
    let execution = async {
        Ok::<_, ExecutionFailure>(match ExecStep::classify(node) {
            ExecStep::Io(IoStep::MapBody(map)) => {
                let (mat, instances) =
                    Box::pin(super::map_body::materialize(ctx, &map, materialized)).await?;
                scope_instances = Some(instances);
                mat
            }
            ExecStep::Pure(pure) => {
                Box::pin(live_materialize_pure(ctx, pure, schema_nodes, materialized)).await?
            }
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
    let mat = match occurrence
        .mutation_journal()
        .scope(Box::pin(execution))
        .await
    {
        Ok(mat) => mat,
        Err(error) => {
            let mut failure = error.at(qualified_step.clone(), ctx.occurrence_path.clone());
            failure = failure.with_dispatches(occurrence.mutation_journal().snapshot());
            failure = failure.with_catalog(&ctx.es.catalog_cgs_hash);
            occurrence.fail(failure.to_string());
            return Err(failure);
        }
    };
    occurrence.record_result(
        mat.result.count(),
        mat.artifact.as_ref().map(|a| a.plasm_uri.clone()),
        mat.result.request_fingerprints.clone(),
        if owns_operations {
            mat.result.operations.clone()
        } else {
            plasm_runtime::OperationLedger::empty()
        },
    );
    if let Some(scope) = ctx.execution_scope {
        if let Err(error) = scope.check() {
            let failure = error
                .at(qualified_step.clone(), ctx.occurrence_path.clone())
                .with_effects(&mat.result.operations);
            occurrence.fail(failure.to_string());
            return Err(failure);
        }
    }
    occurrence.finish(
        OccurrencePhase::Done,
        Some(mat.result.count()),
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
    schema_nodes: &[ValidatedPlanNode],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<MaterializedNode, ExecutionFailure> {
    if let PureStep::Compute(compute) = &pure {
        let source_id = compute.source_node.clone();
        let source_mat = materialized.get(&source_id).ok_or_else(|| {
            step_failure(
                "plan_source_not_materialized",
                format!(
                    "source node `{}` has not been materialized",
                    source_id.as_str()
                ),
            )
        })?;
        let owner_entry_id = source_mat.qualified_entity.entry_id.clone();
        {
            use plasm_core::collection_codec::{CollectionCodec, RecordingCodec};
            RecordingCodec::new().materialize(
                source_mat.result.collection.membership(),
                compute.compute.op.collection_demand(),
            )?;
        }
        let binding_rows = binding_rows_for_compute(&compute.compute, materialized)
            .map_err(|error| ExecutionFailure::from(PureStepError::from(error)))?;
        // These are value inputs, unlike scheduling-only depends_on edges.
        // Union preserves uncertainty; every other collection capture is
        // consumed as a whole value (membership, rendering or branch choice).
        if !matches!(compute.compute.op, ComputeOp::Union { .. }) {
            for binding in binding_rows.keys() {
                let binding_id = PlanNodeId::new(binding.clone())?;
                let input = materialized.get(&binding_id).ok_or_else(|| {
                    step_failure(
                        "compute_binding_not_materialized",
                        format!("compute binding `{binding}` has not been materialized"),
                    )
                })?;
                crate::python_compute::require_complete_collection(&input.result)?;
            }
        }
        let mut value_shapes = Vec::new();
        let computed = if matches!(compute.compute.op, ComputeOp::Python { .. }) {
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
            let checked = crate::python_compute::check_op(ctx.es, &compute.compute.op)
                .map_err(ExecutionFailure::from)?;
            let owner = plasm_core::symbol_tuning::EntityBinding {
                entry_id: source_mat.qualified_entity.entry_id.clone().into(),
                entity: source_mat.qualified_entity.entity.clone().into(),
            };
            crate::python_compute::require_complete_collection(&source_mat.result)?;
            if source_mat.result.count() > crate::python_compute::MAX_INPUT_ROWS {
                return Err(step_failure(
                    "compute_input_row_budget_exceeded",
                    "compute input row budget exceeded",
                ));
            }
            let context = QualifiedEntityKey {
                entry_id: checked.context_entry().into(),
                entity: String::new(),
            };
            let source_context = if source_mat.qualified_entity.entry_id.is_empty() {
                &context
            } else {
                &source_mat.qualified_entity
            };
            let scoped = entry_scoped_execute_session(ctx.es, Some(source_context)).map_err(
                |diagnostic| {
                    step_failure("compute_catalog_context_invalid", diagnostic.to_string())
                },
            )?;
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
            .await
            .map_err(|diagnostic| {
                step_failure("compute_source_rehydration_failed", diagnostic.to_string())
            })?;
            if input.len() != source_mat.result.count() {
                return Err(step_failure(
                    "python_compute_source_count_mismatch",
                    "Python compute materialization does not match the complete source count",
                ));
            }
            crate::python_compute::validate_input_budget(&input).map_err(|diagnostic| {
                step_failure(
                    "python_compute_input_budget_invalid",
                    diagnostic.to_string(),
                )
            })?;
            report(ExecutionStage::Executing);
            let mut rendered = Vec::new();
            if checked.per_row {
                let mut remaining = 1_048_576;
                for (index, row) in input.into_iter().enumerate() {
                    use plasm_core::collection_codec::{
                        CollectionCodec, RecordingCodec, Transform,
                    };
                    let source = source_mat.result.collection.membership();
                    let membership = RecordingCodec::new().derive(
                        source.identity().derived(&("compute_occurrence", index))?,
                        &[source],
                        Transform::Filter {
                            retained: &[index],
                            captures: &[],
                        },
                    )?;
                    let content = crate::python_compute::await_checked(
                        ctx.execution_scope,
                        checked.run(&ctx.st.python_pool, &owner, &membership, &[row]),
                    )
                    .await?;
                    plasm_core::charge_value_budget(&content, &mut remaining).map_err(
                        |diagnostic| {
                            step_failure(
                                "python_compute_value_budget_exceeded",
                                diagnostic.to_string(),
                            )
                        },
                    )?;
                    rendered.push(content);
                }
            } else {
                let content = crate::python_compute::run_worker(
                    &ctx.st.python_pool,
                    checked,
                    owner,
                    source_mat.result.collection.membership(),
                    input,
                    ctx.execution_scope,
                )
                .await?;
                rendered.push(content);
            }
            report(ExecutionStage::Validating);
            let ComputeOp::Python { output_type, .. } = &compute.compute.op else {
                unreachable!()
            };
            let record = output_type.is_non_null_record();
            value_shapes = vec![
                if record {
                    MaterializedValueShape::Record
                } else {
                    MaterializedValueShape::ScalarColumn
                };
                rendered.len()
            ];
            let rows = rendered
                .into_iter()
                .map(|value| {
                    if record {
                        plasm_core::ValueRow::from_output(value)
                    } else {
                        plasm_core::ValueRow::from_iter([("value".into(), value)])
                    }
                })
                .collect();
            ComputedRows::synthetic(rows)
        } else {
            let input_contract = crate::map_body_schema::row_operation_contract(
                ctx.es,
                schema_nodes,
                &compute.compute.source,
            )
            .map_err(|diagnostic| {
                step_failure("compute_input_contract_invalid", diagnostic.to_string())
            })?;
            let input_contract = plasm_core::SyntheticResultSchema::for_value(input_contract)
                .map_err(|diagnostic| {
                    step_failure("compute_input_schema_invalid", diagnostic.to_string())
                })?
                .row_contract()
                .map_err(|diagnostic| {
                    step_failure("compute_input_row_contract_invalid", diagnostic.to_string())
                })?;
            eval_compute_with_row_source(
                &compute.compute,
                &input_contract,
                &source_mat.row_source,
                &binding_rows,
                &crate::graph_rehydrate::GraphSurfaceRehydrator::new(
                    ctx.es,
                    ctx.st,
                    ctx.session_id,
                    ctx.es.cgs.as_ref(),
                ),
            )
            .await?
        };
        let row_identities = propagate_row_identities(
            &source_id,
            &compute.compute.op,
            materialized,
            &computed.occurrences,
            computed.rows.len(),
        )
        .map_err(|diagnostic| {
            step_failure(
                "compute_row_identity_propagation_failed",
                diagnostic.to_string(),
            )
        })?;
        let entity_override = compute.compute.schema.entity.as_deref().map(str::to_string);
        return materialize_synthetic_node(
            ctx.st,
            ctx.es,
            ctx.session_id,
            &pure.into_validated_node(),
            owner_entry_id.as_str(),
            entity_override.as_deref(),
            computed.rows,
            value_shapes,
            row_identities,
            materialized,
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
            .ok_or_else(|| {
                step_failure(
                    "plan_source_not_materialized",
                    format!("source node `{}` has not been materialized", src.as_str()),
                )
            })?,
        None => ctx.es.entry_id.clone(),
    };
    let input_rows = materialized_singleton_inputs(materialized, pure.inputs())?;
    let binding_rows = pure.binding_rows(materialized)?;
    let pm = pure.materialize(
        &PureInputs {
            es: ctx.es,
            schema_nodes,
            source_rows: &source_rows,
            input_rows: &input_rows,
            binding_rows: &binding_rows,
        },
        materialized,
    )?;
    let result = materialize_synthetic_node(
        ctx.st,
        ctx.es,
        ctx.session_id,
        &pure.into_validated_node(),
        owner_entry_id.as_str(),
        pm.entity_override.as_deref(),
        pm.rows,
        pm.value_shapes,
        pm.row_identities,
        materialized,
        ctx.trace,
    )
    .await?;
    Ok(result)
}

pub(super) async fn live_materialize_io(
    ctx: &PlanStepMaterializeCtx<'_>,
    step: &IoStep,
    step_idx: usize,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<MaterializedNode, ExecutionFailure> {
    match step {
        IoStep::MapBody(_) => Err(step_failure(
            "map_body_execution_context_invalid",
            "map body must execute through scoped materialization",
        )),
        IoStep::Capture(_) => Err(step_failure(
            "capture_execution_context_invalid",
            "capture must be installed by its enclosing scope",
        )),
        IoStep::Surface(surface) => {
            let surface = (**surface).clone();
            let scoped_es = entry_scoped_execute_session(ctx.es, surface.qualified_entity.as_ref())
                .map_err(|diagnostic| {
                    step_failure("surface_catalog_context_invalid", diagnostic.to_string())
                })?;
            let parsed = if let Some(ir) = &surface.ir {
                let pe = ParsedExpr {
                    expr: ir.expr.clone(),
                    projection: ir.projection.clone(),
                    field_dot_extract: None,
                };
                let mut input_rows =
                    materialized_result_use_inputs(materialized, &surface.uses_result, None)?;
                let wire_coercion_by_alias =
                    wire_coercion_by_alias_from_inputs(ctx.es, &mut input_rows).map_err(
                        |diagnostic| {
                            step_failure("surface_wire_coercion_invalid", diagnostic.to_string())
                        },
                    )?;
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
                    wire_coercion_by_alias_from_inputs(ctx.es, &mut input_rows).map_err(
                        |diagnostic| {
                            step_failure("surface_wire_coercion_invalid", diagnostic.to_string())
                        },
                    )?;
                let scope = EvalScope::Root {
                    row: &plasm_core::Value::Null,
                };
                let inputs = InputEnv { rows: &input_rows };
                let env = PlanEvalEnv {
                    scope,
                    inputs,
                    wire_coercion_by_alias: &wire_coercion_by_alias,
                };
                instantiate_expr_template(template, &env, &scoped_es.cgs)?
            } else {
                return Err(step_failure(
                    "surface_executable_ir_missing",
                    format!("plan node {} has no executable IR", surface.id.as_str()),
                ));
            };
            let expr_label = &crate::plan_dry_display::render_executable_expr(
                &parsed.expr,
                parsed.projection.as_deref(),
                Some(&scoped_es),
            );
            let host_page = crate::plan_read_bounds::effective_host_page_size(&surface);
            // Surface IO is a deep async execution tree, just like relation and
            // control-flow IO below. Keep it out of the dispatcher's future so
            // nested hydration does not pay its stack cost on every poll.
            let (parsed, mut result, artifact) = Box::pin(execute_plasm_parsed_expr(
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
            ))
            .await?;
            let entity_type = surface
                .qualified_entity
                .as_ref()
                .map(|q| q.entity.as_str())
                .unwrap_or_else(|| surface.id.as_str());
            // Backend acquisition is bounded by host_page above. Do not apply that
            // implicit budget a second time to an already-materialized collection:
            // it hides rows from downstream algebra and mislabels a page Complete.
            // Presentation paging happens after execution, through typed publication.
            // Only an explicit page_size changes this execution-stage observation.
            if let Some(cap) = surface.page_size.filter(|_| {
                !matches!(
                    surface.pushed_read_budget,
                    Some(crate::plan_read_bounds::PushedReadBudget::Complete)
                )
            }) {
                crate::plan_read_bounds::cap_execution_result_page(
                    &scoped_es,
                    &mut result,
                    cap,
                    surface.id.as_str(),
                    surface.qualified_entity.as_ref().ok_or_else(|| {
                        step_failure(
                            "pageable_surface_ownership_missing",
                            format!("pageable step `{}` lacks catalog ownership", surface.id),
                        )
                    })?,
                    ctx.trace.and_then(|t| t.logical_session_ref.as_deref()),
                )?;
            }
            if let Some(scope) = ctx.execution_scope {
                scope.sync_rows_materialized(result.count().max(result.entities().len()));
            }
            let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
                &scoped_es,
                ctx.st,
                ctx.session_id,
                scoped_es.cgs.as_ref(),
            );
            let mut computation_source = result.clone();
            computation_source.collection = result.collection.computation_source().clone();
            let row_source = rehydrator
                .materialize_surface_rows(entity_type, &computation_source)
                .await;
            let identity_entities = rehydrator
                .resolve_source_parents(entity_type, &computation_source)
                .await
                .map_err(|diagnostic| {
                    step_failure("surface_identity_resolution_failed", diagnostic.to_string())
                })?;
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
                value_shapes: Vec::new(),
                optional_fields: Default::default(),
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
            ctx,
            step_idx,
            it,
            materialized,
        ))
        .await?),
    }
}
