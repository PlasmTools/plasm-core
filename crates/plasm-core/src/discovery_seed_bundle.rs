//! Bounded unordered seed bundles for semantic intent routing.

use std::collections::{HashMap, HashSet};

use crate::catalog_search_index::CatalogSearchIndex;
use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_intent_signals::intent_mentions_catalog_id;

pub const DEFAULT_MAX_SEED_BUNDLES: usize = 12;
pub const DEFAULT_MAX_ROOTS_PER_BUNDLE: usize = 3;
pub const DEFAULT_MAX_BUNDLE_PROVIDERS: usize = 20;

#[derive(Debug, Clone, Copy)]
pub struct SeedBundleConfig {
    pub max_bundles: usize,
    pub max_roots_per_bundle: usize,
    pub max_providers: usize,
}

impl Default for SeedBundleConfig {
    fn default() -> Self {
        Self {
            max_bundles: DEFAULT_MAX_SEED_BUNDLES,
            max_roots_per_bundle: DEFAULT_MAX_ROOTS_PER_BUNDLE,
            max_providers: DEFAULT_MAX_BUNDLE_PROVIDERS,
        }
    }
}

/// One selector-facing unordered root set. Roots are canonical; execution order is not implied.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CandidateSeedBundle {
    pub candidate_ids: Vec<String>,
    pub catalogs: Vec<String>,
    pub max_lexical_score: u32,
    pub total_lexical_score: u64,
}

impl CandidateSeedBundle {
    pub fn is_cross_catalog(&self) -> bool {
        self.catalogs.len() > 1
    }
}

pub fn explicitly_named_catalogs(
    intent: &str,
    bundles: &[EntityCandidateBundle],
) -> HashSet<String> {
    let intent_lower = intent.to_ascii_lowercase();
    let mut intent_tokens = CatalogSearchIndex::tokenize(intent);
    for w in intent_lower.split(|c: char| !c.is_alphanumeric()) {
        if w.len() >= 2 {
            intent_tokens.insert(w.to_string());
        }
    }
    bundles
        .iter()
        .filter_map(|bundle| {
            let id_norm = bundle.entry_id.replace('-', " ").to_ascii_lowercase();
            let catalog_tokens: HashSet<String> = id_norm
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() >= 2)
                .map(str::to_string)
                .collect();
            (!catalog_tokens.is_empty()
                && catalog_tokens
                    .iter()
                    .all(|token| intent_tokens.contains(token)))
            .then(|| bundle.entry_id.clone())
        })
        .collect()
}

fn relation_covers(root: &EntityCandidateBundle, target: &EntityCandidateBundle) -> bool {
    root.entry_id == target.entry_id
        && root
            .relation_hints
            .split(';')
            .any(|hint| hint.trim().ends_with(&format!("→{}", target.entity)))
}

fn structurally_redundant(roots: &[&EntityCandidateBundle]) -> bool {
    roots.iter().enumerate().any(|(target_idx, target)| {
        roots
            .iter()
            .enumerate()
            .any(|(root_idx, root)| root_idx != target_idx && relation_covers(root, target))
    })
}

fn bundle_from_roots(roots: &[&EntityCandidateBundle]) -> CandidateSeedBundle {
    let mut catalogs = Vec::new();
    for root in roots {
        if !catalogs.contains(&root.entry_id) {
            catalogs.push(root.entry_id.clone());
        }
    }
    CandidateSeedBundle {
        candidate_ids: roots.iter().map(|root| root.candidate_id.clone()).collect(),
        catalogs,
        max_lexical_score: roots
            .iter()
            .map(|root| root.max_lexical_score)
            .max()
            .unwrap_or_default(),
        total_lexical_score: roots
            .iter()
            .map(|root| u64::from(root.max_lexical_score))
            .sum(),
    }
}

fn compare_bundles(a: &CandidateSeedBundle, b: &CandidateSeedBundle) -> std::cmp::Ordering {
    a.candidate_ids
        .len()
        .cmp(&b.candidate_ids.len())
        .then_with(|| b.total_lexical_score.cmp(&a.total_lexical_score))
        .then_with(|| a.catalogs.cmp(&b.catalogs))
        .then_with(|| a.candidate_ids.cmp(&b.candidate_ids))
}

fn retain_top_bundle(
    bundles: &mut Vec<CandidateSeedBundle>,
    bundle: CandidateSeedBundle,
    limit: usize,
) {
    bundles.push(bundle);
    bundles.sort_by(compare_bundles);
    bundles.truncate(limit);
}

