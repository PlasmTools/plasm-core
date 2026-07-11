//! Coverage assessment reducer.

use std::collections::HashSet;

use crate::discovery_intent_signals as signals;

use super::index::SeedBundleIndexTables;
use super::rewriter::{
    bundle_is_read_only_search, bundle_mutation_score, candidate_ids_for_federated_bundle,
    finalize_ready_bundle_index, prefer_mutation_capable_singleton, rewrite_selected_candidate_ids,
    supporting_capabilities_for_candidate_ids,
};
use super::types::{SeedAlternativeSetRaw, SeedSelectionDecision, SeedSelectionRaw};
use super::validation::SeedSelectionValidationError;

fn build_ready_selection(
    requirement_texts: &[String],
    bundle_indexes: &[usize],
    tables: &SeedBundleIndexTables,
    reasoning: String,
    intent: &str,
) -> Result<SeedSelectionRaw, SeedSelectionValidationError> {
    let mut selected_ids = Vec::new();
    let mut supporting_capability_ids = Vec::new();
    for &bundle_index in bundle_indexes {
        selected_ids.extend(
            tables
                .candidate_ids_by_bundle
                .get(bundle_index)
                .cloned()
                .ok_or(SeedSelectionValidationError::UnknownBundleIndex(
                    bundle_index as i64,
                ))?,
        );
        supporting_capability_ids.extend(
            tables
                .capability_ids_by_bundle
                .get(bundle_index)
                .cloned()
                .ok_or(SeedSelectionValidationError::UnknownBundleIndex(
                    bundle_index as i64,
                ))?,
        );
    }
    selected_ids.sort_unstable();
    selected_ids.dedup();
    selected_ids = rewrite_selected_candidate_ids(selected_ids, requirement_texts, intent, tables);
    supporting_capability_ids =
        supporting_capabilities_for_candidate_ids(&selected_ids, tables);
    Ok(SeedSelectionRaw {
        decision: SeedSelectionDecision::Ready,
        requirements: requirement_texts.to_vec(),
        selected_ids,
        supporting_capability_ids,
        alternative_sets: Vec::new(),
        uncovered_requirements: Vec::new(),
        reasoning,
    })
}

fn hard_miss_for_out_of_catalog_host_goal(
    intent: &str,
    requirement_texts: &[String],
    reasoning: String,
) -> Option<SeedSelectionRaw> {
    if !signals::intent_requires_non_catalog_host_capability(intent) {
        return None;
    }
    Some(SeedSelectionRaw {
        decision: SeedSelectionDecision::HardMiss,
        requirements: requirement_texts.to_vec(),
        selected_ids: Vec::new(),
        supporting_capability_ids: Vec::new(),
        alternative_sets: Vec::new(),
        uncovered_requirements: vec![intent.to_string()],
        reasoning,
    })
}

fn resolve_bundle_index(
    tables: &SeedBundleIndexTables,
    index: i64,
) -> Result<Vec<String>, SeedSelectionValidationError> {
    if index < 0 {
        return Err(SeedSelectionValidationError::UnknownBundleIndex(index));
    }
    tables
        .candidate_ids_by_bundle
        .get(index as usize)
        .cloned()
        .ok_or(SeedSelectionValidationError::UnknownBundleIndex(index))
}

