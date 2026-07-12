//! Breakout markdown + `_meta.plasm.routing` for intent-only abstention paths.

use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_intent_signals::{
    intent_mentions_repo_path, intent_names_catalog,
};
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

const BRAND_LOCK_BEST_EFFORT_MARKER: &str = "brand_lock_best_effort:";

pub fn build_auto_seed_breakout_markdown(outcome: &AutoSeedRouteOutcome, intent: &str) -> String {
    match outcome {
        AutoSeedRouteOutcome::Clarify {
            alternative_sets, ..
        } => format_clarify_breakout(alternative_sets),
        AutoSeedRouteOutcome::HardMiss {
            uncovered_requirements,
            candidate_preview,
            ..
        } => format_hard_miss_breakout(intent, candidate_preview, uncovered_requirements),
        AutoSeedRouteOutcome::RoutingError { message, .. } => format!(
            "## Couldn't route this intent\n\n{message}\n\nRetry `plasm_context` with explicit `seeds`."
        ),
        AutoSeedRouteOutcome::Ready { .. } => String::new(),
    }
}

fn format_clarify_breakout(alternative_sets: &[SeedAlternativeSetRaw]) -> String {
    let mut lines = vec![
        "## Which provider?".into(),
        String::new(),
        "Ask the user, then retry `plasm_context` with `session_mode: \"new\"` and explicit `seeds`."
            .into(),
        String::new(),
    ];
    for (i, alt) in alternative_sets.iter().enumerate() {
        let entities = format_candidate_ids(&alt.candidate_ids);
        lines.push(format!("{}. {} — {entities}", i + 1, alt.label));
    }
    lines.join("\n")
}

fn format_hard_miss_breakout(
    intent: &str,
    candidate_preview: &[EntityCandidateBundle],
    uncovered_requirements: &[String],
) -> String {
    let mut lines = vec![
        "## Couldn't auto-open a session".into(),
        String::new(),
        "**Next:** retry `plasm_context` with explicit `seeds` for the provider in your intent:"
            .into(),
    ];
    for hint in seed_hints_from_preview(intent, candidate_preview, 3) {
        lines.push(format!(
            "- `seeds: [{{api: \"{}\", entity: \"{}\"}}]`",
            hint.entry_id, hint.entity
        ));
    }
    if lines.len() == 3 {
        lines.push("- `seeds: [{api: \"<catalog>\", entity: \"<Entity>\"}]".into());
    }
    lines.push(String::new());
    lines.push(
        "Use `session_mode: \"extend\"` to add more entities, then write `plasm.program` from the teaching TSV."
            .into(),
    );
    if let Some(gap) = format_gap_summary(uncovered_requirements) {
        lines.push(String::new());
        lines.push(gap);
    }
    lines.push(String::new());
    lines.push(
        "Or call `discover_capabilities` to browse what's available.".into(),
    );
    lines.join("\n")
}

struct SeedHint {
    entry_id: String,
    entity: String,
}

fn seed_hints_from_preview(
    intent: &str,
    preview: &[EntityCandidateBundle],
    max: usize,
) -> Vec<SeedHint> {
    let mut hints: Vec<SeedHint> = preview
        .iter()
        .filter(|b| brand_locked_catalog(intent).is_none_or(|c| b.entry_id == c))
        .map(|b| SeedHint {
            entry_id: b.entry_id.clone(),
            entity: b.entity.clone(),
        })
        .collect();
    if intent_mentions_repo_path(intent) {
        hints.sort_by(|a, b| {
            repo_entity_rank(&a.entity)
                .cmp(&repo_entity_rank(&b.entity))
                .then_with(|| a.entity.cmp(&b.entity))
        });
    }
    hints.truncate(max);
    hints
}

fn repo_entity_rank(entity: &str) -> u8 {
    match entity {
        "Repository" => 0,
        "Issue" => 1,
        "PullRequest" => 2,
        "Branch" => 3,
        "Label" => 4,
        _ => 10,
    }
}

fn brand_locked_catalog(intent: &str) -> Option<String> {
    let mut matched = Vec::new();
    for catalog in [
        "github",
        "gitlab",
        "linear",
        "clickup",
        "gmail",
        "google-calendar",
        "google-sheets",
        "google-docs",
        "google-drive",
        "jira",
        "slack",
        "notion",
        "cloudflare",
        "tavily",
    ] {
        if intent_names_catalog(catalog, intent) {
            matched.push(catalog.to_string());
        }
    }
    if matched.len() == 1 {
        matched.into_iter().next()
    } else {
        None
    }
}

