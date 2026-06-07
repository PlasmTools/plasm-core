//! Long-running async plan operations (`s0_oN`) and dry-run plan commit tokens (`pcN`).

use crate::execute_session::ExecuteSession;
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plasm_plan_run::{DryPlasmPlanEvaluation, PlasmPlanRunResult, plan_semantic_dag_json};
use plasm_core::{OperationHandle, PlanCommitId, PlanCommitRef};
use plasm_runtime::CancelSignal;
use serde_json::json;
use sha2::{Digest, Sha256};
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

pub fn verify_plan_commit_for_run(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    plan_json: &serde_json::Value,
) -> Result<(), String> {
    verify_plan_commit_id(es, commit_ref, compute_plan_commit_id(plan_json))
}

/// Verify a plan acceptance token against a dry-run evaluation without building presentation DAG fields.
pub fn verify_plan_commit_for_dry(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    dry: &DryPlasmPlanEvaluation,
) -> Result<(), String> {
    verify_plan_commit_id(es, commit_ref, compute_plan_commit_id_from_dry(dry))
}

fn verify_plan_commit_id(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    commit_id: PlanCommitId,
) -> Result<(), String> {
    let Some(record) = es.get_plan_commit(commit_ref) else {
        return Err(format!(
            "unknown or expired plan_commit_ref `{}` — call `plasm` dry-run again",
            commit_ref.as_str()
        ));
    };
    if record.is_expired() {
        return Err(format!(
            "plan_commit_ref `{}` expired — call `plasm` dry-run again",
            commit_ref.as_str()
        ));
    }
    if commit_id != record.commit_id {
        return Err(format!(
            "plan_commit_ref `{}` does not match the current program — call `plasm` dry-run again",
            commit_ref.as_str()
        ));
    }
    Ok(())
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

#[derive(Debug, Clone, Default)]
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

/// Semantic plan DAG payload for commit-id hashing — strips session-local volatile fields
/// (e.g. MCP `plasm_dag_call_{call_count}` plan names, dry-run summary metadata).
pub fn plan_commit_canonical_dag_json(dag: &serde_json::Value) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for key in ["version", "nodes", "edges", "topological_order", "returns"] {
        if let Some(v) = dag.get(key) {
            obj.insert(key.to_string(), v.clone());
        }
    }
    serde_json::Value::Object(obj)
}

/// Content-addressed id for a validated plan DAG artifact.
pub fn compute_plan_commit_id(plan_json: &serde_json::Value) -> PlanCommitId {
    compute_plan_commit_id_from_semantic(&plan_commit_canonical_dag_json(plan_json))
}