fn validate_requirement_dag(
    requirements: &[(i64, String, Vec<i64>)],
) -> Result<(), SeedSelectionValidationError> {
    let count = requirements.len();
    if count == 0 {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for (index, _, deps) in requirements {
        if *index < 0 || *index as usize >= count {
            return Err(SeedSelectionValidationError::RequirementIndexOutOfRange(
                *index,
            ));
        }
        if !seen.insert(*index) {
            return Err(SeedSelectionValidationError::DuplicateRequirementIndex(
                *index,
            ));
        }
        for dep in deps {
            if *dep < 0 || *dep as usize >= count {
                return Err(SeedSelectionValidationError::InvalidRequirementDependency {
                    requirement_index: *index,
                    depends_on: *dep,
                });
            }
            if *dep == *index {
                return Err(SeedSelectionValidationError::RequirementDependencyCycle);
            }
        }
    }
    for expected in 0..count as i64 {
        if !seen.contains(&expected) {
            return Err(SeedSelectionValidationError::RequirementIndexOutOfRange(
                expected,
            ));
        }
    }

    let mut indegree = vec![0usize; count];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (index, _, deps) in requirements {
        let node = *index as usize;
        for dep in deps {
            let parent = *dep as usize;
            edges[parent].push(node);
            indegree[node] += 1;
        }
    }
    let mut queue: Vec<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect();
    queue.sort_unstable();
    let mut visited = 0usize;
    while let Some(node) = queue.first().copied() {
        queue.remove(0);
        visited += 1;
        for child in &edges[node] {
            indegree[*child] -= 1;
            if indegree[*child] == 0 {
                queue.push(*child);
                queue.sort_unstable();
            }
        }
    }
    if visited != count {
        return Err(SeedSelectionValidationError::RequirementDependencyCycle);
    }
    Ok(())
}

fn clarify_alternative_sets_from_bundle_indexes(
    tables: &SeedBundleIndexTables,
    bundle_indexes: &[usize],
) -> Result<Vec<SeedAlternativeSetRaw>, SeedSelectionValidationError> {
    let mut alternatives = Vec::new();
    for bundle_index in bundle_indexes {
        let candidate_ids = tables
            .candidate_ids_by_bundle
            .get(*bundle_index)
            .cloned()
            .ok_or(SeedSelectionValidationError::UnknownBundleIndex(
                *bundle_index as i64,
            ))?;
        let provider_index = tables.provider_index_for_bundle(*bundle_index).ok_or(
            SeedSelectionValidationError::UnknownBundleIndex(*bundle_index as i64),
        )?;
        let catalogs = tables
            .catalogs_by_provider
            .get(provider_index)
            .ok_or(SeedSelectionValidationError::UnknownProviderIndex(
                provider_index,
            ))?;
        alternatives.push(SeedAlternativeSetRaw {
            candidate_ids,
            label: catalogs.join(" + "),
        });
    }
    Ok(alternatives)
}

fn clarify_from_provider_best_bundles(
    tables: &SeedBundleIndexTables,
    provider_indices: &[usize],
    bundle_candidates: &std::collections::HashMap<usize, usize>,
) -> Result<Vec<SeedAlternativeSetRaw>, SeedSelectionValidationError> {
    let mut bundle_indexes: Vec<usize> = provider_indices
        .iter()
        .filter_map(|provider_index| bundle_candidates.get(provider_index).copied())
        .collect();
    bundle_indexes.sort_unstable();
    bundle_indexes.dedup();
    clarify_alternative_sets_from_bundle_indexes(tables, &bundle_indexes)
}

fn best_bundle_per_provider(
    tables: &SeedBundleIndexTables,
    support: &HashSet<usize>,
) -> std::collections::HashMap<usize, usize> {
    let mut best_by_provider: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for bundle_index in support {
        let Some(provider_index) = tables.provider_index_for_bundle(*bundle_index) else {
            continue;
        };
        best_by_provider
            .entry(provider_index)
            .and_modify(|current| {
                let current_roots = tables.bundle_root_count(*current).unwrap_or(usize::MAX);
                let candidate_roots = tables
                    .bundle_root_count(*bundle_index)
                    .unwrap_or(usize::MAX);
                if candidate_roots < current_roots
                    || (candidate_roots == current_roots && bundle_index < current)
                {
                    *current = *bundle_index;
                }
            })
            .or_insert(*bundle_index);
    }
    best_by_provider
}
fn try_partial_empty_mutation_ready(
    support_by_requirement: &[HashSet<usize>],
    requirement_texts: &[String],
    uncovered_requirements: &[String],
    tables: &SeedBundleIndexTables,
    intent: &str,
    reasoning: String,
) -> Result<Option<SeedSelectionRaw>, SeedSelectionValidationError> {
    if uncovered_requirements.len() != 1 || support_by_requirement.len() < 2 {
        return Ok(None);
    }
    let non_empty: Vec<&HashSet<usize>> = support_by_requirement
        .iter()
        .filter(|support| !support.is_empty())
        .collect();
    if non_empty.len() != 1 {
        return Ok(None);
    }
    let combined = format!("{} {}", requirement_texts.join(" "), intent);
    if !signals::requirement_implies_mutation(&combined) {
        return Ok(None);
    }
    let seed_bundle = *non_empty[0]
        .iter()
        .min_by_key(|&&index| tables.bundle_root_count(index).unwrap_or(usize::MAX))
        .expect("non-empty support");
    let bundle_index = prefer_mutation_capable_singleton(
        seed_bundle,
        &requirement_texts,
        tables,
    );
    if bundle_mutation_score(tables, bundle_index) == 0 {
        return Ok(None);
    }
    Ok(Some(build_ready_selection(
        requirement_texts,
        &[bundle_index],
        tables,
        reasoning,
        intent,
    )?))
}
fn try_over_split_read_only_fallback(
    support_by_requirement: &[HashSet<usize>],
    requirement_texts: &[String],
    tables: &SeedBundleIndexTables,
    intent: &str,
    reasoning: String,
) -> Result<Option<SeedSelectionRaw>, SeedSelectionValidationError> {
    if support_by_requirement.len() < 2
        || !support_by_requirement.iter().any(|support| support.is_empty())
    {
        return Ok(None);
    }
    let non_empty: Vec<&HashSet<usize>> = support_by_requirement
        .iter()
        .filter(|support| !support.is_empty())
        .collect();
    if non_empty.len() < 2 {
        return Ok(None);
    }
    let mut complete = non_empty[0].clone();
    for support in &non_empty[1..] {
        complete.retain(|bundle_index| support.contains(bundle_index));
    }
    if complete.is_empty() {
        return Ok(None);
    }
    let has_mutation = complete
        .iter()
        .any(|&bundle_index| !bundle_is_read_only_search(tables, bundle_index));
    if !has_mutation {
        return Ok(None);
    }
    let bundle_index = *complete
        .iter()
        .min_by_key(|&&index| tables.bundle_root_count(index).unwrap_or(usize::MAX))
        .expect("non-empty complete");
    Ok(Some(build_ready_selection(
        requirement_texts,
        &[bundle_index],
        tables,
        reasoning,
        intent,
    )?))
}

fn requirement_anchors_provider(
    requirement_text: &str,
    tables: &SeedBundleIndexTables,
    bundle_index: usize,
) -> bool {
    let Some(provider_index) = tables.provider_index_for_bundle(bundle_index) else {
        return false;
    };
    let Some(catalogs) = tables.catalogs_by_provider.get(provider_index) else {
        return false;
    };
    catalogs
        .iter()
        .any(|catalog| signals::catalog_mentioned_in_requirement(catalog, requirement_text))
}
fn clarify_from_multi_provider_support(
    tables: &SeedBundleIndexTables,
    support_by_requirement: &[HashSet<usize>],
    requirement_texts: &[String],
    uncovered_requirements: &[String],
    reasoning: String,
) -> Result<Option<SeedSelectionRaw>, SeedSelectionValidationError> {
    let mut best_by_provider: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    let mut saw_multi_provider_requirement = false;
    for (support, _) in support_by_requirement.iter().zip(requirement_texts) {
        if support.is_empty() {
            continue;
        }
        let per_provider = best_bundle_per_provider(tables, support);
        if per_provider.len() >= 2 {
            saw_multi_provider_requirement = true;
        }
        for (provider_index, bundle_index) in per_provider {
            best_by_provider
                .entry(provider_index)
                .and_modify(|current| {
                    if bundle_index < *current {
                        *current = bundle_index;
                    }
                })
                .or_insert(bundle_index);
        }
    }
    if best_by_provider.len() < 2 {
        return Ok(None);
    }
    if uncovered_requirements.is_empty() && !saw_multi_provider_requirement {
        return Ok(None);
    }
    let mut provider_indices: Vec<usize> = best_by_provider.keys().copied().collect();
    provider_indices.sort_unstable();
    let alternative_sets =
        clarify_from_provider_best_bundles(tables, &provider_indices, &best_by_provider)?;
    Ok(Some(SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: requirement_texts.to_vec(),
        selected_ids: Vec::new(),
        supporting_capability_ids: Vec::new(),
        alternative_sets,
        uncovered_requirements: uncovered_requirements.to_vec(),
        reasoning,
    }))
}

