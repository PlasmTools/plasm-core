//! Seed selection validation.

use std::collections::{HashMap, HashSet};

use crate::discovery_auto_seed::EntityCandidateBundle;

use super::types::{SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw};

/// Validation failure → MCP `routing_error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeedSelectionValidationError {
    #[error("unknown bundle index: {0}")]
    UnknownBundleIndex(i64),
    #[error("bundle {bundle_index} references unknown candidate id: {candidate_id}")]
    BundleReferencesUnknownCandidate {
        bundle_index: usize,
        candidate_id: String,
    },
    #[error("unknown provider index: {0}")]
    UnknownProviderIndex(usize),
    #[error("no complete bundle remained after reduction")]
    NoCompleteBundleAfterReduction,
    #[error("requirement index out of range: {0}")]
    RequirementIndexOutOfRange(i64),
    #[error("duplicate requirement index: {0}")]
    DuplicateRequirementIndex(i64),
    #[error("requirement dependency cycle detected")]
    RequirementDependencyCycle,
    #[error("requirement {requirement_index} depends on invalid index {depends_on}")]
    InvalidRequirementDependency {
        requirement_index: i64,
        depends_on: i64,
    },
    #[error("coverage row count mismatch: expected {expected}, got {actual}")]
    CoverageRowCountMismatch { expected: usize, actual: usize },
    #[error("duplicate coverage row for requirement {0}")]
    DuplicateCoverageRequirement(i64),
    #[error("unknown symbol (not in legend): {0}")]
    UnknownSymbol(String),
    #[error("selected catalog outside brand_lock: {0}")]
    BrandLockViolation(String),
    #[error("raw candidate id in symbol field (use s# only): {0}")]
    RawIdHallucination(String),
    #[error("unknown candidate id: {0}")]
    UnknownCandidateId(String),
    #[error("unknown capability id: {0}")]
    UnknownCapabilityId(String),
    #[error("duplicate candidate id in selected_ids")]
    DuplicateSelectedId,
    #[error("ready requires 1–3 unique seeds, got {0}")]
    ReadySeedCount(usize),
    #[error("ready requires non-empty supporting_capability_ids")]
    ReadyMissingSupporting,
    #[error("ready must not have alternative_sets or uncovered_requirements")]
    ReadyHasAbstentionFields,
    #[error("clarify must not select seeds")]
    ClarifySelectedSeeds,
    #[error("clarify requires alternative_sets")]
    ClarifyMissingAlternatives,
    #[error("provider clarify forbidden when brand_lock is set: {0}")]
    ClarifyUnderBrandLock(String),
    #[error("hard_miss must not select seeds")]
    HardMissSelectedSeeds,
    #[error("hard_miss requires uncovered_requirements")]
    HardMissMissingUncovered,
}

/// Validated ready selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedReadySeedSelection {
    pub requirements: Vec<String>,
    pub selected_ids: Vec<String>,
    pub supporting_capability_ids: Vec<String>,
    pub teaching_satellites: Vec<(String, String)>,
    pub reasoning: String,
}

/// Validated abstention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAbstainSeedSelection {
    pub decision: SeedSelectionDecision,
    pub requirements: Vec<String>,
    pub alternative_sets: Vec<SeedAlternativeSetRaw>,
    pub uncovered_requirements: Vec<String>,
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatedSeedSelection {
    Ready(ValidatedReadySeedSelection),
    Abstain(ValidatedAbstainSeedSelection),
}

/// Clarify topology relative to named brands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClarifyKind {
    /// All alternatives share a single catalog (entity / surface disambiguation).
    EntityDisambiguation,
    /// Alternatives span multiple catalogs ("which provider?").
    ProviderDisambiguation,
}

/// Classify clarify alternatives by catalog span (independent of brand_lock membership).
#[must_use]
pub fn classify_clarify(alternative_sets: &[SeedAlternativeSetRaw]) -> ClarifyKind {
    let mut catalogs = HashSet::new();
    for alt in alternative_sets {
        for id in &alt.candidate_ids {
            let catalog = id.split_once(':').map(|(c, _)| c).unwrap_or(id.as_str());
            catalogs.insert(catalog);
        }
    }
    if catalogs.len() <= 1 {
        ClarifyKind::EntityDisambiguation
    } else {
        ClarifyKind::ProviderDisambiguation
    }
}

pub fn validate_seed_selection(
    raw: &SeedSelectionRaw,
    bundles: &[EntityCandidateBundle],
) -> Result<ValidatedSeedSelection, SeedSelectionValidationError> {
    validate_seed_selection_with_brand_lock(raw, bundles, None)
}

