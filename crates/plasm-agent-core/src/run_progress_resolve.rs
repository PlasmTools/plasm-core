//! Resolve logical-session refs to running operations (live session or persisted descriptor).

use plasm_core::OperationHandle;
use std::sync::Arc;

use crate::execute_session::ExecuteSession;
use crate::mcp_logical_ref::parse_logical_session_wire_ref;
use crate::mcp_transport_store::execute_session_registry::PersistedExecuteSessionDescriptor;
use crate::mcp_transport_store::persisted_operations::{
    operation_seq_from_wire, PersistedOperationDescriptor, PersistedOperationPhase,
};
use crate::op_ui_telemetry::OpUiTelemetry;
use crate::server_state::PlasmHostState;

#[derive(Debug, Clone)]
pub struct RunningOpQuery {
    pub logical_session_ref: String,
    pub plan_commit_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub enum RunningOpSource {
    Live,
    Persisted(Box<PersistedOperationDescriptor>),
}

#[derive(Clone)]
pub struct ResolvedRunningOp {
    pub handle: OperationHandle,
    pub live_session: Option<Arc<ExecuteSession>>,
    pub source: RunningOpSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunProgressError {
    BadLogicalRef(String),
    BindingNotFound,
    SessionNotFound,
    NoRunningOperation,
    BadHandle(String),
}

struct RunningOpPick {
    handle: OperationHandle,
    seq: u64,
    plan_commit: Option<String>,
}

impl PlasmHostState {
    pub async fn resolve_running_operation(
        &self,
        query: RunningOpQuery,
    ) -> Result<ResolvedRunningOp, RunProgressError> {
        let logical_id = parse_logical_session_wire_ref(query.logical_session_ref.as_str())
            .map_err(|e| RunProgressError::BadLogicalRef(e.to_string()))?;
        let Some((prompt_hash, session_id)) = self
            .logical_execute_bindings
            .get(&logical_id.as_uuid())
            .await
        else {
            return Err(RunProgressError::BindingNotFound);
        };

        let live_session = self
            .get_execute_session(prompt_hash.as_str(), session_id.as_str())
            .await;

        if let Some(sess) = live_session.as_ref() {
            if let Some(handle) = pick_latest_running(
                running_picks_from_session(sess),
                query.logical_session_ref.as_str(),
                query.plan_commit_ref.as_deref(),
            ) {
                return Ok(ResolvedRunningOp {
                    handle,
                    live_session,
                    source: RunningOpSource::Live,
                });
            }
        }

        let Some(desc) = self
            .execute_session_registry
            .load(prompt_hash.as_str(), session_id.as_str())
            .await
        else {
            return Err(RunProgressError::SessionNotFound);
        };
        let Some(persisted) = select_running_from_descriptor(
            &desc,
            query.logical_session_ref.as_str(),
            query.plan_commit_ref.as_deref(),
        ) else {
            return Err(RunProgressError::NoRunningOperation);
        };
        let handle = OperationHandle::parse(persisted.handle.as_str())
            .map_err(|e| RunProgressError::BadHandle(e.to_string()))?;
        Ok(ResolvedRunningOp {
            handle,
            live_session,
            source: RunningOpSource::Persisted(Box::new(persisted)),
        })
    }

    pub fn snapshot_for_running_op(&self, resolved: &ResolvedRunningOp) -> OpUiTelemetry {
        if let Some(sess) = resolved.live_session.as_ref() {
            if let Some(snap) = sess.operation_progress_ui_snapshot(&resolved.handle) {
                return snap;
            }
        }
        if let RunningOpSource::Persisted(ref desc) = resolved.source {
            return OpUiTelemetry::from_persisted(desc, &resolved.handle);
        }
        OpUiTelemetry::default()
    }
}

fn running_picks_from_session(sess: &ExecuteSession) -> Vec<RunningOpPick> {
    sess.list_running_operation_handles()
        .into_iter()
        .filter_map(|handle| {
            let op = sess.get_operation(&handle)?;
            let seq = operation_seq_from_wire(handle.as_str()).unwrap_or(op.agent_emit.seq);
            Some(RunningOpPick {
                handle,
                seq,
                plan_commit: op
                    .plan_commit_ref
                    .as_ref()
                    .map(|pc| pc.as_str().to_string()),
            })
        })
        .collect()
}

fn running_picks_from_descriptor(desc: &PersistedExecuteSessionDescriptor) -> Vec<RunningOpPick> {
    desc.operations
        .iter()
        .filter(|op| op.phase == PersistedOperationPhase::Running)
        .filter_map(|op| {
            let handle = OperationHandle::parse(op.handle.as_str()).ok()?;
            Some(RunningOpPick {
                handle,
                seq: op
                    .agent_seq
                    .max(operation_seq_from_wire(op.handle.as_str()).unwrap_or(0)),
                plan_commit: op.plan_commit_ref.clone(),
            })
        })
        .collect()
}

fn pick_latest_running(
    picks: Vec<RunningOpPick>,
    logical_session_ref: &str,
    plan_commit_ref: Option<&str>,
) -> Option<OperationHandle> {
    picks
        .into_iter()
        .filter(|pick| handle_matches_logical_ref(&pick.handle, logical_session_ref))
        .filter(|pick| plan_commit_matches(pick.plan_commit.as_deref(), plan_commit_ref))
        .max_by_key(|pick| pick.seq)
        .map(|pick| pick.handle)
}

fn select_running_from_descriptor(
    desc: &PersistedExecuteSessionDescriptor,
    logical_session_ref: &str,
    plan_commit_ref: Option<&str>,
) -> Option<PersistedOperationDescriptor> {
    let handle = pick_latest_running(
        running_picks_from_descriptor(desc),
        logical_session_ref,
        plan_commit_ref,
    )?;
    desc.operations
        .iter()
        .find(|op| op.handle == handle.as_str())
        .cloned()
}

fn handle_matches_logical_ref(handle: &OperationHandle, logical_session_ref: &str) -> bool {
    handle
        .logical_session_ref()
        .is_some_and(|r| r == logical_session_ref)
        || handle
            .as_str()
            .starts_with(&format!("{logical_session_ref}_o"))
}

fn plan_commit_matches(got: Option<&str>, want: Option<&str>) -> bool {
    match (got, want) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(g), Some(w)) => g == w,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
    use crate::execute_session::{ExecuteSession, SessionReuseKey};
    use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::CGS;
    use plasm_runtime::{ExecutionEngine, ExecutionMode};

