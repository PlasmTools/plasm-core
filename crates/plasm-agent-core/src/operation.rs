//! Long-running async plan operations (`l_<token>_oN` MCP / plain `oN` HTTP) and dry-run plan commit tokens (`pcN`).

use crate::execute_session::ExecuteSession;
use crate::operation_progress::{
    async_poll_accept_markdown_suffix, async_poll_progress_markdown_suffix,
    async_poll_unchanged_markdown_suffix, op_plasm_meta_short, render_op_wire_line,
    render_op_wire_markdown, OpWireSig,
};
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plasm_plan_run::{DryPlasmPlanEvaluation, PlasmPlanRunResult};
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

/// Returns true when live execute must be blocked pending dry-run review acceptance.
#[must_use]
pub fn plan_requires_review_gate(
    verdict: PlanDryVerdict,
    force: bool,
    plan_commit_ref: Option<&PlanCommitRef>,
) -> bool {
    verdict == PlanDryVerdict::Review && !force && plan_commit_ref.is_none()
}

/// Review/unbounded plans must not block MCP/HTTP handlers when `wait` defaults to true.
#[must_use]
pub fn live_run_should_auto_async(review: &PlanDryReview, wait_live: bool) -> bool {
    wait_live && review.execution_is_expensive()
}

/// Whether live execute should spawn a background operation (explicit `wait=false` or auto-async).
#[must_use]
pub fn should_spawn_async_live_run(wait_live: bool, review: &PlanDryReview) -> bool {
    !wait_live || live_run_should_auto_async(review, wait_live)
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

/// Verify a plan acceptance token against a dry-run evaluation without building presentation DAG fields.
pub fn verify_plan_commit_for_dry(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    dry: &DryPlasmPlanEvaluation,
) -> Result<(), String> {
    crate::plan_commit_store::verify_plan_commit_id(
        es,
        commit_ref,
        compute_plan_commit_id_from_dry(dry),
    )
    .map_err(|e| e.detail())
}
pub const PLAN_COMMIT_TTL: Duration = Duration::from_secs(600);

/// Cooperative cancellation + optional progress sink for phased plan execution.
#[derive(Clone)]
pub struct ExecutionScope {
    pub cancel: CancelSignal,
    token: CancellationToken,
    progress: Option<Arc<StdMutex<OperationProgress>>>,
    operation_sink: Option<(Arc<ExecuteSession>, OperationHandle)>,
}

impl ExecutionScope {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancel: CancelSignal::new(),
            token: CancellationToken::new(),
            progress: None,
            operation_sink: None,
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
        if let Some((es, handle)) = &self.operation_sink {
            es.update_operation_progress(handle, progress);
        }
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
        if let Some((es, handle)) = &self.operation_sink {
            es.update_operation_progress(handle, progress);
        }
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
        if let Some((es, handle)) = &self.operation_sink {
            es.update_operation_progress(handle, progress);
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
    pub comp: Option<serde_json::Value>,
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
    pub comp: Option<serde_json::Value>,
    pub plan_ux_reflection: Option<serde_json::Value>,
    pub step_order: Vec<String>,
}

/// Narrow poll snapshot for `wait(...)` — avoids cloning cancel signals and full operation state.
#[derive(Debug, Clone)]
pub enum OperationPollSnapshot {
    Running(OperationProgress),
    Succeeded(Arc<PlasmPlanRunResult>),
    Failed(String),
    Cancelled(OperationProgress),
}

#[derive(Debug, Clone)]
pub struct PlanCommitRecord {
    pub commit_ref: PlanCommitRef,
    pub commit_id: PlanCommitId,
    pub dry_review: PlanDryReview,
    pub verdict: PlanDryVerdict,
    pub expires_at: Instant,
}

impl PlanCommitRecord {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
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
    let validated =
        crate::plasm_step_convert::build_validated_plan_from_executable(comp, executable)
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
    render_op_wire_markdown(&line) + &async_poll_accept_markdown_suffix(handle)
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
    let suffix = if unchanged {
        async_poll_unchanged_markdown_suffix(handle)
    } else {
        async_poll_progress_markdown_suffix(handle)
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
    plasm.insert("plan_commit_ref".into(), json!(commit_ref.as_str()));
    plasm.insert(
        "dry_verdict".into(),
        json!(match verdict {
            PlanDryVerdict::Ok => "ok",
            PlanDryVerdict::Review => "review",
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
            "MCP requires namespaced operations: use `wait({r}_oN)` from the tool result"
        )),
        (None, true) => Err(
            "namespaced operation handles are only for MCP `plasm_run` with `plasm_context`"
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
            "plan-only `plasm` cannot run `wait(...)` or `cancel(...)` — use `plasm_run` with the same `logical_session_ref` and program",
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
    let layers = crate::plasm_plan_run::session_cgs_layers(es);
    let map = crate::symbol_map_resolve::resolve_session_symbol_map(
        &crate::symbol_map_resolve::SessionSymbolMapContext {
            session: es,
            cross_cache: symbol_map_cross_cache,
        },
    );
    let parsed =
        plasm_core::expr_parser::parse_with_cgs_layers_program(trimmed, &layers, map, None, false)
            .ok()?;
    match parsed.expr {
        plasm_core::Expr::Wait(_) | plasm_core::Expr::Cancel(_) => Some(parsed.expr),
        _ => None,
    }
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
) -> Result<(), String> {
    let mut accept = accept;
    accept.host = Some(Arc::downgrade(&st));
    es.bind_operation_wire(session_id.as_str());
    es.try_begin_async_operation(handle.clone(), cancel.clone(), accept)?;
    let scope = ExecutionScope::for_async_operation(Arc::clone(&es), handle.clone(), cancel);
    es.emit_op_accept(&handle, &st)?;
    tokio::spawn(async move {
        let result = crate::plasm_plan_run::run_plasm_comp(
            es.as_ref(),
            st.as_ref(),
            prompt_hash.as_str(),
            session_id.as_str(),
            &bundle,
            true,
            None,
            Some(&scope),
        )
        .await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::PlanCommitRef;

    #[test]
    fn is_operation_continuation_program_detects_wait_and_cancel() {
        assert!(is_operation_continuation_program("wait(l_x_o1)"));
        assert!(is_operation_continuation_program("  cancel(l_x_o1)"));
        assert!(!is_operation_continuation_program("e1"));
        assert!(!is_operation_continuation_program("page(l_x_pg1)"));
    }

    #[test]
    fn operation_accept_markdown_includes_poll_discipline() {
        let handle = OperationHandle::mint_namespaced("l_AAAAAAAAQACAAAAAAAAAAQ", 1);
        let md = operation_accept_markdown(&handle, None, None, true);
        assert!(md.contains("wait(l_AAAAAAAAQACAAAAAAAAAAQ_o1)"));
        assert!(md.contains("unrelated live programs"));
        assert!(md.contains("`=`"));
    }

    #[test]
    fn operation_running_markdown_unchanged_includes_keep_polling() {
        let handle = OperationHandle::mint_namespaced("l_AAAAAAAAQACAAAAAAAAAAQ", 2);
        let progress = OperationProgress {
            step: 1,
            step_total: 5,
            label: Some("items".into()),
            rows_materialized: 120,
        };
        let md = operation_running_markdown(&handle, &progress, true);
        assert!(md.contains("Still open"));
        assert!(md.contains("wait(l_AAAAAAAAQACAAAAAAAAAAQ_o2)"));
    }

    #[test]
    fn live_run_should_auto_async_only_for_expensive_review_with_default_wait() {
        let expensive = PlanDryReview {
            has_unbounded_read_root: true,
            has_paginated_list_fetch_all_default: false,
            ..PlanDryReview::default()
        };
        let advisory = PlanDryReview {
            has_full_collection_compute: true,
            ..PlanDryReview::default()
        };
        assert!(live_run_should_auto_async(&expensive, true));
        assert!(!live_run_should_auto_async(&expensive, false));
        assert!(!live_run_should_auto_async(&advisory, true));
        assert!(should_spawn_async_live_run(false, &advisory));
        assert!(should_spawn_async_live_run(true, &expensive));
        assert!(!should_spawn_async_live_run(true, &advisory));
    }

    #[test]
    fn sync_rows_materialized_replaces_not_accumulates() {
        let progress = Arc::new(StdMutex::new(OperationProgress::default()));
        let scope = ExecutionScope {
            cancel: CancelSignal::new(),
            token: CancellationToken::new(),
            progress: Some(Arc::clone(&progress)),
            operation_sink: None,
        };
        scope.add_rows_materialized(50);
        scope.add_rows_materialized(50);
        assert_eq!(progress.lock().unwrap().rows_materialized, 100);
        scope.sync_rows_materialized(42);
        assert_eq!(progress.lock().unwrap().rows_materialized, 42);
    }

    #[test]
    fn plan_requires_review_gate_blocks_without_force_or_commit() {
        assert!(plan_requires_review_gate(
            PlanDryVerdict::Review,
            false,
            None
        ));
        assert!(!plan_requires_review_gate(
            PlanDryVerdict::Review,
            true,
            None
        ));
        assert!(!plan_requires_review_gate(
            PlanDryVerdict::Review,
            false,
            Some(&PlanCommitRef::mint(0))
        ));
        assert!(!plan_requires_review_gate(PlanDryVerdict::Ok, false, None));
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