fn intent_brand_locks_catalog(intent: &str, tables: &SeedBundleIndexTables) -> bool {
    tables
        .catalogs_by_provider()
        .iter()
        .flatten()
        .any(|catalog| signals::catalog_mentioned_in_requirement(catalog, intent))
}

fn maybe_clarify_unbranded_single_provider(
    intent: &str,
    tables: &SeedBundleIndexTables,
    support_by_requirement: &[HashSet<usize>],
    requirement_texts: &[String],
    reasoning: String,
) -> Result<Option<SeedSelectionRaw>, SeedSelectionValidationError> {
    if requirement_texts.len() != 1 || intent_brand_locks_catalog(intent, tables) {
        return Ok(None);
    }
    let support = support_by_requirement
        .first()
        .filter(|support| !support.is_empty())
        .ok_or(SeedSelectionValidationError::NoCompleteBundleAfterReduction)?;
    let support_providers: HashSet<usize> = support
        .iter()
        .filter_map(|bundle_index| tables.provider_index_for_bundle(*bundle_index))
        .collect();
    if support_providers.len() != 1 || tables.catalogs_by_provider().len() < 2 {
        return Ok(None);
    }
    let all_bundles: HashSet<usize> = (0..tables.bundle_count()).collect();
    let best_by_provider = best_bundle_per_provider(tables, &all_bundles);
    if best_by_provider.len() < 2 {
        return Ok(None);
    }
    let mut provider_indices: Vec<usize> = best_by_provider.keys().copied().collect();
    provider_indices.sort_unstable();
    let alternative_sets =
        clarify_from_provider_best_bundles(tables, &provider_indices, &best_by_provider)?;
    Ok(Some(SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: requirement_texts.to_vec(),
        selected_ids: Vec::new(),
        supporting_capability_ids: Vec::new(),
        alternative_sets,
        uncovered_requirements: Vec::new(),
        reasoning,
    }))
}