    fn test_session(ph: String, cgs: Arc<CGS>) -> ExecuteSession {
        ExecuteSession::new(
            ph,
            "p".into(),
            cgs.clone(),
            indexmap::IndexMap::new(),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn pick_latest_running_prefers_latest_seq() {
        let cgs = Arc::new(CGS::new());
        let sess = test_session("ph".into(), cgs);
        let ref_str = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let h1 = sess.mint_operation_handle(ref_str);
        let h2 = sess.mint_operation_handle(ref_str);
        sess.try_begin_async_operation(
            h1.clone(),
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("op1");
        sess.try_begin_async_operation(
            h2.clone(),
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("op2");
        let picked =
            pick_latest_running(running_picks_from_session(&sess), ref_str, None).expect("handle");
        assert_eq!(picked.as_str(), h2.as_str());
    }

    #[test]
    fn plan_commit_ref_filter_matches_running_op() {
        let cgs = Arc::new(CGS::new());
        let sess = test_session("ph".into(), cgs);
        let ref_str = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let h1 = sess.mint_operation_handle(ref_str);
        let accept = crate::operation::OpAcceptContext {
            plan_commit_ref: Some(plasm_core::PlanCommitRef::parse("pc1").expect("pc1")),
            ..Default::default()
        };
        sess.try_begin_async_operation(h1.clone(), plasm_runtime::CancelSignal::new(), accept)
            .expect("op");
        let picked = pick_latest_running(running_picks_from_session(&sess), ref_str, Some("pc1"))
            .expect("handle");
        assert_eq!(picked.as_str(), h1.as_str());
        assert!(
            pick_latest_running(running_picks_from_session(&sess), ref_str, Some("pc2"),).is_none()
        );
    }

    #[tokio::test]
    async fn resolve_bound_logical_session_returns_running_op() {
        let cgs = Arc::new(CGS::new());
        let st = Arc::new(build_plasm_host_state(PlasmHostBootstrap {
            engine: ExecutionEngine::new(Default::default()).expect("engine"),
            mode: ExecutionMode::Live,
            registry: Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
                "default".into(),
                "Default".into(),
                vec!["default".into()],
                cgs.clone(),
            )])),
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            plugin_manager: None,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        }));

        let ref_str = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let logical_id = parse_logical_session_wire_ref(ref_str).expect("logical ref");
        let ph = PromptHashHex::from_prompt_sha256("run-ui-progress-bind-test").to_string();
        let sid = ExecuteSessionId::new_random().to_string();
        let sess = test_session(ph.clone(), cgs.clone());
        let handle = sess.mint_operation_handle(ref_str);
        sess.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            crate::operation::OpAcceptContext::default(),
        )
        .expect("begin op");

        let reuse_key = SessionReuseKey {
            tenant_scope: String::new(),
            entry_id: "default".into(),
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
            entities: vec!["Pet".into()],
            context_intent: None,
            ranked_capabilities: None,
            principal: None,
            plugin_generation_id: None,
            logical_session_id: Some(logical_id.as_uuid().to_string()),
        };
        st.sessions
            .insert(reuse_key, ph.clone(), sid.clone(), sess)
            .await;
        st.logical_execute_bindings
            .insert(logical_id.as_uuid(), ph.clone(), sid.clone())
            .await;

        let resolved = st
            .resolve_running_operation(RunningOpQuery {
                logical_session_ref: ref_str.to_string(),
                plan_commit_ref: None,
            })
            .await
            .expect("resolve");
        let snap = st.snapshot_for_running_op(&resolved);
        assert!(!snap.line.is_empty());
        assert!(!snap.terminal);
        assert_eq!(resolved.handle.as_str(), handle.as_str());
    }
}
