//! Stale-epoch retry policy for optimistic graph branch commit.
//!
//! Live execute, spill I/O, and projection enrichment run on a [`GraphExecuteBranch`]
//! snapshot without holding the session graph mutex. Commit performs an optimistic
//! compare on [`GraphEpoch`]: if another writer bumped the epoch during the unlocked
//! phase, commit returns [`GraphCommitError::StaleParentEpoch`].
//!
//! [`crate::graph_execute::run_with_stale_epoch_retry`] re-forks and re-runs only the
//! contended line (before any commit is visible) up to [`MAX_STALE_EPOCH_RETRIES`] times.
//! There is no full-plan retry: re-running the whole plan would re-issue already-committed
//! mutating lines.

use crate::execute_session::GraphEpoch;

/// Maximum branch re-fork cycles when commit races a concurrent graph writer.
pub const MAX_STALE_EPOCH_RETRIES: u32 = 3;

#[derive(Debug)]
pub enum GraphBranchRunError {
    StaleCommit {
        expected: GraphEpoch,
        found: GraphEpoch,
        attempts: u32,
    },
}

/// Pure bound check: may another branch cycle run after this attempt's stale commit?
pub fn stale_commit_should_retry(attempt: u32) -> bool {
    attempt < MAX_STALE_EPOCH_RETRIES
}

/// Emit telemetry for a stale-epoch branch retry. Call only on the path that actually
/// retries (keeps [`stale_commit_should_retry`] side-effect free).
pub fn record_stale_epoch_branch_retry(attempt: u32, expected: GraphEpoch, found: GraphEpoch) {
    tracing::info!(
        target: "plasm_agent::graph_execute",
        attempt = attempt + 1,
        max = MAX_STALE_EPOCH_RETRIES,
        ?expected,
        ?found,
        "stale graph epoch on commit; retrying branch cycle"
    );
    crate::metrics::record_graph_branch_stale_retry();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_commit_retry_policy_is_bounded() {
        for attempt in 0..MAX_STALE_EPOCH_RETRIES {
            assert!(
                stale_commit_should_retry(attempt),
                "attempt {attempt} should retry"
            );
        }
        assert!(
            !stale_commit_should_retry(MAX_STALE_EPOCH_RETRIES),
            "exhausted budget must stop"
        );
    }
}
