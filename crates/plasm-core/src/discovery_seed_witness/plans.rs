//! Minimal seed-plan enumeration and verification over selected witnesses.

use std::collections::{BTreeSet, HashSet};

use super::corpus::{RequirementWitness, WitnessCorpus, WitnessKind};

/// Max complete plans shortlisted for pairwise compare.
pub const MAX_COMPARE_PLANS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterministicSeedPlan {
    pub symbol: String,
    pub candidate_ids: Vec<String>,
    pub entities: Vec<(String, String)>,
    pub lexical_score: u32,
    pub covered_witness_symbols: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanConstructError {
    EmptyWitnesses,
    Uncoverable { missing: Vec<String> },
}

impl std::fmt::Display for PlanConstructError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyWitnesses => write!(f, "no witnesses selected"),
            Self::Uncoverable { missing } => {
                write!(f, "uncoverable witnesses: {}", missing.join(", "))
            }
        }
    }
}

/// How DirectCapability leaves are covered for plan **seating**.
///
/// Satellite mint uses a separate cover helper
/// ([`super::satellites::candidates_covering_with_satellites`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverMode {
    /// Owner candidate only — no parent relief.
    Strict,
    /// ParentPreferred Ready seating: parents seat Create/read leaves; Actions stay leaf-seated.
    ParentPreferred,
}

/// Sealed cover policy so seating and teaching do not share an anonymous fn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverPolicy {
    Seating(CoverMode),
    Teaching,
}

/// Resolve covering candidate ids under an explicit [`CoverPolicy`].
pub fn covering(
    corpus: &WitnessCorpus,
    witness: &RequirementWitness,
    policy: CoverPolicy,
    selected: &[usize],
) -> Vec<String> {
    match policy {
        CoverPolicy::Seating(mode) => candidates_covering(corpus, witness, mode, selected),
        CoverPolicy::Teaching => {
            super::satellites::candidates_covering_with_satellites(corpus, witness)
        }
    }
}

