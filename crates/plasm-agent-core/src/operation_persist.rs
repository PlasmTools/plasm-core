//! Redis persistence for async operation descriptors (coalesced progress patches).

use plasm_core::OperationHandle;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::task::AbortHandle;

use crate::mcp_transport_store::{
    descriptor_from_operation_state, OperationPersistPatch, PersistedOperationProgress,
};
use crate::operation::OperationState;
use crate::operation_progress::OP_PROGRESS_COALESCE;
use crate::server_state::PlasmHostState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistUrgency {
    Immediate,
    Coalesced,
}

#[derive(Clone, Default)]
pub struct OperationPersistScheduler {
    debounce: Arc<Mutex<HashMap<String, AbortHandle>>>,
}

impl OperationPersistScheduler {
    pub fn schedule(
        &self,
        host: &PlasmHostState,
        prompt_hash: &str,
        session_id: &str,
        patch: OperationPersistPatch,
        urgency: PersistUrgency,
    ) {
        let key = persist_debounce_key(prompt_hash, session_id, &patch);
        let mut guard = self.debounce.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.remove(&key) {
            handle.abort();
        }
        if urgency == PersistUrgency::Immediate {
            drop(guard);
            spawn_patch(
                host.clone(),
                prompt_hash.to_string(),
                session_id.to_string(),
                patch,
            );
            return;
        }
        let debounce = Arc::clone(&self.debounce);
        let host = host.clone();
        let ph = prompt_hash.to_string();
        let sid = session_id.to_string();
        let debounce_key = key.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(OP_PROGRESS_COALESCE).await;
            debounce
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&debounce_key);
            spawn_patch(host, ph, sid, patch);
        });
        guard.insert(key, task.abort_handle());
    }
}

fn spawn_patch(
    host: PlasmHostState,
    prompt_hash: String,
    session_id: String,
    patch: OperationPersistPatch,
) {
    tokio::spawn(async move {
        host.execute_session_registry
            .patch_session_operations(&prompt_hash, &session_id, patch)
            .await;
    });
}

fn persist_debounce_key(
    prompt_hash: &str,
    session_id: &str,
    patch: &OperationPersistPatch,
) -> String {
    let handle = match patch {
        OperationPersistPatch::Upsert(d) => d.handle.as_str(),
        OperationPersistPatch::Progress { handle, .. } => handle.as_str(),
    };
    format!("{prompt_hash}:{session_id}:{handle}")
}

pub(crate) fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn schedule_op_persist(
    host: Option<&PlasmHostState>,
    prompt_hash: &str,
    session_id: &str,
    handle: &OperationHandle,
    op: &OperationState,
    started_at_unix: u64,
    urgency: PersistUrgency,
) {
    let Some(host) = host else {
        return;
    };
    let patch = match urgency {
        PersistUrgency::Immediate => OperationPersistPatch::Upsert(
            descriptor_from_operation_state(handle, op, started_at_unix),
        ),
        PersistUrgency::Coalesced => OperationPersistPatch::Progress {
            handle: handle.as_str().to_string(),
            progress: PersistedOperationProgress::from(&op.progress),
            agent_seq: op.agent_emit.seq,
            agent_last_line: op.agent_emit.last_line.clone(),
        },
    };
    host.operation_persist
        .schedule(host, prompt_hash, session_id, patch, urgency);
}

/// Wire session identity for cross-pod operation persistence hooks.
#[derive(Clone, Default)]
pub(crate) struct OperationWireBinding {
    pub session_id: Option<String>,
    pub started_at_unix_by_handle: HashMap<String, u64>,
}

