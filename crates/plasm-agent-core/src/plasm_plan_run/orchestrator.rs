//! Live plan orchestration.

use super::*;
use crate::evidence_chain::{active_chain, attach_evidence_meta, persist_evidence_sidecars};
use crate::http_execute::run_seal_record_for_handle;
use crate::plasm_plan_run::step_materialize::{
    apply_step_materialize_outcomes, materialize_executable_plan_step, PlanStepMaterializeCtx,
};
use futures::future::join_all;
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
) -> Result<PlasmPlanRunResult, ExecutionFailure> {
    run_plasm_comp_with_dispatch(
        es,
        st,
        prompt_hash,
        session_id,
        bundle,
        run,
        mcp_tool_hooks,
        execution_scope,
        dry,
        mcp_result_policy,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_plasm_comp_python(
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
) -> Result<PlasmPlanRunResult, ExecutionFailure> {
    run_plasm_comp_with_dispatch(
        es,
        st,
        prompt_hash,
        session_id,
        bundle,
        run,
        mcp_tool_hooks,
        execution_scope,
        dry,
        mcp_result_policy,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_plasm_comp_with_dispatch(
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
    python_host_calls: bool,
) -> Result<PlasmPlanRunResult, ExecutionFailure> {
    crate::python_compute::await_checked(
        execution_scope,
        Box::pin(async {
            crate::python_compute::admit_bundle(es, bundle)
                .await
                .map_err(ExecutionFailure::from)
        }),
    )
    .await?;
    let dry = match dry {
        Some(d) => d,
        None => evaluate_plasm_comp_dry(es, bundle)?,
    };
    if !run {
        let comp_wire = crate::plasm_comp_wire::trace_comp_wire_from_dry(&dry);
        return Ok(PlasmPlanRunResult {
            version: dry.version,
            agent_outcome: Default::default(),
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
        dry,
        mcp_tool_hooks,
        execution_scope,
        mcp_result_policy,
        python_host_calls,
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
    dry: DryPlasmPlanEvaluation,
    mcp_tool_hooks: Option<PlanRunTraceHooks>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
    mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
    python_host_calls: bool,
) -> Result<PlasmPlanRunResult, ExecutionFailure> {
    crate::operation::with_plan_execute_scope(execution_scope, async {
        Box::pin(run_executable_plan_phased(
            es,
            st,
            prompt_hash,
            session_id,
            dry,
            mcp_tool_hooks,
            execution_scope,
            mcp_result_policy,
            python_host_calls,
        ))
        .await
    })
    .await
}

/// Logical value shape retained when pure values are normalized into row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaterializedValueShape {
    Record,
    ScalarColumn,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MaterializedValueShapeError {
    #[error("materialized value shape metadata has {metadata} entries for {rows} rows")]
    MetadataCountMismatch { metadata: usize, rows: usize },
    #[error("materialized value shape occurrence {index} is outside row count {rows}")]
    OccurrenceOutOfBounds { index: usize, rows: usize },
}

#[derive(Debug, Clone)]
pub(crate) struct MaterializedNode {
    /// Original pure value shape; scalar bindings live in the explicit value column.
    pub(crate) value_shapes: Vec<MaterializedValueShape>,
    pub(crate) optional_fields: std::collections::BTreeSet<String>,
    pub(crate) qualified_entity: crate::plasm_plan::QualifiedEntityKey,
    pub(crate) result: Arc<ExecutionResult>,
    pub(crate) row_source: MaterializedRowSource,
    /// Parallel canonical identity handles (one per row when known).
    pub(crate) row_identities: Vec<Option<plasm_core::RowIdentity>>,
    pub(crate) artifact: Option<crate::run_artifacts::RunArtifactHandle>,
    pub(crate) display: String,
    pub(crate) projection: Option<Vec<String>>,
}

impl MaterializedNode {
    /// Shape belongs to an occurrence, just like its value and canonical identity.
    /// Empty metadata is the compact representation of an all-record sequence.
    pub(crate) fn value_shape_at(
        &self,
        index: usize,
    ) -> Result<MaterializedValueShape, MaterializedValueShapeError> {
        if !self.value_shapes.is_empty() && self.value_shapes.len() != self.result.count() {
            return Err(MaterializedValueShapeError::MetadataCountMismatch {
                metadata: self.value_shapes.len(),
                rows: self.result.count(),
            });
        }
        if index >= self.result.count() {
            return Err(MaterializedValueShapeError::OccurrenceOutOfBounds {
                index,
                rows: self.result.count(),
            });
        }
        Ok(self
            .value_shapes
            .get(index)
            .copied()
            .unwrap_or(MaterializedValueShape::Record))
    }

    /// Canonical constructor for a node materialized from already-known inline rows (dry stubs, the
    /// pure kernel's output, folded results): a `Cache`-sourced result with no fingerprints and no
    /// artifact. Centralizes the `ExecutionResult` boilerplate that would otherwise be copy-pasted
    /// at every synthetic materialization site.
    pub(crate) fn dry_values(
        cgs: &plasm_core::CGS,
        qualified_entity: crate::plasm_plan::QualifiedEntityKey,
        rows: Vec<plasm_core::ValueRow>,
        row_identities: Vec<Option<plasm_core::RowIdentity>>,
        display: String,
        projection: Option<Vec<String>>,
    ) -> Result<Self, ExecutionFailure> {
        let collection = plasm_runtime::execution::ExecutionCollection::observe_for(
            cgs,
            &("dry_values", &qualified_entity, &rows),
            0,
            rows_to_entities(&qualified_entity.entity, &rows)?,
            plasm_core::collection_codec::Observation::Literal,
        )?;
        Ok(MaterializedNode {
            value_shapes: Vec::new(),
            optional_fields: Default::default(),
            qualified_entity,
            result: Arc::new(ExecutionResult {
                collection,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
                operations: plasm_runtime::OperationLedger::empty(),
            }),
            row_source: inline_row_source_owned(rows),
            row_identities,
            artifact: None,
            display,
            projection,
        })
    }

    pub(crate) fn inline_row_count(&self) -> usize {
        self.row_source.inline_rows().map_or(0, |rows| rows.len())
    }

    /// **CEP-5:** parent entities for relation materialize when the source is GraphBacked.
    pub(crate) async fn resolve_materialized_source_parents(
        &self,
        rehydrator: &crate::graph_rehydrate::GraphSurfaceRehydrator<'_>,
    ) -> Result<
        plasm_core::collection_codec::SharedRows<plasm_runtime::CachedEntity>,
        crate::graph_rehydrate::GraphRehydrateError,
    > {
        // Original observations remain usable as captured parent identities after
        // a write evicts the cache. A projection only names its canonical parent;
        // its reduced payload must not replace a missing canonical graph row.
        let projected_identities = if self.projection.is_some() {
            self.row_identities.as_slice()
        } else {
            &[]
        };
        rehydrator
            .resolve_source_parents_with_identities(
                self.qualified_entity.entity.as_str(),
                self.result.as_ref(),
                projected_identities,
            )
            .await
    }
}

pub(crate) fn inline_row_source(rows: &[plasm_core::ValueRow]) -> MaterializedRowSource {
    MaterializedRowSource::Inline(rows.to_vec())
}

pub(crate) fn inline_row_source_owned(rows: Vec<plasm_core::ValueRow>) -> MaterializedRowSource {
    MaterializedRowSource::Inline(rows)
}

fn pre_layer_materialized_snapshot(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Arc<BTreeMap<PlanNodeId, MaterializedNode>> {
    Arc::new(materialized.clone())
}

pub(crate) struct MaterializedInputRow {
    pub(crate) optional_fields: std::collections::BTreeSet<String>,
    /// Declared value columns; whole-row references must not expose identity metadata.
    pub(crate) value_projection: Option<Vec<String>>,
    pub(crate) node: PlanNodeId,
    /// Catalog-qualified row domain of the source node (alias-specific hole coercion).
    pub(crate) qualified_entity: crate::plasm_plan::QualifiedEntityKey,
    /// CGS `id_field` for the source entity (e.g. `access_token` on AuthSession); `"id"` when unknown.
    pub(crate) id_field: String,
    pub(crate) proof: crate::plasm_plan::InputCardinalityProof,
    pub(crate) row: plasm_core::Value,
    /// All materialized rows for this alias (column refs aggregate across `rows`).
    pub(crate) rows: Vec<plasm_core::ValueRow>,
    pub(crate) row_identity: Option<plasm_core::RowIdentity>,
    pub(crate) row_identities: Vec<Option<plasm_core::RowIdentity>>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_executable_plan_phased(
    es: &ExecuteSession,
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    mut dry: DryPlasmPlanEvaluation,
    mcp_tool_hooks: Option<PlanRunTraceHooks>,
    execution_scope: Option<&crate::operation::ExecutionScope>,
    mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
    python_host_calls: bool,
) -> Result<PlasmPlanRunResult, ExecutionFailure> {
    let executable = dry.executable.clone();
    if let Some(evidence) = active_chain(es, execution_scope) {
        evidence.record_comp_committed(&dry.artifact().comp)?;
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
    let prepared_nodes: HashMap<StepId, ValidatedPlanNode> = dry
        .validated_plan()
        .nodes
        .iter()
        .cloned()
        .map(|node| {
            (
                StepId::new(node.id().as_str().to_string()).expect("validated step id"),
                node,
            )
        })
        .collect();
    let mut evidence_steps = Vec::with_capacity(step_total as usize);
    let mut scope_instances = Vec::new();
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
    let scope_budget = Arc::new(super::map_body::ScopeBudget::default());
    let mat_ctx = PlanStepMaterializeCtx {
        es,
        st,
        session_id,
        plan_shared: &plan_shared,
        approval_policy: &approval_policy,
        flow: &flow,
        trace: trace.as_ref(),
        sink: sink.as_ref(),
        python_host_calls,
        scope_path: Vec::new(),
        scope_budget: scope_budget.clone(),
        occurrence_path: Vec::new(),
        rows_progress: rows_progress.clone(),
        execution_scope,
    };
    let execution: Result<PlasmPlanRunResult, ExecutionFailure> = async {
        let schema_nodes = &dry.validated_plan().nodes;
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
                let prepared_nodes = Arc::new(prepared_nodes.clone());
                let approval_policy = approval_policy.clone();
                let trace_ctx = trace.clone();
                let sink = sink.clone();
                let execution_scope_parallel = execution_scope.cloned();
                let mut joins = Vec::with_capacity(layer.len());
                for step_id in &layer {
                    let step_idx = step_topo_index[step_id];
                    let node = prepared_nodes[step_id].clone();
                    let step_id = step_id.clone();
                    let es = es.clone();
                    let st = st.clone();
                    let session_id = session_id.clone();
                    let materialized_snap = materialized_snap.clone();
                    let plan_shared = Arc::clone(&plan_shared);
                    let approval_policy = approval_policy.clone();
                    let flow = flow.clone();
                    let trace_ctx = trace_ctx.clone();
                    let sink = sink.clone();
                    let rows_progress_step = rows_progress_parallel.clone();
                    let execution_scope_step = execution_scope_parallel.clone();
                    let scope_budget = scope_budget.clone();
                    let parent_span = tracing::Span::current();
                    joins.push(async move {
                        let step_span =
                            crate::spans::plan_step_materialize(&parent_span, step_id.as_str());
                        let mat_ctx = PlanStepMaterializeCtx {
                            es: &es,
                            st: &st,
                            session_id: session_id.as_str(),
                            plan_shared: &plan_shared,
                            approval_policy: &approval_policy,
                            flow: &flow,
                            trace: trace_ctx.as_ref(),
                            sink: sink.as_ref(),
                            python_host_calls,
                            scope_path: Vec::new(),
                            scope_budget,
                            occurrence_path: Vec::new(),
                            rows_progress: rows_progress_step,
                            execution_scope: execution_scope_step.as_ref(),
                        };
                        Box::pin(materialize_executable_plan_step(
                            &mat_ctx,
                            step_idx,
                            &step_id,
                            node,
                            schema_nodes,
                            &materialized_snap,
                        ))
                        .instrument(step_span)
                        .await
                    });
                }
                let completed = join_all(joins).await;
                let mut outcomes = Vec::new();
                let mut failure: Option<ExecutionFailure> = None;
                for result in completed {
                    match result {
                        Ok(value) => outcomes.push(value),
                        Err(error) => {
                            failure = Some(match failure {
                                Some(prior) => prior.merge(error),
                                None => error,
                            });
                        }
                    }
                }
                if let Some(mut error) = failure {
                    for outcome in &outcomes {
                        error = error.with_effects(&outcome.mat.result.operations);
                    }
                    return Err(error);
                }
                outcomes.sort_by_key(|o| o.evidence.step_index);
                apply_step_materialize_outcomes(
                    &mut materialized,
                    &mut evidence_steps,
                    &mut scope_instances,
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
                    let node = prepared_nodes
                        .get(step_id)
                        .ok_or_else(|| {
                            ExecutionFailure::new(
                                plasm_runtime::FailureCause::Program,
                                "plan_step_not_prepared",
                                format!("prepared executable node for step `{step_id}` is missing"),
                            )
                        })?
                        .clone();
                    let outcome = Box::pin(materialize_executable_plan_step(
                        &mat_ctx,
                        step_idx,
                        step_id,
                        node,
                        schema_nodes,
                        &materialized,
                    ))
                    .await?;
                    apply_step_materialize_outcomes(
                        &mut materialized,
                        &mut evidence_steps,
                        &mut scope_instances,
                        &mut approval_receipts,
                        [outcome],
                        execution_scope,
                    );
                }
            }
        }
        if let Some(evidence) = active_chain(es, execution_scope) {
            evidence.record_steps_executed(&evidence_steps)?;
        }

        let return_node_ids = plasm_return_node_ids(&executable.return_).map_err(|error| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Program,
                "plan_return_invalid",
                error.to_string(),
            )
        })?;
        let mut steps = Vec::new();
        let return_names = plasm_return_names(&executable.return_);
        for (i, node_ref) in return_node_ids.iter().enumerate() {
            let mat = materialized.get(node_ref).ok_or_else(|| {
                ExecutionFailure::new(
                    plasm_runtime::FailureCause::Program,
                    "plan_return_node_not_materialized",
                    format!("returned node `{}` was not materialized", node_ref.as_str()),
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
                .await
                .map_err(|diagnostic| {
                    ExecutionFailure::new(
                        plasm_runtime::FailureCause::Runtime,
                        "plan_run_seal_failed",
                        diagnostic.to_string(),
                    )
                })?;
                if let Some(evidence) = active_chain(es, execution_scope) {
                    evidence.record_run_sealed(&seal)?;
                }
            }
            steps.push(PublishedResultStep {
                name: return_names.get(i).cloned().flatten(),
                node_id: Some(node_ref.as_str().to_string()),
                entry_id: Some(mat.qualified_entity.entry_id.clone()),
                entity: Some(mat.qualified_entity.entity.clone()),
                cgs: es
                    .contexts_by_entry
                    .get(&mat.qualified_entity.entry_id)
                    .map(|ctx| ctx.cgs.clone()),
                display: mat.display.clone(),
                projection: mat.projection.clone(),
                result: Arc::clone(&mat.result),
                artifact: mat.artifact.clone(),
            });
        }
        // Returning a row does not hide effects executed by other plan nodes.
        // Keep their receipts separate from the requested row projection.
        for (node_ref, mat) in &materialized {
            if return_node_ids.contains(node_ref) || mat.result.operations.is_empty() {
                continue;
            }
            let mut receipt = mat.result.as_ref().clone();
            receipt.collection = plasm_runtime::execution::ExecutionCollection::derive(
                receipt
                    .collection
                    .membership()
                    .identity()
                    .derived(&"receipt_only")?,
                &[&receipt.collection],
                plasm_core::collection_codec::Transform::Take(0),
                plasm_runtime::execution::PayloadResidency::Materialized(vec![].into()),
            )?;
            receipt.has_more = false;
            receipt.pagination_resume = None;
            receipt.paging_handle = None;
            steps.push(PublishedResultStep {
                name: None,
                node_id: Some(node_ref.as_str().to_string()),
                entry_id: Some(mat.qualified_entity.entry_id.clone()),
                entity: Some(mat.qualified_entity.entity.clone()),
                cgs: es
                    .contexts_by_entry
                    .get(&mat.qualified_entity.entry_id)
                    .map(|ctx| ctx.cgs.clone()),
                display: mat.display.clone(),
                projection: None,
                result: Arc::new(receipt),
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
        )
        .map_err(|diagnostic| {
            ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "plan_result_publication_failed",
                diagnostic.to_string(),
            )
        })?;
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
            if let Some(bundle) = evidence.finish_bundle()? {
                evidence_head_hex = bundle.chain.head.map(|h| h.to_hex());
                persist_evidence_sidecars(
                    &st.run_artifacts,
                    prompt_hash,
                    session_id,
                    &evidence_run_ids,
                    &bundle,
                )
                .await?;
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
        dry.graph_summary["scope_instances"] = serde_json::json!(scope_instances);
        Ok(PlasmPlanRunResult {
            version: dry.version,
            agent_outcome: Default::default(),
            node_results,
            graph_summary: graph_summary_with_approval_receipts(
                dry.graph_summary,
                &approval_receipts,
            ),
            comp: Some(comp),
            code_plan_run_artifacts,
            run_markdown: Some(run_markdown),
            run_plasm_meta,
            return_steps: steps,
            inline_plan_ui: None,
        })
    }
    .await;
    execution.map_err(|failure| error_with_completed_operations(failure, &materialized))
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

fn plasm_return_node_ids(
    ret: &PlasmReturn,
) -> Result<Vec<PlanNodeId>, crate::plasm_plan::PlanAtomError> {
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

/// Preserve acknowledgments from completed steps when a later step aborts the plan.
/// These are confirmed effects, not a claim that the failing step had no effects.
fn error_with_completed_operations(
    mut error: ExecutionFailure,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> ExecutionFailure {
    for value in materialized.values() {
        error = error.with_effects(&value.result.operations);
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node(display: &str) -> MaterializedNode {
        MaterializedNode {
            value_shapes: Vec::new(),
            optional_fields: Default::default(),
            qualified_entity: crate::plasm_plan::QualifiedEntityKey {
                entry_id: "test".into(),
                entity: "Item".into(),
            },
            result: Arc::new(ExecutionResult {
                collection: crate::test_support::execution_fixtures::collection(
                    Vec::new(),
                    plasm_runtime::ResultCoverage::Unknown,
                ),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: Vec::new(),
                operations: plasm_runtime::OperationLedger::empty(),
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
