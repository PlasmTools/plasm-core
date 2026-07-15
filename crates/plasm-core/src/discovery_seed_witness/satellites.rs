//! Teaching satellites: attach/dependent (and 1-hop hop targets) whose DirectCapabilities
//! are covered by workflow seeds via parent cover, but still need `e#` for program reachability.

use std::collections::{BTreeSet, HashSet};

use super::corpus::{RequirementWitness, WitnessCorpus, WitnessKind};
use super::kind::CapBucket;
use super::named_in_intent::witness_named_in_intent;
use super::plans::DeterministicSeedPlan;

/// Max extra entities minted into the teaching table beyond workflow seeds.
pub const MAX_TEACHING_SATELLITES: usize = 4;

/// Outcome of admitting teaching satellites for a ready plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatelliteAdmission {
    /// `(entry_id, entity)` in stable order (not already in the plan seed set).
    Ok(Vec<(String, String)>),
    /// Required leaves exceed [`MAX_TEACHING_SATELLITES`] — caller should clarify.
    Overflow {
        required: Vec<(String, String)>,
        admitted: Vec<(String, String)>,
    },
}

/// True when this witness is an attach/dependent leaf (prune/cover satellite path).
pub fn is_attach_or_dependent_leaf(witness: &RequirementWitness) -> bool {
    witness.seed_nav.is_attach() || witness.seed_class.is_dependent()
}

/// Candidate ids that may cover `witness` for **satellite mint** checks (owner ∪ parents).
///
/// Distinct from plan seating ([`candidates_covering_for_plan`]): admit asks whether an
/// already-chosen workflow seed covers a leaf so we can mint `e#`.
pub fn candidates_covering_with_satellites(
    corpus: &WitnessCorpus,
    witness: &RequirementWitness,
) -> Vec<String> {
    let mut out = Vec::new();
    push_owner_if_present(corpus, witness, &mut out);
    match &witness.kind {
        WitnessKind::DirectCapability { .. } if is_attach_or_dependent_leaf(witness) => {
            push_pool_parents(corpus, witness, &mut out);
            out
        }
        WitnessKind::RelationHop { .. } => out,
        WitnessKind::DirectCapability {
            entry_id, entity, ..
        } => {
            if witness.seed_class.is_primary() || witness.seed_class.is_ambient() {
                return out;
            }
            push_hop_parents(corpus, entry_id, entity, &mut out);
            out
        }
    }
}

/// Candidate ids for **workflow plan seating** (ParentPreferred).
///
/// - Attach/dependent **Action**: owner only (localized mutate keeps the leaf seat).
/// - Attach/dependent **read**: parents if any in-pool, else owner.
/// - Attach/dependent **Create/Update**: parents only when a parent Direct is also in
///   `selected`; otherwise owner alone (localized comment create vs composite Issue+comment).
/// - Primary / ambient / RelationHop: owner only; non-primary hop Directs may admit hop parents.
pub fn candidates_covering_for_plan(
    corpus: &WitnessCorpus,
    witness: &RequirementWitness,
    selected: &[usize],
) -> Vec<String> {
    match &witness.kind {
        WitnessKind::DirectCapability {
            entry_id,
            entity,
            kind,
            ..
        } if is_attach_or_dependent_leaf(witness) => {
            if is_action_kind(kind) {
                return owner_only(corpus, witness);
            }
            let mut parents = Vec::new();
            push_pool_parents(corpus, witness, &mut parents);
            if is_read_kind(kind) {
                if parents.is_empty() {
                    // Peers that declare this entity as a child (attach/own).
                    for w in &corpus.witnesses {
                        let WitnessKind::DirectCapability {
                            entry_id: e,
                            entity: peer,
                            ..
                        } = &w.kind
                        else {
                            continue;
                        };
                        if e != entry_id || peer == entity {
                            continue;
                        }
                        if w.pool.child_targets().any(|child| child == entity.as_str()) {
                            let pid = format!("{entry_id}:{peer}");
                            if corpus_has_owner(corpus, &pid) && !parents.contains(&pid) {
                                parents.push(pid);
                            }
                        }
                    }
                }
                if !parents.is_empty() {
                    let primary_parents: Vec<String> = parents
                        .iter()
                        .filter(|pid| corpus.roles.owner_is_primary(pid))
                        .cloned()
                        .collect();
                    if !primary_parents.is_empty() {
                        return primary_parents;
                    }
                    return parents;
                }
                return owner_only(corpus, witness);
            }
            // Create / Update: parent seats only when parent Direct was also selected.
            let parent_selected = parents.iter().any(|pid| {
                selected.iter().any(|&idx| {
                    corpus
                        .witnesses
                        .get(idx)
                        .is_some_and(|w| &w.owner_candidate_id == pid)
                })
            });
            if parent_selected {
                parents
            } else {
                owner_only(corpus, witness)
            }
        }
        WitnessKind::RelationHop { .. } => owner_only(corpus, witness),
        WitnessKind::DirectCapability {
            entry_id,
            entity,
            kind,
            ..
        } => {
            // own Query/Get: prefer source (collection owner), even when the leaf is primary.
            if witness.seed_nav.is_own() && is_query_get_kind(kind) {
                let mut sources = Vec::new();
                for edge in witness.own_pairs.iter() {
                    if edge.target == *entity {
                        let pid = format!("{entry_id}:{}", edge.source);
                        if corpus_has_owner(corpus, &pid) && !corpus.roles.owner_is_ambient(&pid) {
                            sources.push(pid);
                        }
                    }
                }
                if sources.is_empty() {
                    let mut parents = Vec::new();
                    push_pool_parents(corpus, witness, &mut parents);
                    for pid in parents {
                        if !corpus.roles.owner_is_ambient(&pid) {
                            sources.push(pid);
                        }
                    }
                }
                if !sources.is_empty() {
                    sources.sort();
                    sources.dedup();
                    return sources;
                }
            }
            let mut out = owner_only(corpus, witness);
            if witness.seed_class.is_primary() || witness.seed_class.is_ambient() {
                return out;
            }
            push_hop_parents(corpus, entry_id, entity, &mut out);
            out
        }
    }
}