fn try_cross_provider_federated_ready(
    support_by_requirement: &[HashSet<usize>],
    requirement_texts: &[String],
    tables: &SeedBundleIndexTables,
    intent: &str,
    reasoning: String,
) -> Result<Option<SeedSelectionRaw>, SeedSelectionValidationError> {
    if support_by_requirement.len() < 2
        || support_by_requirement.iter().any(|support| support.is_empty())
    {
        return Ok(None);
    }

    let mut picks: Vec<usize> = Vec::with_capacity(support_by_requirement.len());
    let mut providers = HashSet::new();
    for (support, requirement_text) in support_by_requirement.iter().zip(requirement_texts) {
        let per_provider = best_bundle_per_provider(tables, support);
        if per_provider.len() != 1 {
            return Ok(None);
        }
        let (&provider_index, &bundle_index) = per_provider.iter().next().expect("one provider");
        if !providers.insert(provider_index) {
            return Ok(None);
        }
        if !requirement_anchors_provider(requirement_text, tables, bundle_index) {
            return Ok(None);
        }
        picks.push(bundle_index);
    }
    if picks.len() < 2 {
        return Ok(None);
    }

    let mut selected_ids = Vec::new();
    let mut supporting_capability_ids = Vec::new();
    for (bundle_index, requirement_text) in picks.iter().zip(requirement_texts) {
        let ids = candidate_ids_for_federated_bundle(*bundle_index, requirement_text, tables)?;
        selected_ids.extend(ids);
        supporting_capability_ids.extend(
            tables
                .capability_ids_by_bundle
                .get(*bundle_index)
                .cloned()
                .ok_or(SeedSelectionValidationError::UnknownBundleIndex(
                    *bundle_index as i64,
                ))?,
        );
    }
    selected_ids.sort_unstable();
    selected_ids.dedup();
    selected_ids = rewrite_selected_candidate_ids(selected_ids, requirement_texts, intent, tables);
    supporting_capability_ids = supporting_capabilities_for_candidate_ids(&selected_ids, tables);
    if selected_ids.is_empty() || selected_ids.len() > 2 {
        return Ok(None);
    }

    Ok(Some(SeedSelectionRaw {
        decision: SeedSelectionDecision::Ready,
        requirements: requirement_texts.to_vec(),
        selected_ids,
        supporting_capability_ids,
        alternative_sets: Vec::new(),
        uncovered_requirements: Vec::new(),
        reasoning,
    }))
}

