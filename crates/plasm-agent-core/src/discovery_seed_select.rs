//! Semantic seed-set selection for intent-only `plasm_context` (`semantic-auto-seed` feature).

use std::time::Instant;

use crate::baml_client::sync_client::B;
use crate::baml_client::types::{
    CandidateSeedBundle as BamlBundle, EntityCapabilityEvidence as BamlCap,
    SeedBundleProviderGroup as BamlProviderGroup, SeedBundleRoot as BamlRoot,
    SeedCoverageAssessment as BamlAssessment,
};
use crate::baml_client::ClientRegistry;
use crate::discovery_routing::AutoSeedRouteOutcome;
use anyhow::Context;
use plasm_core::discovery_auto_seed::{EntityCandidateBundle, EntityCandidateConfig};
use plasm_core::discovery_seed_baml::{
    build_seed_selection_presentation, empty_seed_selection_raw, resolve_from_llm_coverage,
    SeedPresentationBundle, SeedPresentationProviderGroup,
};
use plasm_core::discovery_seed_select::{
    validate_seed_selection, SeedSelectionDecision, SeedSelectionRaw,
    SeedSelectionValidationError, ValidatedSeedSelection,
};
use tracing::info;

pub use plasm_core::discovery_seed_select::validation_error_label;

pub fn semantic_auto_seed_enabled() -> bool {
    std::env::var("PLASM_DISCOVERY_SEMANTIC_AUTO_SEED")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn openrouter_model() -> String {
    std::env::var("PLASM_DISCOVERY_AUTO_SEED_MODEL").unwrap_or_else(|_| "openai/gpt-4.1-mini".into())
}

fn openrouter_api_key() -> anyhow::Result<String> {
    std::env::var("OPENROUTER_API_KEY")
        .context("OPENROUTER_API_KEY required for semantic auto-seed")
}

fn to_baml_bundles(bundles: &[SeedPresentationBundle]) -> Vec<BamlBundle> {
    bundles
        .iter()
        .map(|bundle| BamlBundle {
            bundle_index: bundle.bundle_index as i64,
            catalogs: bundle.catalogs.clone(),
            roots: bundle
                .roots
                .iter()
                .map(|root| BamlRoot {
                    catalog: root.catalog.clone(),
                    entity: root.entity.clone(),
                    entity_description: root.entity_description.clone(),
                    capabilities: root
                        .capabilities
                        .iter()
                        .map(|capability| BamlCap {
                            capability_name: capability.capability_name.clone(),
                            kind: capability.kind.clone(),
                            description: capability.description.clone(),
                            lexical_score: capability.lexical_score as i64,
                        })
                        .collect(),
                    relation_coverage: root.relation_coverage.clone(),
                })
                .collect(),
        })
        .collect()
}

fn to_baml_provider_groups(groups: &[SeedPresentationProviderGroup]) -> Vec<BamlProviderGroup> {
    groups
        .iter()
        .map(|group| BamlProviderGroup {
            provider_index: group.provider_index as i64,
            catalogs: group.catalogs.clone(),
            bundle_indexes: group
                .bundle_indexes
                .iter()
                .map(|index| *index as i64)
                .collect(),
            capability_sketch: group.capability_sketch.clone(),
        })
        .collect()
}

fn from_baml(
    assessment: &BamlAssessment,
    presentation: &plasm_core::discovery_seed_baml::SeedSelectionPresentation,
    intent: &str,
) -> Result<SeedSelectionRaw, SeedSelectionValidationError> {
    let requirements = assessment
        .requirements
        .iter()
        .map(|requirement| {
            (
                requirement.requirement_index,
                requirement.text.clone(),
                requirement.depends_on_indexes.clone(),
            )
        })
        .collect();
    let coverage_rows = assessment
        .coverage_rows
        .iter()
        .map(|row| (row.requirement_index, row.supporting_bundle_indexes.clone()))
        .collect();
    resolve_from_llm_coverage(
        requirements,
        coverage_rows,
        assessment.reasoning.clone(),
        &presentation.tables,
        intent,
    )
}

fn preview_bundles(bundles: &[EntityCandidateBundle]) -> Vec<EntityCandidateBundle> {
    bundles.iter().take(3).cloned().collect()
}

pub async fn route_intent_to_seeds<C>(
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<Vec<String>>,
) -> AutoSeedRouteOutcome
where
    C: plasm_core::discovery::CgsDiscovery + plasm_core::discovery::CgsCatalog + Send + Sync,
{
    let started = Instant::now();
    let config = EntityCandidateConfig::default();
    let bundles = match plasm_core::discovery_auto_seed::retrieve_entity_candidate_bundles(
        catalog,
        intent,
        allowed_entry_ids.as_deref(),
        config,
    ) {
        Ok(b) => b,
        Err(e) => {
            return AutoSeedRouteOutcome::RoutingError {
                message: format!("candidate retrieval failed: {e}"),
                selector_latency_ms: started.elapsed().as_millis() as u64,
            };
        }
    };

    let preview = preview_bundles(&bundles);
    let api_key = match openrouter_api_key() {
        Ok(k) => k,
        Err(e) => {
            return AutoSeedRouteOutcome::RoutingError {
                message: e.to_string(),
                selector_latency_ms: started.elapsed().as_millis() as u64,
            };
        }
    };

    let raw = match call_selector(intent, &bundles, &openrouter_model(), &api_key) {
        Ok(r) => r,
        Err(e) => {
            return AutoSeedRouteOutcome::RoutingError {
                message: e.to_string(),
                selector_latency_ms: started.elapsed().as_millis() as u64,
            };
        }
    };

    let latency = started.elapsed().as_millis() as u64;
    match validate_seed_selection(&raw, &bundles) {
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
        Err(e) => AutoSeedRouteOutcome::RoutingError {
            message: format!("selector validation: {e}"),
            selector_latency_ms: latency,
        },
    }
}

fn call_selector(
    intent: &str,
    bundles: &[EntityCandidateBundle],
    model: &str,
    api_key: &str,
) -> anyhow::Result<SeedSelectionRaw> {
    let presentation = match build_seed_selection_presentation(
        intent,
        bundles,
        plasm_core::discovery_seed_bundle::SeedBundleConfig::default(),
    )? {
        Some(presentation) => presentation,
        None => return Ok(empty_seed_selection_raw()),
    };
    crate::baml_client::init();
    let baml_bundles = to_baml_bundles(&presentation.bundles);
    let baml_provider_groups = to_baml_provider_groups(&presentation.provider_groups);
    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        "AutoSeedModel",
        "openai-generic",
        plasm_eval_common::openrouter_eval_llm_options(model, api_key, 0.0, 42),
    );
    registry.set_primary_client("AutoSeedModel");

    let mut last_err = None;
    for attempt in 0..3u32 {
        match B
            .SelectDiscoverySeeds
            .with_client_registry(&registry)
            .call(intent, baml_provider_groups.as_slice(), baml_bundles.as_slice())
        {
            Ok(assessment) => {
                return from_baml(&assessment, &presentation, intent)
                    .context("resolve seed coverage assessment");
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    tracing::warn!(
                        target: "plasm_agent::discovery_auto_seed",
                        attempt = attempt + 1,
                        "SelectDiscoverySeeds retry"
                    );
                }
            }
        }
    }
    Err(last_err
        .map(|e| anyhow::anyhow!("BAML SelectDiscoverySeeds: {e}"))
        .unwrap_or_else(|| anyhow::anyhow!("BAML SelectDiscoverySeeds failed")))
}