/// True when selection seated only peer-primary Create/Search/Query while a closed
/// attach/dependent **Action** matches the intent (Pin vs Message). Caller should
/// clarify — never silently rewrite to the Action.
pub fn dependent_action_shadowed_by_peer_primary(
    corpus: &WitnessCorpus,
    selected: &[usize],
    intent: &str,
) -> Option<Vec<(String, String)>> {
    let selected_directs: Vec<&RequirementWitness> = selected
        .iter()
        .filter_map(|&idx| corpus.witnesses.get(idx))
        .filter(|w| matches!(&w.kind, WitnessKind::DirectCapability { .. }))
        .collect();
    if selected_directs.is_empty() {
        return None;
    }
    if selected_directs.iter().any(|w| {
        matches!(&w.kind, WitnessKind::DirectCapability { kind, .. } if is_action_kind(kind))
            && is_attach_or_dependent_leaf(w)
    }) {
        return None;
    }
    let peer_primary_distractors: Vec<&RequirementWitness> = selected_directs
        .iter()
        .copied()
        .filter(|w| {
            w.seed_class.is_primary()
                && matches!(
                    &w.kind,
                    WitnessKind::DirectCapability { kind, .. }
                        if is_read_kind(kind)
                            || matches!(kind.as_str(), "Create" | "create" | "Update" | "update")
                )
        })
        .collect();
    if peer_primary_distractors.is_empty() {
        return None;
    }
    let distractor_catalogs: HashSet<&str> = peer_primary_distractors
        .iter()
        .filter_map(|w| witness_entry_id(w))
        .collect();

    let mut shadowed: BTreeSet<(String, String)> = BTreeSet::new();
    for (idx, witness) in corpus.witnesses.iter().enumerate() {
        if selected.contains(&idx) {
            continue;
        }
        let WitnessKind::DirectCapability {
            entry_id,
            entity,
            kind,
            ..
        } = &witness.kind
        else {
            continue;
        };
        if !is_action_kind(kind) || !is_attach_or_dependent_leaf(witness) {
            continue;
        }
        if !distractor_catalogs.contains(entry_id.as_str()) {
            continue;
        }
        if !witness_named_in_intent(intent, witness) {
            continue;
        }
        shadowed.insert((entry_id.clone(), entity.clone()));
    }
    if shadowed.is_empty() {
        None
    } else {
        Some(shadowed.into_iter().collect())
    }
}

fn is_action_kind(kind: &str) -> bool {
    CapBucket::is_action(kind)
}

fn is_read_kind(kind: &str) -> bool {
    CapBucket::is_read_kind(kind)
}

fn is_query_get_kind(kind: &str) -> bool {
    CapBucket::is_query_get(kind)
}

fn corpus_has_owner(corpus: &WitnessCorpus, candidate_id: &str) -> bool {
    corpus
        .bundles
        .iter()
        .any(|b| b.candidate_id == candidate_id)
}

fn owner_only(corpus: &WitnessCorpus, witness: &RequirementWitness) -> Vec<String> {
    let mut out = Vec::new();
    push_owner_if_present(corpus, witness, &mut out);
    out
}

