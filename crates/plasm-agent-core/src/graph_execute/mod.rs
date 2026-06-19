//! Fork–execute–commit graph path: live HTTP runs on a branch snapshot without
//! holding the session graph mutex.
//!
//! **CEP-1..3:** epoch monotonicity, stale commit discard, single winner — validated by
//! `shuttle_*` tests in [`concurrency_shuttle`] (primary graph concurrency gate).

mod branch_ops;
mod live_execute;
mod stale_commit;

#[cfg(test)]
mod concurrency_shuttle;

use plasm_runtime::SessionMaterialization;

use crate::execute_session::{ExecuteSession, GraphEpoch};

use branch_ops::{commit_materialization, fork_materialization};

pub use live_execute::{run_with_stale_epoch_retry, LiveBranchExecuteInput};
pub use stale_commit::{stale_commit_should_retry, GraphBranchRunError, MAX_STALE_EPOCH_RETRIES};

/// Materialization fork + parent epoch for optimistic single-writer commit.
pub struct GraphExecuteBranch {
    mat: SessionMaterialization,
    parent_epoch: GraphEpoch,
}

#[derive(Debug)]
pub enum GraphCommitError {
    StaleParentEpoch {
        expected: GraphEpoch,
        found: GraphEpoch,
    },
    Merge(plasm_runtime::RuntimeError),
}

impl GraphExecuteBranch {
    /// Snapshot graph + clone response/query stores under the session lock.
    pub async fn fork(sess: &ExecuteSession) -> Self {
        let guard = sess.lock_graph_cache().await;
        let (mat, parent_epoch) = fork_materialization(guard.materialization());
        Self { mat, parent_epoch }
    }

    pub fn mat_mut(&mut self) -> &mut SessionMaterialization {
        &mut self.mat
    }

    /// Merge branch into session if the graph epoch is unchanged since fork.
    pub async fn commit(self, sess: &ExecuteSession) -> Result<GraphEpoch, GraphCommitError> {
        let mut guard = sess.lock_graph_cache().await;
        commit_materialization(&mut guard, self.parent_epoch, self.mat)
    }
}
