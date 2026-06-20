//! Fork–execute–commit graph path: live HTTP runs on a branch snapshot without
//! holding the session graph mutex.
//!
//! **CEP-1..3, CEP-11, CEP-12:** graph version monotonicity, write-conflict discard,
//! single winner on same-key races, per-store write-set conflicts, and disjoint concurrent
//! commits — validated by named cache/unit tests and `shuttle_*` tests in
//! [`concurrency_shuttle`] (primary graph concurrency gate).

mod branch_ops;
mod live_execute;
mod write_conflict;

#[cfg(test)]
mod concurrency_shuttle;

use plasm_runtime::{BranchMaterializationBase, SessionMaterialization, WriteConflictDetails};

use crate::execute_session::ExecuteSession;

use branch_ops::{commit_materialization, fork_materialization};

pub use live_execute::{run_with_write_conflict_retry, LiveBranchExecuteInput};
pub use write_conflict::{
    write_conflict_should_retry, GraphBranchRunError, MAX_WRITE_CONFLICT_RETRIES,
};

/// Materialization fork + per-store base snapshots for optimistic commit validation.
pub struct GraphExecuteBranch {
    mat: SessionMaterialization,
    base: BranchMaterializationBase,
}

#[derive(Debug)]
pub enum GraphCommitError {
    WriteConflict(WriteConflictDetails),
    Merge(plasm_runtime::RuntimeError),
}

impl GraphExecuteBranch {
    /// Snapshot graph + clone response/query stores under the session lock.
    pub async fn fork(sess: &ExecuteSession) -> Self {
        let guard = sess.lock_graph_cache().await;
        let (mat, base) = fork_materialization(guard.materialization());
        Self { mat, base }
    }

    pub fn mat_mut(&mut self) -> &mut SessionMaterialization {
        &mut self.mat
    }

    /// Merge branch into session when no write-set entry raced a concurrent writer.
    pub async fn commit(self, sess: &ExecuteSession) -> Result<(), GraphCommitError> {
        let mut guard = sess.lock_graph_cache().await;
        commit_materialization(&mut guard, self.base, self.mat)
    }
}
