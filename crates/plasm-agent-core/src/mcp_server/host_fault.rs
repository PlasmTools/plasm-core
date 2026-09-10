//! Host / transport faults for MCP `plasm` tool invoke (not program diagnostics).

/// Non-correctable tool failure (session, persist, live-run host errors).
/// Program diagnostics are success-shaped `Ok(PlasmPlanRunResult)` with [`PlanAgentOutcome`].
#[derive(Debug, Clone)]
pub struct HostFault(pub String);

impl From<String> for HostFault {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for HostFault {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for HostFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
