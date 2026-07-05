//! Per-step comp materialization during live plan execute.

use super::*;
use crate::evidence_chain::StepExecutedRecord;
use crate::plan_execute_shared::PlanLineExecuteShared;
use crate::plan_prepare::PreparedSurfaceBudget;
use crate::plan_read_bounds::PushedReadBudget;
use crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node;
use crate::plasm_step_convert::step_payload_to_validated_node;
use plasm_core::plasm_monad::{PlasmBindGraph, PlasmStepPayload, StepId};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub(crate) struct PlanStepMaterializeOutcome {
    pub(crate) node_id: PlanNodeId,
    pub(crate) mat: MaterializedNode,
    pub(crate) evidence: StepExecutedRecord,
    pub(crate) approval: Option<PlasmPlanApprovalReceipt>,
}

/// Shared session/host context for live plan step materialization.
pub(crate) struct PlanStepMaterializeCtx<'a> {
    pub es: &'a ExecuteSession,
    pub st: &'a PlasmHostState,
    pub session_id: &'a str,
    pub plan_shared: &'a Arc<PlanLineExecuteShared>,
    pub prepared_budgets: &'a HashMap<String, PreparedSurfaceBudget>,
    pub prepared_relation_budgets: &'a HashMap<String, PushedReadBudget>,
    pub approval_policy: &'a PlasmPlanApprovalPolicy,
    pub flow: &'a crate::plan_flow::PlanFlowAnalysis,
    pub trace: Option<&'a PlasmTraceContext>,
    pub sink: Option<&'a McpPlasmTraceSink>,
    pub rows_progress: Option<plasm_runtime::RowsProgressFn>,
    pub execution_scope: Option<&'a crate::operation::ExecutionScope>,
}

