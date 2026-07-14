//! Invariant-only post-resolution pass for seed candidate ids.

use std::collections::{HashMap, HashSet};

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_candidate_graph::TypedCandidateGraph;
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_intent_signals::is_auxiliary_entity_for_mutation;
use crate::discovery_seed_catalog::CatalogWorkflowContext;

/// Dedupe and apply structural seed invariants (no semantic re-ranking).
///
/// When `protected_root_entities` is set, seeds whose entity matches a protected
/// root hint (e.g. coverage `ReadRoot` / `MutateAnchor`) are not rewritten away
/// by parent promotion or scope/mutation swaps.
pub fn apply_seed_invariants(
    selected_ids: Vec<String>,
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
    catalog_context: Option<&CatalogWorkflowContext>,
    candidate_graph: Option<&TypedCandidateGraph>,
) -> Vec<String> {
    apply_seed_invariants_protected(
        selected_ids,
        bundles,
        intent_class,
        catalog_context,
        candidate_graph,
        &[],
    )
}

/// Like [`apply_seed_invariants`], with coverage root-hint protection.
pub fn apply_seed_invariants_protected(
    selected_ids: Vec<String>,
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
    catalog_context: Option<&CatalogWorkflowContext>,
    candidate_graph: Option<&TypedCandidateGraph>,
    protected_root_entities: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = selected_ids
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    out.sort_unstable();
    promote_relation_leaves_to_parent(
        &mut out,
        bundles,
        intent_class,
        catalog_context,
        candidate_graph,
        protected_root_entities,
    );
    swap_mutation_relation_leaf_to_workflow_parent(
        &mut out,
        bundles,
        intent_class,
        catalog_context,
        protected_root_entities,
    );
    swap_scope_container_to_mutation_anchor(
        &mut out,
        bundles,
        intent_class,
        catalog_context,
        protected_root_entities,
    );
    swap_catalog_artifacts_to_concrete_entity(
        &mut out,
        bundles,
        intent_class,
        protected_root_entities,
    );
    swap_auxiliary_when_primary_present(&mut out, bundles, protected_root_entities);
    dedupe_federated_catalogs(&mut out, bundles, protected_root_entities);
    out.sort_unstable();
    out.dedup();
    out
}

fn entity_is_protected(entity: &str, protected_root_entities: &[String]) -> bool {
    if protected_root_entities.is_empty() {
        return false;
    }
    let entity_l = entity.to_ascii_lowercase();
    protected_root_entities
        .iter()
        .any(|hint| hint.eq_ignore_ascii_case(&entity_l) || hint.eq_ignore_ascii_case(entity))
}

/// When multiple seeds share a catalog, keep the mutation anchor or highest-scoring root.
fn dedupe_federated_catalogs(
    selected_ids: &mut Vec<String>,
    bundles: &[EntityCandidateBundle],
    protected_root_entities: &[String],
) {
    let index: HashMap<_, _> = bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    let mut by_catalog: HashMap<String, Vec<String>> = HashMap::new();
    for id in selected_ids.iter() {
        let Some(bundle) = index.get(id.as_str()) else {
            continue;
        };
        by_catalog
            .entry(bundle.entry_id.clone())
            .or_default()
            .push(id.clone());
    }
    if by_catalog.values().all(|ids| ids.len() <= 1) {
        return;
    }
    let mut kept = Vec::new();
    for ids in by_catalog.values() {
        if ids.len() == 1 {
            kept.push(ids[0].clone());
            continue;
        }
        if let Some(protected) = ids.iter().find(|id| {
            index
                .get(id.as_str())
                .is_some_and(|b| entity_is_protected(&b.entity, protected_root_entities))
        }) {
            kept.push((*protected).clone());
            continue;
        }
        let best = ids
            .iter()
            .max_by_key(|id| {
                let bundle = index.get(id.as_str()).unwrap();
                (
                    relation_parent_score(bundle, bundles),
                    is_mutation_bundle(bundle),
                    bundle.max_lexical_score,
                    !is_auxiliary_entity_for_mutation(&bundle.entity),
                )
            })
            .expect("non-empty ids");
        kept.push((*best).clone());
    }
    kept.sort_unstable();
    kept.dedup();
    *selected_ids = kept;
}