impl OperationWireBinding {
    pub(crate) fn started_at(&self, handle: &OperationHandle) -> u64 {
        self.started_at_unix_by_handle
            .get(handle.as_str())
            .copied()
            .unwrap_or_else(unix_now)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use crate::mcp_transport_store::ExecuteSessionRegistry;
    use crate::operation::OperationProgress;
    use crate::run_artifacts::RunArtifactStore;
    use crate::server_state::CatalogBootstrap;
    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::CGS;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

    fn running_op_state(phase: crate::operation::OperationPhase) -> OperationState {
        OperationState {
            phase,
            cancel: plasm_runtime::CancelSignal::new(),
            started_at: std::time::Instant::now(),
            progress: OperationProgress::default(),
            result: None,
            error: None,
            live_executor: true,
            run_artifact_id: None,
            agent_emit: Default::default(),
            display_map: Default::default(),
            plan_commit_ref: None,
            dry_verdict: None,
            auto_async: false,
            mcp_transport_key: None,
            progress_host: None,
            progress_tx: tokio::sync::broadcast::channel(1).0,
            terminal_tx: None,
            comp: None,
            plan_ux_reflection: None,
            step_order: Vec::new(),
        }
    }

    async fn seed_descriptor(
        store: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
        ph: &str,
        sid: &str,
    ) {
        use crate::mcp_transport_store::execute_session_registry::{
            PersistedExecuteSessionDescriptor, PersistedSessionReuseKey,
        };
        let desc = PersistedExecuteSessionDescriptor {
            prompt_hash: ph.into(),
            session_id: sid.into(),
            prompt_text: String::new(),
            entry_id: "default".into(),
            context_entry_ids: vec!["default".into()],
            entities: vec![],
            entity_catalog_entry_ids: vec![],
            tenant_scope: String::new(),
            principal_subject: String::new(),
            http_backend: None,
            principal: None,
            catalog_cgs_hash: "h".into(),
            context_intent: None,
            ranked_capabilities: None,
            domain_revision: 0,
            reuse_key: PersistedSessionReuseKey {
                tenant_scope: String::new(),
                entry_id: "default".into(),
                catalog_cgs_hash: "h".into(),
                entities: vec![],
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
                logical_session_id: None,
            },
            expires_at_unix: u64::MAX,
            catalog_cgs_hashes_by_entry: Default::default(),
            registry_catalog_hashes_by_entry: Default::default(),
            outbound_hosted_kv_by_entry: Default::default(),
            bindings_by_entry: Default::default(),
            plan_commits: vec![],
            plan_commit_next: 0,
            operations: vec![],
            operation_handle_next: 1,
        };
        store.write().await.insert(
            format!("mcp:execute:session:{ph}:{sid}"),
            serde_json::to_string(&desc).unwrap(),
        );
    }

    fn test_host() -> (
        PlasmHostState,
        std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
    ) {
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        let (reg, store) = ExecuteSessionRegistry::with_test_json_store();
        let st = build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
                "default".into(),
                "Default".into(),
                vec![],
                Arc::new(CGS::new()),
            )])),
            catalog_bootstrap: CatalogBootstrap::Fixed,
                        incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        });
        let mut st = st;
        st.oss.execute_session_registry = reg;
        (st, store)
    }

    #[tokio::test]
    async fn coalesced_progress_debounces_redis_patches() {
        let (host, store) = test_host();
        seed_descriptor(&store, "ph", "sid").await;
        let scheduler = OperationPersistScheduler::default();
        let handle = OperationHandle::parse("o1").expect("handle");
        scheduler.schedule(
            &host,
            "ph",
            "sid",
            OperationPersistPatch::Upsert(descriptor_from_operation_state(
                &handle,
                &running_op_state(crate::operation::OperationPhase::Running),
                0,
            )),
            PersistUrgency::Immediate,
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        scheduler.schedule(
            &host,
            "ph",
            "sid",
            OperationPersistPatch::Progress {
                handle: "o1".into(),
                progress: PersistedOperationProgress {
                    step: 1,
                    step_total: 3,
                    label: None,
                    rows_materialized: 0,
                },
                agent_seq: 1,
                agent_last_line: "a".into(),
            },
            PersistUrgency::Coalesced,
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        scheduler.schedule(
            &host,
            "ph",
            "sid",
            OperationPersistPatch::Progress {
                handle: "o1".into(),
                progress: PersistedOperationProgress {
                    step: 2,
                    step_total: 3,
                    label: None,
                    rows_materialized: 5,
                },
                agent_seq: 2,
                agent_last_line: "b".into(),
            },
            PersistUrgency::Coalesced,
        );
        tokio::time::sleep(OP_PROGRESS_COALESCE + Duration::from_millis(100)).await;
        let desc = host
            .execute_session_registry
            .load("ph", "sid")
            .await
            .expect("descriptor after debounced progress");
        assert_eq!(desc.operations.len(), 1);
        assert_eq!(desc.operations[0].progress.step, 2);
        assert_eq!(desc.operations[0].agent_seq, 2);
    }

    #[tokio::test]
    async fn immediate_upsert_cancels_pending_coalesced_progress() {
        let (host, store) = test_host();
        seed_descriptor(&store, "ph2", "sid2").await;
        let scheduler = OperationPersistScheduler::default();
        let handle = OperationHandle::parse("o1").expect("handle");
        scheduler.schedule(
            &host,
            "ph2",
            "sid2",
            OperationPersistPatch::Upsert(descriptor_from_operation_state(
                &handle,
                &running_op_state(crate::operation::OperationPhase::Running),
                0,
            )),
            PersistUrgency::Immediate,
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        scheduler.schedule(
            &host,
            "ph2",
            "sid2",
            OperationPersistPatch::Progress {
                handle: "o1".into(),
                progress: PersistedOperationProgress {
                    step: 1,
                    step_total: 2,
                    label: None,
                    rows_materialized: 0,
                },
                agent_seq: 1,
                agent_last_line: String::new(),
            },
            PersistUrgency::Coalesced,
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        scheduler.schedule(
            &host,
            "ph2",
            "sid2",
            OperationPersistPatch::Upsert(descriptor_from_operation_state(
                &handle,
                &running_op_state(crate::operation::OperationPhase::Succeeded),
                0,
            )),
            PersistUrgency::Immediate,
        );
        tokio::time::sleep(OP_PROGRESS_COALESCE + Duration::from_millis(100)).await;
        let desc = host
            .execute_session_registry
            .load("ph2", "sid2")
            .await
            .expect("terminal upsert");
        assert_eq!(
            desc.operations[0].phase,
            crate::mcp_transport_store::PersistedOperationPhase::Succeeded
        );
        assert_eq!(desc.operations[0].progress.step, 0);
    }
}
