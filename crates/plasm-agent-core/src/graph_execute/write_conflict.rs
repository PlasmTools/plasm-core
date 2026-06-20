//! Per-store write-conflict retry policy for optimistic branch commit.

use plasm_runtime::WriteConflictDetails;

/// Maximum branch re-fork cycles when commit races a concurrent materialization writer.
pub const MAX_WRITE_CONFLICT_RETRIES: u32 = 3;

#[derive(Debug)]
pub enum GraphBranchRunError {
    WriteConflict {
        details: WriteConflictDetails,
        attempts: u32,
    },
}

/// Pure bound check: may another branch cycle run after this attempt's write conflict?
pub fn write_conflict_should_retry(attempt: u32) -> bool {
    attempt < MAX_WRITE_CONFLICT_RETRIES
}

/// Emit telemetry for a write-conflict branch retry. Call only on the path that actually
/// retries (keeps [`write_conflict_should_retry`] side-effect free).
pub fn record_write_conflict_branch_retry(attempt: u32, details: &WriteConflictDetails) {
    tracing::info!(
        target: "plasm_agent::graph_execute",
        attempt = attempt + 1,
        max = MAX_WRITE_CONFLICT_RETRIES,
        graph_refs = ?details.graph_refs,
        response_fingerprints = details.response_fingerprints.len(),
        query_keys = details.query_keys.len(),
        "materialization write conflict on commit; retrying branch cycle"
    );
    crate::metrics::record_graph_branch_write_conflict_retry();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_conflict_retry_policy_is_bounded() {
        for attempt in 0..MAX_WRITE_CONFLICT_RETRIES {
            assert!(
                write_conflict_should_retry(attempt),
                "attempt {attempt} should retry"
            );
        }
        assert!(
            !write_conflict_should_retry(MAX_WRITE_CONFLICT_RETRIES),
            "exhausted budget must stop"
        );
    }
}
