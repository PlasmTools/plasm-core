//! Phase timing for MCP `plasm_run` (resolve → execute → persist).

use std::future::Future;
use std::time::Instant;
use tracing::Instrument;

pub async fn mcp_plasm_run_phase<F, Fut, T>(phase: &'static str, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let started = Instant::now();
    let out = f()
        .instrument(crate::spans::mcp_plasm_run_phase(phase))
        .await;
    crate::metrics::record_mcp_plasm_run_phase(phase, started.elapsed());
    out
}
