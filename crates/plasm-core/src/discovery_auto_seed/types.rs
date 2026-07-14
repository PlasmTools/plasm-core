//! Entity candidate bundle types and retrieval budget knobs.

/// How many capability rows to retrieve before entity grouping.
pub const DEFAULT_RETRIEVE_CAPABILITY_K: usize = 96;
/// Max entity bundles passed to the semantic selector.
pub const DEFAULT_MAX_ENTITY_CANDIDATES: usize = 24;
/// Max entities per catalog in the diversified pool.
pub const DEFAULT_MAX_ENTITIES_PER_CATALOG: usize = 4;
/// Max capability evidence rows per entity bundle.
pub const DEFAULT_MAX_CAPABILITIES_PER_ENTITY: usize = 3;

/// One capability row inside an entity bundle (selector-facing id is stable).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EntityCapabilityEvidence {
    pub capability_id: String,
    pub capability_name: String,
    pub kind: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<String>,
    pub lexical_score: u32,
}

use crate::discovery_candidate_graph::TypedCandidateGraph;

/// One entity-level candidate for seed-set selection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EntityCandidateBundle {
    pub candidate_id: String,
    pub entry_id: String,
    pub entity: String,
    pub entity_description: String,
    /// Diagnostic only — never used as an abstain threshold.
    pub max_lexical_score: u32,
    pub capabilities: Vec<EntityCapabilityEvidence>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub relation_hints: String,
    pub catalog_route_evidence: bool,
}

/// Budget knobs for retrieval + diversification (not final seed count).
#[derive(Debug, Clone, Copy)]
pub struct EntityCandidateConfig {
    pub retrieve_k: usize,
    pub max_entities: usize,
    pub max_per_catalog: usize,
    pub max_capabilities_per_entity: usize,
}

#[derive(Debug, Clone)]
pub struct EntityCandidateRetrieveResult {
    pub bundles: Vec<EntityCandidateBundle>,
    pub catalog_context: crate::discovery_seed_catalog::CatalogWorkflowContext,
    pub intent_class: crate::discovery_intent_class::DiscoveryIntentClass,
    pub named_catalogs: Vec<String>,
    pub candidate_graph: TypedCandidateGraph,
}

impl std::ops::Deref for EntityCandidateRetrieveResult {
    type Target = Vec<EntityCandidateBundle>;

    fn deref(&self) -> &Self::Target {
        &self.bundles
    }
}

impl<'a> IntoIterator for &'a EntityCandidateRetrieveResult {
    type Item = &'a EntityCandidateBundle;
    type IntoIter = std::slice::Iter<'a, EntityCandidateBundle>;

    fn into_iter(self) -> Self::IntoIter {
        self.bundles.iter()
    }
}

impl Default for EntityCandidateConfig {
    fn default() -> Self {
        Self {
            retrieve_k: DEFAULT_RETRIEVE_CAPABILITY_K,
            max_entities: DEFAULT_MAX_ENTITY_CANDIDATES,
            max_per_catalog: DEFAULT_MAX_ENTITIES_PER_CATALOG,
            max_capabilities_per_entity: DEFAULT_MAX_CAPABILITIES_PER_ENTITY,
        }
    }
}