fn enumerate_combinations(
    candidates: &[&EntityCandidateBundle],
    max_roots: usize,
    limit: usize,
) -> Vec<CandidateSeedBundle> {
    fn visit<'a>(
        candidates: &[&'a EntityCandidateBundle],
        start: usize,
        target_roots: usize,
        limit: usize,
        current: &mut Vec<&'a EntityCandidateBundle>,
        out: &mut Vec<CandidateSeedBundle>,
    ) {
        if current.len() == target_roots {
            if !structurally_redundant(current) {
                retain_top_bundle(out, bundle_from_roots(current), limit);
            }
            return;
        }
        for index in start..candidates.len() {
            current.push(candidates[index]);
            visit(candidates, index + 1, target_roots, limit, current, out);
            current.pop();
        }
    }

    let mut out = Vec::new();
    for target_roots in 1..=max_roots.min(candidates.len()) {
        visit(
            candidates,
            0,
            target_roots,
            limit,
            &mut Vec::new(),
            &mut out,
        );
    }
    out
}

fn merged_bundle(
    left: &CandidateSeedBundle,
    right: &CandidateSeedBundle,
    bundle_by_id: &HashMap<&str, &EntityCandidateBundle>,
    max_roots: usize,
) -> Option<CandidateSeedBundle> {
    let mut ids = left.candidate_ids.clone();
    for id in &right.candidate_ids {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    if ids.len() > max_roots {
        return None;
    }
    let roots: Vec<&EntityCandidateBundle> = ids
        .iter()
        .map(|id| bundle_by_id.get(id.as_str()).copied())
        .collect::<Option<Vec<_>>>()?;
    (!structurally_redundant(&roots)).then(|| bundle_from_roots(&roots))
}

/// Build bounded unordered root-set bundles. Single-catalog alternatives are always present.
/// Cross-catalog bundles require ≥2 explicitly named catalogs (entry_id tokens in intent).
pub fn build_candidate_seed_bundles(
    intent: &str,
    bundles: &[EntityCandidateBundle],
    config: SeedBundleConfig,
) -> Vec<CandidateSeedBundle> {
    if config.max_bundles == 0 || config.max_roots_per_bundle == 0 || config.max_providers == 0 {
        return Vec::new();
    }

    let selectable: Vec<&EntityCandidateBundle> = bundles
        .iter()
        .filter(|bundle| !bundle.capabilities.is_empty())
        .collect();
    let bundle_by_id: HashMap<&str, &EntityCandidateBundle> = selectable
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), *bundle))
        .collect();

    let mut catalog_order = Vec::new();
    let mut by_catalog: HashMap<&str, Vec<&EntityCandidateBundle>> = HashMap::new();
    for bundle in &selectable {
        if !by_catalog.contains_key(bundle.entry_id.as_str()) {
            catalog_order.push(bundle.entry_id.as_str());
        }
        by_catalog
            .entry(bundle.entry_id.as_str())
            .or_default()
            .push(*bundle);
    }
    let anchors = explicitly_named_catalogs(intent, bundles);
    let mut retained_catalogs: Vec<&str> = catalog_order
        .iter()
        .copied()
        .take(config.max_providers)
        .collect();
    for catalog in catalog_order
        .iter()
        .copied()
        .filter(|catalog| anchors.contains(*catalog))
    {
        if !retained_catalogs.contains(&catalog) {
            if retained_catalogs.len() == config.max_providers {
                retained_catalogs.pop();
            }
            retained_catalogs.push(catalog);
        }
    }
    catalog_order = retained_catalogs;

    let max_roots_per_bundle = config
        .max_roots_per_bundle
        .min(DEFAULT_MAX_ROOTS_PER_BUNDLE);
    let mut catalog_bundles: HashMap<&str, Vec<CandidateSeedBundle>> = HashMap::new();
    let mut seed_bundles = Vec::new();
    for catalog in &catalog_order {
        let mut roots = by_catalog.get(catalog).cloned().unwrap_or_default();
        roots.sort_by(|a, b| {
            b.max_lexical_score
                .cmp(&a.max_lexical_score)
                .then_with(|| a.candidate_id.cmp(&b.candidate_id))
        });
        roots.truncate(config.max_bundles);
        let generated = enumerate_combinations(&roots, max_roots_per_bundle, config.max_bundles);
        seed_bundles.extend(generated.iter().cloned());
        catalog_bundles.insert(catalog, generated);
    }

    let mut cross_bundles = Vec::new();
    for (left_index, left_catalog) in catalog_order.iter().enumerate() {
        for right_catalog in catalog_order.iter().skip(left_index + 1) {
            let pair_is_allowed = if anchors.len() >= 2 {
                anchors.contains(*left_catalog) && anchors.contains(*right_catalog)
            } else if anchors.len() == 1 {
                anchors.contains(*left_catalog) || anchors.contains(*right_catalog)
            } else {
                false
            };
            if !pair_is_allowed {
                continue;
            }
            for left in catalog_bundles.get(left_catalog).into_iter().flatten() {
                for right in catalog_bundles.get(right_catalog).into_iter().flatten() {
                    if let Some(bundle) =
                        merged_bundle(left, right, &bundle_by_id, max_roots_per_bundle)
                    {
                        retain_top_bundle(&mut cross_bundles, bundle, config.max_bundles);
                    }
                }
            }
        }
    }
    seed_bundles.extend(cross_bundles);
    inject_mirror_catalog_singletons(intent, &by_catalog, &mut seed_bundles);

    let mut seen = HashSet::new();
    seed_bundles.retain(|bundle| {
        let mut key = bundle.candidate_ids.clone();
        key.sort();
        seen.insert(key)
    });
    seed_bundles.sort_by(compare_bundles);
    seed_bundles = finalize_provider_diverse_bundles(seed_bundles, config.max_bundles, &anchors);
    seed_bundles
}

