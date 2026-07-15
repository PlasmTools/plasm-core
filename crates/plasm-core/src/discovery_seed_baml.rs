//! Shared presentation layer for LLM seed selection (BAML-bound crates map these to generated types).

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_candidate_graph::TypedCandidateGraph;
use crate::discovery_seed_bundle::SeedBundleConfig;
use crate::discovery_seed_catalog::CatalogWorkflowContext;
use crate::discovery_seed_select::{
    SeedSelectionDecision, SeedSelectionRaw, SeedSelectionValidationError,
};
use crate::discovery_seed_symbol_map::{SeedSymbolMap, SeedSymbolRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPresentationSymbolCandidate {
    pub symbol: String,
    pub catalog: String,
    pub entity: String,
    pub entity_description: String,
    pub capability_kinds: String,
    pub relation_coverage: String,
    pub entity_aliases: String,
}

#[derive(Debug, Clone)]
pub struct SeedSelectionPresentation {
    pub symbol_map: SeedSymbolMap,
    pub symbol_candidates: Vec<SeedPresentationSymbolCandidate>,
    pub brand_lock_catalogs: Vec<String>,
    pub catalog_context: CatalogWorkflowContext,
    pub candidate_graph: TypedCandidateGraph,
}

pub fn empty_seed_selection_raw() -> SeedSelectionRaw {
    SeedSelectionRaw {
        decision: SeedSelectionDecision::HardMiss,
        requirements: vec![],
        selected_ids: vec![],
        supporting_capability_ids: vec![],
        teaching_satellites: vec![],
        alternative_sets: vec![],
        uncovered_requirements: vec!["no executable candidate bundles".into()],
        reasoning: "empty candidate bundle pool".into(),
    }
}

pub fn build_seed_selection_presentation(
    entity_bundles: &[EntityCandidateBundle],
    _config: SeedBundleConfig,
    catalog_context: CatalogWorkflowContext,
    brand_lock_catalogs: &[String],
    candidate_graph: TypedCandidateGraph,
) -> Result<Option<SeedSelectionPresentation>, SeedSelectionValidationError> {
    if entity_bundles.is_empty() {
        return Ok(None);
    }
    // Closed set must honor brand lock: do not present foreign catalogs for the LLM to pick.
    let locked: Vec<&EntityCandidateBundle> = if brand_lock_catalogs.is_empty() {
        entity_bundles.iter().collect()
    } else {
        entity_bundles
            .iter()
            .filter(|bundle| {
                brand_lock_catalogs
                    .iter()
                    .any(|locked| locked == &bundle.entry_id)
            })
            .collect()
    };
    if locked.is_empty() {
        return Ok(None);
    }
    let locked_owned: Vec<EntityCandidateBundle> = locked.into_iter().cloned().collect();
    let symbol_map = SeedSymbolMap::build(&locked_owned, Some(&catalog_context));
    if symbol_map.symbol_count() == 0 {
        return Ok(None);
    }
    let symbol_candidates = symbol_map.rows().iter().map(presentation_row).collect();
    Ok(Some(SeedSelectionPresentation {
        symbol_map,
        symbol_candidates,
        brand_lock_catalogs: brand_lock_catalogs.to_vec(),
        catalog_context,
        candidate_graph,
    }))
}

fn presentation_row(row: &SeedSymbolRow) -> SeedPresentationSymbolCandidate {
    SeedPresentationSymbolCandidate {
        symbol: row.symbol.clone(),
        catalog: row.catalog.clone(),
        entity: row.entity.clone(),
        entity_description: row.entity_description.clone(),
        capability_kinds: row.capability_kinds.clone(),
        relation_coverage: row.relation_coverage.clone(),
        entity_aliases: row.entity_aliases.clone(),
    }
}
