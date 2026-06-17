//! Phase timing for MCP `plasm_run` (resolve → execute → persist).

use std::time::Instant;

pub struct McpPlasmRunPhaseRecorder {
    run_started: Instant,
    lap: Instant,
}

impl McpPlasmRunPhaseRecorder {
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            run_started: now,
            lap: now,
        }
    }

    pub fn record(&mut self, phase: &'static str) {
        let elapsed = self.lap.elapsed();
        crate::metrics::record_mcp_plasm_run_phase(phase, elapsed);
        let _phase_span = crate::spans::mcp_plasm_run_phase(phase);
        let _guard = _phase_span.enter();
        tracing::info!(
            target: "plasm_agent.mcp.plasm_run.phase",
            phase,
            elapsed_ms = elapsed.as_millis(),
            total_ms = self.run_started.elapsed().as_millis(),
        );
        self.lap = Instant::now();
    }
}
