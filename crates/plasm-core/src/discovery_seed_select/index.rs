//! Bundle index tables and provider presentation.

use std::collections::{HashMap, HashSet};

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_seed_bundle::CandidateSeedBundle;

use super::validation::SeedSelectionValidationError;

/// Stable bundle-index lookup tables for one selector call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedBundleIndexTables {
    pub(crate) candidate_ids_by_bundle: Vec<Vec<String>>,
    pub(crate) capability_ids_by_bundle: Vec<Vec<String>>,
    pub(crate) capability_kinds_by_bundle: Vec<HashSet<String>>,
    pub(crate) entry_entity_by_candidate_id: HashMap<String, (String, String)>,
    pub(crate) relation_hints_by_candidate_id: HashMap<String, String>,
    provider_index_by_bundle: Vec<usize>,
    pub(crate) catalogs_by_provider: Vec<Vec<String>>,
}

impl SeedBundleIndexTables {
    pub fn bundle_count(&self) -> usize {
        self.candidate_ids_by_bundle.len()
    }

    pub fn catalogs_by_provider(&self) -> &[Vec<String>] {
        &self.catalogs_by_provider
    }

    pub fn provider_index_for_bundle(&self, bundle_index: usize) -> Option<usize> {
        self.provider_index_by_bundle.get(bundle_index).copied()
    }

    pub fn bundle_indexes_for_provider(&self, provider_index: usize) -> Vec<usize> {
        self.provider_index_by_bundle
            .iter()
            .enumerate()
            .filter_map(|(bundle_index, index)| (*index == provider_index).then_some(bundle_index))
            .collect()
    }

    pub fn bundle_root_count(&self, bundle_index: usize) -> Option<usize> {
        self.candidate_ids_by_bundle.get(bundle_index).map(Vec::len)
    }
}

/// Catalog-provider grouping for selector presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedBundleProviderGroup {
    pub provider_index: usize,
    pub catalogs: Vec<String>,
    pub bundle_indexes: Vec<usize>,
    /// Compact entity/kind summary for LLM coverage enumeration (not used by reducer).
    pub capability_sketch: String,
}

pub fn seed_bundle_presentation_order(
    tables: &SeedBundleIndexTables,
    seed_bundles: &[CandidateSeedBundle],
) -> Vec<usize> {
    let mut indexes: Vec<usize> = (0..seed_bundles.len()).collect();
    indexes.sort_by(|left, right| {
        seed_bundles[*left].candidate_ids.len().cmp(
            &seed_bundles[*right].candidate_ids.len(),
        )
        .then_with(|| {
            tables
                .provider_index_for_bundle(*left)
                .unwrap_or(usize::MAX)
                .cmp(&tables.provider_index_for_bundle(*right).unwrap_or(usize::MAX))
        })
        .then_with(|| left.cmp(right))
    });
    indexes
}

fn capability_sketch_for_provider(
    bundle_indexes: &[usize],
    seed_bundles: &[CandidateSeedBundle],
    bundle_by_id: &std::collections::HashMap<&str, &EntityCandidateBundle>,
) -> String {
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for bundle_index in bundle_indexes {
        let Some(seed_bundle) = seed_bundles.get(*bundle_index) else {
            continue;
        };
        for id in &seed_bundle.candidate_ids {
            if !seen.insert(id.as_str()) {
                continue;
            }
            let Some(entity_bundle) = bundle_by_id.get(id.as_str()) else {
                continue;
            };
            let kinds: HashSet<&str> = entity_bundle
                .capabilities
                .iter()
                .map(|capability| capability.kind.as_str())
                .collect();
            let mut kinds: Vec<&str> = kinds.into_iter().collect();
            kinds.sort_unstable();
            lines.push(format!(
                "{}:{}[{}]",
                entity_bundle.entry_id,
                entity_bundle.entity,
                kinds.join(",")
            ));
        }
    }
    lines.sort_unstable();
    lines.join("; ")
}

