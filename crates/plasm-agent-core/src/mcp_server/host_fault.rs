//! Host / transport faults for MCP `plasm` tool invoke (not program diagnostics).

/// Non-correctable tool failure (session, persist, live-run host errors).
/// Program diagnostics are success-shaped `Ok(PlasmPlanRunResult)` with [`PlanAgentOutcome`].
#[derive(Debug, Clone)]
pub struct HostFault(pub plasm_runtime::ExecutionFailure);

impl From<String> for HostFault {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<&str> for HostFault {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

impl std::fmt::Display for HostFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl From<plasm_runtime::ExecutionFailure> for HostFault {
    fn from(value: plasm_runtime::ExecutionFailure) -> Self {
        Self(value)
    }
}

impl HostFault {
    pub fn into_tool_result(self) -> rust_mcp_sdk::schema::CallToolResult {
        use rust_mcp_sdk::schema::schema_utils::CallToolError;
        use rust_mcp_sdk::schema::CallToolResult;
        let mut result = CallToolResult::with_error(CallToolError::from_message(
            [
                Some(self.0.to_string()),
                Some(self.0.diagnostic().to_owned()),
                self.0.recovery_instructions().map(str::to_owned),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
        ));
        result.structured_content =
            serde_json::json!({"status": "execution_failed", "failure": self.0, "recovery_instructions": self.0.recovery_instructions()})
                .as_object()
                .cloned();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_write_mcp_reports_execution_scoped_uncertainty() {
        let mut failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Upstream,
            "upstream_rejection",
            "service rejected the requested state",
        );
        failure.recovery = plasm_runtime::RecoveryDisposition::ReconcileEffects;
        failure.effects_unresolved = true;
        let result = HostFault(failure).into_tool_result();
        let wire = serde_json::to_string(&result).unwrap();
        assert!(wire.contains("A new execution is permitted"));
        assert!(wire.contains("Failed dispatches may have taken effect"));
        assert!(wire.contains("service rejected the requested state"));
        assert!(serde_json::to_string(&result.content)
            .unwrap()
            .contains("service rejected the requested state"));
        assert_eq!(
            result.structured_content.as_ref().unwrap()["failure"]["diagnostic"],
            "service rejected the requested state"
        );
    }

    #[test]
    fn execution_failure_mcp_preserves_recovery_and_diagnostic() {
        let failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::ResponseContract,
            "response_contract_violation",
            "repair program: field rejected by service",
        )
        .at("map/write", vec![2, 1]);
        let result = HostFault(failure.clone()).into_tool_result();
        let wire = serde_json::to_string(&result).unwrap();
        assert!(wire.contains("repair program: field rejected by service"));
        assert_eq!(result.is_error, Some(true));
        let decoded: plasm_runtime::ExecutionFailure =
            serde_json::from_value(result.structured_content.unwrap()["failure"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(failure).unwrap()
        );
    }
}
