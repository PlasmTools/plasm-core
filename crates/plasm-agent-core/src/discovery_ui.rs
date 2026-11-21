//! Human-facing search status alongside the session declarations in MCP content.
use serde_json::{json, Value};

pub(crate) const URI: &str = "ui://plasm/discovery";
pub(crate) const HTML: &str = include_str!("mcp_app/discovery.html");

pub(crate) fn project(
    intent: &str,
    receipt: &crate::discovery_service::RoutingReceipt,
    waves: &[crate::http_execute::CapabilityWaveOutcome],
    uncertain_calls: &[String],
) -> Value {
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for item in &receipt.search.deferred {
        let reason = serde_json::to_value(&item.reason)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        *reasons.entry(reason).or_default() += 1;
    }
    json!({
        "intent": intent,
        "declarations": waves.iter().filter(|wave| wave.teaching_prompt_chars_added > 0)
            .map(|wave| wave.markdown_delta.trim()).filter(|text| !text.is_empty()).collect::<Vec<_>>(),
        "uncertain_calls": uncertain_calls,
        "provider_unavailable": receipt.search.has_unavailable_provider(),
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
    fn projection_preserves_intent_verified_call_warnings_and_search_limits() {
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
        let value = project(
            "Find the requested records.",
            &route,
            &[],
            &["e3.query".into()],
        );
        assert_eq!(value["intent"], "Find the requested records.");
        assert_eq!(value["uncertain_calls"], json!(["e3.query"]));
        assert!(value.get("capabilities").is_none());
        assert_eq!(value["judged"], 1);
        assert_eq!(value["deferred"], 1);
        assert_eq!(value["unexplored"], 1);
        assert_eq!(value["deferred_reasons"]["input_bytes"], 1);
        assert!(value.get("authorization").is_none());
    }

    #[test]
    fn view_uses_the_session_declarations_verbatim() {
        let declaration = "# matrix::Record\ne3:\n  e3.query() -> Rows[e3]";
        let wave = crate::http_execute::CapabilityWaveOutcome {
            mode: "new".into(),
            entry_id: "matrix".into(),
            entities: vec!["Record".into()],
            markdown_delta: declaration.into(),
            reused_session: false,
            teaching_prompt_chars_added: declaration.len() as u64,
            relations_delta: vec![],
        };
        let value = project("Find records.", &receipt(), &[wave], &[]);
        assert_eq!(value["declarations"], json!([declaration]));
        assert!(value.get("capabilities").is_none());
    }

    #[test]
    fn provider_outage_is_independent_of_selection_and_budget() {
        for (reason, unavailable) in [
            (DeferredReason::InputBytes, false),
            (DeferredReason::OperationUncertain, false),
            (DeferredReason::ProviderUnavailable, true),
        ] {
            let mut route = receipt();
            route.search.deferred = vec![DeferredCandidate {
                reference: route.closure.as_ref().unwrap().business[0].clone(),
                reason,
            }];
            let view = project("Read records.", &route, &[], &[]);
            assert_eq!(view["provider_unavailable"], unavailable);
            assert!(view["has_closure"].as_bool().unwrap());
        }
    }

    #[test]
    fn unmatched_receipt_does_not_invent_capabilities() {
        let mut route = receipt();
        route.closure = None;
        let value = project("Find records.", &route, &[], &[]);
        assert_eq!(value["has_closure"], false);
        assert_eq!(value["uncertain_calls"], json!([]));
    }

    #[test]
    fn delivery_keeps_agent_text_and_mounts_only_for_app_hosts() {
        use crate::mcp_delivery::McpDeliveryProfile;
        let payload = project("Find records.", &receipt(), &[], &[]);
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
