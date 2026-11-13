//! Human-facing projection of one discovery receipt. Agent teaching is unchanged.
use serde_json::{json, Value};

pub(crate) const URI: &str = "ui://plasm/discovery";
pub(crate) const HTML: &str = include_str!("mcp_app/discovery.html");

pub(crate) fn project(intent: &str, receipt: &crate::discovery_service::RoutingReceipt) -> Value {
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for item in &receipt.search.deferred {
        let reason = serde_json::to_value(&item.reason)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        *reasons.entry(reason).or_default() += 1;
    }
    let capabilities = receipt
        .closure
        .as_ref()
        .map(|closure| {
            closure
                .business
                .iter()
                .map(|reference| {
                    let document = receipt
                        .retrieval
                        .candidates
                        .iter()
                        .find(|candidate| &candidate.reference == reference)
                        .map(|candidate| &candidate.document);
                    json!({
                        "catalog": reference.catalog,
                        "capability": reference.capability,
                        "description": document.map(|d| d.operation.contract.as_str()),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "intent": intent,
        "capabilities": capabilities,
        "judged": receipt.matching.matches.len(),
        "unexplored": receipt.search.unexplored.len(),
        "deferred": receipt.search.deferred.len(),
        "deferred_reasons": reasons,
        "lexical_truncated": receipt.retrieval.lexical_truncated,
        "vector_truncated": receipt.retrieval.vector_truncated,
        "has_closure": receipt.closure.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_matcher::online::{DeferredCandidate, DeferredReason};
    use plasm_core::prerequisites::CapabilityRef;

    fn receipt() -> crate::discovery_service::RoutingReceipt {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        serde_json::from_value(fixture["routing"].clone()).unwrap()
    }

    #[test]
    fn projection_preserves_intent_selected_evidence_and_unknowns() {
        let mut route = receipt();
        route.search.deferred.push(DeferredCandidate {
            reference: CapabilityRef {
                catalog: "matrix".into(),
                capability: "later".into(),
            },
            reason: DeferredReason::InputBytes,
        });
        route.search.unexplored.insert(CapabilityRef {
            catalog: "matrix".into(),
            capability: "later".into(),
        });
        let value = project("Find the requested records.", &route);
        assert_eq!(value["intent"], "Find the requested records.");
        assert_eq!(value["capabilities"][0]["capability"], "record_read");
        assert_eq!(
            value["capabilities"][0]["description"],
            "Read selected records."
        );
        assert_eq!(value["judged"], 1);
        assert_eq!(value["deferred"], 1);
        assert_eq!(value["unexplored"], 1);
        assert_eq!(value["deferred_reasons"]["input_bytes"], 1);
        assert!(value.get("authorization").is_none());
    }

    #[test]
    fn unmatched_receipt_does_not_invent_capabilities() {
        let mut route = receipt();
        route.closure = None;
        let value = project("Find records.", &route);
        assert_eq!(value["has_closure"], false);
        assert_eq!(value["capabilities"], json!([]));
    }

    #[test]
    fn delivery_keeps_agent_text_and_mounts_only_for_app_hosts() {
        use crate::mcp_delivery::McpDeliveryProfile;
        let payload = project("Find records.", &receipt());
        for (profile, ui_expected) in [
            (McpDeliveryProfile::ContentOnly, false),
            (McpDeliveryProfile::ToolFallback, true),
            (McpDeliveryProfile::FullApps, true),
        ] {
            let mut meta = serde_json::Map::new();
            meta.insert("discovery".into(), payload.clone());
            let result = crate::mcp_ui_payload::DualLaneToolResult {
                content: "agent teaching".into(),
                plasm_meta: meta,
                profile,
                inline_plan_ui: None,
            }
            .into_call_tool_result();
            let wire = serde_json::to_value(&result).unwrap();
            assert_eq!(wire["content"][0]["text"], "agent teaching");
            assert_eq!(
                wire["_meta"]["ui"]["resourceUri"].as_str().is_some(),
                ui_expected
            );
            if matches!(profile, McpDeliveryProfile::FullApps) {
                assert_eq!(wire["structuredContent"]["ui"]["discovery"], payload);
            } else {
                assert!(wire.get("structuredContent").is_none());
            }
        }
        assert!(crate::mcp_app::bundle_for_uri(URI).is_some());
    }
}
