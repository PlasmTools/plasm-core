//! Per-execute-row coordination gates for parallel MCP `plasm_context` exposure commits
//! and single-flight logical-session open (CEP-13).

use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone, Debug, Eq)]
pub struct ExecuteCoordKey {
    pub prompt_hash: String,
    pub session_id: String,
}

impl PartialEq for ExecuteCoordKey {
    fn eq(&self, other: &Self) -> bool {
        self.prompt_hash == other.prompt_hash && self.session_id == other.session_id
    }
}

impl Hash for ExecuteCoordKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.prompt_hash.hash(state);
        self.session_id.hash(state);
    }
}

/// Narrow exposure-coordination layer: microsecond commit gates, no I/O under lock.
#[derive(Default)]
pub struct SessionCoordination {
    logical_open: DashMap<Uuid, Arc<Mutex<()>>>,
    exposure_commit: DashMap<ExecuteCoordKey, Arc<Mutex<()>>>,
}

impl SessionCoordination {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn with_logical_open<F, Fut, T>(&self, logical_id: Uuid, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let gate = self
            .logical_open
            .entry(logical_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = gate.lock().await;
        f().await
    }

    pub async fn with_exposure_commit<F, Fut, T>(&self, key: &ExecuteCoordKey, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let gate = self
            .exposure_commit
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = gate.lock().await;
        f().await
    }
}
