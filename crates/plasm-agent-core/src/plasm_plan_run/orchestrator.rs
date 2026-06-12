//! Live plan orchestration.

use super::*;
use crate::evidence_chain::{
    attach_evidence_meta, chain, persist_evidence_sidecars, StepExecutedRecord,
};
use crate::http_execute::run_seal_record_for_handle;
use crate::plasm_comp_lift::ExecutablePlasmComp;
use crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node;
use crate::plasm_step_convert::step_payload_to_validated_node;
use plasm_core::PlasmReturn;

#[allow(clippy::too_many_arguments)]
pub async fn run_plasm_comp(
    es: &ExecuteSession,
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
    run: bool,
    mcp_tool_hooks: Option<PlasmPlanRunHooks<'_>>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
) -> Result<PlasmPlanRunResult, String> {
    let dry = evaluate_plasm_comp_dry(es, bundle)?;
    if !run {
        let comp = crate::plasm_comp_wire::plasm_comp_json_from_dry(&dry);
        return Ok(PlasmPlanRunResult {
            version: dry.version,
            node_results: dry.node_results,
            graph_summary: dry.graph_summary,
            comp,
            code_plan_run_artifacts: Vec::new(),
            run_markdown: None,
            run_plasm_meta: None,
            return_steps: Vec::new(),
        });
    }
    run_plasm_comp_scoped(
        es,
        st,
        prompt_hash,
        session_id,
        bundle.executable(),
        dry,
        mcp_tool_hooks,
        execution_scope,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_plasm_comp_scoped(
    es: &ExecuteSession,
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    executable: &ExecutablePlasmComp,
    dry: DryPlasmPlanEvaluation,
    mcp_tool_hooks: Option<PlasmPlanRunHooks<'_>>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
) -> Result<PlasmPlanRunResult, String> {
    crate::operation::with_plan_execute_scope(execution_scope, async {
        run_executable_plan_phased(
            es,
            st,
            prompt_hash,
            session_id,
            executable,
            dry,
            mcp_tool_hooks,
            execution_scope,
        )
        .await
    })
    .await
}

#[derive(Debug, Clone)]
pub(crate) struct MaterializedNode {
    pub(crate) entry_id: String,
    pub(crate) entity: String,
    pub(crate) result: ExecutionResult,
    pub(crate) row_source: MaterializedRowSource,
    /// Raw row values for downstream language semantics. Synthetic scalar bindings stay scalar here
    /// even though display/publication wraps them as `{ "value": ... }` cached entities.
    pub(crate) rows: Vec<serde_json::Value>,
    /// Parallel canonical identity handles (one per row when known).
    pub(crate) row_identities: Vec<Option<plasm_core::RowIdentity>>,
    pub(crate) artifact: Option<crate::run_artifacts::RunArtifactHandle>,
    pub(crate) display: String,
    pub(crate) projection: Option<Vec<String>>,
}

pub(crate) fn inline_row_source(rows: &[serde_json::Value]) -> MaterializedRowSource {
    MaterializedRowSource::Inline(rows.to_vec())
}

pub(crate) struct MaterializedInputRow {
    pub(crate) node: PlanNodeId,
    pub(crate) proof: crate::plasm_plan::InputCardinalityProof,
    pub(crate) row: serde_json::Value,
    pub(crate) row_identity: Option<plasm_core::RowIdentity>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_executable_plan_phased(
    es: &ExecuteSession,
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    executable: &ExecutablePlasmComp,
    dry: DryPlasmPlanEvaluation,
    mcp_tool_hooks: Option<PlasmPlanRunHooks<'_>>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
) -> Result<PlasmPlanRunResult, String> {
    let mut materialized: BTreeMap<PlanNodeId, MaterializedNode> = BTreeMap::new();
    let approval_policy = PlasmPlanApprovalPolicy::automatic();
    let mut approval_receipts: Vec<PlasmPlanApprovalReceipt> = Vec::new();
    let mut trace = None;
    let mut sink = None;
    let mut meta_index = None;
    if let Some(hooks) = mcp_tool_hooks {
        trace = Some(hooks.trace);
        sink = Some(hooks.sink);
        meta_index = Some(hooks.meta_index);
    }
    let step_total = executable.steps_topo.len() as u32;
    let mut evidence_steps = Vec::with_capacity(step_total as usize);
    for (step_idx, (step_id, payload)) in executable.steps_topo.iter().enumerate() {
        if let Some(scope) = execution_scope {
            scope.check()?;
            scope.set_progress(
                step_idx as u32 + 1,
                step_total,
                Some(step_id.as_str().to_string()),
            );
        }
        let node = step_payload_to_validated_node(step_id, payload, &executable.bind)?;
        if let Some(gate) = inferred_node_approval(&node) {
            let receipt = approval_policy.review(gate);
            match receipt.decision {
                PlasmPlanApprovalDecision::Approved => approval_receipts.push(receipt),
            }
        }
        let mat = match &node {
            ValidatedPlanNode::Surface(surface) => {
                let parsed = if let Some(ir) = &surface.ir {
                    let pe = ParsedExpr {
                        expr: ir.expr.clone(),
                        projection: ir.projection.clone(),
                    };
                    instantiate_parsed_expr_plan_inputs(pe, &surface.uses_result, &materialized)?
                } else if let Some(template) = &surface.ir_template {
                    let input_rows =
                        materialized_result_use_inputs(&materialized, &surface.uses_result)?;
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
                    entry_scoped_execute_session(es, surface.qualified_entity.as_ref())?;
                let host_page = crate::plan_read_bounds::effective_host_page_size(surface);
                let rows_progress = execution_scope.and_then(|s| s.rows_progress_fn());
                let (parsed, mut result, artifact) = execute_plasm_parsed_expr(
                    st,
                    &scoped_es,
                    session_id,
                    expr_label,
                    parsed,
                    trace.as_ref(),
                    step_idx as i64,
                    host_page,
                    surface.pushed_read_budget.clone(),
                    rows_progress,
                )
                .await?;
                if let Some(cap) = host_page {
                    if result.entities.len() > cap {
                        result.entities.truncate(cap);
                        result.count = result.entities.len();
                    }
                }
                let entity_type = surface
                    .qualified_entity
                    .as_ref()
                    .map(|q| q.entity.as_str())
                    .unwrap_or_else(|| node.id().as_str());
                let row_source = crate::graph_rehydrate::materialize_surface_rows(
                    &scoped_es,
                    st,
                    scoped_es.cgs.as_ref(),
                    entity_type,
                    &result,
                )
                .await;
                let mat_entities = crate::graph_rehydrate::materialized_entities_for_surface(
                    &scoped_es,
                    st,
                    session_id,
                    scoped_es.cgs.as_ref(),
                    entity_type,
                    &result,
                )
                .await;
                let mat_rows = match &row_source {
                    MaterializedRowSource::Inline(rows) => rows.clone(),
                    MaterializedRowSource::GraphBacked { .. } => Vec::new(),
                };
                if let Some(scope) = execution_scope {
                    scope.sync_rows_materialized(result.count.max(result.entities.len()));
                }
                if let Some(sink) = sink.as_ref() {
                    trace_record_plasm_line(
                        sink, step_idx, expr_label, &parsed, &result, &scoped_es,
                    )
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
                        .unwrap_or_else(|| es.entry_id.clone()),
                    entity: surface
                        .qualified_entity
                        .as_ref()
                        .map(|q| q.entity.clone())
                        .unwrap_or_else(|| node.id().as_str().to_string()),
                    display: crate::expr_display::expr_display(&parsed.expr),
                    projection: parsed.projection,
                    row_source,
                    rows: mat_rows,
                    row_identities: row_identities_from_entities(
                        &scoped_es,
                        parsed.expr.primary_entity(),
                        &mat_entities,
                    ),
                    result,
                    artifact,
                }
            }
            ValidatedPlanNode::Data(data) => {
                let rows = plan_value_to_rows(&data.data)?;
                let empty_identities = vec![None; rows.len()];
                materialize_synthetic_node(
                    st,
                    es,
                    session_id,
                    &node,
                    es.entry_id.as_str(),
                    None,
                    rows,
                    empty_identities,
                    trace.as_ref(),
                )
                .await?
            }
            ValidatedPlanNode::Derive(derive) => {
                let owner_entry_id = materialized
                    .get(&derive.source)
                    .map(|m| m.entry_id.clone())
                    .unwrap_or_else(|| es.entry_id.clone());
                let source_rows =
                    materialized_rows(es, st, session_id, &materialized, &derive.source).await?;
                let input_rows = materialized_singleton_inputs(&materialized, &derive.inputs)?;
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
                materialize_synthetic_node(
                    st,
                    es,
                    session_id,
                    &node,
                    owner_entry_id.as_str(),
                    None,
                    rows,
                    empty_identities,
                    trace.as_ref(),
                )
                .await?
            }
            ValidatedPlanNode::Compute(compute) => {
                let owner_entry_id = PlanNodeId::new(compute.compute.source.clone())
                    .ok()
                    .and_then(|source| materialized.get(&source).map(|m| m.entry_id.clone()))
                    .unwrap_or_else(|| es.entry_id.clone());
                let source_id = PlanNodeId::new(compute.compute.source.clone())?;
                let source_mat = materialized.get(&source_id).ok_or_else(|| {
                    format!(
                        "source node {:?} has not been materialized",
                        source_id.as_str()
                    )
                })?;
                let scoped_cgs = es.cgs.as_ref();
                let rows = eval_compute_with_row_source(
                    &compute.compute,
                    &source_mat.row_source,
                    es,
                    st,
                    session_id,
                    scoped_cgs,
                )
                .await?;
                let row_identities = propagate_row_identities(
                    &source_id,
                    &compute.compute.op,
                    &materialized,
                    rows.len(),
                )?;
                materialize_synthetic_node(
                    st,
                    es,
                    session_id,
                    &node,
                    owner_entry_id.as_str(),
                    compute.compute.schema.entity.as_deref(),
                    rows,
                    row_identities,
                    trace.as_ref(),
                )
                .await?
            }
            ValidatedPlanNode::RelationTraversal(relation) => {
                materialize_validated_relation_traversal(
                    st,
                    es,
                    session_id,
                    step_idx,
                    &node,
                    relation,
                    &materialized,
                    trace.as_ref(),
                    sink.as_ref(),
                )
                .await?
            }
            ValidatedPlanNode::ForEach(for_each) => {
                materialize_for_each_node(
                    st,
                    es,
                    session_id,
                    step_idx,
                    for_each,
                    &materialized,
                    trace.as_ref(),
                    sink.as_ref(),
                )
                .await?
            }
        };
        let source_line = render_node_operation(&node);
        let parsed_evidence = parsed_expr_for_plan_node(&node);
        let step_entry_id = mat.entry_id.clone();
        let step_fps = mat.result.request_fingerprints.clone();
        materialized.insert(node.id().clone(), mat);
        evidence_steps.push(StepExecutedRecord {
            step_id: step_id.as_str().to_string(),
            step_index: step_idx as u32,
            entry_id: Some(step_entry_id),
            source_line,
            parsed: parsed_evidence,
            request_fingerprints: step_fps,
        });
    }
    if let Some(evidence) = chain(es) {
        evidence
            .record_steps_executed(&evidence_steps)
            .map_err(|e| format!("evidence step_executed: {e}"))?;
    }

    let return_node_ids = plasm_return_node_ids(&executable.return_)?;
    let mut steps = Vec::new();
    let return_names = plasm_return_names(&executable.return_);
    for (i, node_ref) in return_node_ids.iter().enumerate() {
        let mat = materialized.get(node_ref).ok_or_else(|| {
            format!(
                "plan.return materialized node {:?} missing",
                node_ref.as_str()
            )
        })?;
        if let Some(h) = &mat.artifact {
            let seal = run_seal_record_for_handle(
                st,
                es,
                prompt_hash,
                session_id,
                h,
                Some(node_ref.as_str().to_string()),
            )
            .await?;
            if let Some(evidence) = chain(es) {
                evidence
                    .record_run_sealed(&seal)
                    .map_err(|e| format!("evidence run_sealed: {e}"))?;
            }
        }
        steps.push(PublishedResultStep {
            name: return_names.get(i).cloned().flatten(),
            node_id: Some(node_ref.as_str().to_string()),
            entry_id: Some(mat.entry_id.clone()),
            entity: Some(mat.entity.clone()),
            cgs: es
                .contexts_by_entry
                .get(&mat.entry_id)
                .map(|ctx| ctx.cgs.clone()),
            display: mat.display.clone(),
            projection: mat.projection.clone(),
            result: mat.result.clone(),
            artifact: mat.artifact.clone(),
        });
    }
    let return_steps = steps.clone();
    let out = publish_plasm_result_steps(es.cgs.as_ref().into(), meta_index, &steps);
    let comp = crate::plasm_comp_wire::plasm_comp_json_from_dry(&dry);
    let mut code_plan_run_artifacts = Vec::new();
    let mut evidence_run_ids = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let Some(h) = step.artifact.as_ref() else {
            continue;
        };
        evidence_run_ids.push(h.run_id);
        code_plan_run_artifacts.push(CodePlanRunArtifactRef {
            run_id: h.run_id.to_wire(),
            artifact_uri: Some(h.plasm_uri.clone()),
            canonical_artifact_uri: Some(h.canonical_plasm_uri.clone()),
            artifact_path: Some(h.http_path.clone()),
            run_step: Some(i),
            node_id: step.node_id.clone(),
            display: Some(step.display.clone()),
            request_fingerprints: h.request_fingerprints.clone(),
        });
    }
    let mut evidence_head_hex = None;
    if let Some(evidence) = chain(es) {
        if let Some(bundle) = evidence
            .finish_bundle()
            .map_err(|e| format!("evidence finish: {e}"))?
        {
            evidence_head_hex = bundle.chain.head.map(|h| h.to_hex());
            persist_evidence_sidecars(
                &st.run_artifacts,
                prompt_hash,
                session_id,
                &evidence_run_ids,
                &bundle,
            )
            .await
            .map_err(|e| format!("evidence persist: {e}"))?;
        }
    }
    let mut run_plasm_meta = out.tool_meta;
    if let Some(evidence) = chain(es) {
        run_plasm_meta = attach_evidence_meta(
            run_plasm_meta,
            prompt_hash,
            session_id,
            evidence.as_ref(),
            &evidence_run_ids,
            evidence_head_hex,
        );
    }
    Ok(PlasmPlanRunResult {
        version: dry.version,
        node_results: dry.node_results,
        graph_summary: graph_summary_with_approval_receipts(dry.graph_summary, &approval_receipts),
        comp,
        code_plan_run_artifacts,
        run_markdown: Some(out.markdown),
        run_plasm_meta,
        return_steps,
    })
}

fn plasm_return_node_ids(ret: &PlasmReturn) -> Result<Vec<PlanNodeId>, String> {
    match ret {
        PlasmReturn::Step { step } => Ok(vec![PlanNodeId::new(step.as_str().to_string())?]),
        PlasmReturn::Parallel { steps } => steps
            .iter()
            .map(|s| PlanNodeId::new(s.as_str().to_string()))
            .collect(),
    }
}

fn plasm_return_names(ret: &PlasmReturn) -> Vec<Option<String>> {
    match ret {
        PlasmReturn::Step { step } => vec![Some(step.as_str().to_string())],
        PlasmReturn::Parallel { steps } => {
            steps.iter().map(|s| Some(s.as_str().to_string())).collect()
        }
    }
}