/// Keep top singletons per provider, reserving multi-root depth for intent-named catalogs.
///
/// Naive round-robin across every provider fills `limit` at depth 0 and drops secondary
/// roots (e.g. `github:Repository` behind Issue) on branded workflow intents.
fn finalize_provider_diverse_bundles(
    bundles: Vec<CandidateSeedBundle>,
    limit: usize,
    anchors: &HashSet<String>,
) -> Vec<CandidateSeedBundle> {
    if bundles.len() <= limit {
        return bundles;
    }
    /// Max single-root bundles retained per provider in the first pass.
    const PER_PROVIDER_SINGLES: usize = 3;

    let mut singles_by_provider: HashMap<String, Vec<CandidateSeedBundle>> = HashMap::new();
    let mut provider_order: Vec<String> = Vec::new();
    for bundle in &bundles {
        if bundle.candidate_ids.len() == 1 && bundle.catalogs.len() == 1 {
            let provider = bundle.catalogs[0].clone();
            if !singles_by_provider.contains_key(&provider) {
                provider_order.push(provider.clone());
            }
            singles_by_provider
                .entry(provider)
                .or_default()
                .push(bundle.clone());
        }
    }
    for list in singles_by_provider.values_mut() {
        list.sort_by(compare_bundles);
        list.truncate(PER_PROVIDER_SINGLES);
    }
    // Named catalogs first so they claim multi-root depth before peer diversity fills the cap.
    provider_order.sort_by_key(|p| (!anchors.contains(p), p.clone()));

    let mut retained = Vec::with_capacity(limit);
    let mut retained_keys = HashSet::new();

    let push_single = |provider: &str,
                       depth: usize,
                       retained: &mut Vec<CandidateSeedBundle>,
                       retained_keys: &mut HashSet<Vec<String>>,
                       singles_by_provider: &HashMap<String, Vec<CandidateSeedBundle>>|
     -> bool {
        let Some(list) = singles_by_provider.get(provider) else {
            return false;
        };
        let Some(bundle) = list.get(depth) else {
            return false;
        };
        let mut key = bundle.candidate_ids.clone();
        key.sort();
        if retained_keys.insert(key) {
            retained.push(bundle.clone());
            true
        } else {
            false
        }
    };

    // Phase 1: fill all depths for anchor providers (up to PER_PROVIDER_SINGLES each).
    for provider in provider_order.iter().filter(|p| anchors.contains(*p)) {
        for depth in 0..PER_PROVIDER_SINGLES {
            if retained.len() >= limit {
                break;
            }
            push_single(
                provider,
                depth,
                &mut retained,
                &mut retained_keys,
                &singles_by_provider,
            );
        }
    }

    // Phase 2: round-robin remaining providers for catalog diversity.
    let mut depth = 0usize;
    while retained.len() < limit && depth < PER_PROVIDER_SINGLES {
        let mut added = false;
        for provider in provider_order.iter().filter(|p| !anchors.contains(*p)) {
            if retained.len() >= limit {
                break;
            }
            if push_single(
                provider,
                depth,
                &mut retained,
                &mut retained_keys,
                &singles_by_provider,
            ) {
                added = true;
            }
        }
        if !added {
            break;
        }
        depth += 1;
    }

    for bundle in bundles {
        if retained.len() >= limit {
            break;
        }
        let mut key = bundle.candidate_ids.clone();
        key.sort();
        if retained_keys.insert(key) {
            retained.push(bundle);
        }
    }
    retained.truncate(limit);
    retained
}

