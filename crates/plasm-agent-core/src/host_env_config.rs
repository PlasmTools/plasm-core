//! Process-wide host env knobs (parsed once).

use std::sync::OnceLock;
use std::time::Duration;

static BOUNDED_SYNC_RUN_DEADLINE: OnceLock<Duration> = OnceLock::new();

/// Wall-clock cap for bounded sync MCP `plasm_run` (non-expensive committed plans).
pub fn bounded_sync_run_deadline() -> Duration {
    *BOUNDED_SYNC_RUN_DEADLINE.get_or_init(|| {
        std::env::var("PLASM_MCP_BOUNDED_SYNC_RUN_DEADLINE_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(90))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_sync_run_deadline_default_is_ninety_seconds() {
        assert_eq!(bounded_sync_run_deadline(), Duration::from_secs(90));
    }
}
