//! Breakout markdown + `_meta.plasm.routing` for intent-only abstention paths.

use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_seed_select::SeedAlternativeSetRaw;

/// Outcome of auto-seed routing before session mint.
#[derive(Debug, Clone)]
pub enum AutoSeedRouteOutcome {
    Ready {
        seeds: Vec<(String, String)>,
        supporting_capability_ids: Vec<String>,
        requirements: Vec<String>,
        reasoning: String,
        candidate_preview: Vec<EntityCandidateBundle>,
        selector_latency_ms: u64,
    },
    Clarify {
        requirements: Vec<String>,
        alternative_sets: Vec<SeedAlternativeSetRaw>,
        reasoning: String,
        candidate_preview: Vec<EntityCandidateBundle>,
        selector_latency_ms: u64,
    },
    HardMiss {
        requirements: Vec<String>,
        uncovered_requirements: Vec<String>,
        reasoning: String,
        candidate_preview: Vec<EntityCandidateBundle>,
        selector_latency_ms: u64,
    },
    RoutingError {
        message: String,
        selector_latency_ms: u64,
    },
}

pub fn build_auto_seed_breakout_markdown(outcome: &AutoSeedRouteOutcome) -> String {
    match outcome {
        AutoSeedRouteOutcome::Clarify {
            alternative_sets,
            reasoning,
            ..
        } => {
            let mut lines = vec![
                "## Clarify before opening a session".into(),
                String::new(),
                "Multiple plausible API/entity bundles match this intent. Ask the user which applies, then retry `plasm_context` with `session_mode: \"new\"` and explicit `seeds`.".into(),
            ];
            if !reasoning.is_empty() {
                lines.push(format!("Selector note: {reasoning}"));
            }
            lines.push(String::new());
            lines.push("Alternatives:".into());
            for (i, alt) in alternative_sets.iter().enumerate() {
                lines.push(format!("{}. {} — {:?}", i + 1, alt.label, alt.candidate_ids));
            }
            lines.join("\n")
        }
        AutoSeedRouteOutcome::HardMiss {
            uncovered_requirements,
            reasoning,
            ..
        } => {
            let mut lines = vec![
                "## No adequate catalog match".into(),
                String::new(),
                "Call `discover_capabilities` to browse tenant-safe capabilities, narrow the intent with vendor/action/domain terms, or retry with explicit `seeds` if you already know the `api`/`entity`.".into(),
            ];
            if !uncovered_requirements.is_empty() {
                lines.push(format!(
                    "Uncovered: {}",
                    uncovered_requirements.join("; ")
                ));
            }
            if !reasoning.is_empty() {
                lines.push(format!("Selector note: {reasoning}"));
            }
            lines.join("\n")
        }
        AutoSeedRouteOutcome::RoutingError { message, .. } => format!(
            "## Selector unavailable (`routing_error`)\n\nSemantic auto-seed failed: {message}\n\nRetry `plasm_context`, pass explicit `seeds`, or call `discover_capabilities`."
        ),
        AutoSeedRouteOutcome::Ready { .. } => String::new(),
    }
}

pub fn build_routing_meta(
    outcome: &AutoSeedRouteOutcome,
    selector_mode: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut routing = serde_json::Map::new();
    routing.insert("selector_mode".into(), serde_json::json!(selector_mode));
    match outcome {
        AutoSeedRouteOutcome::Ready {
            seeds,
            supporting_capability_ids,
            requirements,
            candidate_preview,
            selector_latency_ms,
            ..
        } => {
            routing.insert("decision".into(), serde_json::json!("ready"));
            routing.insert(
                "selected_seeds".into(),
                serde_json::json!(seeds
                    .iter()
                    .map(|(api, ent)| serde_json::json!({ "api": api, "entity": ent }))
                    .collect::<Vec<_>>()),
            );
            routing.insert(
                "supporting_capability_ids".into(),
                serde_json::json!(supporting_capability_ids),
            );
            routing.insert("requirements".into(), serde_json::json!(requirements));
            routing.insert(
                "candidate_preview".into(),
                serde_json::json!(tenant_safe_preview(candidate_preview)),
            );
            routing.insert(
                "selector_latency_ms".into(),
                serde_json::json!(selector_latency_ms),
            );
        }
        AutoSeedRouteOutcome::Clarify {
            alternative_sets,
            requirements,
            candidate_preview,
            selector_latency_ms,
            ..
        } => {
            routing.insert("decision".into(), serde_json::json!("clarify"));
            routing.insert("requirements".into(), serde_json::json!(requirements));
            routing.insert("alternatives".into(), serde_json::json!(alternative_sets));
            routing.insert(
                "candidate_preview".into(),
                serde_json::json!(tenant_safe_preview(candidate_preview)),
            );
            routing.insert(
                "selector_latency_ms".into(),
                serde_json::json!(selector_latency_ms),
            );
        }
        AutoSeedRouteOutcome::HardMiss {
            uncovered_requirements,
            requirements,
            candidate_preview,
            selector_latency_ms,
            ..
        } => {
            routing.insert("decision".into(), serde_json::json!("hard_miss"));
            routing.insert("requirements".into(), serde_json::json!(requirements));
            routing.insert(
                "uncovered_requirements".into(),
                serde_json::json!(uncovered_requirements),
            );
            routing.insert(
                "candidate_preview".into(),
                serde_json::json!(tenant_safe_preview(candidate_preview)),
            );
            routing.insert(
                "selector_latency_ms".into(),
                serde_json::json!(selector_latency_ms),
            );
        }
        AutoSeedRouteOutcome::RoutingError {
            selector_latency_ms,
            ..
        } => {
            routing.insert("decision".into(), serde_json::json!("routing_error"));
            routing.insert(
                "selector_latency_ms".into(),
                serde_json::json!(selector_latency_ms),
            );
        }
    }
    routing
}

fn tenant_safe_preview(bundles: &[EntityCandidateBundle]) -> Vec<serde_json::Value> {
    bundles
        .iter()
        .map(|b| {
            serde_json::json!({
                "api": b.entry_id,
                "entity": b.entity,
                "description": b.entity_description,
                "top_capabilities": b.capabilities.iter().take(2).map(|c| &c.capability_name).collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clarify_breakout_mentions_user() {
        let md = build_auto_seed_breakout_markdown(&AutoSeedRouteOutcome::Clarify {
            requirements: vec!["invoices".into()],
            alternative_sets: vec![
                SeedAlternativeSetRaw {
                    candidate_ids: vec!["gmail:Thread".into()],
                    label: "Gmail".into(),
                },
                SeedAlternativeSetRaw {
                    candidate_ids: vec!["outlook:Message".into()],
                    label: "Outlook".into(),
                },
            ],
            reasoning: String::new(),
            candidate_preview: vec![],
            selector_latency_ms: 1,
        });
        assert!(md.contains("explicit `seeds`"));
        assert!(md.contains("Alternatives"));
    }
}
