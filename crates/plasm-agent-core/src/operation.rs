//! Long-running async plan operations (`l_<token>_oN` MCP / plain `oN` HTTP) and dry-run plan commit tokens (`pcN`).

use crate::execute_session::ExecuteSession;
use crate::operation_progress::{
    op_plasm_meta_short, render_op_wire_line, render_op_wire_markdown, OpWireSig,
};
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plan_flow::{FlowAdmission, FlowDenial};
use crate::plan_flow_policy::PolicyRevision;
use crate::plasm_plan_run::{DryPlasmPlanEvaluation, PlanRunTraceHooks, PlasmPlanRunResult};
use crate::server_state::PlasmHostState;
use plasm_core::{OperationHandle, PlanCommitId, PlanCommitRef};
use plasm_runtime::CancelSignal;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static PLAN_EXECUTE_CANCEL: Option<CancelSignal>;
}

/// Scope surface-node HTTP execute with the async plan's cooperative cancel signal.
pub async fn with_plan_execute_cancel<Fut, T>(cancel: Option<CancelSignal>, fut: Fut) -> T
where
    Fut: std::future::Future<Output = T>,
{
    PLAN_EXECUTE_CANCEL.scope(cancel, fut).await
}

pub async fn with_plan_execute_scope<Fut, T>(scope: Option<&ExecutionScope>, fut: Fut) -> T
where
    Fut: std::future::Future<Output = T>,
{
    let cancel = scope.map(|s| s.cancel.clone());
    with_plan_execute_cancel(cancel, fut).await
}

pub(crate) fn plan_execute_cancel_signal() -> Option<CancelSignal> {
    PLAN_EXECUTE_CANCEL.try_with(|c| c.clone()).ok().flatten()
}

/// Persisted plan commit payload before flow re-verify on rehydrate.
#[derive(Debug, Clone)]
pub struct RehydratedPlanCommit {
    pub commit_ref: PlanCommitRef,
    pub commit_id: PlanCommitId,
    pub domain_revision: u32,
    pub policy_revision: PolicyRevision,
    pub artifact: crate::plasm_comp_wire::PlasmCompArtifact,
    pub program: String,
    pub dry_review: PlanDryReview,
    pub verdict: PlanDryVerdict,
    pub expires_at: Instant,
    pub dry_cache: PlanCommitDryCache,
}

pub fn verify_plan_commit_for_run(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    plan_json: &serde_json::Value,
) -> Result<(), String> {
    crate::plan_commit_store::verify_plan_commit_id(
        es,
        commit_ref,
        compute_plan_commit_id(plan_json),
    )
    .map_err(|e| e.detail())
}

/// Verify a plan acceptance token against the compiled comp (stable across dry/live paths).
pub fn verify_plan_commit_for_comp(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    comp: &plasm_core::PlasmComp,
) -> Result<(), String> {
    crate::plan_commit_store::verify_plan_commit_id(
        es,
        commit_ref,
        compute_plan_commit_id_from_semantic(&plan_commit_canonical_comp(comp)),
    )
    .map_err(|e| e.detail())
}

/// Verify a plan acceptance token against a dry-run evaluation without building presentation DAG fields.
pub fn verify_plan_commit_for_dry(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    dry: &DryPlasmPlanEvaluation,
) -> Result<(), String> {
    verify_plan_commit_for_comp(es, commit_ref, &dry.artifact().comp)
}
pub const PLAN_COMMIT_TTL: Duration = Duration::from_secs(600);

/// Cooperative cancellation + optional progress sink for phased plan execution.
#[derive(Clone)]
pub struct ExecutionScope {
    pub cancel: CancelSignal,
    token: CancellationToken,
    progress: Option<Arc<StdMutex<OperationProgress>>>,
    operation_sink: Option<(Arc<ExecuteSession>, OperationHandle)>,
    /// Isolated evidence chain for this async live run (CEP-13 concurrent `plasm_run`).
    pub evidence: Option<Arc<crate::evidence_chain::EvidenceChainSession>>,
}