/// Enumerate minimal 1–3 seed plans covering every selected witness (Strict / orphan tests).
pub fn construct_minimal_plans(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Result<Vec<DeterministicSeedPlan>, PlanConstructError> {
    construct_minimal_plans_with_cover(corpus, selected, CoverMode::Strict)
}

/// Ready-path plan enumeration under [`CoverMode::ParentPreferred`].
pub fn construct_workflow_seed_plans(
    corpus: &WitnessCorpus,
    selected: &[usize],
) -> Result<Vec<DeterministicSeedPlan>, PlanConstructError> {
    construct_minimal_plans_with_cover(corpus, selected, CoverMode::ParentPreferred)
}

pub fn construct_minimal_plans_with_cover(
    corpus: &WitnessCorpus,
    selected: &[usize],
    cover: CoverMode,
) -> Result<Vec<DeterministicSeedPlan>, PlanConstructError> {
    if selected.is_empty() {
        return Err(PlanConstructError::EmptyWitnesses);
    }

    let mut missing = Vec::new();
    let mut cover_lists: Vec<Vec<String>> = Vec::with_capacity(selected.len());
    for &idx in selected {
        let witness = &corpus.witnesses[idx];
        let covers = candidates_covering(corpus, witness, cover, selected);
        if covers.is_empty() {
            missing.push(witness.symbol.clone());
        }
        cover_lists.push(covers);
    }
    if !missing.is_empty() {
        return Err(PlanConstructError::Uncoverable { missing });
    }

    let universe: Vec<String> = {
        let mut set = BTreeSet::new();
        for list in &cover_lists {
            for id in list {
                set.insert(id.clone());
            }
        }
        set.into_iter().collect()
    };

    let mut raw_plans: Vec<Vec<String>> = Vec::new();
    let n = universe.len();
    for size in 1..=3.min(n) {
        for combo in combinations(n, size) {
            let ids: Vec<String> = combo.iter().map(|&i| universe[i].clone()).collect();
            if covers_all(&ids, &cover_lists) {
                raw_plans.push(ids);
            }
        }
    }

    // Minimality: drop any plan that properly contains a smaller plan.
    raw_plans.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let mut minimal: Vec<Vec<String>> = Vec::new();
    for plan in raw_plans {
        let plan_set: HashSet<&str> = plan.iter().map(String::as_str).collect();
        let dominated = minimal.iter().any(|smaller| {
            smaller.iter().all(|id| plan_set.contains(id.as_str())) && smaller.len() < plan.len()
        });
        if !dominated {
            minimal.push(plan);
        }
    }

    if minimal.is_empty() {
        return Err(PlanConstructError::Uncoverable {
            missing: selected
                .iter()
                .map(|&i| corpus.witnesses[i].symbol.clone())
                .collect(),
        });
    }

    let covered_symbols: Vec<String> = selected
        .iter()
        .map(|&i| corpus.witnesses[i].symbol.clone())
        .collect();

    let mut plans: Vec<DeterministicSeedPlan> = minimal
        .into_iter()
        .map(|ids| materialize_plan(corpus, ids, &covered_symbols))
        .collect();

    plans.sort_by(|a, b| {
        a.candidate_ids
            .len()
            .cmp(&b.candidate_ids.len())
            .then_with(|| b.lexical_score.cmp(&a.lexical_score))
            .then_with(|| a.summary.cmp(&b.summary))
    });

    for (idx, plan) in plans.iter_mut().enumerate() {
        plan.symbol = format!("p{}", idx + 1);
    }

    Ok(plans)
}

/// Shortlist highest-score plans for pairwise compare.
pub fn shortlist_plans(plans: &[DeterministicSeedPlan]) -> Vec<DeterministicSeedPlan> {
    plans.iter().take(MAX_COMPARE_PLANS).cloned().collect()
}

/// Prefer a non-attach root plan when a same-size attach-leaf-only rival exists.
///
/// Catalog-neutral: uses authored `seed_nav` / `seed_class` stamps only. Avoids
/// LLM pairwise anointing Comment/Review leaves when Issue/PR roots also cover.
/// Prefer `primary` over `ambient` when both are non-leaf rivals.
pub fn prefer_primary_cover_plan<'a>(
    corpus: &WitnessCorpus,
    plans: &'a [DeterministicSeedPlan],
) -> Option<&'a DeterministicSeedPlan> {
    if plans.len() < 2 {
        return None;
    }
    let min_len = plans.iter().map(|p| p.candidate_ids.len()).min()?;
    let sized: Vec<&DeterministicSeedPlan> = plans
        .iter()
        .filter(|p| p.candidate_ids.len() == min_len)
        .collect();
    if sized.len() < 2 {
        return None;
    }

    let attach_only = |plan: &DeterministicSeedPlan| -> bool {
        !plan.candidate_ids.is_empty()
            && plan.candidate_ids.iter().all(|id| {
                corpus.witnesses.iter().any(|w| {
                    &w.owner_candidate_id == id
                        && matches!(&w.kind, WitnessKind::DirectCapability { .. })
                        && super::satellites::is_attach_or_dependent_leaf(w)
                })
            })
    };

    let has_primary_seed = |plan: &DeterministicSeedPlan| -> bool {
        plan.candidate_ids
            .iter()
            .any(|id| corpus.roles.owner_is_primary(id))
    };

    let leaf_plans: Vec<&DeterministicSeedPlan> =
        sized.iter().copied().filter(|p| attach_only(p)).collect();
    let non_leaf: Vec<&DeterministicSeedPlan> =
        sized.iter().copied().filter(|p| !attach_only(p)).collect();
    if non_leaf.is_empty() {
        return None;
    }
    if leaf_plans.is_empty() && non_leaf.len() > 1 {
        // Prefer primary over ambient among non-leaf peers.
        let primaries: Vec<&DeterministicSeedPlan> = non_leaf
            .iter()
            .copied()
            .filter(|p| has_primary_seed(p))
            .collect();
        if primaries.len() == 1 {
            return Some(primaries[0]);
        }
        return None;
    }
    if !leaf_plans.is_empty() {
        let primaries: Vec<&DeterministicSeedPlan> = non_leaf
            .iter()
            .copied()
            .filter(|p| has_primary_seed(p))
            .collect();
        if primaries.len() == 1 {
            return Some(primaries[0]);
        }
        if primaries.is_empty() && non_leaf.len() == 1 {
            return Some(non_leaf[0]);
        }
    }
    None
}