fn format_gap_summary(uncovered: &[String]) -> Option<String> {
    if uncovered.is_empty() {
        return None;
    }
    let show = uncovered.len().min(3);
    let mut parts: Vec<String> = uncovered.iter().take(show).cloned().collect();
    if uncovered.len() > show {
        parts.push(format!("and {} more", uncovered.len() - show));
    }
    Some(format!(
        "Some steps may need explicit seeds or a smaller intent: {}.",
        parts.join("; ")
    ))
}

fn format_candidate_ids(candidate_ids: &[String]) -> String {
    candidate_ids
        .iter()
        .map(|id| {
            id.split_once(':')
                .map(|(_, entity)| entity.to_string())
                .unwrap_or_else(|| id.clone())
        })
        .collect::<Vec<_>>()
        .join(", ")
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
            reasoning,
        } => {
            routing.insert("decision".into(), serde_json::json!("ready"));
            if reasoning.contains(BRAND_LOCK_BEST_EFFORT_MARKER) {
                routing.insert("best_effort".into(), serde_json::json!(true));
            }
            insert_selector_reasoning(&mut routing, reasoning);
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
            reasoning,
        } => {
            routing.insert("decision".into(), serde_json::json!("clarify"));
            insert_selector_reasoning(&mut routing, reasoning);
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
            reasoning,
        } => {
            routing.insert("decision".into(), serde_json::json!("hard_miss"));
            insert_selector_reasoning(&mut routing, reasoning);
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
            message,
        } => {
            routing.insert("decision".into(), serde_json::json!("routing_error"));
            routing.insert("message".into(), serde_json::json!(message));
            routing.insert(
                "selector_latency_ms".into(),
                serde_json::json!(selector_latency_ms),
            );
        }
    }
    routing
}

fn insert_selector_reasoning(routing: &mut serde_json::Map<String, serde_json::Value>, reasoning: &str) {
    if !reasoning.is_empty() {
        routing.insert(
            "selector_reasoning".into(),
            serde_json::json!(reasoning),
        );
    }
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

    fn preview_bundle(entry_id: &str, entity: &str) -> EntityCandidateBundle {
        EntityCandidateBundle {
            candidate_id: format!("{entry_id}:{entity}"),
            entry_id: entry_id.into(),
            entity: entity.into(),
            entity_description: String::new(),
            max_lexical_score: 1,
            capabilities: vec![],
            relation_hints: String::new(),
            catalog_route_evidence: false,
        }
    }

    #[test]
    fn clarify_breakout_is_brief_and_human() {
        let md = build_auto_seed_breakout_markdown(
            &AutoSeedRouteOutcome::Clarify {
                requirements: vec!["invoices".into()],
                alternative_sets: vec![
                    SeedAlternativeSetRaw {
                        candidate_ids: vec!["github:Issue".into(), "github:Repository".into()],
                        label: "GitHub".into(),
                    },
                    SeedAlternativeSetRaw {
                        candidate_ids: vec!["clickup:Task".into()],
                        label: "ClickUp".into(),
                    },
                ],
                reasoning: "internal bundle 0 vs 1".into(),
                candidate_preview: vec![],
                selector_latency_ms: 1,
            },
            "tasks on my plate",
        );
        assert!(md.contains("Which provider?"));
        assert!(md.contains("explicit `seeds`"));
        assert!(md.contains("GitHub — Issue, Repository"));
        assert!(!md.contains("Selector note"));
        assert!(!md.contains("bundle_index"));
    }

    #[test]
    fn hard_miss_breakout_suggests_seeds_without_internal_jargon() {
        let md = build_auto_seed_breakout_markdown(
            &AutoSeedRouteOutcome::HardMiss {
                requirements: vec!["create issue".into()],
                uncovered_requirements: vec![
                    "Create an issue".into(),
                    "Create a branch".into(),
                    "Open a pull request".into(),
                    "Add labels".into(),
                ],
                reasoning: "bundle 0 lacks create; bundle 5 lacks branch".into(),
                candidate_preview: vec![
                    preview_bundle("github", "Repository"),
                    preview_bundle("github", "Issue"),
                ],
                selector_latency_ms: 1,
            },
            "GitHub repo ryan-s-roberts/tool-test: create issue and branch",
        );
        assert!(md.contains("Couldn't auto-open"));
        assert!(md.contains("github"));
        assert!(md.contains("Repository"));
        assert!(!md.contains("Selector note"));
        assert!(!md.contains("Uncovered:"));
        assert!(!md.contains("bundle_index"));
        let meta = build_routing_meta(
            &AutoSeedRouteOutcome::HardMiss {
                requirements: vec!["create issue".into()],
                uncovered_requirements: vec!["Create an issue".into()],
                reasoning: "internal".into(),
                candidate_preview: vec![],
                selector_latency_ms: 1,
            },
            "semantic",
        );
        assert_eq!(
            meta.get("selector_reasoning").and_then(|v| v.as_str()),
            Some("internal")
        );
    }
}