/// Validate selector output; when `brand_lock` is non-empty, reject provider-level clarify
/// and selected seeds outside the lock.
pub fn validate_seed_selection_with_brand_lock(
    raw: &SeedSelectionRaw,
    bundles: &[EntityCandidateBundle],
    brand_lock: Option<&[String]>,
) -> Result<ValidatedSeedSelection, SeedSelectionValidationError> {
    let candidate_ids: HashSet<&str> = bundles.iter().map(|b| b.candidate_id.as_str()).collect();
    let capability_ids: HashSet<String> = bundles
        .iter()
        .flat_map(|b| b.capabilities.iter().map(|c| c.capability_id.clone()))
        .collect();
    let brand_lock: HashSet<&str> = brand_lock
        .unwrap_or(&[])
        .iter()
        .map(|s| s.as_str())
        .collect();

    for id in &raw.selected_ids {
        if !candidate_ids.contains(id.as_str()) {
            return Err(SeedSelectionValidationError::UnknownCandidateId(id.clone()));
        }
        if !brand_lock.is_empty() {
            let catalog = id.split_once(':').map(|(c, _)| c).unwrap_or(id.as_str());
            if !brand_lock.contains(catalog) {
                return Err(SeedSelectionValidationError::BrandLockViolation(
                    catalog.to_string(),
                ));
            }
        }
    }
    for id in &raw.supporting_capability_ids {
        if !capability_ids.contains(id) {
            return Err(SeedSelectionValidationError::UnknownCapabilityId(
                id.clone(),
            ));
        }
    }
    for alt in &raw.alternative_sets {
        for id in &alt.candidate_ids {
            if !candidate_ids.contains(id.as_str()) {
                return Err(SeedSelectionValidationError::UnknownCandidateId(id.clone()));
            }
        }
    }

    if raw.selected_ids.len() != raw.selected_ids.iter().collect::<HashSet<_>>().len() {
        return Err(SeedSelectionValidationError::DuplicateSelectedId);
    }

    match raw.decision {
        SeedSelectionDecision::Ready => {
            if raw.selected_ids.is_empty() || raw.selected_ids.len() > 3 {
                return Err(SeedSelectionValidationError::ReadySeedCount(
                    raw.selected_ids.len(),
                ));
            }
            if raw.supporting_capability_ids.is_empty() {
                return Err(SeedSelectionValidationError::ReadyMissingSupporting);
            }
            if !raw.alternative_sets.is_empty() || !raw.uncovered_requirements.is_empty() {
                return Err(SeedSelectionValidationError::ReadyHasAbstentionFields);
            }
            Ok(ValidatedSeedSelection::Ready(ValidatedReadySeedSelection {
                requirements: raw.requirements.clone(),
                selected_ids: raw.selected_ids.clone(),
                supporting_capability_ids: raw.supporting_capability_ids.clone(),
                teaching_satellites: raw.teaching_satellites.clone(),
                reasoning: raw.reasoning.clone(),
            }))
        }
        SeedSelectionDecision::Clarify => {
            if !raw.selected_ids.is_empty() || !raw.supporting_capability_ids.is_empty() {
                return Err(SeedSelectionValidationError::ClarifySelectedSeeds);
            }
            if raw.alternative_sets.len() < 2 {
                return Err(SeedSelectionValidationError::ClarifyMissingAlternatives);
            }
            if !brand_lock.is_empty() {
                for alt in &raw.alternative_sets {
                    for id in &alt.candidate_ids {
                        let catalog = id.split_once(':').map(|(c, _)| c).unwrap_or(id.as_str());
                        if !brand_lock.contains(catalog) {
                            return Err(SeedSelectionValidationError::BrandLockViolation(
                                catalog.to_string(),
                            ));
                        }
                    }
                }
                // Named brands ⇒ only entity-level clarify; reject provider disambiguation.
                if classify_clarify(&raw.alternative_sets) == ClarifyKind::ProviderDisambiguation {
                    return Err(SeedSelectionValidationError::ClarifyUnderBrandLock(
                        brand_lock.iter().cloned().collect::<Vec<_>>().join(","),
                    ));
                }
            }
            Ok(ValidatedSeedSelection::Abstain(
                ValidatedAbstainSeedSelection {
                    decision: SeedSelectionDecision::Clarify,
                    requirements: raw.requirements.clone(),
                    alternative_sets: raw.alternative_sets.clone(),
                    uncovered_requirements: raw.uncovered_requirements.clone(),
                    reasoning: raw.reasoning.clone(),
                },
            ))
        }
        SeedSelectionDecision::HardMiss => {
            if !raw.selected_ids.is_empty() || !raw.supporting_capability_ids.is_empty() {
                return Err(SeedSelectionValidationError::HardMissSelectedSeeds);
            }
            if raw.uncovered_requirements.is_empty() {
                return Err(SeedSelectionValidationError::HardMissMissingUncovered);
            }
            Ok(ValidatedSeedSelection::Abstain(
                ValidatedAbstainSeedSelection {
                    decision: SeedSelectionDecision::HardMiss,
                    requirements: raw.requirements.clone(),
                    alternative_sets: raw.alternative_sets.clone(),
                    uncovered_requirements: raw.uncovered_requirements.clone(),
                    reasoning: raw.reasoning.clone(),
                },
            ))
        }
    }
}

