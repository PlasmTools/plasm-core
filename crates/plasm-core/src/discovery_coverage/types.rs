//! Core types for the unified discovery coverage model (v2: SeedPlan + slots).

use std::collections::BTreeMap;

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_seed_select::{
    SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw,
};
use crate::schema::CapabilityKind;

/// Minimum lexical-score margin between top and second provider for unbranded ready.
/// Frozen from holdout risk-coverage sweep; not an env toggle.
/// Minimum BM25 milli-gap between top and second provider plans to allow unbranded ready.
pub const READY_MARGIN: u32 = 500;

/// Closed vocabulary of requirement slots aligned to CGS semantics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum RequirementSlot {
    ReadRoot {
        entity_hint: Option<String>,
    },
    RelationHop {
        wire: String,
        target: String,
    },
    MutateAnchor {
        op: CapabilityKind,
        entity_hint: Option<String>,
    },
    FederateSlot {
        entry_id: String,
    },
}

impl RequirementSlot {
    pub fn label(&self) -> String {
        match self {
            Self::ReadRoot { entity_hint } => format!("read_root:{entity_hint:?}"),
            Self::RelationHop { wire, target } => format!("relation:{wire}→{target}"),
            Self::MutateAnchor { op, entity_hint } => format!("mutate:{op:?}:{entity_hint:?}"),
            Self::FederateSlot { entry_id } => format!("federate:{entry_id}"),
        }
    }

    /// Stable signature used to compare plan shapes across providers.
    pub fn signature_key(&self) -> String {
        match self {
            Self::ReadRoot { entity_hint } => {
                format!("read:{}", entity_hint.as_deref().unwrap_or("*"))
            }
            Self::RelationHop { wire, target } => format!("hop:{wire}:{target}"),
            Self::MutateAnchor { op, entity_hint } => {
                format!("mutate:{op:?}:{}", entity_hint.as_deref().unwrap_or("*"))
            }
            Self::FederateSlot { entry_id } => format!("fed:{entry_id}"),
        }
    }
}

/// Legacy alias retained for callers that still speak in flat requirements.
pub type DiscoveryRequirement = RequirementSlot;

/// Provider constraint from explicit entry_id / registry alias mentions only.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ProviderConstraint {
    #[default]
    Unbranded,
    Locked(Vec<String>),
    Rejected(Vec<String>),
}

/// How a single seed candidate covers slots.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeedSatisfiability {
    pub entry_id: String,
    pub entity: String,
    pub candidate_id: String,
    pub lexical_score: u32,
    pub catalog_route_evidence: bool,
    pub direct_slots: Vec<usize>,
    pub via_relation_slots: Vec<usize>,
    pub bundle: EntityCandidateBundle,
}

impl SeedSatisfiability {
    pub fn covers(&self, slot_index: usize) -> bool {
        self.direct_slots.contains(&slot_index) || self.via_relation_slots.contains(&slot_index)
    }

    pub fn covers_all(&self, slot_count: usize) -> bool {
        (0..slot_count).all(|idx| self.covers(idx))
    }
}

/// Ordered 1–3 seed set that jointly covers requirement slots.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeedPlan {
    pub seeds: Vec<SeedSatisfiability>,
    pub covers: Vec<usize>,
    pub lexical_score: u32,
    pub slot_signature: String,
}

impl SeedPlan {
    pub fn from_seeds(seeds: Vec<SeedSatisfiability>, required_slots: &[usize]) -> Option<Self> {
        if seeds.is_empty() || seeds.len() > 3 {
            return None;
        }
        let mut covers: Vec<usize> = seeds
            .iter()
            .flat_map(|seed| {
                seed.direct_slots
                    .iter()
                    .chain(seed.via_relation_slots.iter())
                    .copied()
            })
            .collect();
        covers.sort_unstable();
        covers.dedup();
        if required_slots
            .iter()
            .any(|idx| !covers.contains(idx))
        {
            return None;
        }
        let lexical_score = seeds.iter().map(|s| s.lexical_score).sum();
        Some(Self {
            seeds,
            covers,
            lexical_score,
            slot_signature: String::new(),
        })
    }

    pub fn with_signature(mut self, signature: String) -> Self {
        self.slot_signature = signature;
        self
    }

    pub fn candidate_ids(&self) -> Vec<String> {
        self.seeds
            .iter()
            .map(|seed| seed.candidate_id.clone())
            .collect()
    }

    pub fn primary_provider(&self) -> Option<&str> {
        self.seeds.first().map(|seed| seed.entry_id.as_str())
    }
}

/// Provider-level ambiguity after enumeration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProviderAmbiguity {
    None,
    Between {
        providers: Vec<String>,
        equivalent_plans: bool,
    },
}

/// Deterministic plan derived from intent + catalogs (before enumeration).
///
/// Brand preference uses [`ProviderConstraint::Locked`] from explicit entry_id /
/// registry alias mentions only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscoveryCoveragePlan {
    pub slots: Vec<RequirementSlot>,
    pub provider_constraint: ProviderConstraint,
    pub catalog_route: Vec<String>,
}

impl DiscoveryCoveragePlan {
    /// Compatibility accessor used during migration.
    pub fn requirements(&self) -> &[RequirementSlot] {
        &self.slots
    }

    pub fn slot_signature(&self) -> String {
        let mut keys: Vec<_> = self.slots.iter().map(RequirementSlot::signature_key).collect();
        keys.sort();
        keys.join("|")
    }
}

/// Output of coverage evaluation (enumeration + satisfiability).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverageEvaluation {
    pub plan: DiscoveryCoveragePlan,
    pub satisfiable_plans_by_provider: BTreeMap<String, Vec<SeedPlan>>,
    /// Flat seed view derived from plans (for presentation / pool metrics).
    pub satisfiable_by_provider: BTreeMap<String, Vec<SeedSatisfiability>>,
    pub satisfiable_federation_tuples: Vec<Vec<SeedSatisfiability>>,
    pub uncovered: Vec<RequirementSlot>,
    pub ambiguity: ProviderAmbiguity,
    pub bundles: Vec<EntityCandidateBundle>,
}

/// Routing outcome from coverage evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageRoute {
    Clarify {
        alternative_sets: Vec<SeedAlternativeSetRaw>,
        reasoning: String,
    },
    HardMiss {
        uncovered: Vec<String>,
        reasoning: String,
    },
    Select {
        selected: Vec<SeedSatisfiability>,
        provider: String,
        tie_candidates: Vec<Vec<SeedSatisfiability>>,
        plan: SeedPlan,
    },
}

/// Full pipeline result for selector integration.
#[derive(Debug, Clone)]
pub struct CoveragePipelineResult {
    pub evaluation: CoverageEvaluation,
    pub route: CoverageRoute,
    pub selection: Option<SeedSelectionRaw>,
}

/// Shadow metrics for eval harness.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoverageShadowMetrics {
    pub coverage_ambiguous: bool,
    pub coverage_satisfiable: bool,
    pub coverage_gold_recall: bool,
    pub coverage_plan_recall: bool,
    pub coverage_entity_recall: bool,
    pub plan_route_decision: String,
    pub plan_select_exact: bool,
    pub satisfiable_provider_count: usize,
    pub uncovered_count: usize,
}

impl CoverageRoute {
    pub fn decision_label(&self) -> SeedSelectionDecision {
        match self {
            Self::Clarify { .. } => SeedSelectionDecision::Clarify,
            Self::HardMiss { .. } => SeedSelectionDecision::HardMiss,
            Self::Select { .. } => SeedSelectionDecision::Ready,
        }
    }
}
