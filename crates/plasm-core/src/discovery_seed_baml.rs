//! Shared presentation layer for LLM seed selection (BAML-bound crates map these to generated types).

use std::collections::HashMap;

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_seed_bundle::{
    build_candidate_seed_bundles, CandidateSeedBundle, SeedBundleConfig,
};
use crate::discovery_seed_select::{
    build_seed_bundle_index_tables, build_seed_bundle_provider_groups,
    resolve_seed_coverage_assessment, seed_bundle_presentation_order, SeedBundleIndexTables,
    SeedSelectionDecision, SeedSelectionRaw, SeedSelectionValidationError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPresentationCapability {
    pub capability_name: String,
    pub kind: String,
    pub description: String,
    pub lexical_score: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPresentationRoot {
    pub catalog: String,
    pub entity: String,
    pub entity_description: String,
    pub capabilities: Vec<SeedPresentationCapability>,
    pub relation_coverage: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPresentationBundle {
    pub bundle_index: usize,
    pub catalogs: Vec<String>,
    pub roots: Vec<SeedPresentationRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPresentationProviderGroup {
    pub provider_index: usize,
    pub catalogs: Vec<String>,
    pub bundle_indexes: Vec<usize>,
    pub capability_sketch: String,
}

#[derive(Debug, Clone)]
pub struct SeedSelectionPresentation {
    pub seed_bundles: Vec<CandidateSeedBundle>,
    pub presentation_order: Vec<usize>,
    pub bundles: Vec<SeedPresentationBundle>,
    pub provider_groups: Vec<SeedPresentationProviderGroup>,
    pub tables: SeedBundleIndexTables,
}

pub fn empty_seed_selection_raw() -> SeedSelectionRaw {
    SeedSelectionRaw {
        decision: SeedSelectionDecision::HardMiss,
        requirements: vec![],
        selected_ids: vec![],
        supporting_capability_ids: vec![],
        alternative_sets: vec![],
        uncovered_requirements: vec!["no executable candidate bundles".into()],
        reasoning: "empty candidate bundle pool".into(),
    }
}

pub fn build_seed_selection_presentation(
    intent: &str,
    entity_bundles: &[EntityCandidateBundle],
    config: SeedBundleConfig,
) -> Result<Option<SeedSelectionPresentation>, SeedSelectionValidationError> {
    let seed_bundles = build_candidate_seed_bundles(intent, entity_bundles, config);
    if seed_bundles.is_empty() {
        return Ok(None);
    }
    let tables = build_seed_bundle_index_tables(&seed_bundles, entity_bundles)?;
    let presentation_order = seed_bundle_presentation_order(&tables, &seed_bundles);
    let bundles = build_presentation_bundles(&seed_bundles, entity_bundles, &presentation_order)?;
    let provider_groups =
        build_presentation_provider_groups(&tables, &seed_bundles, entity_bundles);
    Ok(Some(SeedSelectionPresentation {
        seed_bundles,
        presentation_order,
        bundles,
        provider_groups,
        tables,
    }))
}

fn build_presentation_bundles(
    seed_bundles: &[CandidateSeedBundle],
    entity_bundles: &[EntityCandidateBundle],
    presentation_order: &[usize],
) -> Result<Vec<SeedPresentationBundle>, SeedSelectionValidationError> {
    let bundle_by_id: HashMap<&str, &EntityCandidateBundle> = entity_bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    presentation_order
        .iter()
        .map(|&bundle_index| {
            let seed_bundle = seed_bundles.get(bundle_index).ok_or(
                SeedSelectionValidationError::UnknownBundleIndex(bundle_index as i64),
            )?;
            let roots = seed_bundle
                .candidate_ids
                .iter()
                .map(|id| {
                    let bundle = bundle_by_id.get(id.as_str()).ok_or_else(|| {
                        SeedSelectionValidationError::BundleReferencesUnknownCandidate {
                            bundle_index,
                            candidate_id: id.clone(),
                        }
                    })?;
                    Ok(SeedPresentationRoot {
                        catalog: bundle.entry_id.clone(),
                        entity: bundle.entity.clone(),
                        entity_description: bundle.entity_description.clone(),
                        capabilities: bundle
                            .capabilities
                            .iter()
                            .map(|capability| SeedPresentationCapability {
                                capability_name: capability.capability_name.clone(),
                                kind: capability.kind.clone(),
                                description: capability.description.clone(),
                                lexical_score: capability.lexical_score,
                            })
                            .collect(),
                        relation_coverage: bundle.relation_hints.clone(),
                    })
                })
                .collect::<Result<Vec<_>, SeedSelectionValidationError>>()?;
            Ok(SeedPresentationBundle {
                bundle_index,
                catalogs: seed_bundle.catalogs.clone(),
                roots,
            })
        })
        .collect()
}

fn build_presentation_provider_groups(
    tables: &SeedBundleIndexTables,
    seed_bundles: &[CandidateSeedBundle],
    entity_bundles: &[EntityCandidateBundle],
) -> Vec<SeedPresentationProviderGroup> {
    build_seed_bundle_provider_groups(tables, seed_bundles, entity_bundles)
        .into_iter()
        .map(|group| SeedPresentationProviderGroup {
            provider_index: group.provider_index,
            catalogs: group.catalogs,
            bundle_indexes: group.bundle_indexes,
            capability_sketch: group.capability_sketch,
        })
        .collect()
}

pub fn resolve_from_llm_coverage(
    requirements: Vec<(i64, String, Vec<i64>)>,
    coverage_rows: Vec<(i64, Vec<i64>)>,
    reasoning: String,
    tables: &SeedBundleIndexTables,
    intent: &str,
) -> Result<SeedSelectionRaw, SeedSelectionValidationError> {
    resolve_seed_coverage_assessment(
        requirements,
        coverage_rows,
        reasoning,
        tables,
        intent,
    )
}