impl ExecutionScope {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancel: CancelSignal::new(),
            token: CancellationToken::new(),
            progress: None,
            operation_sink: None,
            evidence: None,
        }
    }

    #[must_use]
    pub fn for_async_operation(
        es: Arc<ExecuteSession>,
        handle: OperationHandle,
        cancel: CancelSignal,
    ) -> Self {
        Self {
            cancel,
            token: CancellationToken::new(),
            progress: Some(Arc::new(StdMutex::new(OperationProgress::default()))),
            operation_sink: Some((es, handle)),
            evidence: None,
        }
    }

    pub fn check(&self) -> Result<(), String> {
        if self.cancel.is_cancelled() || self.token.is_cancelled() {
            return Err("operation cancelled".to_string());
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
        self.token.cancel();
    }

    pub fn set_progress(&self, step: u32, step_total: u32, label: Option<String>) {
        let progress = OperationProgress {
            step,
            step_total,
            label: label.clone(),
            rows_materialized: self
                .progress
                .as_ref()
                .and_then(|p| p.lock().ok())
                .map(|g| g.rows_materialized)
                .unwrap_or(0),
        };
        if let Some(p) = &self.progress {
            if let Ok(mut g) = p.lock() {
                *g = progress.clone();
            }
        }
        self.report_progress(progress);
    }

    pub fn add_rows_materialized(&self, rows: usize) {
        if rows == 0 {
            return;
        }
        let progress = if let Some(p) = &self.progress {
            p.lock()
                .ok()
                .map(|mut g| {
                    g.rows_materialized = g.rows_materialized.saturating_add(rows as u64);
                    g.clone()
                })
                .unwrap_or_default()
        } else {
            return;
        };
        self.report_progress(progress);
    }

    /// Set row count to `rows` (final sync after pagination; avoids double-counting incremental progress).
    pub fn sync_rows_materialized(&self, rows: usize) {
        let progress = if let Some(p) = &self.progress {
            p.lock()
                .ok()
                .map(|mut g| {
                    g.rows_materialized = rows as u64;
                    g.clone()
                })
                .unwrap_or_default()
        } else {
            return;
        };
        self.report_progress(progress);
    }

    fn report_progress(&self, progress: OperationProgress) {
        if let Some((es, handle)) = &self.operation_sink {
            es.update_operation_progress(handle, progress.clone());
        }
    }

    #[must_use]
    pub fn rows_progress_fn(&self) -> Option<plasm_runtime::RowsProgressFn> {
        self.progress.as_ref()?;
        let scope = self.clone();
        Some(std::sync::Arc::new(move |n: usize| {
            scope.add_rows_materialized(n)
        }))
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Default for ExecutionScope {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ExecutionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionScope")
            .field("cancelled", &self.cancel.is_cancelled())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationPhase {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationProgress {
    pub step: u32,
    pub step_total: u32,
    pub label: Option<String>,
    pub rows_materialized: u64,
}

#[derive(Debug, Clone)]
pub struct OperationState {
    pub phase: OperationPhase,
    pub cancel: CancelSignal,
    pub started_at: Instant,
    pub progress: OperationProgress,
    pub result: Option<Arc<PlasmPlanRunResult>>,
    pub error: Option<String>,
    /// When false, this row was rehydrated from Redis (no local executor).
    pub live_executor: bool,
    /// Durable run snapshot id for cross-pod terminal `wait`.
    pub run_artifact_id: Option<String>,
    pub agent_emit: crate::operation_progress::OperationAgentEmitState,
    pub display_map: HashMap<String, String>,
    pub plan_commit_ref: Option<PlanCommitRef>,
    pub dry_verdict: Option<PlanDryVerdict>,
    pub auto_async: bool,
    pub mcp_transport_key: Option<String>,
    pub progress_host: Option<std::sync::Weak<PlasmHostState>>,
    pub progress_tx: tokio::sync::broadcast::Sender<crate::operation_progress::OpProgressEvent>,
    /// Wakes server-side `await_operation_terminal` when phase becomes terminal.
    pub terminal_tx: Option<tokio::sync::watch::Sender<OperationPhase>>,
    pub comp: Option<plasm_trace::TraceCompWire>,
    pub plan_ux_reflection: Option<serde_json::Value>,
    pub step_order: Vec<String>,
}

/// Context captured when an async live run is accepted (accept line + push routing).
#[derive(Clone, Default)]
pub struct OpAcceptContext {
    pub plan_commit_ref: Option<PlanCommitRef>,
    pub dry_verdict: Option<PlanDryVerdict>,
    pub auto_async: bool,
    pub mcp_transport_key: Option<String>,
    pub display_map: HashMap<String, String>,
    pub host: Option<std::sync::Weak<PlasmHostState>>,
    pub comp: Option<plasm_trace::TraceCompWire>,
    pub plan_ux_reflection: Option<serde_json::Value>,
    pub step_order: Vec<String>,
    pub plan_trace: Option<crate::trace_hub::PlanRunTraceHooks>,
    pub mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
    pub evidence_anchors: plasm_evidence::EvidenceAnchors,
}

/// Narrow poll snapshot for `wait(...)` — avoids cloning cancel signals and full operation state.
#[derive(Debug, Clone)]
pub enum OperationPollSnapshot {
    Running(OperationProgress),
    Succeeded(Arc<PlasmPlanRunResult>),
    Failed(String),
    Cancelled(OperationProgress),
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PlanCommitDryCache {
    #[serde(default)]
    pub version: serde_json::Value,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub topological_order: Vec<String>,
    #[serde(default)]
    pub node_results: Vec<serde_json::Value>,
    #[serde(default)]
    pub graph_summary: serde_json::Value,
    #[serde(default)]
    pub parallel_root_surfaces_only: bool,
    #[serde(default)]
    pub staged_nodes: Vec<String>,
    #[serde(default)]
    pub execution_unsupported: Vec<String>,
    #[serde(default)]
    pub lowered_ir_digest: String,
}

impl PlanCommitDryCache {
    pub fn from_dry(dry: &crate::plasm_plan_run::DryPlasmPlanEvaluation) -> Self {
        let lowered_ir_digest =
            crate::plasm_plan_run::lowered_ir_digest_from_validated_plan(dry.validated_plan())
                .as_str()
                .to_string();
        Self {
            version: dry.version.clone(),
            name: dry.name.clone(),
            topological_order: dry.topological_order.clone(),
            node_results: dry.node_results.clone(),
            graph_summary: dry.graph_summary.clone(),
            parallel_root_surfaces_only: dry.parallel_root_surfaces_only,
            staged_nodes: dry.staged_nodes.clone(),
            execution_unsupported: dry.execution_unsupported.clone(),
            lowered_ir_digest,
        }
    }

    #[must_use]
    pub fn is_populated(&self) -> bool {
        !self.topological_order.is_empty() || !self.node_results.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct PlanCommitRecord {
    pub commit_ref: PlanCommitRef,
    pub commit_id: PlanCommitId,
    /// Pinned [`ExecuteSession::domain_revision`] at dry-run registration (CEP-13 stale `pcN` guard).
    pub domain_revision: u32,
    pub policy_revision: PolicyRevision,
    pub artifact: crate::plasm_comp_wire::PlasmCompArtifact,
    pub program: String,
    pub dry_review: PlanDryReview,
    pub verdict: PlanDryVerdict,
    pub expires_at: Instant,
    pub dry_cache: PlanCommitDryCache,
    flow: FlowAdmission,
}

impl PlanCommitRecord {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    pub(crate) fn flow_admission(&self) -> &FlowAdmission {
        &self.flow
    }

    /// Register a reviewed dry-run as a durable plan commit (single construction site).
    pub fn from_dry_review(
        commit_ref: PlanCommitRef,
        commit_id: PlanCommitId,
        domain_revision: u32,
        dry: &DryPlasmPlanEvaluation,
        program: String,
        verdict: PlanDryVerdict,
        expires_at: Instant,
    ) -> Result<Self, FlowDenial> {
        let flow = dry.admit_for_commit()?;
        let policy_revision = flow.policy_revision.unwrap_or_default();
        Ok(Self {
            commit_ref,
            commit_id,
            domain_revision,
            policy_revision,
            artifact: dry.artifact().clone(),
            program,
            dry_review: dry.review.clone(),
            verdict,
            expires_at,
            dry_cache: PlanCommitDryCache::from_dry(dry),
            flow,
        })
    }

    pub fn rehydrated_from_persisted(
        es: &ExecuteSession,
        input: RehydratedPlanCommit,
    ) -> Result<Self, String> {
        let RehydratedPlanCommit {
            commit_ref,
            commit_id,
            domain_revision,
            policy_revision,
            artifact,
            program,
            dry_review,
            verdict,
            expires_at,
            dry_cache,
        } = input;
        let bundle = crate::plasm_comp_bundle::PlasmCompBundle::new(artifact.clone())
            .map_err(|e| format!("invalid rehydrated comp: {e}"))?;
        let executable = bundle.executable();
        let prepared =
            crate::plan_prepare::build_prepared_validated_plan(&artifact.comp, executable)
                .map_err(|e| format!("rehydrated plan validation failed: {e}"))?;
        let topological_order = if dry_cache.topological_order.is_empty() {
            executable
                .steps_topo
                .iter()
                .map(|(id, _)| id.as_str().to_string())
                .collect()
        } else {
            dry_cache.topological_order.clone()
        };
        let catalog = es.build_flow_catalog_view();
        let checked = crate::plan_flow::verify_plan_flow(
            prepared.artifact(),
            &topological_order,
            &catalog,
            &es.flow_policy,
        );
        let flow = checked.admit().map_err(|denial| {
            format!(
                "rehydrated plan flow denied ({:?}, {} violation(s))",
                denial.verdict,
                denial.violations.len()
            )
        })?;
        if flow.policy_revision.unwrap_or_default() != policy_revision {
            return Err(format!(
                "rehydrated plan policy revision mismatch (stored {}, current {:?})",
                policy_revision.0,
                flow.policy_revision.map(|r| r.0)
            ));
        }
        Ok(Self {
            commit_ref,
            commit_id,
            domain_revision,
            policy_revision,
            artifact,
            program,
            dry_review,
            verdict,
            expires_at,
            dry_cache,
            flow,
        })
    }

    #[cfg(test)]
    pub fn for_tests(
        commit_ref: PlanCommitRef,
        commit_id: PlanCommitId,
        artifact: crate::plasm_comp_wire::PlasmCompArtifact,
        verdict: PlanDryVerdict,
    ) -> Self {
        Self {
            commit_ref,
            commit_id,
            domain_revision: 0,
            policy_revision: PolicyRevision::default(),
            artifact,
            program: String::new(),
            dry_review: PlanDryReview::default(),
            verdict,
            expires_at: Instant::now() + PLAN_COMMIT_TTL,
            dry_cache: PlanCommitDryCache::default(),
            flow: FlowAdmission::for_tests(),
        }
    }
}

/// Semantic comp payload for commit-id hashing — strips session-local volatile fields.
pub fn plan_commit_canonical_comp_json(comp: &serde_json::Value) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for key in ["version", "steps", "bind", "return"] {
        if let Some(v) = comp.get(key) {
            obj.insert(key.to_string(), v.clone());
        }
    }
    serde_json::Value::Object(obj)
}

/// Same as [`plan_commit_canonical_comp_json`] for typed comp.
pub fn plan_commit_canonical_comp(comp: &plasm_core::PlasmComp) -> serde_json::Value {
    plasm_core::plasm_comp_commit_canonical(comp)
}

/// Content-addressed id for a validated plan comp artifact.
pub fn compute_plan_commit_id(comp_json: &serde_json::Value) -> PlanCommitId {
    compute_plan_commit_id_from_semantic(&plan_commit_canonical_comp_json(comp_json))
}

/// Hash the semantic DAG payload directly (no volatile `name` / `summary` fields).
pub fn compute_plan_commit_id_from_semantic(semantic: &serde_json::Value) -> PlanCommitId {
    let canonical_str = serde_json::to_string(semantic).unwrap_or_default();
    let digest = Sha256::digest(canonical_str.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    PlanCommitId::from_canonical_bytes(bytes)
}

/// Content-addressed id from a dry-run evaluation (semantic comp bind graph).
pub fn compute_plan_commit_id_from_dry(dry: &DryPlasmPlanEvaluation) -> PlanCommitId {
    use crate::plasm_comp_wire::plasm_comp_commit_canonical;
    compute_plan_commit_id_from_semantic(&plasm_comp_commit_canonical(&dry.artifact().comp))
}

/// Build accept metadata for async plan runs (display map + optional MCP transport).
pub(crate) fn op_accept_context_from_executable(
    plan_commit_ref: Option<PlanCommitRef>,
    dry_verdict: Option<PlanDryVerdict>,
    auto_async: bool,
    mcp_transport_key: Option<String>,
    executable: &crate::plasm_comp_lift::ExecutablePlasmComp,
    comp: &plasm_core::PlasmComp,
) -> OpAcceptContext {
    let order: Vec<String> = executable
        .steps_topo
        .iter()
        .map(|(id, _)| id.as_str().to_string())
        .collect();
    let validated = crate::plan_prepare::build_prepared_validated_plan(comp, executable)
        .expect("executable comp already validated at dry-run");
    let display_map = crate::plan_dry_display::plan_node_display_map(validated.artifact(), &order);
    OpAcceptContext {
        plan_commit_ref,
        dry_verdict,
        auto_async,
        mcp_transport_key,
        display_map,
        host: None,
        comp: None,
        plan_ux_reflection: None,
        step_order: Vec::new(),
        plan_trace: None,
        mcp_result_policy: None,
        evidence_anchors: plasm_evidence::EvidenceAnchors::default(),
    }
}

impl OpAcceptContext {
    pub fn with_mcp_result_policy(
        mut self,
        policy: crate::mcp_run_markdown::McpResultTransportPolicy,
    ) -> Self {
        self.mcp_result_policy = Some(policy);
        self
    }

    pub fn with_plan_trace(mut self, hooks: crate::trace_hub::PlanRunTraceHooks) -> Self {
        self.plan_trace = Some(hooks);
        self
    }

    pub fn with_evidence_anchors(mut self, anchors: plasm_evidence::EvidenceAnchors) -> Self {
        self.evidence_anchors = anchors;
        self
    }

    pub(crate) fn with_run_explorer(
        mut self,
        payload: &crate::run_explorer_meta::RunExplorerAcceptPayload,
    ) -> Self {
        self.comp = Some(payload.comp_wire.clone());
        self.plan_ux_reflection = Some(payload.plan_ux_reflection.clone());
        self.step_order = payload.step_order.clone();
        self
    }
}

pub fn operation_accept_markdown(
    handle: &OperationHandle,
    plan_commit_ref: Option<&PlanCommitRef>,
    dry_verdict: Option<PlanDryVerdict>,
    _auto_async: bool,
) -> String {
    let line = render_op_wire_line(
        handle,
        OpWireSig::Accept,
        None,
        plan_commit_ref,
        dry_verdict,
        None,
    );
    let suffix = if handle.is_plain() {
        crate::operation_progress::http_poll_accept_markdown_suffix(handle)
    } else {
        crate::operation_progress::async_poll_accept_markdown_suffix(handle)
    };
    render_op_wire_markdown(&line) + &suffix
}

pub fn operation_running_markdown(
    handle: &OperationHandle,
    progress: &OperationProgress,
    unchanged: bool,
) -> String {
    let sig = if unchanged {
        OpWireSig::Unchanged
    } else {
        OpWireSig::Running
    };
    let line = render_op_wire_line(handle, sig, Some(progress), None, None, None);
    let suffix = if handle.is_plain() {
        if unchanged {
            crate::operation_progress::http_poll_unchanged_markdown_suffix(handle)
        } else {
            crate::operation_progress::http_poll_progress_markdown_suffix(handle)
        }
    } else if unchanged {
        crate::operation_progress::async_poll_unchanged_markdown_suffix(handle)
    } else {
        crate::operation_progress::async_poll_progress_markdown_suffix(handle)
    };
    render_op_wire_markdown(&line) + &suffix
}

pub fn operation_terminal_markdown(
    handle: &OperationHandle,
    phase: OperationPhase,
    progress: Option<&OperationProgress>,
    error: Option<&str>,
) -> String {
    let sig = match phase {
        OperationPhase::Succeeded => OpWireSig::Done,
        OperationPhase::Cancelled => OpWireSig::Cancelled,
        OperationPhase::Failed => OpWireSig::Failed,
        OperationPhase::Running => OpWireSig::Running,
    };
    let line = render_op_wire_line(handle, sig, progress, None, None, error);
    render_op_wire_markdown(&line)
}

pub fn operation_cancelled_markdown(
    handle: &OperationHandle,
    progress: Option<&OperationProgress>,
) -> String {
    operation_terminal_markdown(handle, OperationPhase::Cancelled, progress, None)
}

pub fn operation_meta_object(
    handle: &OperationHandle,
    sig: OpWireSig,
    seq: u64,
    progress: Option<&OperationProgress>,
    plan_commit_ref: Option<&PlanCommitRef>,
) -> serde_json::Map<String, serde_json::Value> {
    op_plasm_meta_short(handle, sig, seq, progress, plan_commit_ref)
}

/// Markdown + `_meta.plasm` for an async live-run accept (`wait=false` or auto-async).
pub fn async_live_run_accept_parts(
    handle: &OperationHandle,
    plan_commit_ref: Option<&PlanCommitRef>,
    verdict: PlanDryVerdict,
    auto_async: bool,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    let markdown = operation_accept_markdown(handle, plan_commit_ref, Some(verdict), auto_async);
    let mut meta = serde_json::Map::new();
    let mut plasm = operation_meta_object(handle, OpWireSig::Accept, 1, None, plan_commit_ref);
    if auto_async {
        plasm.insert("auto_async".into(), json!(true));
    }
    plasm.insert(
        "dry_verdict".into(),
        json!(match verdict {
            PlanDryVerdict::Ok => "ok",
            PlanDryVerdict::Review => "review",
            PlanDryVerdict::Deny => "deny",
        }),
    );
    meta.insert("plasm".into(), serde_json::Value::Object(plasm));
    (markdown, meta)
}

pub fn plan_commit_meta(
    commit_ref: &PlanCommitRef,
    review: &PlanDryReview,
    verdict: PlanDryVerdict,
) -> serde_json::Map<String, serde_json::Value> {
    let mut dry_review = serde_json::Map::new();
    dry_review.insert(
        "has_unprojected_multi_row_read".into(),
        json!(review.has_unprojected_multi_row_read),
    );
    dry_review.insert(
        "has_unbounded_read_root".into(),
        json!(review.has_unbounded_read_root),
    );
    dry_review.insert(
        "has_full_collection_compute".into(),
        json!(review.has_full_collection_compute),
    );
    dry_review.insert(
        "has_foreach_fanout_risk".into(),
        json!(review.has_foreach_fanout_risk),
    );
    dry_review.insert(
        "has_relation_many_source_fanout".into(),
        json!(review.has_relation_many_source_fanout),
    );
    dry_review.insert(
        "has_query_limit_row_filter".into(),
        json!(review.has_query_limit_row_filter),
    );
    if !review.unused_seeds.is_empty() {
        dry_review.insert("unused_seeds".into(), json!(review.unused_seeds));
    }
    let mut plasm = serde_json::Map::new();
    plasm.insert("run_ref".into(), json!(commit_ref.as_str()));
    plasm.insert(
        "dry_verdict".into(),
        json!(match verdict {
            PlanDryVerdict::Ok => "ok",
            PlanDryVerdict::Review => "review",
            PlanDryVerdict::Deny => "deny",
        }),
    );
    plasm.insert("dry_review".into(), serde_json::Value::Object(dry_review));
    plasm
}

/// Resolve operation handle namespace for MCP (`l_<token>_oM`) vs HTTP (`oM` only).
pub fn resolve_operation_storage_handle(
    trace: Option<&crate::trace_sink_emit::PlasmTraceContext>,
    handle: &OperationHandle,
) -> Result<OperationHandle, String> {
    let s = handle.as_str();
    let mcp_ref = trace.and_then(|t| t.logical_session_ref.as_deref());
    let is_ns = handle.is_logical_namespaced();
    match (mcp_ref, is_ns) {
        (Some(r), true) => {
            let slot = handle
                .logical_session_ref()
                .ok_or_else(|| format!("invalid namespaced operation handle `{s}`"))?;
            if slot != r {
                return Err(format!(
                    "operation handle ref `{slot}` does not match current logical_session_ref `{r}`"
                ));
            }
            Ok(handle.clone())
        }
        (Some(r), false) => Err(format!(
            "namespaced logical-session operation required: use `wait({r}_oN)` from the operation result"
        )),
        (None, true) => Err(
            "namespaced operation handles require a logical_session_ref context"
                .to_string(),
        ),
        (None, false) => Ok(handle.clone()),
    }
}

/// True when `program` is a host operation continuation (`wait(h)` / `cancel(h)`), not a Plasm surface program.
#[must_use]
pub fn is_operation_continuation_program(program: &str) -> bool {
    let trimmed = program.trim();
    trimmed.starts_with("wait(") || trimmed.starts_with("cancel(")
}

/// MCP `plasm` (dry-run) must not dispatch live operation continuations.
#[must_use]
pub fn plasm_dry_run_continuation_error(program: &str) -> Option<&'static str> {
    if is_operation_continuation_program(program) {
        Some(
            "plan-only `plasm` cannot run `wait(...)` or `cancel(...)`; MCP live runs await server-side via `plasm_run` with `run_ref`",
        )
    } else {
        None
    }
}

pub(crate) fn try_parse_operation_continuation(
    es: &ExecuteSession,
    program: &str,
    symbol_map_cross_cache: Option<&plasm_core::SymbolMapCrossRequestCache>,
) -> Option<plasm_core::Expr> {
    if !is_operation_continuation_program(program) {
        return None;
    }
    let trimmed = program.trim();
    let stack = crate::plasm_plan_run::session_cgs_layer_stack(es);
    let map = crate::symbol_map_resolve::resolve_session_symbol_map(
        &crate::symbol_map_resolve::SessionSymbolMapContext {
            session: es,
            cross_cache: symbol_map_cross_cache,
        },
    );
    let parsed =
        plasm_core::expr_parser::parse_with_cgs_layers_program(trimmed, &stack, map, None, false)
            .ok()?;
    match parsed.expr {
        plasm_core::Expr::Wait(_) | plasm_core::Expr::Cancel(_) => Some(parsed.expr),
        _ => None,
    }
}

/// Run one live plan on the host worker pool (shared by async spawn + stale-epoch retry).
#[allow(clippy::too_many_arguments)]
async fn run_plasm_comp_on_pool(
    pool: &crate::live_plan_run_worker::LivePlanRunPool,
    es: Arc<ExecuteSession>,
    st: Arc<crate::server_state::PlasmHostState>,
    prompt_hash: String,
    session_id: String,
    bundle: crate::plasm_comp_bundle::PlasmCompBundle,
    scope: ExecutionScope,
    plan_hooks: Option<PlanRunTraceHooks>,
    mcp_result_policy: Option<crate::mcp_run_markdown::McpResultTransportPolicy>,
    telemetry: Arc<plasm_runtime::LiveRunTelemetry>,
    dry: Option<DryPlasmPlanEvaluation>,
) -> Result<PlasmPlanRunResult, String> {
    pool.run(move || async move {
        plasm_runtime::with_live_run_telemetry(telemetry, async move {
            crate::plasm_plan_run::run_plasm_comp(
                es.as_ref(),
                st.as_ref(),
                prompt_hash.as_str(),
                session_id.as_str(),
                &bundle,
                true,
                plan_hooks,
                Some(&scope),
                dry,
                mcp_result_policy,
            )
            .await
        })
        .await
    })
    .await
}

/// Start a background live plan run; poll with `wait(handle)` / cancel with `cancel(handle)`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_async_plan_run(
    es: Arc<ExecuteSession>,
    st: Arc<crate::server_state::PlasmHostState>,
    prompt_hash: String,
    session_id: String,
    bundle: crate::plasm_comp_bundle::PlasmCompBundle,
    handle: OperationHandle,
    cancel: CancelSignal,
    accept: OpAcceptContext,
    dry: Option<crate::plasm_plan_run::DryPlasmPlanEvaluation>,
) -> Result<(), String> {
    let mut accept = accept;
    accept.host = Some(Arc::downgrade(&st));
    let plan_hooks = accept.plan_trace.clone();
    let mcp_result_policy = accept.mcp_result_policy;
    es.bind_operation_wire(session_id.as_str());
    es.try_begin_async_operation(handle.clone(), cancel.clone(), accept.clone())?;
    let mut scope = ExecutionScope::for_async_operation(Arc::clone(&es), handle.clone(), cancel);
    if let Some(run_chain) = crate::evidence_chain::start_run_evidence_chain(
        es.as_ref(),
        session_id.as_str(),
        accept.evidence_anchors.clone(),
    )
    .map_err(|e| format!("evidence begin: {e}"))?
    {
        scope.evidence = Some(run_chain);
    }
    es.emit_op_accept(&handle, &st)?;
    let telemetry = Arc::new(plasm_runtime::LiveRunTelemetry::new());
    es.install_live_run_telemetry(Arc::clone(&telemetry));
    let ticker_cancel = tokio_util::sync::CancellationToken::new();
    let ticker = spawn_op_progress_ticker(
        Arc::clone(&es),
        handle.clone(),
        Arc::clone(&st),
        ticker_cancel.clone(),
    );
    let scope_for_run = scope.clone();
    let es_run = Arc::clone(&es);
    let st_run = Arc::clone(&st);
    tokio::spawn(async move {
        // Write-conflict retry lives at the branch level (`run_with_write_conflict_retry`), which
        // re-forks only the contended line before any commit is visible. Re-running the whole
        // plan here would re-issue already-committed mutating lines, so the plan executes once.
        let pool = st_run.live_plan_pool();
        let result = run_plasm_comp_on_pool(
            pool.as_ref(),
            es_run,
            st_run,
            prompt_hash,
            session_id,
            bundle,
            scope_for_run,
            plan_hooks,
            mcp_result_policy,
            telemetry,
            dry,
        )
        .await;
        ticker_cancel.cancel();
        let _ = ticker.await;
        es.clear_live_run_telemetry();
        match result {
            Ok(out) => {
                let run_artifact_id = out
                    .code_plan_run_artifacts
                    .first()
                    .map(|a| a.run_id.clone());
                es.finalize_operation_succeeded_with_artifact(
                    &handle,
                    out,
                    run_artifact_id,
                    Some(st.as_ref()),
                );
            }
            Err(e) => {
                if scope.cancel.is_cancelled() {
                    es.cancel_operation(&handle, Some(st.as_ref()));
                } else {
                    es.finalize_operation_failed(&handle, e, Some(st.as_ref()));
                }
            }
        }
    });
    Ok(())
}

/// Periodic MCP op notifications while a live run is in flight (HTTP telemetry coalesce).
pub fn spawn_op_progress_ticker(
    es: Arc<ExecuteSession>,
    handle: OperationHandle,
    st: Arc<crate::server_state::PlasmHostState>,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = interval.tick() => {
                    es.try_emit_op_running_coalesced(&handle, Some(st.as_ref()));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_gate::{plan_requires_review_gate, EvaluatedPlanGate, PlanGateContext};
    use plasm_core::PlanCommitRef;

    #[test]
    fn is_operation_continuation_program_detects_wait_and_cancel() {
        assert!(is_operation_continuation_program("wait(l_x_o1)"));
        assert!(is_operation_continuation_program("  cancel(l_x_o1)"));
        assert!(!is_operation_continuation_program("e1"));
        assert!(!is_operation_continuation_program("page(l_x_pg1)"));
    }

    #[test]
    fn operation_accept_markdown_notes_server_await() {
        let handle = OperationHandle::mint_namespaced("l_AAAAAAAAQACAAAAAAAAAAQ", 1);
        let md = operation_accept_markdown(&handle, None, None, true);
        assert!(md.contains("l_AAAAAAAAQACAAAAAAAAAAQ_o1"));
        assert!(md.contains("awaits server-side"));
        assert!(md.contains("do not poll"));
    }

    #[test]
    fn operation_running_markdown_unchanged_notes_server_await() {
        let handle = OperationHandle::mint_namespaced("l_AAAAAAAAQACAAAAAAAAAAQ", 2);
        let progress = OperationProgress {
            step: 1,
            step_total: 5,
            label: Some("items".into()),
            rows_materialized: 120,
        };
        let md = operation_running_markdown(&handle, &progress, true);
        assert!(md.contains('='));
        assert!(md.contains("items"));
        assert!(md.contains("awaits server-side"));
    }

    #[test]
    fn sync_rows_materialized_replaces_not_accumulates() {
        let progress = Arc::new(StdMutex::new(OperationProgress::default()));
        let scope = ExecutionScope {
            cancel: CancelSignal::new(),
            token: CancellationToken::new(),
            progress: Some(Arc::clone(&progress)),
            operation_sink: None,
            evidence: None,
        };
        scope.add_rows_materialized(50);
        scope.add_rows_materialized(50);
        assert_eq!(progress.lock().unwrap().rows_materialized, 100);
        scope.sync_rows_materialized(42);
        assert_eq!(progress.lock().unwrap().rows_materialized, 42);
    }

    #[test]
    fn plan_requires_review_gate_blocks_without_force_or_commit() {
        let gate = EvaluatedPlanGate {
            verdict: PlanDryVerdict::Review,
            admission: Ok(FlowAdmission::for_tests()),
        };
        assert!(plan_requires_review_gate(
            &gate,
            PlanGateContext {
                force: false,
                plan_commit_ref: None,
            }
        ));
        assert!(!plan_requires_review_gate(
            &gate,
            PlanGateContext {
                force: true,
                plan_commit_ref: None,
            }
        ));
        assert!(!plan_requires_review_gate(
            &gate,
            PlanGateContext {
                force: false,
                plan_commit_ref: Some(&PlanCommitRef::mint(0)),
            }
        ));
        let ok_gate = EvaluatedPlanGate {
            verdict: PlanDryVerdict::Ok,
            admission: Ok(FlowAdmission::for_tests()),
        };
        assert!(!plan_requires_review_gate(
            &ok_gate,
            PlanGateContext {
                force: false,
                plan_commit_ref: None,
            }
        ));
    }

    #[test]
    fn plan_commit_id_ignores_volatile_plan_name_and_summary() {
        let base = json!({
            "version": 1,
            "name": "plasm_comp_call_1",
            "steps": { "n0": { "kind": "invoke" } },
            "bind": { "topo": ["n0"], "deps": {} },
            "return": { "kind": "step", "step": "n0" },
            "summary": { "unused_seeds": ["OtherEntity"] },
        });
        let renamed = json!({
            "version": 1,
            "name": "plasm_comp_call_99",
            "steps": { "n0": { "kind": "invoke" } },
            "bind": { "topo": ["n0"], "deps": {} },
            "return": { "kind": "step", "step": "n0" },
            "summary": { "unused_seeds": [] },
        });
        assert_eq!(
            compute_plan_commit_id(&base),
            compute_plan_commit_id(&renamed)
        );
        let semantic = json!({
            "version": 1,
            "steps": { "n0": { "kind": "invoke" } },
            "bind": { "topo": ["n0"], "deps": {} },
            "return": { "kind": "step", "step": "n0" },
        });
        assert_eq!(
            compute_plan_commit_id(&base),
            compute_plan_commit_id_from_semantic(&semantic)
        );
    }

    #[test]
    fn plan_commit_semantic_comp_hash_benchmark() {
        use std::time::{Duration, Instant};

        let mut steps = serde_json::Map::new();
        let mut topo = Vec::new();
        let mut deps = serde_json::Map::new();
        for i in 0..64 {
            let id = format!("n{i}");
            topo.push(json!(id));
            steps.insert(
                id.clone(),
                json!({
                    "kind": "invoke",
                    "effect_class": "read",
                    "operation": "query Query(LangItem all)",
                }),
            );
            if i > 0 {
                deps.insert(id, json!([format!("n{}", i - 1)]));
            }
        }
        let semantic = json!({
            "version": 1,
            "steps": steps,
            "bind": { "topo": topo, "deps": deps },
            "return": { "kind": "step", "step": "n63" },
        });
        let presentation = json!({
            "version": 1,
            "name": "plasm_comp_call_1",
            "steps": semantic["steps"].clone(),
            "bind": semantic["bind"].clone(),
            "return": semantic["return"].clone(),
            "summary": { "unused_seeds": ["OtherEntity"] },
        });

        let _ = compute_plan_commit_id_from_semantic(&semantic);
        let _ = compute_plan_commit_id(&presentation);

        let start = Instant::now();
        for _ in 0..100 {
            let _ = compute_plan_commit_id_from_semantic(&semantic);
            let _ = compute_plan_commit_id(&presentation);
        }
        let elapsed = start.elapsed();
        let cap = Duration::from_millis(
            std::env::var("PLASM_PLAN_COMMIT_DAG_HASH_MAX_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(750),
        );
        assert!(
            elapsed < cap,
            "plan commit hash (100 iter, 64 steps) took {:?}, cap {:?}",
            elapsed,
            cap
        );
    }
}
