//! Live plan orchestration.

use super::*;
use crate::evidence_chain::{active_chain, attach_evidence_meta, persist_evidence_sidecars};
use crate::http_execute::run_seal_record_for_handle;
use crate::plasm_comp_lift::ExecutablePlasmComp;
use crate::plasm_plan_run::step_materialize::{
    apply_step_materialize_outcomes, materialize_executable_plan_step, PlanStepMaterializeCtx,
};
use futures::future::try_join_all;
use plasm_core::plasm_monad::{PlasmStepPayload, StepId};
use plasm_core::PlasmReturn;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tracing::Instrument;

#[allow(clippy::too_many_arguments)]
pub async fn run_plasm_comp(
    es: &ExecuteSession,
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
    run: bool,
    mcp_tool_hooks: Option<PlanRunTraceHooks>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
    dry: Option<DryPlasmPlanEvaluation>,
    mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
) -> Result<PlasmPlanRunResult, String> {
    let dry = match dry {
        Some(d) => d,
        None => evaluate_plasm_comp_dry(es, bundle)?,
    };
    if !run {
        let comp_wire = crate::plasm_comp_wire::trace_comp_wire_from_dry(&dry);
        return Ok(PlasmPlanRunResult {
            version: dry.version,
            node_results: dry.node_results,
            graph_summary: dry.graph_summary,
            comp: Some(comp_wire),
            code_plan_run_artifacts: Vec::new(),
            run_markdown: None,
            run_plasm_meta: None,
            return_steps: Vec::new(),
            inline_plan_ui: None,
        });
    }
    // Heap-box the scoped live runner: debug async state machines for plan materialize
    // exceed the default thread stack when nested under callers (NAPI block_on, tests).
    Box::pin(run_plasm_comp_scoped(
        es,
        st,
        prompt_hash,
        session_id,
        bundle.executable(),
        dry,
        mcp_tool_hooks,
        execution_scope,
        mcp_result_policy,
    ))
    .instrument(crate::spans::plan_live_run())
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
    mcp_tool_hooks: Option<PlanRunTraceHooks>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
    mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
) -> Result<PlasmPlanRunResult, String> {
    crate::operation::with_plan_execute_scope(execution_scope, async {
        Box::pin(run_executable_plan_phased(
            es,
            st,
            prompt_hash,
            session_id,
            executable,
            dry,
            mcp_tool_hooks,
            execution_scope,
            mcp_result_policy,
        ))
        .await
    })
    .await
}

#[derive(Debug, Clone)]
pub(crate) struct MaterializedNode {
    pub(crate) entry_id: String,
    pub(crate) entity: String,
    pub(crate) result: Arc<ExecutionResult>,
    pub(crate) row_source: MaterializedRowSource,
    /// Parallel canonical identity handles (one per row when known).
    pub(crate) row_identities: Vec<Option<plasm_core::RowIdentity>>,
    pub(crate) artifact: Option<crate::run_artifacts::RunArtifactHandle>,
    pub(crate) display: String,
    pub(crate) projection: Option<Vec<String>>,
}

impl MaterializedNode {
    /// Canonical constructor for a node materialized from already-known inline rows (dry stubs, the
    /// pure kernel's output, folded results): a `Cache`-sourced result with no fingerprints and no
    /// artifact. Centralizes the `ExecutionResult` boilerplate that would otherwise be copy-pasted
    /// at every synthetic materialization site.
    pub(crate) fn inline_cache(
        entry_id: String,
        entity: String,
        rows: Vec<serde_json::Value>,
        row_identities: Vec<Option<plasm_core::RowIdentity>>,
        display: String,
        projection: Option<Vec<String>>,
    ) -> Self {
        MaterializedNode {
            entry_id,
            entity,
            result: Arc::new(ExecutionResult {
                count: rows.len(),
                entities: Vec::new(),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
            }),
            row_source: inline_row_source_owned(rows),
            row_identities,
            artifact: None,
            display,
            projection,
        }
    }

    pub(crate) fn inline_row_count(&self) -> usize {
        self.row_source.inline_rows().map_or(0, |rows| rows.len())
    }

    /// **CEP-5:** parent entities for relation materialize when the source is GraphBacked.
    pub(crate) async fn resolve_materialized_source_parents(
        &self,
        rehydrator: &crate::graph_rehydrate::GraphSurfaceRehydrator<'_>,
    ) -> Vec<plasm_runtime::CachedEntity> {
        rehydrator
            .resolve_source_parents_with_identities(
                self.entity.as_str(),
                self.result.as_ref(),
                &self.row_identities,
            )
            .await
    }
}

