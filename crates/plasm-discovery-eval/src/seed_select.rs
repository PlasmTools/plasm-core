//! LLM requirement-to-bundle coverage assessment for seed selection.

use anyhow::Context;
use plasm_core::discovery_auto_seed::EntityCandidateBundle;
use plasm_core::discovery_seed_baml::{
    build_seed_selection_presentation, empty_seed_selection_raw, resolve_from_llm_coverage,
    SeedPresentationBundle, SeedPresentationProviderGroup, SeedSelectionPresentation,
};
use plasm_core::discovery_seed_bundle::SeedBundleConfig;
use plasm_core::discovery_seed_select::{SeedSelectionRaw, SeedSelectionValidationError};
use plasm_eval_common::openrouter_eval_llm_options;

use crate::baml_client::sync_client::B;
use crate::baml_client::types::{
    CandidateSeedBundle as BamlBundle, EntityCapabilityEvidence as BamlCap,
    SeedBundleProviderGroup as BamlProviderGroup, SeedBundleRoot as BamlRoot,
    SeedCoverageAssessment as BamlAssessment,
};
use crate::baml_client::ClientRegistry;

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
    presentation: &SeedSelectionPresentation,
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

#[cfg(feature = "llm-rerank")]
pub fn select_discovery_seeds(
    intent: &str,
    bundles: &[EntityCandidateBundle],
    model: &str,
    api_key: &str,
    temperature: f64,
    seed: u64,
) -> anyhow::Result<SeedSelectionRaw> {
    let presentation = match build_seed_selection_presentation(intent, bundles, SeedBundleConfig::default())? {
        Some(presentation) => presentation,
        None => return Ok(empty_seed_selection_raw()),
    };

    crate::baml_client::init();
    let baml_bundles = to_baml_bundles(&presentation.bundles);
    let baml_provider_groups = to_baml_provider_groups(&presentation.provider_groups);
    let mut registry = ClientRegistry::new();
    registry.add_llm_client(
        "EvalModel",
        "openai-generic",
        openrouter_eval_llm_options(model, api_key, temperature, seed),
    );
    registry.set_primary_client("EvalModel");

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
            Err(error) => {
                last_err = Some(error);
                if attempt < 2 {
                    eprintln!("seed-select retry {}/2", attempt + 1);
                }
            }
        }
    }
    Err(last_err
        .map(|error| anyhow::anyhow!("BAML SelectDiscoverySeeds: {error}"))
        .unwrap_or_else(|| anyhow::anyhow!("BAML SelectDiscoverySeeds failed")))
    .context("LLM seed coverage assessment")
}

#[cfg(not(feature = "llm-rerank"))]
pub fn select_discovery_seeds(
    _intent: &str,
    _bundles: &[EntityCandidateBundle],
    _model: &str,
    _api_key: &str,
    _temperature: f64,
    _seed: u64,
) -> anyhow::Result<SeedSelectionRaw> {
    anyhow::bail!("rebuild with --features llm-rerank")
}