fn clarify_from_union_support(
    tables: &SeedBundleIndexTables,
    support_by_requirement: &[HashSet<usize>],
    requirement_texts: &[String],
    reasoning: String,
) -> Result<Option<SeedSelectionRaw>, SeedSelectionValidationError> {
    let mut best_by_provider: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for support in support_by_requirement {
        for bundle_index in support {
            let provider_index = tables.provider_index_for_bundle(*bundle_index).ok_or(
                SeedSelectionValidationError::UnknownBundleIndex(*bundle_index as i64),
            )?;
            best_by_provider
                .entry(provider_index)
                .and_modify(|current| {
                    if bundle_index < current {
                        *current = *bundle_index;
                    }
                })
                .or_insert(*bundle_index);
        }
    }
    if best_by_provider.len() < 2 {
        return Ok(None);
    }
    let mut provider_indices: Vec<usize> = best_by_provider.keys().copied().collect();
    provider_indices.sort_unstable();
    let alternative_sets =
        clarify_from_provider_best_bundles(tables, &provider_indices, &best_by_provider)?;
    Ok(Some(SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: requirement_texts.to_vec(),
        selected_ids: Vec::new(),
        supporting_capability_ids: Vec::new(),
        alternative_sets,
        uncovered_requirements: Vec::new(),
        reasoning,
    }))
}