/// Hash the semantic DAG payload directly (no volatile `name` / `summary` fields).
pub fn compute_plan_commit_id_from_semantic(semantic: &serde_json::Value) -> PlanCommitId {
    let canonical_str = serde_json::to_string(semantic).unwrap_or_default();
    let digest = Sha256::digest(canonical_str.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    PlanCommitId::from_canonical_bytes(bytes)
}

/// Content-addressed id from a dry-run evaluation (builds semantic DAG once).
pub fn compute_plan_commit_id_from_dry(dry: &DryPlasmPlanEvaluation) -> PlanCommitId {
    compute_plan_commit_id_from_semantic(&plan_semantic_dag_json(dry))
}

pub fn operation_accept_markdown(
    handle: &OperationHandle,
    plan_commit_ref: Option<&PlanCommitRef>,
    dry_verdict: Option<PlanDryVerdict>,
) -> String {
    let verdict = dry_verdict
        .map(|v| match v {
            PlanDryVerdict::Ok => "ok",
            PlanDryVerdict::Review => "review",
        })
        .unwrap_or("ok");
    let pc = plan_commit_ref
        .map(|r| format!(" · plan `{r}`"))
        .unwrap_or_default();
    format!(
        "```text\n`{}` · verdict {verdict}{pc}\nPoll: `plasm_run` with `wait({})` · Cancel: `plasm_run` with `cancel({})`\n```",
        handle.as_str(),
        handle.as_str(),
        handle.as_str()
    )
}

pub fn operation_running_markdown(handle: &OperationHandle, progress: &OperationProgress) -> String {
    let step = if progress.step_total > 0 {
        format!("step {}/{}", progress.step, progress.step_total)
    } else {
        "running".to_string()
    };
    let label = progress
        .label
        .as_deref()
        .map(|l| format!(" · `{l}`"))
        .unwrap_or_default();
    let rows = if progress.rows_materialized > 0 {
        format!(" · {} rows materialized", progress.rows_materialized)
    } else {
        String::new()
    };
    format!(
        "```text\n`{}` · {step}{label}{rows}\n```",
        handle.as_str()
    )
}

pub fn operation_cancelled_markdown(handle: &OperationHandle) -> String {
    format!("```text\n`{}` · cancelled\n```", handle.as_str())
}

pub fn operation_meta_object(
    logical_session_ref: &str,
    handle: &OperationHandle,
    phase: OperationPhase,
    progress: Option<&OperationProgress>,
    plan_commit_ref: Option<&PlanCommitRef>,
    dry_verdict: Option<PlanDryVerdict>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut continuity = serde_json::Map::new();
    continuity.insert(
        "phase".into(),
        json!(match phase {
            OperationPhase::Running => "running",
            OperationPhase::Succeeded => "succeeded",
            OperationPhase::Failed => "failed",
            OperationPhase::Cancelled => "cancelled",
        }),
    );
    continuity.insert(
        "operation_handle".into(),
        json!(handle.as_str()),
    );
    if let Some(pc) = plan_commit_ref {
        continuity.insert("plan_commit_ref".into(), json!(pc.as_str()));
    }
    if let Some(v) = dry_verdict {
        continuity.insert(
            "dry_verdict".into(),
            json!(match v {
                PlanDryVerdict::Ok => "ok",
                PlanDryVerdict::Review => "review",
            }),
        );
    }
    let mut op = serde_json::Map::new();
    if let Some(p) = progress {
        op.insert("step".into(), json!(p.step));
        op.insert("step_total".into(), json!(p.step_total));
        if let Some(ref l) = p.label {
            op.insert("label".into(), json!(l));
        }
        if p.rows_materialized > 0 {
            op.insert("rows_materialized".into(), json!(p.rows_materialized));
        }
    }
    let mut plasm = serde_json::Map::new();
    plasm.insert("logical_session_ref".into(), json!(logical_session_ref));
    plasm.insert("continuity".into(), serde_json::Value::Object(continuity));
    if !op.is_empty() {
        plasm.insert("operation".into(), serde_json::Value::Object(op));
    }
    plasm
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

/// Resolve operation handle namespace for MCP (`sN_oM`) vs HTTP (`oM` only — rejected for MCP).
pub fn resolve_operation_storage_handle(
    trace: Option<&crate::trace_sink_emit::PlasmTraceContext>,
    handle: &OperationHandle,
) -> Result<OperationHandle, String> {
    let s = handle.as_str();
    let mcp_slot = trace.and_then(|t| t.logical_session_ref.as_deref());
    let is_ns = handle.logical_session_ref().is_some();
    match (mcp_slot, is_ns) {
        (Some(r), true) => {
            let slot = handle.logical_session_ref().ok_or_else(|| {
                format!("invalid namespaced operation handle `{s}`")
            })?;
            if slot != r {
                return Err(format!(
                    "operation handle slot `{slot}` does not match current logical_session_ref `{r}`"
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

pub(crate) fn try_parse_operation_continuation(
    es: &ExecuteSession,
    program: &str,
) -> Option<plasm_core::Expr> {
    let trimmed = program.trim();
    if !(trimmed.starts_with("wait(") || trimmed.starts_with("cancel(")) {
        return None;
    }
    let layers = crate::plasm_plan_run::session_cgs_layers(es);
    let map = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(es, None);
    let parsed = plasm_core::expr_parser::parse_with_cgs_layers_program(
        trimmed,
        &layers,
        map,
        None,
        false,
    )
    .ok()?;
    match parsed.expr {
        plasm_core::Expr::Wait(_) | plasm_core::Expr::Cancel(_) => Some(parsed.expr),
        _ => None,
    }
}

/// Start a background live plan run; poll with `wait(handle)` / cancel with `cancel(handle)`.
pub fn spawn_async_plan_run(
    es: Arc<ExecuteSession>,
    st: Arc<crate::server_state::PlasmHostState>,
    prompt_hash: String,
    session_id: String,
    validated: crate::plasm_plan::ValidatedPlan,
    handle: OperationHandle,
    cancel: CancelSignal,
) {
    es.register_operation(
        handle.clone(),
        OperationState {
            phase: OperationPhase::Running,
            cancel: cancel.clone(),
            started_at: Instant::now(),
            progress: OperationProgress::default(),
            result: None,
            error: None,
        },
    );
    let scope = ExecutionScope::for_async_operation(Arc::clone(&es), handle.clone(), cancel);
    tokio::spawn(async move {
        let result = crate::plasm_plan_run::run_validated_plasm_plan(
            es.as_ref(),
            st.as_ref(),
            prompt_hash.as_str(),
            session_id.as_str(),
            &validated,
            true,
            None,
            Some(&scope),
        )
        .await;
        match result {
            Ok(out) => es.finalize_operation_succeeded(&handle, out),
            Err(e) => {
                if scope.cancel.is_cancelled() {
                    es.cancel_operation(&handle);
                } else {
                    es.finalize_operation_failed(&handle, e);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::PlanCommitRef;

    #[test]
    fn plan_requires_review_gate_blocks_without_force_or_commit() {
        assert!(plan_requires_review_gate(PlanDryVerdict::Review, false, None));
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
            "name": "plasm_dag_call_1",
            "nodes": [{"id": "n0", "kind": "surface"}],
            "edges": [],
            "topological_order": ["n0"],
            "returns": ["n0"],
            "summary": { "unused_seeds": ["OtherEntity"] },
        });
        let renamed = json!({
            "version": 1,
            "name": "plasm_dag_call_99",
            "nodes": [{"id": "n0", "kind": "surface"}],
            "edges": [],
            "topological_order": ["n0"],
            "returns": ["n0"],
            "summary": { "unused_seeds": [] },
        });
        assert_eq!(
            compute_plan_commit_id(&base),
            compute_plan_commit_id(&renamed)
        );
        let semantic = json!({
            "version": 1,
            "nodes": [{"id": "n0", "kind": "surface"}],
            "edges": [],
            "topological_order": ["n0"],
            "returns": ["n0"],
        });
        assert_eq!(
            compute_plan_commit_id(&base),
            compute_plan_commit_id_from_semantic(&semantic)
        );
    }

    #[test]
    fn plan_commit_semantic_dag_hash_benchmark() {
        use std::time::{Duration, Instant};

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut order = Vec::new();
        for i in 0..64 {
            let id = format!("n{i}");
            order.push(id.clone());
            nodes.push(json!({
                "id": id,
                "kind": "surface",
                "effect_class": "read",
                "result_shape": "rows",
                "dependencies": if i == 0 { json!([]) } else { json!([format!("n{}", i - 1)]) },
                "uses_result": json!([]),
                "operation": "query Query(LangItem all)",
            }));
            if i > 0 {
                edges.push(json!({ "from": format!("n{}", i - 1), "to": format!("n{i}") }));
            }
        }
        let semantic = json!({
            "version": 1,
            "nodes": nodes,
            "edges": edges,
            "topological_order": order,
            "returns": ["n63"],
        });
        let presentation = json!({
            "version": 1,
            "name": "plasm_dag_call_1",
            "nodes": semantic["nodes"].clone(),
            "edges": semantic["edges"].clone(),
            "topological_order": semantic["topological_order"].clone(),
            "returns": semantic["returns"].clone(),
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
            "plan commit hash (100 iter, 64 nodes) took {:?}, cap {:?}",
            elapsed,
            cap
        );
    }
}
