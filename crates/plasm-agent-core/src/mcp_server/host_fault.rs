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
    pub fn into_tool_result(
        self,
        delivery: crate::mcp_delivery::McpDeliveryProfile,
    ) -> rust_mcp_sdk::schema::CallToolResult {
        use rust_mcp_sdk::schema::schema_utils::CallToolError;
        use rust_mcp_sdk::schema::CallToolResult;
        let mut result =
            CallToolResult::with_error(CallToolError::from_message(agent_failure_summary(&self.0)));
        if delivery.emits_structured_ui() {
            result.structured_content = serde_json::json!({
                "status": "execution_failed",
                "failure": self.0,
                "recovery_instructions": self.0.recovery_instructions()
            })
            .as_object()
            .cloned();
        }
        result
    }
}

fn agent_failure_summary(failure: &plasm_runtime::ExecutionFailure) -> String {
    use plasm_runtime::{MutationDispatchStatus, RecoveryDisposition};
    let visible_node = failure
        .node
        .as_deref()
        .filter(|node| !crate::plan_dry_display::is_synthetic_plan_node_id_public(node));
    let mut lines = vec![match visible_node {
        Some(node) => format!("Execution stopped at {node}: {}", failure.diagnostic()),
        None => format!("Execution stopped: {}", failure.diagnostic()),
    }];
    let mut effects = Vec::new();
    for receipt in &failure.effects {
        let identities = |status: plasm_runtime::OperationInvocationStatus| {
            receipt
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.status == status)
                .map(|occurrence| {
                    occurrence
                        .source_identity
                        .clone()
                        .unwrap_or_else(|| format!("row #{}", occurrence.source_index))
                })
                .collect::<Vec<_>>()
        };
        if receipt.completed > 0 {
            let completed = identities(plasm_runtime::OperationInvocationStatus::Completed);
            effects.push(format!(
                "{}/{}: {} completed{}",
                receipt.entry_id,
                receipt.capability,
                receipt.completed,
                if completed.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", completed.join(", "))
                }
            ));
        }
        if receipt.failed > 0 {
            let failed = identities(plasm_runtime::OperationInvocationStatus::Failed);
            effects.push(format!(
                "{}/{}: {} failed{}",
                receipt.entry_id,
                receipt.capability,
                receipt.failed,
                if failed.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", failed.join(", "))
                }
            ));
        }
    }
    let has_dispatch_evidence = !failure.dispatches.is_empty();
    let mut dispatches = std::collections::BTreeMap::new();
    for dispatch in &failure.dispatches {
        let status = match dispatch.status {
            MutationDispatchStatus::Unresolved => "without transport response",
            MutationDispatchStatus::ResponseReceived => "with transport response",
        };
        *dispatches
            .entry((
                dispatch.operation.entry_id.as_str(),
                dispatch.operation.capability.as_str(),
                status,
            ))
            .or_insert(0usize) += 1;
    }
    for ((entry, capability, status), count) in dispatches {
        effects.push(format!("{entry}/{capability}: {count} {status}"));
    }
    if !effects.is_empty() {
        lines.push(format!("Effects: {}.", effects.join("; ")));
        if has_dispatch_evidence {
            lines.push("Dispatch history may overlap completed and failed effects above.".into());
        }
    }
    if failure.effects_unresolved && !has_dispatch_evidence {
        lines.push("Some effects remain unresolved.".into());
    }
    lines.push(
        match failure.recovery {
            RecoveryDisposition::RepairProgram => "Revise the program and plan again.",
            RecoveryDisposition::ObtainAuthorization => "Obtain authorization before another run.",
            RecoveryDisposition::ReconcileEffects => {
                "Check completed and unresolved effects before another run; this run did not roll back."
            }
            RecoveryDisposition::Stop => "Stop this execution.",
        }
        .into(),
    );
    lines.join("\n")
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
        let result =
            HostFault(failure).into_tool_result(crate::mcp_delivery::McpDeliveryProfile::FullApps);
        let wire = serde_json::to_string(&result).unwrap();
        assert!(wire.contains("Some effects remain unresolved"));
        assert!(wire.contains("Check completed and unresolved effects"));
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
        let result = HostFault(failure.clone())
            .into_tool_result(crate::mcp_delivery::McpDeliveryProfile::FullApps);
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

    #[test]
    fn model_delivery_uses_semantic_failure_text_without_receipt_json() {
        let mut failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Upstream,
            "upstream_rejection",
            "Insufficient funds",
        )
        .at("__py0", vec![]);
        failure.recovery = plasm_runtime::RecoveryDisposition::ReconcileEffects;
        failure.effects_unresolved = true;
        let result = HostFault(failure)
            .into_tool_result(crate::mcp_delivery::McpDeliveryProfile::ContentOnly);
        assert!(result.structured_content.is_none());
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("Insufficient funds"));
        assert!(text.contains("Some effects remain unresolved"));
        assert!(!text.contains("catalog_digest"));
        assert!(!text.contains("__py0"));
    }

    #[test]
    fn completed_receipt_does_not_hide_unresolved_effects() {
        let mut failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Transport,
            "transport_error",
            "write interrupted",
        );
        failure.recovery = plasm_runtime::RecoveryDisposition::ReconcileEffects;
        failure.effects_unresolved = true;
        failure
            .effects
            .push(plasm_runtime::execution_failure::EffectReceipt {
                entry_id: "fixture".into(),
                capability: "write".into(),
                completed: 1,
                failed: 0,
                occurrences: Vec::new(),
            });
        let text = agent_failure_summary(&failure);
        assert!(text.contains("fixture/write: 1 completed"), "{text}");
        assert!(text.contains("Some effects remain unresolved"), "{text}");
    }

    #[test]
    fn mixed_fanout_keeps_identities_without_double_counting_dispatches() {
        let mut failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Upstream,
            "upstream_rejection",
            "second write rejected",
        );
        failure.recovery = plasm_runtime::RecoveryDisposition::ReconcileEffects;
        failure.effects_unresolved = true;
        failure
            .effects
            .push(plasm_runtime::execution_failure::EffectReceipt {
                entry_id: "fixture".into(),
                capability: "write".into(),
                completed: 1,
                failed: 1,
                occurrences: vec![
                    plasm_runtime::execution_failure::EffectOccurrence {
                        source_index: 0,
                        source_identity: Some("Item/a".into()),
                        status: plasm_runtime::OperationInvocationStatus::Completed,
                    },
                    plasm_runtime::execution_failure::EffectOccurrence {
                        source_index: 1,
                        source_identity: Some("Item/b".into()),
                        status: plasm_runtime::OperationInvocationStatus::Failed,
                    },
                ],
            });
        failure.dispatches.push(plasm_runtime::MutationDispatch {
            operation: plasm_runtime::OperationIdentity {
                entry_id: "fixture".into(),
                capability: "write".into(),
            },
            request_fingerprint: "opaque".into(),
            status: plasm_runtime::MutationDispatchStatus::ResponseReceived,
        });
        let result = HostFault(failure)
            .into_tool_result(crate::mcp_delivery::McpDeliveryProfile::ContentOnly);
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("1 completed [Item/a]"), "{text}");
        assert!(text.contains("1 failed [Item/b]"), "{text}");
        assert!(text.contains("Dispatch history may overlap"), "{text}");
        assert!(!text.contains("completion unproven"), "{text}");
        assert!(result.structured_content.is_none());
    }
}