fn relation_parent_score(
    bundle: &EntityCandidateBundle,
    bundles: &[EntityCandidateBundle],
) -> usize {
    let is_leaf = is_relation_leaf(&bundle.entry_id, &bundle.entity, bundles);
    if is_leaf {
        0
    } else if bundle.relation_hints.is_empty() {
        1
    } else {
        2
    }
}

pub(crate) fn relation_hint_targets_entity(relation_hints: &str, entity: &str) -> bool {
    relation_hints
        .split(';')
        .any(|hint| hint.trim().ends_with(&format!("→{}", entity)))
}

fn is_relation_leaf(entry_id: &str, entity: &str, bundles: &[EntityCandidateBundle]) -> bool {
    bundles.iter().any(|bundle| {
        bundle.entry_id == entry_id && relation_hint_targets_entity(&bundle.relation_hints, entity)
    })
}

fn is_mutation_bundle(bundle: &EntityCandidateBundle) -> bool {
    bundle.capabilities.iter().any(|cap| {
        matches!(
            cap.kind.as_str(),
            "Create" | "Action" | "Update" | "Delete" | "Transition"
        )
    })
}

fn is_localized_mutation_anchor(
    intent_class: &DiscoveryIntentClass,
    entry_id: &str,
    entity: &str,
    catalog_context: Option<&CatalogWorkflowContext>,
) -> bool {
    if !matches!(intent_class, DiscoveryIntentClass::LocalizedMutation) {
        return false;
    }
    if let Some(ctx) = catalog_context {
        if ctx.is_localized_mutation(entry_id)
            && ctx.mutation_anchor_entity(entry_id).as_deref() == Some(entity)
        {
            return true;
        }
    }
    false
}

fn promote_relation_leaves_to_parent(
    selected_ids: &mut [String],
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
    catalog_context: Option<&CatalogWorkflowContext>,
    candidate_graph: Option<&TypedCandidateGraph>,
    protected_root_entities: &[String],
) {
    if !matches!(intent_class, DiscoveryIntentClass::ReadListNav) {
        return;
    }
    let index: HashMap<_, _> = bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    for id in selected_ids.iter_mut() {
        let Some(bundle) = index.get(id.as_str()) else {
            continue;
        };
        if entity_is_protected(&bundle.entity, protected_root_entities) {
            continue;
        }
        if let Some(graph) = candidate_graph {
            if let Some(parent_entity) = graph.parent_for_leaf(&bundle.entry_id, &bundle.entity) {
                if let Some(parent) = bundles.iter().find(|other| {
                    other.entry_id == bundle.entry_id && other.entity == parent_entity
                }) {
                    *id = parent.candidate_id.clone();
                    continue;
                }
            }
        }
        if !is_relation_leaf(&bundle.entry_id, &bundle.entity, bundles) {
            continue;
        }
        if is_localized_mutation_anchor(
            intent_class,
            &bundle.entry_id,
            &bundle.entity,
            catalog_context,
        ) {
            continue;
        }
        if let Some(parent) = bundles.iter().find(|other| {
            other.entry_id == bundle.entry_id
                && relation_hint_targets_entity(&other.relation_hints, &bundle.entity)
        }) {
            *id = parent.candidate_id.clone();
        }
    }
}

fn is_catalog_artifact_entity(entity: &str) -> bool {
    is_auxiliary_entity_for_mutation(entity) || entity.ends_with("NavigationLink")
}

