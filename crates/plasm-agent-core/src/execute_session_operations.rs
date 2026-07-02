//! Cross-pod async operation persistence on [`super::ExecuteSession`] (Redis descriptor slice).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use plasm_core::OperationHandle;

use crate::mcp_transport_store::{
    descriptor_from_operation_state, max_operation_seq, prune_terminal_operations,
    OperationPersistSnapshot, PersistedOperationPhase,
};
use crate::operation_persist::{OperationWireBinding, PersistUrgency};
use crate::plan_dry_display::PlanDryVerdict;
use plasm_core::PlanCommitRef;

impl super::ExecuteSession {
    pub(crate) fn bind_operation_wire(&self, session_id: &str) {
        if let Ok(mut wire) = self.operation_wire.lock() {
            wire.session_id = Some(session_id.to_string());
        }
    }

    pub(crate) fn operation_wire_snapshot(&self) -> OperationWireBinding {
        self.operation_wire
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    pub(crate) fn persist_operation_state(
        &self,
        handle: &OperationHandle,
        urgency: PersistUrgency,
    ) {
        let wire = self.operation_wire_snapshot();
        let Some(session_id) = wire.session_id.as_deref() else {
            return;
        };
        let started_at = wire.started_at(handle);
        let (op, host) = {
            let map = self
                .operation_by_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(op) = map.get(handle) else {
                return;
            };
            (op.clone(), operation_host_from_op(op))
        };
        crate::operation_persist::schedule_op_persist(
            host.as_deref(),
            self.prompt_hash.as_str(),
            session_id,
            handle,
            &op,
            started_at,
            urgency,
        );
    }

    pub(crate) fn snapshot_operations_for_persist(&self) -> OperationPersistSnapshot {
        let map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let wire = self.operation_wire_snapshot();
        let mut operations = map
            .iter()
            .map(|(handle, op)| {
                descriptor_from_operation_state(handle, op, wire.started_at(handle))
            })
            .collect::<Vec<_>>();
        operations = prune_terminal_operations(operations);
        let counter = self.operation_handle_next.load(Ordering::Relaxed);
        let max_seq = max_operation_seq(&operations);
        OperationPersistSnapshot {
            operations,
            operation_handle_next: counter.max(max_seq.saturating_add(1)),
        }
    }

    pub(crate) fn restore_persisted_operations(&self, snap: &OperationPersistSnapshot) {
        let mut map = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut wire = self
            .operation_wire
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for desc in &snap.operations {
            let Ok(handle) = OperationHandle::parse(&desc.handle) else {
                continue;
            };
            wire.started_at_unix_by_handle
                .insert(desc.handle.clone(), desc.started_at_unix);
            let phase = match desc.phase {
                PersistedOperationPhase::Running => crate::operation::OperationPhase::Running,
                PersistedOperationPhase::Succeeded => crate::operation::OperationPhase::Succeeded,
                PersistedOperationPhase::Failed => crate::operation::OperationPhase::Failed,
                PersistedOperationPhase::Cancelled => crate::operation::OperationPhase::Cancelled,
            };
            let dry_verdict = desc.dry_verdict.as_deref().and_then(|v| match v {
                "ok" => Some(PlanDryVerdict::Ok),
                "review" => Some(PlanDryVerdict::Review),
                "deny" => Some(PlanDryVerdict::Deny),
                _ => None,
            });
            let plan_commit_ref = desc
                .plan_commit_ref
                .as_deref()
                .and_then(PlanCommitRef::parse);
            let (progress_tx, _) = tokio::sync::broadcast::channel(64);
            map.insert(
                handle,
                crate::operation::OperationState {
                    phase,
                    cancel: plasm_runtime::CancelSignal::new(),
                    started_at: Instant::now(),
                    progress: desc.progress.clone().into(),
                    result: None,
                    error: desc.error.clone(),
                    live_executor: false,
                    run_artifact_id: desc.run_artifact_id.clone(),
                    agent_emit: crate::operation_progress::OperationAgentEmitState {
                        seq: desc.agent_seq,
                        last_line: desc.agent_last_line.clone(),
                        ..Default::default()
                    },
                    display_map: desc.display_map.clone(),
                    plan_commit_ref,
                    dry_verdict,
                    auto_async: false,
                    mcp_transport_key: None,
                    progress_host: None,
                    progress_tx,
                    terminal_tx: None,
                    comp: None,
                    plan_ux_reflection: None,
                    step_order: Vec::new(),
                },
            );
        }
        let current = self.operation_handle_next.load(Ordering::Relaxed);
        if snap.operation_handle_next > current {
            // Snapshot stores the next seq to assign; mint uses fetch_add(1)+1.
            self.operation_handle_next.store(
                snap.operation_handle_next.saturating_sub(1),
                Ordering::Relaxed,
            );
        }
    }

    pub fn set_operation_run_artifact_id(&self, handle: &OperationHandle, run_artifact_id: String) {
        if let Some(op) = self
            .operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(handle)
        {
            op.run_artifact_id = Some(run_artifact_id);
        }
    }

    pub fn operation_has_live_executor(&self, handle: &OperationHandle) -> bool {
        self.operation_by_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)
            .map(|op| op.live_executor)
            .unwrap_or(false)
    }
}

