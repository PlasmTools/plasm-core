//! Deterministic seed-retrieval prep (replaces separate lexicon-mapping LLM stage).

use indexmap::IndexMap;

use crate::catalog_search_index::CatalogSearchIndex;
use crate::discovery::{CgsCatalog, DiscoveryError};
use crate::discovery_controlled_lexicon::explicit_named_catalogs_from_intent;
use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_intent_signals::intent_mentions_repo_path;
use crate::discovery_seed_catalog::{
    build_catalog_seed_index, is_localized_mutation, match_intent_to_catalog,
    suggests_mutation_workflow, suggests_repo_scoped_workflow,
};
use crate::schema::CGS;

/// Inputs for entity-candidate retrieval derived without an LLM mapping call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRetrievalPrep {
    pub named_catalogs: Vec<String>,
    pub intent_class: DiscoveryIntentClass,
}

/// Load CGS graphs for allowed catalogs.
pub fn load_catalogs_for_entries<C: CgsCatalog>(
    catalog: &C,
    entry_ids: &[String],
) -> IndexMap<String, CGS> {
    let mut out = IndexMap::new();
    for entry_id in entry_ids {
        if out.contains_key(entry_id) {
            continue;
        }
        if let Ok(ctx) = catalog.load_context(entry_id) {
            out.insert(entry_id.clone(), (*ctx.cgs).clone());
        }
    }
    out
}

/// Resolve allowed entry ids (tenant scope or full registry).
pub fn resolve_allowed_entry_ids<C: CgsCatalog>(
    catalog: &C,
    allowed_catalogs: &[String],
) -> Vec<String> {
    if allowed_catalogs.is_empty() {
        catalog
            .list_entries()
            .into_iter()
            .map(|meta| meta.entry_id)
            .collect()
    } else {
        allowed_catalogs.to_vec()
    }
}

/// Deterministic brand lock + retrieval intent class (no LLM).
pub fn prepare_seed_retrieval<C: CgsCatalog>(
    catalog: &C,
    intent: &str,
    allowed_catalogs: &[String],
) -> Result<SeedRetrievalPrep, DiscoveryError> {
    let entry_ids = resolve_allowed_entry_ids(catalog, allowed_catalogs);
    let catalogs = load_catalogs_for_entries(catalog, &entry_ids);
    Ok(prepare_seed_retrieval_from_catalogs(
        &catalogs, intent, &entry_ids,
    ))
}

/// Same prep when catalogs are already loaded (eval harness).
pub fn prepare_seed_retrieval_from_catalogs(
    catalogs: &IndexMap<String, CGS>,
    intent: &str,
    allowed_entry_ids: &[String],
) -> SeedRetrievalPrep {
    let named_catalogs =
        explicit_named_catalogs_from_intent(catalogs, intent, Some(allowed_entry_ids));
    let intent_class = infer_retrieval_intent_class(catalogs, intent, &named_catalogs);
    SeedRetrievalPrep {
        named_catalogs,
        intent_class,
    }
}

/// Infer the retrieval policy needed to build a useful entity pool.
///
/// Final routing remains the selector's responsibility; this class only controls
/// deterministic mutation/workflow candidate injection.
fn infer_retrieval_intent_class(
    catalogs: &IndexMap<String, CGS>,
    intent: &str,
    named_catalogs: &[String],
) -> DiscoveryIntentClass {
    let catalog_ids: Vec<&str> = if named_catalogs.is_empty() {
        catalogs.keys().map(String::as_str).collect()
    } else {
        named_catalogs.iter().map(String::as_str).collect()
    };

    if intent_mentions_repo_path(intent) {
        return DiscoveryIntentClass::RepoScopedWorkflow;
    }

    let search = CatalogSearchIndex::build_from_index_map(catalogs);
    for entry_id in catalog_ids {
        let Some(cgs) = catalogs.get(entry_id) else {
            continue;
        };
        let index = build_catalog_seed_index(entry_id, cgs);

        let localized = DiscoveryIntentClass::LocalizedMutation;
        let localized_match = match_intent_to_catalog(intent, &index, &search, &localized);
        if is_localized_mutation(&localized, &localized_match, &index) {
            return localized;
        }

        let repo = DiscoveryIntentClass::RepoScopedWorkflow;
        let repo_match = match_intent_to_catalog(intent, &index, &search, &repo);
        if suggests_repo_scoped_workflow(&repo, intent, &repo_match, &index) {
            return repo;
        }

        let workflow = DiscoveryIntentClass::WorkflowMutation;
        let workflow_match = match_intent_to_catalog(intent, &index, &search, &workflow);
        if suggests_mutation_workflow(&workflow, &workflow_match, &index) {
            return workflow;
        }
    }

    DiscoveryIntentClass::ReadListNav
}