fn swap_catalog_artifacts_to_concrete_entity(
    selected_ids: &mut [String],
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
    protected_root_entities: &[String],
) {
    if !matches!(intent_class, DiscoveryIntentClass::ReadListNav) {
        return;
    }
    let index: HashMap<_, _> = bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    for id in selected_ids.iter_mut() {
        let Some(bundle) = index.get(id.as_str()) else {
            continue;
        };
        if entity_is_protected(&bundle.entity, protected_root_entities) {
            continue;
        }
        if !is_catalog_artifact_entity(&bundle.entity) {
            continue;
        }
        if let Some(replacement) = bundles
            .iter()
            .filter(|candidate| {
                candidate.entry_id == bundle.entry_id
                    && !is_catalog_artifact_entity(&candidate.entity)
                    && !candidate.capabilities.is_empty()
            })
            .max_by_key(|candidate| candidate.max_lexical_score)
        {
            *id = replacement.candidate_id.clone();
        }
    }
}

/// Create/open on a workflow often misfires on relation leaves — swap to mutating parent.
fn swap_mutation_relation_leaf_to_workflow_parent(
    selected_ids: &mut [String],
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
    catalog_context: Option<&CatalogWorkflowContext>,
    protected_root_entities: &[String],
) {
    if !intent_class.is_mutation_family() {
        return;
    }
    let index: HashMap<_, _> = bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    for id in selected_ids.iter_mut() {
        let Some(bundle) = index.get(id.as_str()) else {
            continue;
        };
        if entity_is_protected(&bundle.entity, protected_root_entities) {
            continue;
        }
        if !is_relation_leaf(&bundle.entry_id, &bundle.entity, bundles) {
            continue;
        }
        if is_localized_mutation_anchor(
            intent_class,
            &bundle.entry_id,
            &bundle.entity,
            catalog_context,
        ) {
            continue;
        }
        if let Some(parent) = bundles.iter().find(|other| {
            other.entry_id == bundle.entry_id
                && relation_hint_targets_entity(&other.relation_hints, &bundle.entity)
                && is_mutation_bundle(other)
        }) {
            *id = parent.candidate_id.clone();
        }
    }
}

/// Scope containers (Channel, Issue, …) are parameters — swap to mutation anchor when intent mutates a related leaf.
fn swap_scope_container_to_mutation_anchor(
    selected_ids: &mut [String],
    bundles: &[EntityCandidateBundle],
    intent_class: &DiscoveryIntentClass,
    catalog_context: Option<&CatalogWorkflowContext>,
    protected_root_entities: &[String],
) {
    if !matches!(intent_class, DiscoveryIntentClass::LocalizedMutation) {
        return;
    }
    let index: HashMap<_, _> = bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    for id in selected_ids.iter_mut() {
        let Some(container) = index.get(id.as_str()) else {
            continue;
        };
        if entity_is_protected(&container.entity, protected_root_entities) {
            continue;
        }
        let Some(anchor) = bundles.iter().find(|candidate| {
            candidate.entry_id == container.entry_id
                && relation_hint_targets_entity(&container.relation_hints, &candidate.entity)
                && is_localized_mutation_anchor(
                    intent_class,
                    &candidate.entry_id,
                    &candidate.entity,
                    catalog_context,
                )
        }) else {
            continue;
        };
        *id = anchor.candidate_id.clone();
    }
}

fn swap_auxiliary_when_primary_present(
    selected_ids: &mut [String],
    bundles: &[EntityCandidateBundle],
    protected_root_entities: &[String],
) {
    let index: HashMap<_, _> = bundles
        .iter()
        .map(|bundle| (bundle.candidate_id.as_str(), bundle))
        .collect();
    for id in selected_ids.iter_mut() {
        let Some(bundle) = index.get(id.as_str()) else {
            continue;
        };
        if entity_is_protected(&bundle.entity, protected_root_entities) {
            continue;
        }
        if !is_auxiliary_entity_for_mutation(&bundle.entity) {
            continue;
        }
        if let Some(replacement) = bundles.iter().find(|other| {
            other.entry_id == bundle.entry_id
                && !is_auxiliary_entity_for_mutation(&other.entity)
                && !other.capabilities.is_empty()
                && relation_hint_targets_entity(&other.relation_hints, &bundle.entity)
        }) {
            *id = replacement.candidate_id.clone();
        }
    }
}