fn operation_host_from_op(
    op: &crate::operation::OperationState,
) -> Option<Arc<crate::server_state::PlasmHostState>> {
    op.progress_host.as_ref().and_then(|w| w.upgrade())
}

pub(crate) fn running_handles_from_map(
    map: &std::collections::HashMap<OperationHandle, crate::operation::OperationState>,
) -> Vec<OperationHandle> {
    map.iter()
        .filter(|(_, op)| op.phase == crate::operation::OperationPhase::Running && op.live_executor)
        .map(|(h, _)| h.clone())
        .collect()
}

pub(crate) fn format_too_many_operations_error(handles: &[OperationHandle], cap: usize) -> String {
    let count = handles.len();
    const MAX_LIST: usize = 8;
    let listed: Vec<&str> = handles.iter().take(MAX_LIST).map(|h| h.as_str()).collect();
    let mut list = listed.join(", ");
    if count > MAX_LIST {
        list.push_str(", …");
    }
    let example = handles
        .first()
        .map(|h| h.as_str())
        .unwrap_or("l_<token>_oN");
    format!(
        "too_many_operations ({count}/{cap}): wait or cancel outstanding handles before starting more — poll `wait({example})` or `cancel({example})`; running: {list}"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use indexmap::IndexMap;
    use plasm_core::{CgsContext, OperationHandle, CGS};

    use super::super::ExecuteSession;
    use crate::mcp_transport_store::{
        persisted_operations::MAX_TERMINAL_OPS_PERSIST, PersistedOperationDescriptor,
        PersistedOperationPhase, PersistedOperationProgress,
    };

    #[test]
    fn snapshot_restore_operations_roundtrip() {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        es.bind_operation_wire("sid");
        let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        es.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("register");
        es.update_operation_progress(
            &handle,
            crate::operation::OperationProgress {
                step: 2,
                step_total: 5,
                label: Some("fetch".into()),
                rows_materialized: 10,
            },
        );
        let snap = es.snapshot_operations_for_persist();
        assert_eq!(snap.operations.len(), 1);
        assert_eq!(snap.operations[0].handle, handle.as_str());
        assert_eq!(snap.operations[0].progress.step, 2);

        let es2 = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            Arc::new(CGS::new()),
            IndexMap::new(),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        es2.restore_persisted_operations(&snap);
        let op = es2.get_operation(&handle).expect("restored");
        assert!(!op.live_executor);
        assert_eq!(op.progress.step, 2);
        assert_eq!(op.progress.label.as_deref(), Some("fetch"));
    }

    #[test]
    fn rehydrated_running_stubs_do_not_consume_live_cap() {
        use std::env;
        let prev = env::var("PLASM_MAX_RUNNING_OPS_PER_SESSION").ok();
        unsafe {
            env::set_var("PLASM_MAX_RUNNING_OPS_PER_SESSION", "2");
        }
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        let mut stubs = Vec::new();
        for i in 1..=2 {
            stubs.push(PersistedOperationDescriptor {
                handle: format!("l_AAAAAAAAQACAAAAAAAAAAQ_o{i}"),
                phase: PersistedOperationPhase::Running,
                progress: PersistedOperationProgress::default(),
                started_at_unix: 0,
                error: None,
                run_artifact_id: None,
                plan_commit_ref: None,
                dry_verdict: None,
                display_map: Default::default(),
                agent_seq: 0,
                agent_last_line: String::new(),
            });
        }
        es.restore_persisted_operations(&crate::mcp_transport_store::OperationPersistSnapshot {
            operations: stubs,
            operation_handle_next: 3,
        });
        for _ in 0..2 {
            let h = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
            es.try_begin_async_operation(
                h,
                plasm_runtime::CancelSignal::new(),
                crate::operation::OpAcceptContext::default(),
            )
            .expect("live op despite rehydrated stubs");
        }
        match prev {
            Some(v) => unsafe {
                env::set_var("PLASM_MAX_RUNNING_OPS_PER_SESSION", v);
            },
            None => unsafe {
                env::remove_var("PLASM_MAX_RUNNING_OPS_PER_SESSION");
            },
        }
    }

    #[test]
    fn snapshot_prunes_old_terminal_operations() {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            "hash".into(),
            None,
            None,
        );
        es.bind_operation_wire("sid");
        for i in 1..=40 {
            let handle = OperationHandle::parse(format!("o{i}")).expect("handle");
            es.try_begin_async_operation(
                handle.clone(),
                plasm_runtime::CancelSignal::new(),
                crate::operation::OpAcceptContext::default(),
            )
            .expect("register");
            es.finalize_operation_succeeded(
                &handle,
                crate::plasm_plan_run::PlasmPlanRunResult {
                    version: serde_json::json!({}),
                    node_results: Vec::new(),
                    graph_summary: serde_json::json!({}),
                    comp: None,
                    code_plan_run_artifacts: Vec::new(),
                    run_markdown: None,
                    run_plasm_meta: None,
                    return_steps: Vec::new(),
                },
                None,
            );
        }
        let snap = es.snapshot_operations_for_persist();
        assert!(
            snap.operations.len() <= MAX_TERMINAL_OPS_PERSIST + 1,
            "len={}",
            snap.operations.len()
        );
    }
}