/// When the selector emits provider-level clarify despite named brands, collapse
/// into a federation **Ready** (≤3 seeds).
///
/// Strategy:
/// 1. Prefer one candidate per clarify alternative when alts are cleanly
///    one-catalog-per-brand and fully cover `brand_lock`.
/// 2. Otherwise pick one bundle per `brand_lock` catalog (intent/alt hints,
///    then lexical score).
///
/// Returns `None` when brand coverage cannot be satisfied from `bundles`.
#[must_use]
pub fn try_federation_ready_under_brand_lock(
    raw: &SeedSelectionRaw,
    bundles: &[EntityCandidateBundle],
    brand_lock: &[String],
    intent: &str,
) -> Option<ValidatedReadySeedSelection> {
    if brand_lock.is_empty() || raw.decision != SeedSelectionDecision::Clarify {
        return None;
    }
    if !(2..=3).contains(&brand_lock.len()) {
        return None;
    }
    // Never promote entity-level clarify (same catalog) into multi-seed Ready.
    if classify_clarify(&raw.alternative_sets) == ClarifyKind::EntityDisambiguation {
        return None;
    }

    let selected_ids = federation_selected_ids_from_alts(raw, bundles, brand_lock)
        .or_else(|| federation_selected_ids_from_bundles(raw, bundles, brand_lock, intent))?;

    let supporting = super::rewriter::supporting_capabilities_from_bundles(&selected_ids, bundles);
    if supporting.is_empty() {
        return None;
    }

    Some(ValidatedReadySeedSelection {
        requirements: raw.requirements.clone(),
        selected_ids,
        supporting_capability_ids: supporting,
        teaching_satellites: Vec::new(),
        reasoning: format!(
            "brand_lock_best_effort: federation ready from provider clarify under named brands ({})",
            brand_lock.join(",")
        ),
    })
}

fn federation_selected_ids_from_alts(
    raw: &SeedSelectionRaw,
    bundles: &[EntityCandidateBundle],
    brand_lock: &[String],
) -> Option<Vec<String>> {
    let n = raw.alternative_sets.len();
    if n != brand_lock.len() || !(2..=3).contains(&n) {
        return None;
    }
    let lock: HashSet<&str> = brand_lock.iter().map(String::as_str).collect();
    let candidate_ids: HashSet<&str> = bundles.iter().map(|b| b.candidate_id.as_str()).collect();

    let mut selected_ids = Vec::new();
    let mut covered: HashSet<&str> = HashSet::new();
    for alt in &raw.alternative_sets {
        let mut cats = HashSet::new();
        for id in &alt.candidate_ids {
            let catalog = id.split_once(':').map(|(c, _)| c).unwrap_or(id.as_str());
            cats.insert(catalog);
        }
        if cats.len() != 1 {
            return None;
        }
        let catalog = *cats.iter().next()?;
        if !lock.contains(catalog) || !covered.insert(catalog) {
            return None;
        }
        let pick = alt
            .candidate_ids
            .iter()
            .find(|id| candidate_ids.contains(id.as_str()))?;
        if !selected_ids.iter().any(|s| s == pick) {
            selected_ids.push(pick.clone());
        }
    }
    if !lock.iter().all(|c| covered.contains(c)) {
        return None;
    }
    if selected_ids.len() != brand_lock.len() {
        return None;
    }
    Some(selected_ids)
}