fn push_owner_if_present(
    corpus: &WitnessCorpus,
    witness: &RequirementWitness,
    out: &mut Vec<String>,
) {
    if corpus
        .bundles
        .iter()
        .any(|b| b.candidate_id == witness.owner_candidate_id)
    {
        out.push(witness.owner_candidate_id.clone());
    }
}

fn push_pool_parents(corpus: &WitnessCorpus, witness: &RequirementWitness, out: &mut Vec<String>) {
    for parent_entity in witness.pool.parent_entities() {
        for bundle in &corpus.bundles {
            if bundle.entity == parent_entity
                && bundle.entry_id == witness_entry_id(witness).unwrap_or(bundle.entry_id.as_str())
                && !out.iter().any(|id| id == &bundle.candidate_id)
            {
                out.push(bundle.candidate_id.clone());
            }
        }
    }
}

fn push_hop_parents(corpus: &WitnessCorpus, entry_id: &str, entity: &str, out: &mut Vec<String>) {
    for bundle in &corpus.bundles {
        if bundle.entry_id != entry_id {
            continue;
        }
        let Some(parent_w) = corpus.witnesses.iter().find(|w| {
            w.owner_candidate_id == bundle.candidate_id
                && matches!(
                    &w.kind,
                    WitnessKind::DirectCapability {
                        entity: e, ..
                    } if e == &bundle.entity
                )
        }) else {
            continue;
        };
        if parent_w.pool.child_targets().any(|t| t == entity)
            && !out.iter().any(|id| id == &bundle.candidate_id)
        {
            out.push(bundle.candidate_id.clone());
        }
    }
}

fn witness_entry_id(witness: &RequirementWitness) -> Option<&str> {
    match &witness.kind {
        WitnessKind::DirectCapability { entry_id, .. }
        | WitnessKind::RelationHop { entry_id, .. } => Some(entry_id.as_str()),
    }
}

fn witness_entity_ref(witness: &RequirementWitness) -> Option<(String, String)> {
    match &witness.kind {
        WitnessKind::DirectCapability {
            entry_id, entity, ..
        } => Some((entry_id.clone(), entity.clone())),
        WitnessKind::RelationHop {
            entry_id,
            target_entity,
            ..
        } => Some((entry_id.clone(), target_entity.clone())),
    }
}

/// Admit teaching satellites for covered witnesses whose owners are not workflow seeds.
///
/// `selected` should be the **pre-prune** witness indices when available so attach reads
/// dropped by FO prune still mint `e#`. When `intent` is set, also revive attach/dependent
/// (and hop) Directs in the corpus that plan seeds cover and the intent names (authored
/// discovery aliases) — so IssueComment Create is not lost when assessment folds "comment" into Issue.
pub fn admit_teaching_satellites(
    corpus: &WitnessCorpus,
    plan: &DeterministicSeedPlan,
    selected: &[usize],
    intent: Option<&str>,
) -> SatelliteAdmission {
    let seed_ids: HashSet<&str> = plan.candidate_ids.iter().map(String::as_str).collect();
    let seed_entities: HashSet<(&str, &str)> = plan
        .entities
        .iter()
        .map(|(e, ent)| (e.as_str(), ent.as_str()))
        .collect();

    let mut consider: BTreeSet<usize> = selected.iter().copied().collect();
    if let Some(intent) = intent {
        for (idx, witness) in corpus.witnesses.iter().enumerate() {
            if consider.contains(&idx) {
                continue;
            }
            if witness_eligible_satellite(corpus, witness, intent, &seed_ids) {
                consider.insert(idx);
            }
        }
    }

    let mut required: BTreeSet<(String, String)> = BTreeSet::new();
    for idx in consider {
        let Some(witness) = corpus.witnesses.get(idx) else {
            continue;
        };
        if let Some(intent) = intent {
            if !witness_named_in_intent(intent, witness) {
                continue;
            }
        }
        if !is_satellite_candidate_witness(corpus, witness) {
            continue;
        }
        let Some((entry_id, entity)) = witness_entity_ref(witness) else {
            continue;
        };
        if seed_entities.contains(&(entry_id.as_str(), entity.as_str())) {
            continue;
        }
        if seed_ids.contains(witness.owner_candidate_id.as_str()) {
            continue;
        }
        let covers = candidates_covering_with_satellites(corpus, witness);
        if !covers.iter().any(|id| seed_ids.contains(id.as_str())) {
            continue;
        }
        required.insert((entry_id, entity));
    }

    let required_vec: Vec<(String, String)> = required.into_iter().collect();
    if required_vec.len() > MAX_TEACHING_SATELLITES {
        let admitted = required_vec
            .iter()
            .take(MAX_TEACHING_SATELLITES)
            .cloned()
            .collect();
        return SatelliteAdmission::Overflow {
            required: required_vec,
            admitted,
        };
    }
    SatelliteAdmission::Ok(required_vec)
}

