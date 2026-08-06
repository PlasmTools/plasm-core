//! Semantic seed-set routing for intent-only `plasm_context`.

use std::time::Instant;

use anyhow::Context;
use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_coverage::retrieve_via_coverage;
use plasm_core::discovery_seed_pipeline::prepare_seed_retrieval;
use plasm_core::discovery_seed_select::{
    try_federation_ready_under_brand_lock, validate_seed_selection_with_brand_lock,
    SeedSelectionDecision, SeedSelectionValidationError, ValidatedSeedSelection,
};
use plasm_semantic_seed::{
    select_discovery_seeds, SelectorCatalogHost, SelectorConfig, SelectorRequest,
};
use tracing::info;

use crate::discovery_routing::AutoSeedRouteOutcome;

pub use plasm_core::discovery_seed_select::validation_error_label;

pub fn semantic_auto_seed_enabled() -> bool {
    std::env::var("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn openrouter_model() -> String {
    std::env::var("PLASM_DISCOVERY_AUTO_SEED_MODEL")
        .unwrap_or_else(|_| "openai/gpt-4.1-mini".into())
}

fn openrouter_api_key() -> anyhow::Result<String> {
    std::env::var("OPENROUTER_API_KEY")
        .context("OPENROUTER_API_KEY required for semantic auto-seed")
}

pub async fn route_intent_to_seeds<C>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<Vec<String>>,
    exclude_exposed: Option<&[(String, String)]>,
) -> AutoSeedRouteOutcome
where
    C: plasm_core::discovery::CgsDiscovery + plasm_core::discovery::CgsCatalog + Send + Sync,
{
    let started = Instant::now();
    let api_key = match openrouter_api_key() {
        Ok(key) => key,
        Err(error) => return routing_error(started, error),
    };
    let model = openrouter_model();
    let allowed = allowed_entry_ids.as_deref().unwrap_or_default();
    let prep = match prepare_seed_retrieval(catalog, intent, allowed) {
        Ok(prep) => prep,
        Err(error) => {
            return AutoSeedRouteOutcome::RoutingError {
                message: format!("retrieval prep failed: {error}"),
                selector_latency_ms: elapsed_ms(started),
            };
        }
    };

    let retrieved = match retrieve_via_coverage(
        catalog,
        intent,
        allowed_entry_ids.as_deref(),
        &prep.intent_class,
        &prep.named_catalogs,
    ) {
        Ok(retrieved) => retrieved,
        Err(error) => {
            return AutoSeedRouteOutcome::RoutingError {
                message: format!("candidate retrieval failed: {error}"),
                selector_latency_ms: elapsed_ms(started),
            };
        }
    };

    // Exclude already-exposed entities from the candidate pool so extend routes a delta.
    let mut bundles = retrieved.bundles;
    if let Some(exposed) = exclude_exposed {
        if !exposed.is_empty() {
            bundles.retain(|b| {
                !exposed.iter().any(|(e, n)| {
                    e == &b.entry_id && n.eq_ignore_ascii_case(&b.entity)
                })
            });
        }
    }
    if bundles.is_empty() {
        return AutoSeedRouteOutcome::Noop {
            reasoning: "all candidates already exposed on this session".into(),
            selector_latency_ms: elapsed_ms(started),
        };
    }

    let preview = preview_bundles(&bundles);
    let allowed_slice: Vec<String> = allowed_entry_ids.clone().unwrap_or_default();
    let raw = match select_discovery_seeds(
        SelectorRequest {
            intent,
            intent_class: &retrieved.intent_class,
            bundles: &bundles,
            catalog_context: &retrieved.catalog_context,
            brand_lock_catalogs: &retrieved.named_catalogs,
            candidate_graph: retrieved.candidate_graph.clone(),
        },
        SelectorConfig {
            client_name: "AutoSeedModel",
            model: &model,
            api_key: &api_key,
            temperature: 0.0,
            seed: 42,
        },
        Some(SelectorCatalogHost {
            catalog,
            allowed_entry_ids: if allowed_slice.is_empty() {
                &[]
            } else {
                &allowed_slice
            },
        }),
    ) {
        Ok(raw) => raw,
        Err(error) => return routing_error(started, error),
    };

    let latency = elapsed_ms(started);
    match validate_seed_selection_with_brand_lock(
        &raw,
        &bundles,
        Some(retrieved.named_catalogs.as_slice()),
    ) {
        Ok(ValidatedSeedSelection::Ready(ready)) => {
            let seeds = plasm_core::discovery_seed_select::seeds_from_candidate_ids(
                &bundles,
                &ready.selected_ids,
            );
            info!(
                target: "plasm_agent::discovery_auto_seed",
                seed_count = seeds.len(),
                candidate_count = bundles.len(),
                selector_latency_ms = latency,
                "semantic auto-seed ready"
            );
            AutoSeedRouteOutcome::Ready {
                seeds,
                teaching_satellites: ready.teaching_satellites,
                supporting_capability_ids: ready.supporting_capability_ids,
                requirements: ready.requirements,
                reasoning: ready.reasoning,
                candidate_preview: preview,
                selector_latency_ms: latency,
            }
        }
        Ok(ValidatedSeedSelection::Abstain(abstain)) => match abstain.decision {
            SeedSelectionDecision::Clarify => AutoSeedRouteOutcome::Clarify {
                requirements: abstain.requirements,
                alternative_sets: abstain.alternative_sets,
                reasoning: abstain.reasoning,
                candidate_preview: preview,
                selector_latency_ms: latency,
            },
            SeedSelectionDecision::HardMiss => AutoSeedRouteOutcome::HardMiss {
                requirements: abstain.requirements,
                uncovered_requirements: abstain.uncovered_requirements,
                reasoning: abstain.reasoning,
                candidate_preview: preview,
                selector_latency_ms: latency,
            },
            SeedSelectionDecision::Ready => AutoSeedRouteOutcome::RoutingError {
                message: "internal: ready nested in abstain".into(),
                selector_latency_ms: latency,
            },
        },
        Err(error) => {
            // Named multi-brand intents: selector often emits provider clarify; collapse to Ready.
            if matches!(
                &error,
                SeedSelectionValidationError::ClarifyUnderBrandLock(_)
            ) {
                if let Some(ready) = try_federation_ready_under_brand_lock(
                    &raw,
                    &bundles,
                    retrieved.named_catalogs.as_slice(),
                ) {
                    let seeds = plasm_core::discovery_seed_select::seeds_from_candidate_ids(
                        &bundles,
                        &ready.selected_ids,
                    );
                    info!(
                        target: "plasm_agent::discovery_auto_seed",
                        seed_count = seeds.len(),
                        candidate_count = bundles.len(),
                        selector_latency_ms = latency,
                        "semantic auto-seed ready (federation brand_lock repair)"
                    );
                    return AutoSeedRouteOutcome::Ready {
                        seeds,
                        teaching_satellites: ready.teaching_satellites,
                        supporting_capability_ids: ready.supporting_capability_ids,
                        requirements: ready.requirements,
                        reasoning: ready.reasoning,
                        candidate_preview: preview,
                        selector_latency_ms: latency,
                    };
                }
            }
            AutoSeedRouteOutcome::RoutingError {
                message: format!("selector validation: {error}"),
                selector_latency_ms: latency,
            }
        }
    }
}

fn preview_bundles(bundles: &[EntityCandidateBundle]) -> Vec<EntityCandidateBundle> {
    bundles.iter().take(3).cloned().collect()
}

fn routing_error(started: Instant, error: impl std::fmt::Display) -> AutoSeedRouteOutcome {
    AutoSeedRouteOutcome::RoutingError {
        message: error.to_string(),
        selector_latency_ms: elapsed_ms(started),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}