fn federation_selected_ids_from_bundles(
    raw: &SeedSelectionRaw,
    bundles: &[EntityCandidateBundle],
    brand_lock: &[String],
    intent: &str,
) -> Option<Vec<String>> {
    let hint = {
        let mut parts = Vec::new();
        parts.push(intent.to_ascii_lowercase());
        for r in &raw.requirements {
            parts.push(r.to_ascii_lowercase());
        }
        parts.push(raw.reasoning.to_ascii_lowercase());
        for alt in &raw.alternative_sets {
            parts.push(alt.label.to_ascii_lowercase());
            for id in &alt.candidate_ids {
                parts.push(id.to_ascii_lowercase());
            }
        }
        parts.join(" ")
    };

    // Preferred candidate ids mentioned in clarify alts, keyed by catalog.
    let mut alt_pref: HashMap<&str, Vec<&str>> = HashMap::new();
    for alt in &raw.alternative_sets {
        for id in &alt.candidate_ids {
            let catalog = id.split_once(':').map(|(c, _)| c).unwrap_or(id.as_str());
            alt_pref.entry(catalog).or_default().push(id.as_str());
        }
    }

    let mut selected_ids = Vec::new();
    for brand in brand_lock {
        let catalog = brand.as_str();
        let mut catalog_bundles: Vec<&EntityCandidateBundle> =
            bundles.iter().filter(|b| b.entry_id == catalog).collect();
        if catalog_bundles.is_empty() {
            return None;
        }
        catalog_bundles.sort_by(|a, b| {
            b.max_lexical_score
                .cmp(&a.max_lexical_score)
                .then_with(|| a.candidate_id.cmp(&b.candidate_id))
        });

        let pick = alt_pref
            .get(catalog)
            .and_then(|prefs| {
                prefs.iter().find_map(|pid| {
                    catalog_bundles
                        .iter()
                        .find(|b| b.candidate_id == *pid)
                        .map(|b| b.candidate_id.clone())
                })
            })
            .or_else(|| {
                catalog_bundles
                    .iter()
                    .find(|b| {
                        let ent = b.entity.to_ascii_lowercase();
                        !ent.is_empty() && hint.contains(&ent)
                    })
                    .map(|b| b.candidate_id.clone())
            })
            .or_else(|| catalog_bundles.first().map(|b| b.candidate_id.clone()))?;

        if !selected_ids.iter().any(|s| s == &pick) {
            selected_ids.push(pick);
        }
    }
    if selected_ids.len() != brand_lock.len() {
        return None;
    }
    Some(selected_ids)
}

/// Map validated candidate ids to `{entry_id, entity}` seeds.
pub fn seeds_from_candidate_ids(
    bundles: &[EntityCandidateBundle],
    selected_ids: &[String],
) -> Vec<(String, String)> {
    let index: std::collections::HashMap<&str, (&str, &str)> = bundles
        .iter()
        .map(|b| {
            (
                b.candidate_id.as_str(),
                (b.entry_id.as_str(), b.entity.as_str()),
            )
        })
        .collect();
    selected_ids
        .iter()
        .filter_map(|id| {
            index
                .get(id.as_str())
                .map(|(eid, ent)| (eid.to_string(), ent.to_string()))
        })
        .collect()
}

/// Diagnostic label for validation failures (logged / eval metrics).
pub fn validation_error_label(e: &SeedSelectionValidationError) -> &'static str {
    match e {
        SeedSelectionValidationError::UnknownBundleIndex(_) => "unknown_bundle_index",
        SeedSelectionValidationError::BundleReferencesUnknownCandidate { .. } => {
            "bundle_references_unknown_candidate"
        }
        SeedSelectionValidationError::UnknownProviderIndex(_) => "unknown_provider_index",
        SeedSelectionValidationError::NoCompleteBundleAfterReduction => {
            "no_complete_bundle_after_reduction"
        }
        SeedSelectionValidationError::RequirementIndexOutOfRange(_) => {
            "requirement_index_out_of_range"
        }
        SeedSelectionValidationError::DuplicateRequirementIndex(_) => "duplicate_requirement_index",
        SeedSelectionValidationError::RequirementDependencyCycle => "requirement_dependency_cycle",
        SeedSelectionValidationError::InvalidRequirementDependency { .. } => {
            "invalid_requirement_dependency"
        }
        SeedSelectionValidationError::CoverageRowCountMismatch { .. } => {
            "coverage_row_count_mismatch"
        }
        SeedSelectionValidationError::DuplicateCoverageRequirement(_) => {
            "duplicate_coverage_requirement"
        }
        SeedSelectionValidationError::UnknownSymbol(_) => "symbol_hallucination",
        SeedSelectionValidationError::BrandLockViolation(_) => "brand_lock_violation",
        SeedSelectionValidationError::RawIdHallucination(_) => "raw_id_hallucination",
        SeedSelectionValidationError::UnknownCandidateId(_) => "unknown_candidate_id",
        SeedSelectionValidationError::UnknownCapabilityId(_) => "unknown_capability_id",
        SeedSelectionValidationError::DuplicateSelectedId => "duplicate_selected_id",
        SeedSelectionValidationError::ReadySeedCount(_) => "ready_seed_count",
        SeedSelectionValidationError::ReadyMissingSupporting => "ready_missing_supporting",
        SeedSelectionValidationError::ReadyHasAbstentionFields => "ready_has_abstention_fields",
        SeedSelectionValidationError::ClarifySelectedSeeds => "clarify_selected_seeds",
        SeedSelectionValidationError::ClarifyMissingAlternatives => "clarify_missing_alternatives",
        SeedSelectionValidationError::ClarifyUnderBrandLock(_) => "clarify_under_brand_lock",
        SeedSelectionValidationError::HardMissSelectedSeeds => "hard_miss_selected_seeds",
        SeedSelectionValidationError::HardMissMissingUncovered => "hard_miss_missing_uncovered",
    }
}
