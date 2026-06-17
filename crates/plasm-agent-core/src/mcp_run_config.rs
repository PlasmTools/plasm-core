//! MCP `plasm_run` timing and deadline configuration.

use std::time::Duration;

/// Wall-clock cap for bounded sync live runs (`CommittedPlan` not expensive).
pub fn bounded_sync_run_deadline() -> Duration {
    std::env::var("PLASM_MCP_BOUNDED_SYNC_RUN_DEADLINE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(90))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_sync_run_deadline_default_is_ninety_seconds() {
        std::env::remove_var("PLASM_MCP_BOUNDED_SYNC_RUN_DEADLINE_MS");
        assert_eq!(bounded_sync_run_deadline(), Duration::from_secs(90));
    }
}
