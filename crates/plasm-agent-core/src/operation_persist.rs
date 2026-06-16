//! Redis persistence for async operation descriptors (progress writes follow in-memory coalesce).

use std::collections::HashMap;

use plasm_core::OperationHandle;

use crate::mcp_transport_store::{descriptor_from_operation_state, OperationPersistPatch};
use crate::operation::OperationState;
use crate::server_state::PlasmHostState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistUrgency {
    Immediate,
    Coalesced,
}

#[derive(Clone, Default)]
pub struct OperationPersistScheduler;

impl OperationPersistScheduler {
    pub fn schedule(
        &self,
        host: &PlasmHostState,
        prompt_hash: &str,
        session_id: &str,
        patch: OperationPersistPatch,
        _urgency: PersistUrgency,
    ) {
        let host = host.clone();
        let prompt_hash = prompt_hash.to_string();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            host.execute_session_registry
                .patch_session_operations(&prompt_hash, &session_id, patch)
                .await;
        });
    }
}

pub(crate) fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn schedule_op_persist_from_state(
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
    let desc = descriptor_from_operation_state(handle, op, started_at_unix);
    host.operation_persist.schedule(
        host,
        prompt_hash,
        session_id,
        OperationPersistPatch::Upsert(desc),
        urgency,
    );
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