/// Verify a Ready workflow plan under [`CoverMode::ParentPreferred`].
pub fn verify_plan(
    corpus: &WitnessCorpus,
    plan: &DeterministicSeedPlan,
    selected: &[usize],
) -> bool {
    verify_plan_with_cover(corpus, plan, selected, CoverMode::ParentPreferred)
}

pub fn verify_plan_with_cover(
    corpus: &WitnessCorpus,
    plan: &DeterministicSeedPlan,
    selected: &[usize],
    cover: CoverMode,
) -> bool {
    if plan.candidate_ids.is_empty() || plan.candidate_ids.len() > 3 {
        return false;
    }
    if !corpus.brand_lock_catalogs.is_empty() {
        for id in &plan.candidate_ids {
            let Some(bundle) = corpus.bundles.iter().find(|b| &b.candidate_id == id) else {
                return false;
            };
            if !corpus
                .brand_lock_catalogs
                .iter()
                .any(|l| l == &bundle.entry_id)
            {
                return false;
            }
        }
    }
    let cover_lists: Vec<Vec<String>> = selected
        .iter()
        .map(|&idx| candidates_covering(corpus, &corpus.witnesses[idx], cover, selected))
        .collect();
    covers_all(&plan.candidate_ids, &cover_lists)
}

pub(super) fn candidates_covering(
    corpus: &WitnessCorpus,
    witness: &RequirementWitness,
    cover: CoverMode,
    selected: &[usize],
) -> Vec<String> {
    match cover {
        CoverMode::Strict => candidates_covering_strict(corpus, witness),
        CoverMode::ParentPreferred => {
            super::satellites::candidates_covering_for_plan(corpus, witness, selected)
        }
    }
}

fn candidates_covering_strict(corpus: &WitnessCorpus, witness: &RequirementWitness) -> Vec<String> {
    if corpus
        .bundles
        .iter()
        .any(|b| b.candidate_id == witness.owner_candidate_id)
    {
        vec![witness.owner_candidate_id.clone()]
    } else {
        Vec::new()
    }
}

fn covers_all(ids: &[String], cover_lists: &[Vec<String>]) -> bool {
    let set: HashSet<&str> = ids.iter().map(String::as_str).collect();
    cover_lists
        .iter()
        .all(|covers| covers.iter().any(|id| set.contains(id.as_str())))
}

fn materialize_plan(
    corpus: &WitnessCorpus,
    candidate_ids: Vec<String>,
    covered_witness_symbols: &[String],
) -> DeterministicSeedPlan {
    let mut entities = Vec::new();
    let mut lexical = 0u32;
    let mut parts = Vec::new();
    for id in &candidate_ids {
        if let Some(bundle) = corpus.bundles.iter().find(|b| &b.candidate_id == id) {
            entities.push((bundle.entry_id.clone(), bundle.entity.clone()));
            lexical = lexical.saturating_add(bundle.max_lexical_score);
            let kinds: Vec<&str> = bundle
                .capabilities
                .iter()
                .map(|c| c.kind.as_str())
                .collect();
            parts.push(format!(
                "{}.{} ops=[{}]",
                bundle.entry_id,
                bundle.entity,
                kinds.join(",")
            ));
        }
    }
    DeterministicSeedPlan {
        symbol: String::new(),
        candidate_ids,
        entities,
        lexical_score: lexical,
        covered_witness_symbols: covered_witness_symbols.to_vec(),
        summary: parts.join(" + "),
    }
}

fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut cur = Vec::with_capacity(k);
    fn rec(start: usize, n: usize, k: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == k {
            out.push(cur.clone());
            return;
        }
        for i in start..n {
            cur.push(i);
            rec(i + 1, n, k, cur, out);
            cur.pop();
        }
    }
    rec(0, n, k, &mut cur, &mut out);
    out
}