pub fn supporting_capabilities_from_bundles(
    candidate_ids: &[String],
    bundles: &[EntityCandidateBundle],
) -> Vec<String> {
    let selected: HashSet<&str> = candidate_ids.iter().map(String::as_str).collect();
    let mut capabilities = Vec::new();
    for bundle in bundles {
        if !selected.contains(bundle.candidate_id.as_str()) {
            continue;
        }
        for capability in &bundle.capabilities {
            capabilities.push(capability.capability_id.clone());
        }
    }
    capabilities.sort_unstable();
    capabilities.dedup();
    capabilities
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_auto_seed::EntityCapabilityEvidence;
    use crate::discovery_seed_catalog::CatalogWorkflowContext;
    use crate::loader::load_schema_dir;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn bundle(
        id: &str,
        eid: &str,
        ent: &str,
        kind: &str,
        relations: &str,
    ) -> EntityCandidateBundle {
        EntityCandidateBundle {
            candidate_id: id.into(),
            entry_id: eid.into(),
            entity: ent.into(),
            entity_description: String::new(),
            max_lexical_score: 1,
            capabilities: vec![EntityCapabilityEvidence {
                capability_id: format!("{eid}:{ent}:cap"),
                capability_name: "cap".into(),
                kind: kind.into(),
                description: String::new(),
                reason_codes: vec![],
                lexical_score: 1,
            }],
            relation_hints: relations.into(),
            catalog_route_evidence: false,
        }
    }

    fn workflow_fixture_cgs() -> Option<crate::schema::CGS> {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seed_workflow_matrix");
        if !dir.join("domain.yaml").is_file() {
            return None;
        }
        load_schema_dir(&dir).ok()
    }

    fn localized_mutation_context(intent: &str) -> Option<CatalogWorkflowContext> {
        let cgs = workflow_fixture_cgs()?;
        let mut catalogs: HashMap<String, &crate::schema::CGS> = HashMap::new();
        catalogs.insert("seed_workflow".to_string(), &cgs);
        Some(CatalogWorkflowContext::build(
            &catalogs,
            intent,
            &DiscoveryIntentClass::LocalizedMutation,
            &["seed_workflow".into()],
        ))
    }

    #[test]
    fn read_only_leaf_promotes_to_relation_parent() {
        let bundles = vec![
            bundle(
                "catalog_a:ParentNav",
                "catalog_a",
                "ParentNav",
                "Query",
                "tags→TagLeaf; notes→NoteLeaf",
            ),
            bundle("catalog_a:TagLeaf", "catalog_a", "TagLeaf", "Query", ""),
        ];
        let out = apply_seed_invariants(
            vec!["catalog_a:TagLeaf".into()],
            &bundles,
            &DiscoveryIntentClass::ReadListNav,
            None,
            None,
        );
        assert_eq!(out, vec!["catalog_a:ParentNav"]);
    }

    #[test]
    fn mutation_leaf_with_action_caps_promotes_on_read_list() {
        let bundles = vec![
            bundle(
                "catalog_a:WorkflowParent",
                "catalog_a",
                "WorkflowParent",
                "Query",
                "reviews→ReviewLeaf",
            ),
            bundle(
                "catalog_a:ReviewLeaf",
                "catalog_a",
                "ReviewLeaf",
                "Action",
                "",
            ),
        ];
        let out = apply_seed_invariants(
            vec!["catalog_a:ReviewLeaf".into()],
            &bundles,
            &DiscoveryIntentClass::ReadListNav,
            None,
            None,
        );
        assert_eq!(out, vec!["catalog_a:WorkflowParent"]);
    }

    #[test]
    fn localized_mutation_swaps_scope_container_to_anchor() {
        let Some(ctx) =
            localized_mutation_context("Post a summary comment on the incident ticket.")
        else {
            return;
        };
        let bundles = vec![
            bundle(
                "seed_workflow:Ticket",
                "seed_workflow",
                "Ticket",
                "Query",
                "notes→TicketNote",
            ),
            bundle(
                "seed_workflow:TicketNote",
                "seed_workflow",
                "TicketNote",
                "Create",
                "",
            ),
        ];
        let out = apply_seed_invariants(
            vec!["seed_workflow:Ticket".into()],
            &bundles,
            &DiscoveryIntentClass::LocalizedMutation,
            Some(&ctx),
            None,
        );
        assert_eq!(out, vec!["seed_workflow:TicketNote"]);
    }

    #[test]
    fn localized_mutation_anchor_not_promoted_to_scope_container() {
        let Some(ctx) =
            localized_mutation_context("Post a summary comment on the incident ticket.")
        else {
            return;
        };
        let bundles = vec![
            bundle(
                "seed_workflow:Ticket",
                "seed_workflow",
                "Ticket",
                "Query",
                "notes→TicketNote",
            ),
            bundle(
                "seed_workflow:TicketNote",
                "seed_workflow",
                "TicketNote",
                "Create",
                "",
            ),
        ];
        let out = apply_seed_invariants(
            vec!["seed_workflow:TicketNote".into()],
            &bundles,
            &DiscoveryIntentClass::LocalizedMutation,
            Some(&ctx),
            None,
        );
        assert_eq!(out, vec!["seed_workflow:TicketNote"]);
    }

    #[test]
    fn create_on_review_leaf_swaps_to_workflow_parent() {
        let bundles = vec![
            bundle(
                "catalog_a:WorkflowParent",
                "catalog_a",
                "WorkflowParent",
                "Create",
                "reviews→ReviewLeaf",
            ),
            bundle(
                "catalog_a:ReviewLeaf",
                "catalog_a",
                "ReviewLeaf",
                "Action",
                "",
            ),
        ];
        let out = apply_seed_invariants(
            vec!["catalog_a:ReviewLeaf".into()],
            &bundles,
            &DiscoveryIntentClass::WorkflowMutation,
            None,
            None,
        );
        assert_eq!(out, vec!["catalog_a:WorkflowParent"]);
    }

    #[test]
    fn context_artifact_swaps_to_concrete_parent_on_read_list() {
        let bundles = vec![
            bundle(
                "catalog_a:ParentNav",
                "catalog_a",
                "ParentNav",
                "Query",
                "tags→TagLeaf",
            ),
            bundle(
                "catalog_a:TriageContext",
                "catalog_a",
                "TriageContext",
                "Query",
                "",
            ),
        ];
        let out = apply_seed_invariants(
            vec!["catalog_a:TriageContext".into()],
            &bundles,
            &DiscoveryIntentClass::ReadListNav,
            None,
            None,
        );
        assert_eq!(out, vec!["catalog_a:ParentNav"]);
    }

    #[test]
    fn protected_root_message_not_promoted_to_thread() {
        let bundles = vec![
            bundle(
                "gmail:Thread",
                "gmail",
                "Thread",
                "Query",
                "messages→Message",
            ),
            bundle("gmail:Message", "gmail", "Message", "Query", ""),
        ];
        let out = apply_seed_invariants_protected(
            vec!["gmail:Message".into()],
            &bundles,
            &DiscoveryIntentClass::ReadListNav,
            None,
            None,
            &["Message".into()],
        );
        assert_eq!(out, vec!["gmail:Message"]);
    }

    #[test]
    fn protected_selected_channel_not_promoted_to_guild() {
        let bundles = vec![
            bundle(
                "discord:Guild",
                "discord",
                "Guild",
                "Query",
                "channels→Channel",
            ),
            bundle("discord:Channel", "discord", "Channel", "Query", ""),
        ];
        let out = apply_seed_invariants_protected(
            vec!["discord:Channel".into()],
            &bundles,
            &DiscoveryIntentClass::ReadListNav,
            None,
            None,
            &["Channel".into()],
        );
        assert_eq!(out, vec!["discord:Channel"]);
    }
}