pub(crate) fn inline_row_source(rows: &[serde_json::Value]) -> MaterializedRowSource {
    MaterializedRowSource::Inline(rows.to_vec())
}

pub(crate) fn inline_row_source_owned(rows: Vec<serde_json::Value>) -> MaterializedRowSource {
    MaterializedRowSource::Inline(rows)
}

fn pre_layer_materialized_snapshot(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Arc<BTreeMap<PlanNodeId, MaterializedNode>> {
    Arc::new(materialized.clone())
}

pub(crate) struct MaterializedInputRow {
    pub(crate) node: PlanNodeId,
    pub(crate) proof: crate::plasm_plan::InputCardinalityProof,
    pub(crate) row: serde_json::Value,
    /// All materialized rows for this alias (column refs aggregate across `rows`).
    pub(crate) rows: Vec<serde_json::Value>,
    pub(crate) row_identity: Option<plasm_core::RowIdentity>,
    pub(crate) row_identities: Vec<Option<plasm_core::RowIdentity>>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_executable_plan_phased(
    es: &ExecuteSession,
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    executable: &ExecutablePlasmComp,
    mut dry: DryPlasmPlanEvaluation,
    mcp_tool_hooks: Option<PlanRunTraceHooks>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
    mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
) -> Result<PlasmPlanRunResult, String> {
    if let Some(evidence) = active_chain(es, execution_scope) {
        evidence
            .record_comp_committed(&dry.artifact().comp)
            .map_err(|e| format!("evidence comp_committed: {e}"))?;
    }
    let node_results = dry.take_node_results_for_live();
    let flow = dry.flow.clone();
    let plan_shared = Arc::new(
        crate::plan_execute_shared::PlanLineExecuteShared::prepare(es, st, session_id).await,
    );
    let mut materialized: BTreeMap<PlanNodeId, MaterializedNode> = BTreeMap::new();
    let approval_policy = PlasmPlanApprovalPolicy::automatic();
    let mut approval_receipts: Vec<PlasmPlanApprovalReceipt> = Vec::new();
    let mut trace = None;
    let mut sink = None;
    let meta_index_for_publish = mcp_tool_hooks
        .as_ref()
        .and_then(|hooks| hooks.meta_index.clone());
    if let Some(hooks) = mcp_tool_hooks {
        trace = Some(hooks.trace);
        sink = Some(hooks.sink);
    }
    let step_total = executable.steps_topo.len() as u32;
    let prepared_budgets =
        crate::plan_prepare::prepared_surface_budget_lookup(dry.validated_plan());
    let prepared_relation_budgets =
        crate::plan_prepare::prepared_relation_budget_lookup(dry.validated_plan());
    let mut evidence_steps = Vec::with_capacity(step_total as usize);
    let step_topo_index: HashMap<StepId, usize> = executable
        .steps_topo
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.clone(), i))
        .collect();
    let payload_by_step: HashMap<StepId, PlasmStepPayload> = executable
        .steps_topo
        .iter()
        .map(|(id, payload)| (id.clone(), payload.clone()))
        .collect();
    let layers = super::plan_schedule::bind_topo_execution_layers(&executable.bind)?;
    // PEC witness: the executable schedule is a total function of the semantic DAG (`steps` + `bind`)
    // that the durable commit id already seals, so this digest is identical at `plasm` (dry) and
    // `plasm_run` (replay). Emitting it makes the plan-vs-run schedule identity observable.
    let schedule_order: Vec<String> = executable
        .steps_topo
        .iter()
        .map(|(id, _)| id.as_str().to_string())
        .collect();
    let schedule_digest =
        super::ScheduleDigest::from_validated_plan(dry.validated_plan(), &schedule_order);
    tracing::debug!(
        target: "plasm.pec",
        schedule_digest = %schedule_digest.to_hex(),
        steps = step_total,
        "executable schedule lowered (pure/io classified)"
    );
    let rows_progress = execution_scope.and_then(|s| s.rows_progress_fn());
    let mat_ctx = PlanStepMaterializeCtx {
        es,
        st,
        session_id,
        plan_shared: &plan_shared,
        prepared_budgets: &prepared_budgets,
        prepared_relation_budgets: &prepared_relation_budgets,
        approval_policy: &approval_policy,
        flow: &flow,
        trace: trace.as_ref(),
        sink: sink.as_ref(),
        rows_progress: rows_progress.clone(),
        execution_scope,
    };
    for layer in layers {
        if let Some(scope) = execution_scope {
            scope.check()?;
        }
        let parallel = super::plan_schedule::layer_parallel_safe(&layer, &payload_by_step);
        if parallel {
            let rows_progress_parallel = rows_progress.clone();
            if let Some(scope) = execution_scope {
                if let Some(max_idx) = layer
                    .iter()
                    .filter_map(|id| step_topo_index.get(id).copied())
                    .max()
                {
                    scope.set_progress(
                        max_idx as u32 + 1,
                        step_total,
                        Some(format!("parallel layer ({} steps)", layer.len())),
                    );
                }
            }
            let materialized_snap = pre_layer_materialized_snapshot(&materialized);
            let es = es.clone();
            let st = st.clone();
            let session_id = session_id.to_string();
            let plan_shared = Arc::clone(&plan_shared);
            let prepared_budgets = prepared_budgets.clone();
            let prepared_relation_budgets = prepared_relation_budgets.clone();
            let approval_policy = approval_policy.clone();
            let trace_ctx = trace.clone();
            let sink = sink.clone();
            let bind = Arc::new(executable.bind.clone());
            let execution_scope_parallel = execution_scope.cloned();
            let mut joins = Vec::with_capacity(layer.len());
            for step_id in &layer {
                let step_idx = step_topo_index[step_id];
                let payload = payload_by_step[step_id].clone();
                let step_id = step_id.clone();
                let es = es.clone();
                let st = st.clone();
                let session_id = session_id.clone();
                let materialized_snap = materialized_snap.clone();
                let plan_shared = Arc::clone(&plan_shared);
                let prepared_budgets = prepared_budgets.clone();
                let prepared_relation_budgets = prepared_relation_budgets.clone();
                let approval_policy = approval_policy.clone();
                let flow = flow.clone();
                let trace_ctx = trace_ctx.clone();
                let sink = sink.clone();
                let bind = Arc::clone(&bind);
                let rows_progress_step = rows_progress_parallel.clone();
                let execution_scope_step = execution_scope_parallel.clone();
                let parent_span = tracing::Span::current();
                joins.push(async move {
                    let step_span =
                        crate::spans::plan_step_materialize(&parent_span, step_id.as_str());
                    let mat_ctx = PlanStepMaterializeCtx {
                        es: &es,
                        st: &st,
                        session_id: session_id.as_str(),
                        plan_shared: &plan_shared,
                        prepared_budgets: &prepared_budgets,
                        prepared_relation_budgets: &prepared_relation_budgets,
                        approval_policy: &approval_policy,
                        flow: &flow,
                        trace: trace_ctx.as_ref(),
                        sink: sink.as_ref(),
                        rows_progress: rows_progress_step,
                        execution_scope: execution_scope_step.as_ref(),
                    };
                    Box::pin(materialize_executable_plan_step(
                        &mat_ctx,
                        step_idx,
                        &step_id,
                        &payload,
                        bind.as_ref(),
                        &materialized_snap,
                    ))
                    .instrument(step_span)
                    .await
                });
            }
            let mut outcomes = try_join_all(joins).await?;
            outcomes.sort_by_key(|o| o.evidence.step_index);
            apply_step_materialize_outcomes(
                &mut materialized,
                &mut evidence_steps,
                &mut approval_receipts,
                outcomes,
                execution_scope,
            );
        } else {
            for step_id in &layer {
                let step_idx = step_topo_index[step_id];
                if let Some(scope) = execution_scope {
                    scope.set_progress(
                        step_idx as u32 + 1,
                        step_total,
                        Some(step_id.as_str().to_string()),
                    );
                }
                let payload = payload_by_step
                    .get(step_id)
                    .ok_or_else(|| format!("missing payload for step {step_id}"))?;
                let outcome = Box::pin(materialize_executable_plan_step(
                    &mat_ctx,
                    step_idx,
                    step_id,
                    payload,
                    &executable.bind,
                    &materialized,
                ))
                .instrument(crate::spans::plan_step_materialize(
                    &tracing::Span::current(),
                    step_id.as_str(),
                ))
                .await?;
                apply_step_materialize_outcomes(
                    &mut materialized,
                    &mut evidence_steps,
                    &mut approval_receipts,
                    [outcome],
                    execution_scope,
                );
            }
        }
    }
    if let Some(evidence) = active_chain(es, execution_scope) {
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
            if let Some(evidence) = active_chain(es, execution_scope) {
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
            result: Arc::clone(&mat.result),
            artifact: mat.artifact.clone(),
        });
    }
    let out = crate::http_execute::publish_with_shared_meta_index(
        es.cgs.as_ref().into(),
        meta_index_for_publish,
        &steps,
        mcp_result_policy
            .as_ref()
            .unwrap_or(&crate::mcp_run_markdown::McpResultTransportPolicy::default()),
    )?;
    let comp = crate::plasm_comp_wire::trace_comp_wire_from_dry(&dry);
    let mut code_plan_run_artifacts = Vec::new();
    let mut evidence_run_ids = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let Some(h) = step.artifact.as_ref() else {
            continue;
        };
        evidence_run_ids.push(h.run_id);
        code_plan_run_artifacts.push(code_plan_run_artifact_ref(
            h,
            i + 1,
            &step.node_id,
            step.display.as_str(),
        ));
    }
    let mut evidence_head_hex = None;
    if let Some(evidence) = active_chain(es, execution_scope) {
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
    if let Some(meta) = run_plasm_meta.as_mut() {
        crate::symbol_map_resolve::attach_symbol_map_stability_to_run_meta(meta, es);
    }
    if let Some(evidence) = active_chain(es, execution_scope) {
        run_plasm_meta = attach_evidence_meta(
            run_plasm_meta,
            prompt_hash,
            session_id,
            evidence.as_ref(),
            &evidence_run_ids,
            evidence_head_hex,
        );
    }
    let run_markdown = if mcp_result_policy.is_some() {
        if let Some(plasm) = run_plasm_meta
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .and_then(|v| v.as_object())
        {
            let artifact = steps.first().and_then(|s| s.artifact.as_ref());
            crate::mcp_agent_present::AgentContent::run(
                crate::mcp_agent_present::RunTokens::from_live_result(plasm, artifact),
                &out.markdown,
            )
            .render()
        } else {
            out.markdown
        }
    } else {
        out.markdown
    };
    Ok(PlasmPlanRunResult {
        version: dry.version,
        node_results,
        graph_summary: graph_summary_with_approval_receipts(dry.graph_summary, &approval_receipts),
        comp: Some(comp),
        code_plan_run_artifacts,
        run_markdown: Some(run_markdown),
        run_plasm_meta,
        return_steps: steps,
        inline_plan_ui: None,
    })
}