pub fn build_seed_bundle_provider_groups(
    tables: &SeedBundleIndexTables,
    seed_bundles: &[CandidateSeedBundle],
    entity_bundles: &[EntityCandidateBundle],
) -> Vec<SeedBundleProviderGroup> {
    let bundle_by_id: std::collections::HashMap<&str, &EntityCandidateBundle> = entity_bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    tables
        .catalogs_by_provider()
        .iter()
        .enumerate()
        .map(|(provider_index, catalogs)| {
            let bundle_indexes = tables.bundle_indexes_for_provider(provider_index);
            let capability_sketch =
                capability_sketch_for_provider(&bundle_indexes, seed_bundles, &bundle_by_id);
            SeedBundleProviderGroup {
                provider_index,
                catalogs: catalogs.clone(),
                bundle_indexes,
                capability_sketch,
            }
        })
        .collect()
}

pub fn build_seed_bundle_index_tables(
    seed_bundles: &[CandidateSeedBundle],
    bundles: &[EntityCandidateBundle],
) -> Result<SeedBundleIndexTables, SeedSelectionValidationError> {
    let bundle_by_id: std::collections::HashMap<&str, &EntityCandidateBundle> = bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    let candidate_ids_by_bundle = seed_bundles
        .iter()
        .map(|bundle| bundle.candidate_ids.clone())
        .collect();
    let capability_ids_by_bundle = seed_bundles
        .iter()
        .enumerate()
        .map(|(bundle_index, bundle)| {
            let mut capability_ids = Vec::new();
            for id in &bundle.candidate_ids {
                let entity_bundle = bundle_by_id.get(id.as_str()).ok_or_else(|| {
                    SeedSelectionValidationError::BundleReferencesUnknownCandidate {
                        bundle_index,
                        candidate_id: id.clone(),
                    }
                })?;
                capability_ids.extend(
                    entity_bundle
                        .capabilities
                        .iter()
                        .map(|capability| capability.capability_id.clone()),
                );
            }
            Ok(capability_ids)
        })
        .collect::<Result<Vec<_>, SeedSelectionValidationError>>()?;
    let capability_kinds_by_bundle = seed_bundles
        .iter()
        .enumerate()
        .map(|(bundle_index, bundle)| {
            let mut kinds = HashSet::new();
            for id in &bundle.candidate_ids {
                let entity_bundle = bundle_by_id.get(id.as_str()).ok_or_else(|| {
                    SeedSelectionValidationError::BundleReferencesUnknownCandidate {
                        bundle_index,
                        candidate_id: id.clone(),
                    }
                })?;
                for capability in &entity_bundle.capabilities {
                    kinds.insert(capability.kind.clone());
                }
            }
            Ok(kinds)
        })
        .collect::<Result<Vec<_>, SeedSelectionValidationError>>()?;
    let mut entry_entity_by_candidate_id = HashMap::new();
    let mut relation_hints_by_candidate_id = HashMap::new();
    for bundle in bundles {
        entry_entity_by_candidate_id.insert(
            bundle.candidate_id.clone(),
            (bundle.entry_id.clone(), bundle.entity.clone()),
        );
        relation_hints_by_candidate_id.insert(
            bundle.candidate_id.clone(),
            bundle.relation_hints.clone(),
        );
    }
    let mut catalogs_by_provider: Vec<Vec<String>> = Vec::new();
    let provider_index_by_bundle = seed_bundles
        .iter()
        .map(|bundle| {
            if let Some(index) = catalogs_by_provider
                .iter()
                .position(|catalogs| catalogs == &bundle.catalogs)
            {
                index
            } else {
                let index = catalogs_by_provider.len();
                catalogs_by_provider.push(bundle.catalogs.clone());
                index
            }
        })
        .collect();
    Ok(SeedBundleIndexTables {
        candidate_ids_by_bundle,
        capability_ids_by_bundle,
        capability_kinds_by_bundle,
        entry_entity_by_candidate_id,
        relation_hints_by_candidate_id,
        provider_index_by_bundle,
        catalogs_by_provider,
    })
}