fn inject_mirror_catalog_singletons(
    intent: &str,
    by_catalog: &HashMap<&str, Vec<&EntityCandidateBundle>>,
    seed_bundles: &mut Vec<CandidateSeedBundle>,
) {
    for (catalog, roots) in by_catalog {
        if !intent_mentions_catalog_id(catalog, intent) {
            continue;
        }
        let Some(top) = roots.first().copied() else {
            continue;
        };
        let bundle = bundle_from_roots(&[top]);
        let mut key = bundle.candidate_ids.clone();
        key.sort();
        if seed_bundles.iter().any(|existing| {
            let mut existing_key = existing.candidate_ids.clone();
            existing_key.sort();
            existing_key == key
        }) {
            continue;
        }
        seed_bundles.push(bundle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_auto_seed::EntityCapabilityEvidence;

    fn bundle(api: &str, entity: &str, score: u32, relations: &str) -> EntityCandidateBundle {
        EntityCandidateBundle {
            candidate_id: format!("{api}:{entity}"),
            entry_id: api.into(),
            entity: entity.into(),
            entity_description: String::new(),
            max_lexical_score: score,
            capabilities: vec![EntityCapabilityEvidence {
                capability_id: format!("{api}:{entity}:query"),
                capability_name: "query".into(),
                kind: "Query".into(),
                effect: crate::SemanticEffect::Read,
                description: String::new(),
                reason_codes: vec![],
                lexical_score: score,
            }],
            relation_hints: relations.into(),
            catalog_route_evidence: true,
        }
    }

    #[test]
    fn default_presentation_bound_is_twelve() {
        assert_eq!(SeedBundleConfig::default().max_bundles, 12);
    }

    #[test]
    fn unanchored_alternatives_do_not_become_cross_catalog_bundles() {
        let bundles = vec![
            bundle("github", "Issue", 10, ""),
            bundle("jira", "Issue", 9, ""),
        ];
        let out = build_candidate_seed_bundles("search issues", &bundles, Default::default());
        assert!(out.iter().all(|b| !b.is_cross_catalog()));
    }

    #[test]
    fn explicit_named_catalogs_build_cross_catalog_bundle() {
        let bundles = vec![
            bundle("github", "Issue", 10, ""),
            bundle("clickup", "Task", 9, ""),
        ];
        // Cross-catalog from ≥2 named entry_ids — not English sync/mirror verbs.
        let out = build_candidate_seed_bundles(
            "Move github issues into clickup tasks",
            &bundles,
            Default::default(),
        );
        assert!(
            out.iter().any(|b| {
                b.candidate_ids == ["github:Issue".to_string(), "clickup:Task".to_string()]
            }),
            "got {out:?}"
        );
    }

    #[test]
    fn relation_covered_root_pair_is_pruned() {
        let bundles = vec![
            bundle("gmail", "Thread", 10, "messages→Message"),
            bundle("gmail", "Message", 9, ""),
        ];
        let out =
            build_candidate_seed_bundles("find thread messages", &bundles, Default::default());
        assert!(!out.iter().any(|b| {
            b.candidate_ids.contains(&"gmail:Thread".to_string())
                && b.candidate_ids.contains(&"gmail:Message".to_string())
        }));
    }

    #[test]
    fn bounded_enumeration_preserves_single_root_alternatives() {
        let bundles = vec![
            bundle("github", "Issue", 10, ""),
            bundle("github", "PullRequest", 9, ""),
            bundle("github", "Repository", 8, ""),
        ];
        let out = build_candidate_seed_bundles(
            "search GitHub",
            &bundles,
            SeedBundleConfig {
                max_bundles: 3,
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|b| b.candidate_ids.len() == 1));
    }

    #[test]
    fn bounded_enumeration_keeps_highest_scoring_pairs() {
        let bundles = vec![
            bundle("github", "A", 100, ""),
            bundle("github", "B", 99, ""),
            bundle("github", "C", 98, ""),
            bundle("github", "D", 1, ""),
        ];
        let out = build_candidate_seed_bundles(
            "search GitHub",
            &bundles,
            SeedBundleConfig {
                max_bundles: 6,
                ..Default::default()
            },
        );
        let pairs: Vec<&CandidateSeedBundle> =
            out.iter().filter(|b| b.candidate_ids.len() == 2).collect();
        assert_eq!(pairs.len(), 2);
        assert!(pairs
            .iter()
            .all(|b| !b.candidate_ids.contains(&"github:D".to_string())));
    }

    #[test]
    fn finalize_keeps_singleton_per_provider_before_truncating() {
        let bundles = vec![
            bundle("github", "Issue", 100, ""),
            bundle("github", "PullRequest", 99, ""),
            bundle("jira", "Issue", 98, ""),
            bundle("outlook", "Message", 97, ""),
            bundle("gmail", "Message", 96, ""),
        ];
        let out = build_candidate_seed_bundles(
            "search issues and mail",
            &bundles,
            SeedBundleConfig {
                max_bundles: 4,
                ..Default::default()
            },
        );
        let catalogs: HashSet<String> = out
            .iter()
            .filter(|b| b.candidate_ids.len() == 1)
            .flat_map(|b| b.catalogs.clone())
            .collect();
        assert!(catalogs.contains("github"));
        assert!(catalogs.contains("jira"));
        assert!(catalogs.contains("gmail"));
        assert!(catalogs.contains("outlook"));
    }
}
