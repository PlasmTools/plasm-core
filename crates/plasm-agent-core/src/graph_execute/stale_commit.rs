//! Stale-epoch retry policy for optimistic graph branch commit.
//!
//! Live execute, spill I/O, and projection enrichment run on a [`GraphExecuteBranch`]
//! snapshot without holding the session graph mutex. Commit performs an optimistic
//! compare on [`GraphEpoch`]: if another writer bumped the epoch during the unlocked
//! phase, commit returns [`GraphCommitError::StaleParentEpoch`].
//!
//! HTTP execute ([`crate::http_execute`]) re-runs the full cycle (re-fork, re-execute,
//! re-commit) up to [`MAX_STALE_EPOCH_RETRIES`] times.

use crate::execute_session::GraphEpoch;

/// Maximum full branch cycles when commit races a concurrent graph writer.
pub const MAX_STALE_EPOCH_RETRIES: u32 = 3;

#[derive(Debug)]
pub enum GraphBranchRunError {
    StaleCommit {
        expected: GraphEpoch,
        found: GraphEpoch,
        attempts: u32,
    },
}

/// Log + return whether a stale commit should retry another full branch cycle.
pub fn stale_commit_should_retry(attempt: u32, expected: GraphEpoch, found: GraphEpoch) -> bool {
    if attempt >= MAX_STALE_EPOCH_RETRIES {
        return false;
    }
    tracing::info!(
        target: "plasm_agent::graph_execute",
        attempt = attempt + 1,
        max = MAX_STALE_EPOCH_RETRIES,
        ?expected,
        ?found,
        "stale graph epoch on commit; retrying branch cycle"
    );
    true
}