fn witness_eligible_satellite(
    corpus: &WitnessCorpus,
    witness: &RequirementWitness,
    intent: &str,
    seed_ids: &HashSet<&str>,
) -> bool {
    witness_named_in_intent(intent, witness)
        && is_satellite_candidate_witness(corpus, witness)
        && candidates_covering_with_satellites(corpus, witness)
            .iter()
            .any(|id| seed_ids.contains(id.as_str()))
}

/// Apply satellite admission onto a Ready [`SeedSelectionRaw`] (multipass + coverage).
///
/// On overflow, flips the decision to Clarify with extend-hint alternatives.
pub fn apply_teaching_satellites_to_ready(
    mut raw: crate::discovery_seed_select::SeedSelectionRaw,
    corpus: &WitnessCorpus,
    plan: &DeterministicSeedPlan,
    satellite_indices: &[usize],
    intent: Option<&str>,
) -> crate::discovery_seed_select::SeedSelectionRaw {
    use crate::discovery_seed_select::{SeedAlternativeSetRaw, SeedSelectionDecision};

    match admit_teaching_satellites(corpus, plan, satellite_indices, intent) {
        SatelliteAdmission::Ok(sats) => {
            let sat_summary = if sats.is_empty() {
                "none".to_string()
            } else {
                sats.iter()
                    .map(|(e, ent)| format!("{e}.{ent}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            raw.teaching_satellites = sats;
            if !raw.reasoning.contains("teaching_satellites=") {
                raw.reasoning = format!("{} | teaching_satellites={sat_summary}", raw.reasoning);
            }
            raw
        }
        SatelliteAdmission::Overflow { required, .. } => {
            let names: Vec<String> = required
                .iter()
                .map(|(e, ent)| format!("{e}:{ent}"))
                .collect();
            raw.decision = SeedSelectionDecision::Clarify;
            raw.selected_ids.clear();
            raw.supporting_capability_ids.clear();
            raw.teaching_satellites.clear();
            raw.alternative_sets = vec![
                SeedAlternativeSetRaw {
                    label: format!("extend_required_leaves:{}", names.join(",")),
                    candidate_ids: Vec::new(),
                },
                SeedAlternativeSetRaw {
                    label: "narrow_intent".into(),
                    candidate_ids: Vec::new(),
                },
            ];
            raw.reasoning = format!(
                "{} | satellite_overflow required={}",
                raw.reasoning,
                names.join(",")
            );
            raw
        }
    }
}

fn is_satellite_candidate_witness(corpus: &WitnessCorpus, witness: &RequirementWitness) -> bool {
    if is_attach_or_dependent_leaf(witness) {
        return true;
    }
    if matches!(&witness.kind, WitnessKind::RelationHop { .. }) {
        return true;
    }
    // Hop-target Direct (Branch under Repository): cover list includes a non-owner parent.
    if matches!(&witness.kind, WitnessKind::DirectCapability { .. }) {
        let covers = candidates_covering_with_satellites(corpus, witness);
        return covers
            .iter()
            .any(|id| id.as_str() != witness.owner_candidate_id.as_str());
    }
    false
}

/// True when a **workflow seed candidate** is teaching-leaf shaped and should lose
/// pairwise seats to a parent-rooted plan covering the same witnesses.
///
/// Attach/dependent leaves and non-primary hop Directs demote. Primary/`own`
/// targets stay workflow seats — Channel/Thread must not steal Message when the
/// intent is the owned entity itself.
pub fn seed_candidate_is_teaching_leaf(corpus: &WitnessCorpus, candidate_id: &str) -> bool {
    corpus.witnesses.iter().any(|w| {
        if w.owner_candidate_id != candidate_id {
            return false;
        }
        if is_attach_or_dependent_leaf(w) {
            return true;
        }
        if w.seed_class.is_primary() || w.seed_class.is_ambient() {
            return false;
        }
        matches!(&w.kind, WitnessKind::DirectCapability { .. })
            && candidates_covering_with_satellites(corpus, w)
                .iter()
                .any(|id| id.as_str() != w.owner_candidate_id.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_auto_seed::{EntityCandidateBundle, EntityCapabilityEvidence};
    use crate::discovery_seed_witness::corpus::WitnessKind;
    use crate::discovery_seed_witness::plans::{
        construct_minimal_plans_with_cover, construct_workflow_seed_plans, CoverMode,
        DeterministicSeedPlan,
    };
    use crate::discovery_seed_witness::role_index::CorpusRoleIndex;
    use crate::discovery_seed_witness::roles::{PoolLinks, SeedClassStamp, SeedNavStamp};
    use crate::schema::{DiscoverySeedClass, DiscoverySeedNav};

    fn sketch(id: &str, name: &str, kind: &str, score: u32) -> EntityCapabilityEvidence {
        EntityCapabilityEvidence {
            capability_id: id.into(),
            capability_name: name.into(),
            kind: kind.into(),
            description: name.into(),
            reason_codes: vec![],
            lexical_score: score,
        }
    }

    fn bundle(
        id: &str,
        entity: &str,
        score: u32,
        caps: Vec<EntityCapabilityEvidence>,
        hints: &str,
    ) -> EntityCandidateBundle {
        EntityCandidateBundle {
            candidate_id: id.into(),
            entry_id: "github".into(),
            entity: entity.into(),
            entity_description: entity.into(),
            max_lexical_score: score,
            capabilities: caps,
            relation_hints: hints.into(),
            catalog_route_evidence: true,
        }
    }

    /// Minimal corpus-shaped witnesses without full CGS (unit isolation).
    fn leaf_and_parent_corpus() -> (WitnessCorpus, usize, usize) {
        use std::collections::HashMap;
        let issue = RequirementWitness {
            symbol: "w1".into(),
            kind: WitnessKind::DirectCapability {
                entry_id: "github".into(),
                entity: "Issue".into(),
                capability_id: "github:Issue:issue_create".into(),
                capability_name: "issue_create".into(),
                kind: "Create".into(),
                description: "Create issue".into(),
            },
            owner_candidate_id: "github:Issue".into(),
            lexical_score: 90,
            summary: "Create Issue".into(),
            entity_description: "Issue".into(),
            aliases: "issue|bug|ticket|github issue".into(),
            pool: PoolLinks {
                parents: BTreeSet::new(),
                children: [
                    crate::discovery_seed_witness::roles::PoolChild::new(
                        "comments",
                        "IssueComment",
                    ),
                    crate::discovery_seed_witness::roles::PoolChild::new("labels", "Label"),
                ]
                .into_iter()
                .collect(),
                siblings: BTreeSet::new(),
            },
            seed_class: SeedClassStamp::Authored(DiscoverySeedClass::Primary),
            seed_nav: SeedNavStamp::Unset,
            own_pairs: Default::default(),
        };
        let comment = RequirementWitness {
            symbol: "w2".into(),
            kind: WitnessKind::DirectCapability {
                entry_id: "github".into(),
                entity: "IssueComment".into(),
                capability_id: "github:IssueComment:issue_comment_create".into(),
                capability_name: "issue_comment_create".into(),
                kind: "Create".into(),
                description: "Create comment".into(),
            },
            owner_candidate_id: "github:IssueComment".into(),
            lexical_score: 70,
            summary: "Create IssueComment".into(),
            entity_description: "IssueComment".into(),
            aliases: "comment|issue comment|triage note".into(),
            pool: PoolLinks {
                parents: ["Issue".into()].into_iter().collect(),
                children: BTreeSet::new(),
                siblings: BTreeSet::new(),
            },
            seed_class: SeedClassStamp::Authored(DiscoverySeedClass::Dependent),
            seed_nav: SeedNavStamp::Authored(DiscoverySeedNav::Attach),
            own_pairs: Default::default(),
        };
        let label = RequirementWitness {
            symbol: "w3".into(),
            kind: WitnessKind::DirectCapability {
                entry_id: "github".into(),
                entity: "Label".into(),
                capability_id: "github:Label:label_query".into(),
                capability_name: "label_query".into(),
                kind: "Query".into(),
                description: "List labels".into(),
            },
            owner_candidate_id: "github:Label".into(),
            lexical_score: 60,
            summary: "Query Label".into(),
            entity_description: "Label".into(),
            aliases: "label|labels".into(),
            pool: PoolLinks {
                parents: ["Issue".into()].into_iter().collect(),
                children: BTreeSet::new(),
                siblings: BTreeSet::new(),
            },
            seed_class: SeedClassStamp::Authored(DiscoverySeedClass::Dependent),
            seed_nav: SeedNavStamp::Authored(DiscoverySeedNav::Attach),
            own_pairs: Default::default(),
        };
        let bundles = vec![
            bundle(
                "github:Issue",
                "Issue",
                90,
                vec![sketch(
                    "github:Issue:issue_create",
                    "issue_create",
                    "Create",
                    90,
                )],
                "comments→IssueComment labels→Label",
            ),
            bundle(
                "github:IssueComment",
                "IssueComment",
                70,
                vec![sketch(
                    "github:IssueComment:issue_comment_create",
                    "issue_comment_create",
                    "Create",
                    70,
                )],
                "",
            ),
            bundle(
                "github:Label",
                "Label",
                60,
                vec![sketch(
                    "github:Label:label_query",
                    "label_query",
                    "Query",
                    60,
                )],
                "",
            ),
        ];
        let witnesses = vec![issue, comment, label];
        let mut symbol_to_index = HashMap::new();
        for (i, w) in witnesses.iter().enumerate() {
            symbol_to_index.insert(w.symbol.clone(), i);
        }
        let corpus = WitnessCorpus {
            roles: CorpusRoleIndex::build(&witnesses),
            witnesses,
            bundles,
            brand_lock_catalogs: vec!["github".into()],
            symbol_to_index,
        };
        (corpus, 0, 1) // Issue idx, IssueComment idx
    }

    #[test]
    fn ambient_container_does_not_cover_primary_direct() {
        use std::collections::HashMap;
        let issue = RequirementWitness {
            symbol: "w1".into(),
            kind: WitnessKind::DirectCapability {
                entry_id: "gitlab".into(),
                entity: "Issue".into(),
                capability_id: "gitlab:Issue:Query".into(),
                capability_name: "issue_query".into(),
                kind: "Query".into(),
                description: "Query issues".into(),
            },
            owner_candidate_id: "gitlab:Issue".into(),
            lexical_score: 80,
            summary: "Query Issue".into(),
            entity_description: "Issue".into(),
            aliases: "issue".into(),
            pool: PoolLinks {
                parents: ["Project".into()].into_iter().collect(),
                children: BTreeSet::new(),
                siblings: BTreeSet::new(),
            },
            seed_class: SeedClassStamp::Authored(DiscoverySeedClass::Primary),
            seed_nav: SeedNavStamp::Unset,
            own_pairs: Default::default(),
        };
        let project = RequirementWitness {
            symbol: "w2".into(),
            kind: WitnessKind::DirectCapability {
                entry_id: "gitlab".into(),
                entity: "Project".into(),
                capability_id: "gitlab:Project:Get".into(),
                capability_name: "project_get".into(),
                kind: "Get".into(),
                description: "Get project".into(),
            },
            owner_candidate_id: "gitlab:Project".into(),
            lexical_score: 40,
            summary: "Get Project".into(),
            entity_description: "Project".into(),
            aliases: "project".into(),
            pool: PoolLinks {
                parents: BTreeSet::new(),
                children: [crate::discovery_seed_witness::roles::PoolChild::new(
                    "issues", "Issue",
                )]
                .into_iter()
                .collect(),
                siblings: BTreeSet::new(),
            },
            seed_class: SeedClassStamp::Authored(DiscoverySeedClass::Ambient),
            seed_nav: SeedNavStamp::Authored(DiscoverySeedNav::Locate),
            own_pairs: Default::default(),
        };
        let bundles = vec![
            EntityCandidateBundle {
                candidate_id: "gitlab:Issue".into(),
                entry_id: "gitlab".into(),
                entity: "Issue".into(),
                entity_description: "Issue".into(),
                max_lexical_score: 80,
                capabilities: vec![sketch("gitlab:Issue:Query", "issue_query", "Query", 80)],
                relation_hints: String::new(),
                catalog_route_evidence: true,
            },
            EntityCandidateBundle {
                candidate_id: "gitlab:Project".into(),
                entry_id: "gitlab".into(),
                entity: "Project".into(),
                entity_description: "Project".into(),
                max_lexical_score: 40,
                capabilities: vec![sketch("gitlab:Project:Get", "project_get", "Get", 40)],
                relation_hints: String::new(),
                catalog_route_evidence: true,
            },
        ];
        let mut witnesses = vec![issue, project];
        let mut symbol_to_index = HashMap::new();
        for (idx, w) in witnesses.iter_mut().enumerate() {
            symbol_to_index.insert(w.symbol.clone(), idx);
        }
        let corpus = WitnessCorpus {
            roles: CorpusRoleIndex::build(&witnesses),
            witnesses,
            bundles,
            brand_lock_catalogs: vec![],
            symbol_to_index,
        };
        let covers = candidates_covering_with_satellites(&corpus, &corpus.witnesses[0]);
        assert_eq!(
            covers,
            vec!["gitlab:Issue".to_string()],
            "ambient Project must not cover primary Issue: {covers:?}"
        );
    }

    #[test]
    fn parent_covers_attach_dependent_create_and_query() {
        let (corpus, issue_idx, comment_idx) = leaf_and_parent_corpus();
        let label_idx = 2;
        let comment_covers =
            candidates_covering_with_satellites(&corpus, &corpus.witnesses[comment_idx]);
        assert!(
            comment_covers.iter().any(|id| id == "github:Issue"),
            "Issue parent must cover IssueComment Create: {comment_covers:?}"
        );
        assert!(comment_covers.iter().any(|id| id == "github:IssueComment"));
        let label_covers =
            candidates_covering_with_satellites(&corpus, &corpus.witnesses[label_idx]);
        assert!(label_covers.iter().any(|id| id == "github:Issue"));
        let issue_covers =
            candidates_covering_with_satellites(&corpus, &corpus.witnesses[issue_idx]);
        assert_eq!(issue_covers, vec!["github:Issue".to_string()]);
    }

    #[test]
    fn plans_issue_alone_cover_comment_and_label_leaves() {
        let (corpus, issue_idx, comment_idx) = leaf_and_parent_corpus();
        let label_idx = 2;
        let selected = [issue_idx, comment_idx, label_idx];
        let plans = construct_workflow_seed_plans(&corpus, &selected).expect("plans");
        assert!(
            plans.iter().any(|p| p.candidate_ids == ["github:Issue"]),
            "expected Issue-only minimal plan, got {:?}",
            plans
                .iter()
                .map(|p| p.candidate_ids.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            !plans
                .iter()
                .any(|p| p.candidate_ids == ["github:IssueComment"]),
            "ParentPreferred must not seat leaf Comment when Issue parent covers Create: {:?}",
            plans
                .iter()
                .map(|p| p.candidate_ids.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn plan_action_leaf_not_covered_by_parent_channel() {
        use std::collections::HashMap;
        let pin = RequirementWitness {
            symbol: "w1".into(),
            kind: WitnessKind::DirectCapability {
                entry_id: "slack".into(),
                entity: "Pin".into(),
                capability_id: "slack:Pin:pin_add".into(),
                capability_name: "pin_add".into(),
                kind: "Action".into(),
                description: "Pin a message".into(),
            },
            owner_candidate_id: "slack:Pin".into(),
            lexical_score: 90,
            summary: "Action Pin".into(),
            entity_description: "Pin".into(),
            aliases: "pin|pin message|pin_add".into(),
            pool: PoolLinks {
                parents: ["Channel".into()].into_iter().collect(),
                children: BTreeSet::new(),
                siblings: BTreeSet::new(),
            },
            seed_class: SeedClassStamp::Authored(DiscoverySeedClass::Dependent),
            seed_nav: SeedNavStamp::Authored(DiscoverySeedNav::Attach),
            own_pairs: Default::default(),
        };
        let channel = RequirementWitness {
            symbol: "w2".into(),
            kind: WitnessKind::DirectCapability {
                entry_id: "slack".into(),
                entity: "Channel".into(),
                capability_id: "slack:Channel:Query".into(),
                capability_name: "channel_query".into(),
                kind: "Query".into(),
                description: "Query channel".into(),
            },
            owner_candidate_id: "slack:Channel".into(),
            lexical_score: 50,
            summary: "Query Channel".into(),
            entity_description: "Channel".into(),
            aliases: "channel".into(),
            pool: PoolLinks {
                parents: BTreeSet::new(),
                children: [
                    crate::discovery_seed_witness::roles::PoolChild::new("pins", "Pin"),
                    crate::discovery_seed_witness::roles::PoolChild::new("messages", "Message"),
                ]
                .into_iter()
                .collect(),
                siblings: BTreeSet::new(),
            },
            seed_class: SeedClassStamp::Authored(DiscoverySeedClass::Primary),
            seed_nav: SeedNavStamp::Unset,
            own_pairs: Default::default(),
        };
        let bundles = vec![
            EntityCandidateBundle {
                candidate_id: "slack:Pin".into(),
                entry_id: "slack".into(),
                entity: "Pin".into(),
                entity_description: "Pin".into(),
                max_lexical_score: 90,
                capabilities: vec![sketch("slack:Pin:pin_add", "pin_add", "Action", 90)],
                relation_hints: String::new(),
                catalog_route_evidence: true,
            },
            EntityCandidateBundle {
                candidate_id: "slack:Channel".into(),
                entry_id: "slack".into(),
                entity: "Channel".into(),
                entity_description: "Channel".into(),
                max_lexical_score: 50,
                capabilities: vec![sketch("slack:Channel:Query", "channel_query", "Query", 50)],
                relation_hints: String::new(),
                catalog_route_evidence: true,
            },
        ];
        let witnesses = vec![pin, channel];
        let mut symbol_to_index = HashMap::new();
        for (i, w) in witnesses.iter().enumerate() {
            symbol_to_index.insert(w.symbol.clone(), i);
        }
        let corpus = WitnessCorpus {
            roles: CorpusRoleIndex::build(&witnesses),
            witnesses,
            bundles,
            brand_lock_catalogs: vec![],
            symbol_to_index,
        };
        let plan_covers = candidates_covering_for_plan(&corpus, &corpus.witnesses[0], &[0]);
        assert_eq!(
            plan_covers,
            vec!["slack:Pin".to_string()],
            "Action Pin must stay leaf-seated: {plan_covers:?}"
        );
        let admit_covers = candidates_covering_with_satellites(&corpus, &corpus.witnesses[0]);
        assert!(
            admit_covers.iter().any(|id| id == "slack:Channel"),
            "admit cover may still credit Channel parent: {admit_covers:?}"
        );
        let plans = construct_workflow_seed_plans(&corpus, &[0]).expect("plans");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].candidate_ids, vec!["slack:Pin".to_string()]);
    }

    #[test]
    fn plan_create_alone_keeps_leaf_owner() {
        let (corpus, _issue_idx, comment_idx) = leaf_and_parent_corpus();
        let covers =
            candidates_covering_for_plan(&corpus, &corpus.witnesses[comment_idx], &[comment_idx]);
        assert_eq!(
            covers,
            vec!["github:IssueComment".to_string()],
            "localized Create without parent selected seats leaf: {covers:?}"
        );
    }

    #[test]
    fn plan_create_with_parent_selected_prefers_parent() {
        let (corpus, issue_idx, comment_idx) = leaf_and_parent_corpus();
        let covers = candidates_covering_for_plan(
            &corpus,
            &corpus.witnesses[comment_idx],
            &[issue_idx, comment_idx],
        );
        assert_eq!(
            covers,
            vec!["github:Issue".to_string()],
            "Create leaf with parent Direct selected seats parent: {covers:?}"
        );
    }

    #[test]
    fn strict_construct_requires_leaf_owner() {
        let (corpus, issue_idx, comment_idx) = leaf_and_parent_corpus();
        let selected = [issue_idx, comment_idx];
        let strict = construct_minimal_plans_with_cover(&corpus, &selected, CoverMode::Strict)
            .expect("strict");
        assert!(
            !strict.iter().any(|p| p.candidate_ids == ["github:Issue"]),
            "Strict must not admit Issue-alone when Comment Create remains: {:?}",
            strict
                .iter()
                .map(|p| p.candidate_ids.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn admit_satellites_for_comment_and_label_when_issue_seeded() {
        let (corpus, issue_idx, comment_idx) = leaf_and_parent_corpus();
        let label_idx = 2;
        let plan = DeterministicSeedPlan {
            symbol: "p1".into(),
            candidate_ids: vec!["github:Issue".into()],
            entities: vec![("github".into(), "Issue".into())],
            lexical_score: 90,
            covered_witness_symbols: vec!["w1".into(), "w2".into(), "w3".into()],
            summary: "github.Issue".into(),
        };
        let admitted =
            admit_teaching_satellites(&corpus, &plan, &[issue_idx, comment_idx, label_idx], None);
        match admitted {
            SatelliteAdmission::Ok(sats) => {
                assert!(sats
                    .iter()
                    .any(|(e, n)| e == "github" && n == "IssueComment"));
                assert!(sats.iter().any(|(e, n)| e == "github" && n == "Label"));
                assert!(!sats.iter().any(|(_, n)| n == "Issue"));
            }
            other => panic!("expected Ok satellites, got {other:?}"),
        }
    }

    #[test]
    fn admit_satellites_from_intent_when_comment_not_in_selection() {
        let (corpus, issue_idx, _comment_idx) = leaf_and_parent_corpus();
        let plan = DeterministicSeedPlan {
            symbol: "p1".into(),
            candidate_ids: vec!["github:Issue".into()],
            entities: vec![("github".into(), "Issue".into())],
            lexical_score: 90,
            covered_witness_symbols: vec!["w1".into()],
            summary: "github.Issue".into(),
        };
        let intent = "create an issue and add a comment to the issue";
        let admitted = admit_teaching_satellites(&corpus, &plan, &[issue_idx], Some(intent));
        match admitted {
            SatelliteAdmission::Ok(sats) => {
                assert!(
                    sats.iter()
                        .any(|(e, n)| e == "github" && n == "IssueComment"),
                    "expected IssueComment satellite from intent, got {sats:?}"
                );
            }
            other => panic!("expected Ok satellites, got {other:?}"),
        }
    }
}