pub(crate) fn apply_step_materialize_outcomes(
    materialized: &mut BTreeMap<PlanNodeId, MaterializedNode>,
    evidence_steps: &mut Vec<StepExecutedRecord>,
    approval_receipts: &mut Vec<PlasmPlanApprovalReceipt>,
    outcomes: impl IntoIterator<Item = PlanStepMaterializeOutcome>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
) {
    for outcome in outcomes {
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
    payload: &PlasmStepPayload,
    bind: &PlasmBindGraph,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<PlanStepMaterializeOutcome, String> {
    let node = step_payload_to_validated_node(step_id, payload, bind)?;
    let source_line = render_node_operation(&node);
    let parsed_evidence = parsed_expr_for_plan_node(&node);
    let approval = ctx
        .flow
        .approval_gate_for_node(node.id().as_str())
        .map(|gate| ctx.approval_policy.review(gate));
    let node_id = node.id().clone();
    let mat = match node {
        ValidatedPlanNode::Surface(mut surface) => {
            crate::plan_prepare::apply_prepared_surface_budget(&mut surface, ctx.prepared_budgets);
            let parsed = if let Some(ir) = &surface.ir {
                let pe = ParsedExpr {
                    expr: ir.expr.clone(),
                    projection: ir.projection.clone(),
                };
                instantiate_parsed_expr_plan_inputs(pe, &surface.uses_result, materialized)?
            } else if let Some(template) = &surface.ir_template {
                let input_rows = materialized_result_use_inputs(
                    materialized,
                    &surface.uses_result,
                    surface.ir_template.as_ref(),
                )?;
                let scope = EvalScope::Root {
                    row: &serde_json::Value::Null,
                };
                let inputs = InputEnv { rows: &input_rows };
                let env = PlanEvalEnv {
                    scope,
                    inputs,
                    wire_coercion: None,
                };
                instantiate_expr_template(template, &env)?
            } else {
                return Err(format!(
                    "plan node {} has no executable IR",
                    surface.id.as_str()
                ));
            };
            let expr_label = surface
                .ir
                .as_ref()
                .and_then(|ir| ir.display_expr.as_deref())
                .or(surface.display_expr.as_deref())
                .unwrap_or("<ir>");
            let scoped_es =
                entry_scoped_execute_session(ctx.es, surface.qualified_entity.as_ref())?;
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
            if let Some(cap) = host_page {
                crate::plan_read_bounds::cap_execution_result_page(
                    &scoped_es,
                    &mut result,
                    cap,
                    surface.id.as_str(),
                    entity_type,
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
            MaterializedNode {
                entry_id: surface
                    .qualified_entity
                    .as_ref()
                    .map(|q| q.entry_id.clone())
                    .or_else(|| {
                        crate::catalog_ownership::resolve_qualified_entity_key(
                            &scoped_es,
                            parsed.expr.primary_entity(),
                            None,
                        )
                        .ok()
                        .map(|q| q.entry_id)
                    })
                    .unwrap_or_else(|| ctx.es.entry_id.clone()),
                entity: surface
                    .qualified_entity
                    .as_ref()
                    .map(|q| q.entity.clone())
                    .unwrap_or_else(|| surface.id.as_str().to_string()),
                display: crate::expr_display::expr_display(&parsed.expr),
                projection: parsed.projection,
                row_source,
                row_identities,
                result: Arc::new(result),
                artifact,
            }
        }
        ValidatedPlanNode::Data(data) => {
            let rows = plan_value_to_rows(&data.data)?;
            let empty_identities = vec![None; rows.len()];
            let node = ValidatedPlanNode::Data(data);
            materialize_synthetic_node(
                ctx.st,
                ctx.es,
                ctx.session_id,
                &node,
                ctx.es.entry_id.as_str(),
                None,
                rows,
                empty_identities,
                ctx.trace,
            )
            .await?
        }
        ValidatedPlanNode::Derive(derive) => {
            let owner_entry_id = materialized
                .get(&derive.source)
                .map(|m| m.entry_id.clone())
                .unwrap_or_else(|| ctx.es.entry_id.clone());
            let source_rows =
                materialized_rows(ctx.es, ctx.st, ctx.session_id, materialized, &derive.source)
                    .await?;
            let input_rows = materialized_singleton_inputs(materialized, &derive.inputs)?;
            let mut rows = Vec::with_capacity(source_rows.len());
            for row in source_rows {
                let scope = EvalScope::Bound {
                    row: &row,
                    binding: &derive.item_binding,
                };
                let inputs = InputEnv { rows: &input_rows };
                let env = PlanEvalEnv {
                    scope,
                    inputs,
                    wire_coercion: None,
                };
                rows.push(eval_plan_value(&derive.value, &env)?);
            }
            let empty_identities = vec![None; rows.len()];
            let node = ValidatedPlanNode::Derive(derive);
            materialize_synthetic_node(
                ctx.st,
                ctx.es,
                ctx.session_id,
                &node,
                owner_entry_id.as_str(),
                None,
                rows,
                empty_identities,
                ctx.trace,
            )
            .await?
        }
        ValidatedPlanNode::Compute(compute) => {
            let owner_entry_id = PlanNodeId::new(compute.compute.source.clone())
                .ok()
                .and_then(|source| materialized.get(&source).map(|m| m.entry_id.clone()))
                .unwrap_or_else(|| ctx.es.entry_id.clone());
            let source_id = PlanNodeId::new(compute.compute.source.clone())?;
            let source_mat = materialized.get(&source_id).ok_or_else(|| {
                format!(
                    "source node {:?} has not been materialized",
                    source_id.as_str()
                )
            })?;
            let cross_binding_rows = cross_binding_rows_for_render(&compute.compute, materialized)?;
            let scoped_cgs = ctx.es.cgs.as_ref();
            let rows = eval_compute_with_row_source(
                &compute.compute,
                &source_mat.row_source,
                &cross_binding_rows,
                ctx.es,
                ctx.st,
                ctx.session_id,
                scoped_cgs,
            )
            .await?;
            let row_identities = propagate_row_identities(
                &source_id,
                &compute.compute.op,
                materialized,
                rows.len(),
            )?;
            let entity_override = compute.compute.schema.entity.as_deref().map(str::to_string);
            let node = ValidatedPlanNode::Compute(compute);
            materialize_synthetic_node(
                ctx.st,
                ctx.es,
                ctx.session_id,
                &node,
                owner_entry_id.as_str(),
                entity_override.as_deref(),
                rows,
                row_identities,
                ctx.trace,
            )
            .await?
        }
        ValidatedPlanNode::RelationTraversal(mut relation) => {
            crate::plan_prepare::apply_prepared_relation_budget(
                &mut relation,
                ctx.prepared_relation_budgets,
            );
            let node = ValidatedPlanNode::RelationTraversal(relation);
            let ValidatedPlanNode::RelationTraversal(relation_ref) = &node else {
                unreachable!("relation traversal node");
            };
            materialize_validated_relation_traversal(
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
            )
            .await?
        }
        ValidatedPlanNode::ForEach(ref for_each) => {
            materialize_for_each_node(
                ctx.st,
                ctx.es,
                ctx.session_id,
                step_idx,
                for_each,
                materialized,
                ctx.trace,
                ctx.sink,
                Some(Arc::clone(ctx.plan_shared)),
            )
            .await?
        }
    };
    let step_entry_id = mat.entry_id.clone();
    let step_fps = mat.result.request_fingerprints.clone();
    Ok(PlanStepMaterializeOutcome {
        node_id,
        mat,
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