/// Reduce requirement-to-bundle coverage into ready/clarify/hard-miss.
pub fn resolve_seed_coverage_assessment(
    requirements: Vec<(i64, String, Vec<i64>)>,
    coverage_rows: Vec<(i64, Vec<i64>)>,
    reasoning: String,
    tables: &SeedBundleIndexTables,
    intent: &str,
) -> Result<SeedSelectionRaw, SeedSelectionValidationError> {
    validate_requirement_dag(&requirements)?;
    let requirement_texts: Vec<String> = {
        let mut ordered = vec![String::new(); requirements.len()];
        for (index, text, _) in &requirements {
            ordered[*index as usize] = text.clone();
        }
        ordered
    };

    if coverage_rows.len() != requirements.len() {
        return Err(SeedSelectionValidationError::CoverageRowCountMismatch {
            expected: requirements.len(),
            actual: coverage_rows.len(),
        });
    }

    let mut support_by_requirement: Vec<HashSet<usize>> = vec![HashSet::new(); requirements.len()];
    let mut seen_rows = HashSet::new();
    for (requirement_index, supporting_bundle_indexes) in coverage_rows {
        if requirement_index < 0 || requirement_index as usize >= requirements.len() {
            return Err(SeedSelectionValidationError::RequirementIndexOutOfRange(
                requirement_index,
            ));
        }
        if !seen_rows.insert(requirement_index) {
            return Err(SeedSelectionValidationError::DuplicateCoverageRequirement(
                requirement_index,
            ));
        }
        let mut support = HashSet::new();
        for bundle_index in supporting_bundle_indexes {
            resolve_bundle_index(tables, bundle_index)?;
            support.insert(bundle_index as usize);
        }
        support_by_requirement[requirement_index as usize] = support;
    }

    let mut uncovered_requirements = Vec::new();
    for (text, support) in requirement_texts.iter().zip(&support_by_requirement) {
        if support.is_empty() {
            uncovered_requirements.push(text.clone());
        }
    }
    if !uncovered_requirements.is_empty() {
        if let Some(clarify) = clarify_from_multi_provider_support(
            tables,
            &support_by_requirement,
            &requirement_texts,
            &uncovered_requirements,
            reasoning.clone(),
        )? {
            return Ok(clarify);
        }
        if let Some(ready) = try_over_split_read_only_fallback(
            &support_by_requirement,
            &requirement_texts,
            tables,
            intent,
            reasoning.clone(),
        )? {
            if hard_miss_for_out_of_catalog_host_goal(intent, &requirement_texts, reasoning.clone())
                .is_none()
            {
                return Ok(ready);
            }
        }
        if let Some(ready) = try_partial_empty_mutation_ready(
            &support_by_requirement,
            &requirement_texts,
            &uncovered_requirements,
            tables,
            intent,
            reasoning.clone(),
        )? {
            if hard_miss_for_out_of_catalog_host_goal(intent, &requirement_texts, reasoning.clone())
                .is_none()
            {
                return Ok(ready);
            }
        }
        if let Some(hard_miss) =
            hard_miss_for_out_of_catalog_host_goal(intent, &requirement_texts, reasoning.clone())
        {
            return Ok(hard_miss);
        }
        return Ok(SeedSelectionRaw {
            decision: SeedSelectionDecision::HardMiss,
            requirements: requirement_texts,
            selected_ids: Vec::new(),
            supporting_capability_ids: Vec::new(),
            alternative_sets: Vec::new(),
            uncovered_requirements,
            reasoning,
        });
    }

    let mut complete: HashSet<usize> = (0..tables.bundle_count()).collect();
    for support in &support_by_requirement {
        complete.retain(|bundle_index| support.contains(bundle_index));
    }
    if complete.is_empty() {
        if let Some(federated) = try_cross_provider_federated_ready(
            &support_by_requirement,
            &requirement_texts,
            tables,
            intent,
            reasoning.clone(),
        )? {
            if hard_miss_for_out_of_catalog_host_goal(intent, &requirement_texts, reasoning.clone())
                .is_none()
            {
                return Ok(federated);
            }
        }
        if let Some(clarify) = clarify_from_union_support(
            tables,
            &support_by_requirement,
            &requirement_texts,
            reasoning.clone(),
        )? {
            return Ok(clarify);
        }
        return Ok(SeedSelectionRaw {
            decision: SeedSelectionDecision::HardMiss,
            requirements: requirement_texts.clone(),
            selected_ids: Vec::new(),
            supporting_capability_ids: Vec::new(),
            alternative_sets: Vec::new(),
            uncovered_requirements: requirement_texts,
            reasoning,
        });
    }

    let root_counts = complete
        .iter()
        .map(|bundle_index| {
            tables.bundle_root_count(*bundle_index).ok_or(
                SeedSelectionValidationError::UnknownBundleIndex(*bundle_index as i64),
            )
        })
        .collect::<Result<Vec<_>, SeedSelectionValidationError>>()?;
    let Some(min_roots) = root_counts.into_iter().min() else {
        return Err(SeedSelectionValidationError::NoCompleteBundleAfterReduction);
    };
    let minimal_complete: Vec<usize> = complete
        .into_iter()
        .filter(|bundle_index| tables.bundle_root_count(*bundle_index) == Some(min_roots))
        .collect();

    let mut best_by_provider: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for bundle_index in minimal_complete {
        let provider_index = tables.provider_index_for_bundle(bundle_index).ok_or(
            SeedSelectionValidationError::UnknownBundleIndex(bundle_index as i64),
        )?;
        best_by_provider
            .entry(provider_index)
            .and_modify(|current| {
                if bundle_index < *current {
                    *current = bundle_index;
                }
            })
            .or_insert(bundle_index);
    }

    let mut finalists: Vec<usize> = best_by_provider.into_values().collect();
    finalists.sort_unstable();

    if finalists.len() == 1 {
        if let Some(clarify) = maybe_clarify_unbranded_single_provider(
            intent,
            tables,
            &support_by_requirement,
            &requirement_texts,
            reasoning.clone(),
        )? {
            return Ok(clarify);
        }
        let bundle_index = finalize_ready_bundle_index(
            finalists[0],
            &support_by_requirement,
            &requirement_texts,
            intent,
            tables,
        );
        if let Some(hard_miss) =
            hard_miss_for_out_of_catalog_host_goal(intent, &requirement_texts, reasoning.clone())
        {
            return Ok(hard_miss);
        }
        return build_ready_selection(
            &requirement_texts,
            &[bundle_index],
            tables,
            reasoning,
            intent,
        );
    }

    let alternative_sets = finalists
        .into_iter()
        .map(|bundle_index| {
            let candidate_ids = tables
                .candidate_ids_by_bundle
                .get(bundle_index)
                .cloned()
                .ok_or(SeedSelectionValidationError::UnknownBundleIndex(
                    bundle_index as i64,
                ))?;
            let provider_index = tables.provider_index_for_bundle(bundle_index).ok_or(
                SeedSelectionValidationError::UnknownBundleIndex(bundle_index as i64),
            )?;
            let catalogs = tables.catalogs_by_provider.get(provider_index).ok_or(
                SeedSelectionValidationError::UnknownProviderIndex(provider_index),
            )?;
            Ok(SeedAlternativeSetRaw {
                candidate_ids,
                label: catalogs.join(" + "),
            })
        })
        .collect::<Result<Vec<_>, SeedSelectionValidationError>>()?;

    Ok(SeedSelectionRaw {
        decision: SeedSelectionDecision::Clarify,
        requirements: requirement_texts,
        selected_ids: Vec::new(),
        supporting_capability_ids: Vec::new(),
        alternative_sets,
        uncovered_requirements: Vec::new(),
        reasoning,
    })
}