fn code_plan_run_artifact_ref(
    handle: &crate::run_artifacts::RunArtifactHandle,
    run_step: usize,
    node_id: &Option<String>,
    display: &str,
) -> CodePlanRunArtifactRef {
    CodePlanRunArtifactRef {
        run_id: handle.run_id.to_wire(),
        artifact_uri: Some(handle.plasm_uri.clone()),
        canonical_artifact_uri: Some(handle.canonical_plasm_uri.clone()),
        artifact_path: Some(handle.http_path.clone()),
        run_step: Some(run_step),
        node_id: node_id.clone(),
        display: Some(display.to_string()),
        request_fingerprints: handle.request_fingerprints.clone(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node(display: &str) -> MaterializedNode {
        MaterializedNode {
            entry_id: "test".into(),
            entity: "Item".into(),
            result: Arc::new(ExecutionResult {
                count: 0,
                entities: Vec::new(),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: Vec::new(),
            }),
            row_source: MaterializedRowSource::Inline(Vec::new()),
            row_identities: Vec::new(),
            artifact: None,
            display: display.into(),
            projection: None,
        }
    }

    #[test]
    fn cep_9_parallel_layer_uses_frozen_materialized_snapshot() {
        let source = PlanNodeId::new("source").expect("source id");
        let later = PlanNodeId::new("later").expect("later id");
        let mut materialized = BTreeMap::from([(source.clone(), test_node("before"))]);

        let snapshot = pre_layer_materialized_snapshot(&materialized);
        materialized.get_mut(&source).expect("source node").display = "after".into();
        materialized.insert(later.clone(), test_node("later"));

        assert_eq!(
            snapshot.get(&source).expect("snapshot source").display,
            "before",
            "CEP-9: parallel workers must observe pre-layer materialized state"
        );
        assert!(
            !snapshot.contains_key(&later),
            "CEP-9: same-layer materialization must not appear in the worker snapshot"
        );
    }
}
