//! Host / transport faults for MCP `plasm` tool invoke (not program diagnostics).

/// Non-correctable tool failure (session, persist, live-run host errors).
/// Program diagnostics are success-shaped `Ok(PlasmPlanRunResult)` with [`PlanAgentOutcome`].
#[derive(Debug, Clone)]
pub struct HostFault(pub plasm_runtime::ExecutionFailure);

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
        _delivery: crate::mcp_delivery::McpDeliveryProfile,
    ) -> rust_mcp_sdk::schema::CallToolResult {
        use rust_mcp_sdk::schema::schema_utils::CallToolError;
        use rust_mcp_sdk::schema::CallToolResult;
        let mut result = CallToolResult::with_error(CallToolError::from_message(
            crate::agent_presentation::render_failure_complete(&self.0).into_string(),
        ));
        // Structured content can be forwarded into model context by a host.
        // Full machine evidence therefore belongs to the existing opaque meta
        // lane for every delivery profile, never the agent content projection.
        result.meta = serde_json::json!({"plasm": {
            "status": "execution_failed", "failure": self.0
        }})
        .as_object()
        .cloned();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_failure_content_preserves_every_recorded_identity() {
        let mut failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Upstream,
            "rejected",
            "provider rejected item",
        );
        let identities: Vec<_> = (0..75)
            .map(|i| format!("Item/{i}/{}", "x".repeat(100)))
            .collect();
        failure
            .effects
            .push(plasm_runtime::execution_failure::EffectReceipt {
                resolved_effects: Vec::new(),
                entry_id: "matrix".into(),
                capability: "write".into(),
                completed: 75,
                failed: 0,
                occurrences: identities
                    .iter()
                    .enumerate()
                    .map(
                        |(i, identity)| plasm_runtime::execution_failure::EffectOccurrence {
                            scope_path: vec!["selected".into()],
                            occurrence_path: vec![i],
                            source_index: i,
                            source_identity: Some(identity.clone()),
                            status: plasm_runtime::OperationInvocationStatus::Completed,
                        },
                    )
                    .collect(),
            });
        let result = HostFault(failure)
            .into_tool_result(crate::mcp_delivery::McpDeliveryProfile::ContentOnly);
        let content = serde_json::to_string(&result.content).unwrap();
        for identity in identities {
            assert!(content.contains(&identity));
        }
        assert!(!content.contains("omitted"));
    }
    #[test]
    fn nested_machine_paths_are_not_agent_locations() {
        let failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Upstream,
            "provider_rejected",
            "Insufficient balance",
        )
        .at("ack/__py5/__scope7/selected", vec![1]);
        let result = HostFault(failure.clone())
            .into_tool_result(crate::mcp_delivery::McpDeliveryProfile::ContentOnly);
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("ack/selected"));
        assert!(!text.contains("__py"));
        assert!(!text.contains("__scope"));
        assert_eq!(
            result.meta.unwrap()["plasm"]["failure"]["node"],
            "ack/__py5/__scope7/selected"
        );
    }
    #[test]
    fn every_delivery_profile_separates_agent_text_from_machine_evidence() {
        use crate::mcp_delivery::McpDeliveryProfile;
        let failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Transport,
            "transport_error",
            "provider response unavailable",
        )
        .at("__py0", vec![1, 2]);
        for profile in [
            McpDeliveryProfile::ContentOnly,
            McpDeliveryProfile::ToolFallback,
            McpDeliveryProfile::FullApps,
        ] {
            let result = HostFault(failure.clone()).into_tool_result(profile);
            assert!(result.structured_content.is_none());
            let text = serde_json::to_string(&result.content).unwrap();
            assert!(text.contains("provider response unavailable"));
            assert!(!text.contains("occurrence_path"));
            assert!(!text.contains("__py0"));
            assert_eq!(
                result.meta.unwrap()["plasm"]["failure"],
                serde_json::to_value(&failure).unwrap()
            );
        }
    }

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
            result.meta.as_ref().unwrap()["plasm"]["failure"]["diagnostic"],
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
            serde_json::from_value(result.meta.unwrap()["plasm"]["failure"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(failure).unwrap()
        );
    }

    #[test]
    fn collection_evidence_fault_is_not_a_program_repair_or_debug_dump() {
        use plasm_core::collection_codec::{CollectionFault, CollectionIdentity, EvidenceGap, Gap};
        let identity = CollectionIdentity {
            catalog: [42; 32],
            expression: [99; 32],
            epoch: 0,
        };
        let fault = CollectionFault::Incomplete {
            identity: identity.clone(),
            coverage: plasm_core::collection_codec::ResultCoverage::Unknown,
            gaps: [EvidenceGap {
                source: identity,
                reason: Gap::UndeclaredMembership,
            }]
            .into(),
        };
        let result = HostFault(plasm_runtime::ExecutionFailure::from(fault))
            .into_tool_result(crate::mcp_delivery::McpDeliveryProfile::FullApps);
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("complete membership"));
        assert!(!text.contains("CollectionIdentity"));
        assert!(!text.contains("EvidenceGap"));
        assert!(!text.contains("Revise the program"));
        let failure = &result.meta.unwrap()["plasm"]["failure"];
        assert_eq!(failure["cause"], "response_contract");
        assert_eq!(failure["code"], "collection_incomplete");
        assert_eq!(failure["recovery"], "stop");
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
                resolved_effects: Vec::new(),
                entry_id: "fixture".into(),
                capability: "write".into(),
                completed: 1,
                failed: 0,
                occurrences: Vec::new(),
            });
        let text = crate::agent_presentation::render_failure(&failure);
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
                resolved_effects: Vec::new(),
                entry_id: "fixture".into(),
                capability: "write".into(),
                completed: 1,
                failed: 1,
                occurrences: vec![
                    plasm_runtime::execution_failure::EffectOccurrence {
                        scope_path: vec!["write".into()],
                        occurrence_path: vec![0],
                        source_index: 0,
                        source_identity: Some("Item/a".into()),
                        status: plasm_runtime::OperationInvocationStatus::Completed,
                    },
                    plasm_runtime::execution_failure::EffectOccurrence {
                        scope_path: vec!["write".into()],
                        occurrence_path: vec![1],
                        source_index: 1,
                        source_identity: Some("Item/b".into()),
                        status: plasm_runtime::OperationInvocationStatus::Failed,
                    },
                ],
            });
        failure.dispatches.push(plasm_runtime::MutationDispatch {
            resolved_effect: None,
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
        assert!(
            text.contains("1 completed [Item/a (occurrence [0] at write)]"),
            "{text}"
        );
        assert!(
            text.contains("1 failed [Item/b (occurrence [1] at write)]"),
            "{text}"
        );
        assert!(!text.contains("Dispatch history may overlap"), "{text}");
        assert!(!text.contains("completion unproven"), "{text}");
        assert!(result.structured_content.is_none());
    }
}
