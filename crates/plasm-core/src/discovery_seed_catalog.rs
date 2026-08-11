//! Catalog-derived workflow scope and seed-root ranking for semantic auto-seed.

use std::collections::{HashMap, HashSet};

use crate::discovery_intent_class::DiscoveryIntentClass;
use crate::discovery_intent_signals::intent_mentions_repo_path;
use crate::schema::{CapabilityKind, CapabilitySchema, DiscoverySeedClass, DiscoverySeedNav, CGS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogCapabilityMeta {
    pub(crate) name: String,
    pub(crate) kind: CapabilityKind,
    pub(crate) operation_phrases: Vec<String>,
    pub(crate) target_phrases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSeedIndex {
    pub entry_id: String,
    pub(crate) entity_phrases: HashMap<String, Vec<String>>,
    pub(crate) mutation_caps: HashMap<String, Vec<CatalogCapabilityMeta>>,
    outgoing: HashMap<String, HashSet<String>>,
    incoming: HashMap<String, HashSet<String>>,
    pub(crate) compound_key_entities: HashSet<String>,
    key_var_counts: HashMap<String, usize>,
    /// Authored `entities.*.discovery.seed_class`.
    entity_seed_class: HashMap<String, DiscoverySeedClass>,
    /// Authored `relations.*.discovery.seed_nav` keyed by (from_entity, target_entity).
    relation_seed_nav: HashMap<(String, String), DiscoverySeedNav>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkflowMatch {
    pub entry_id: String,
    pub matched_entities: HashSet<String>,
    pub matched_mutation_entities: HashSet<String>,
    /// Entities with operation-term capability hits (not target-term-only).
    pub matched_operation_entities: HashSet<String>,
    pub matched_capabilities: Vec<(String, String)>,
    pub compound_key_hit: bool,
}

#[derive(Debug, Clone)]
pub struct CatalogWorkflowContext {
    indexes: HashMap<String, CatalogSeedIndex>,
    matches: HashMap<String, WorkflowMatch>,
    intent_class: DiscoveryIntentClass,
    named_catalogs: Vec<String>,
    intent: String,
    search: crate::catalog_search_index::CatalogSearchIndex,
}

impl Default for CatalogWorkflowContext {
    fn default() -> Self {
        Self {
            indexes: HashMap::new(),
            matches: HashMap::new(),
            intent_class: DiscoveryIntentClass::default(),
            named_catalogs: Vec::new(),
            intent: String::new(),
            search: crate::catalog_search_index::CatalogSearchIndex::empty(),
        }
    }
}

impl CatalogWorkflowContext {
    pub fn build(
        catalogs: &HashMap<String, &CGS>,
        intent: &str,
        intent_class: &DiscoveryIntentClass,
        named_catalogs: &[String],
    ) -> Self {
        let search = crate::catalog_search_index::CatalogSearchIndex::build_from_cgs_map(catalogs);
        Self::build_with_search(catalogs, intent, intent_class, named_catalogs, &search)
    }

    pub fn build_with_search(
        catalogs: &HashMap<String, &CGS>,
        intent: &str,
        intent_class: &DiscoveryIntentClass,
        named_catalogs: &[String],
        search: &crate::catalog_search_index::CatalogSearchIndex,
    ) -> Self {
        let mut indexes = HashMap::new();
        let mut matches = HashMap::new();
        for (entry_id, cgs) in catalogs {
            let index = build_catalog_seed_index(entry_id, cgs);
            let workflow = match_intent_to_catalog(intent, &index, search, intent_class);
            indexes.insert(entry_id.clone(), index);
            matches.insert(entry_id.clone(), workflow);
        }
        Self {
            indexes,
            matches,
            intent_class: intent_class.clone(),
            named_catalogs: named_catalogs.to_vec(),
            intent: intent.to_string(),
            search: search.clone(),
        }
    }

    pub fn search_index(&self) -> &crate::catalog_search_index::CatalogSearchIndex {
        &self.search
    }

    pub fn index(&self, entry_id: &str) -> Option<&CatalogSeedIndex> {
        self.indexes.get(entry_id)
    }

    pub fn workflow_match(&self, entry_id: &str) -> Option<&WorkflowMatch> {
        self.matches.get(entry_id)
    }

    pub fn suggests_multi_entity_workflow(&self, entry_id: &str) -> bool {
        self.indexes
            .get(entry_id)
            .zip(self.matches.get(entry_id))
            .is_some_and(|(index, workflow)| {
                suggests_multi_entity_workflow(&self.intent_class, &self.intent, workflow, index)
            })
    }

    pub fn suggests_mutation_workflow(&self, entry_id: &str) -> bool {
        self.indexes
            .get(entry_id)
            .zip(self.matches.get(entry_id))
            .is_some_and(|(index, workflow)| {
                suggests_mutation_workflow(&self.intent_class, workflow, index)
            })
    }

    pub fn suggests_repo_scoped_workflow(&self, entry_id: &str) -> bool {
        self.indexes
            .get(entry_id)
            .zip(self.matches.get(entry_id))
            .is_some_and(|(index, workflow)| {
                suggests_repo_scoped_workflow(&self.intent_class, &self.intent, workflow, index)
            })
    }

    pub fn is_localized_mutation(&self, entry_id: &str) -> bool {
        self.indexes
            .get(entry_id)
            .zip(self.matches.get(entry_id))
            .is_some_and(|(index, workflow)| {
                is_localized_mutation(&self.intent_class, workflow, index)
            })
    }

    pub fn mutation_anchor_entity(&self, entry_id: &str) -> Option<String> {
        let index = self.indexes.get(entry_id)?;
        let workflow = self.matches.get(entry_id)?;
        mutation_anchor_entity(index, workflow)
    }

    /// Authored entity `discovery.seed_class`, if any.
    pub fn entity_seed_class(&self, entry_id: &str, entity: &str) -> Option<DiscoverySeedClass> {
        self.indexes
            .get(entry_id)
            .and_then(|idx| idx.entity_seed_class.get(entity).copied())
    }

    /// Authored relation `discovery.seed_nav` for an in-catalog edge (from → target).
    pub fn relation_seed_nav(
        &self,
        entry_id: &str,
        from_entity: &str,
        target_entity: &str,
    ) -> Option<DiscoverySeedNav> {
        self.indexes.get(entry_id).and_then(|idx| {
            idx.relation_seed_nav
                .get(&(from_entity.to_string(), target_entity.to_string()))
                .copied()
        })
    }

    pub fn entity_phrase_match_score(&self, entry_id: &str, entity: &str, text: &str) -> i32 {
        self.search.entity_score(entry_id, entity, text) as i32
    }

    /// Authored `discovery.names` / `qualifier_names` token-cover hit for this intent.
    pub fn entity_authored_name_hit(&self, entry_id: &str, entity: &str) -> bool {
        self.indexes
            .get(entry_id)
            .is_some_and(|idx| entity_name_hit(idx, entity, &self.intent))
    }

    /// Stamp-shaped teaching-satellite leaf (dependent / attach / 1-hop), without
    /// requiring an authored name hit. Shared by pool inject and witness admit.
    pub fn entity_is_satellite_shape(
        &self,
        entry_id: &str,
        entity: &str,
        pool_parents: &HashSet<String>,
        cgs: Option<&CGS>,
    ) -> bool {
        match self.entity_seed_class(entry_id, entity) {
            Some(DiscoverySeedClass::Primary) | Some(DiscoverySeedClass::Ambient) => {
                return false;
            }
            Some(DiscoverySeedClass::Dependent) => return true,
            None => {}
        }
        for parent in pool_parents {
            if self
                .relation_seed_nav(entry_id, parent, entity)
                .is_some_and(|nav| matches!(nav, DiscoverySeedNav::Attach))
            {
                return true;
            }
        }
        let Some(cgs) = cgs else {
            return false;
        };
        for parent in pool_parents {
            if cgs.get_entity(parent.as_str()).is_some_and(|ent| {
                ent.relations
                    .values()
                    .any(|rel| rel.target_resource == entity)
            }) {
                return true;
            }
        }
        false
    }

    /// Authored phrase hit + satellite shape — pool protection / inject enrollment.
    pub fn entity_is_authored_satellite_leaf(
        &self,
        entry_id: &str,
        entity: &str,
        pool_parents: &HashSet<String>,
        cgs: Option<&CGS>,
    ) -> bool {
        self.entity_authored_name_hit(entry_id, entity)
            && self.entity_is_satellite_shape(entry_id, entity, pool_parents, cgs)
    }

    /// Workflow roots to protect when forcing leaves into a capped pool.
    pub fn stamp_protect_root_entities(&self, entry_id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        if let Some(root) = self.workflow_root_entity(entry_id) {
            seen.insert(root.clone());
            out.push(root);
        }
        if let Some(index) = self.indexes.get(entry_id) {
            for (entity, class) in &index.entity_seed_class {
                if matches!(
                    class,
                    DiscoverySeedClass::Primary | DiscoverySeedClass::Ambient
                ) && seen.insert(entity.clone())
                {
                    // Ambient only when already matched / present is decided by caller.
                    if matches!(class, DiscoverySeedClass::Primary)
                        || self
                            .workflow_match(entry_id)
                            .is_some_and(|m| m.matched_entities.contains(entity))
                    {
                        out.push(entity.clone());
                    }
                }
            }
        }
        out
    }

    pub fn workflow_root_entity(&self, entry_id: &str) -> Option<String> {
        let index = self.indexes.get(entry_id)?;
        let workflow = self.matches.get(entry_id)?;
        workflow_root_entity(index, workflow)
    }

    pub fn workflow_root_rank(&self, entry_id: &str, entity: &str) -> Option<i32> {
        let index = self.indexes.get(entry_id)?;
        let workflow = self.matches.get(entry_id)?;
        Some(workflow_root_rank(entity, index, workflow))
    }

    pub fn any_multi_entity_workflow(&self) -> bool {
        self.indexes
            .keys()
            .any(|entry_id| self.suggests_multi_entity_workflow(entry_id))
    }

    pub fn any_localized_mutation(&self) -> bool {
        self.indexes
            .keys()
            .any(|entry_id| self.is_localized_mutation(entry_id))
    }

    pub fn any_repo_scoped_workflow(&self) -> bool {
        self.indexes
            .keys()
            .any(|entry_id| self.suggests_repo_scoped_workflow(entry_id))
    }

    pub fn branded_entry_ids(&self) -> Vec<String> {
        self.named_catalogs.clone()
    }

    pub fn intent_class(&self) -> &DiscoveryIntentClass {
        &self.intent_class
    }
}

pub fn build_catalog_seed_index(entry_id: &str, cgs: &CGS) -> CatalogSeedIndex {
    let mut entity_phrases = HashMap::new();
    let mut mutation_caps = HashMap::new();
    let mut outgoing = HashMap::new();
    let mut incoming: HashMap<String, HashSet<String>> = HashMap::new();
    let mut compound_key_entities = HashSet::new();
    let mut key_var_counts = HashMap::new();
    let mut entity_seed_class = HashMap::new();
    let mut relation_seed_nav = HashMap::new();

    for (entity_name, entity) in &cgs.entities {
        let entity_key = entity_name.to_string();
        key_var_counts.insert(entity_key.clone(), entity.key_vars.len());
        entity_phrases.insert(
            entity_key.clone(),
            entity_phrase_list(entity_name.as_str(), entity),
        );
        if entity.key_vars.len() >= 2 {
            compound_key_entities.insert(entity_key.clone());
        }
        if let Some(class) = entity.discovery.as_ref().and_then(|d| d.seed_class) {
            entity_seed_class.insert(entity_key.clone(), class);
        }
        let mut targets = HashSet::new();
        for rel in entity.relations.values() {
            let target = rel.target_resource.to_string();
            targets.insert(target.clone());
            incoming
                .entry(target.clone())
                .or_default()
                .insert(entity_key.clone());
            if let Some(nav) = rel.discovery.as_ref().and_then(|d| d.seed_nav) {
                relation_seed_nav.insert((entity_key.clone(), target.clone()), nav);
            }
        }
        if !targets.is_empty() {
            outgoing.insert(entity_key.clone(), targets);
        }

        let mut caps = Vec::new();
        for kind in [
            CapabilityKind::Create,
            CapabilityKind::Action,
            CapabilityKind::Update,
            CapabilityKind::Delete,
        ] {
            for cap in cgs.find_capabilities(entity_name.as_str(), kind) {
                if !cap.is_remote_mutation() {
                    continue;
                }
                caps.push(CatalogCapabilityMeta {
                    name: cap.name.to_string(),
                    kind: cap.kind,
                    operation_phrases: capability_operation_phrases(cap),
                    target_phrases: capability_target_phrases(cap),
                });
            }
        }
        if !caps.is_empty() {
            mutation_caps.insert(entity_key, caps);
        }
    }

    CatalogSeedIndex {
        entry_id: entry_id.to_string(),
        entity_phrases,
        mutation_caps,
        outgoing,
        incoming,
        compound_key_entities,
        key_var_counts,
        entity_seed_class,
        relation_seed_nav,
    }
}

impl CatalogSeedIndex {
    pub fn outgoing_relation_count(&self, entity: &str) -> usize {
        self.outgoing
            .get(entity)
            .map(|targets| targets.len())
            .unwrap_or(0)
    }

    pub fn entity_phrase_match_score(
        &self,
        search: &crate::catalog_search_index::CatalogSearchIndex,
        entity: &str,
        text: &str,
    ) -> i32 {
        search.entity_score(&self.entry_id, entity, text) as i32
    }

    /// Best mutation kind by BM25 among caps whose authored operation terms are covered.
    pub fn best_mutation_kind_for_intent(
        &self,
        search: &crate::catalog_search_index::CatalogSearchIndex,
        intent: &str,
    ) -> Option<(CapabilityKind, i32)> {
        let mut best_score = 0i32;
        let mut best_kind = None;
        for caps in self.mutation_caps.values() {
            for cap in caps {
                if !operation_terms_hit(cap, intent) {
                    continue;
                }
                let score = search.capability_score(&self.entry_id, &cap.name, intent) as i32;
                if score > best_score {
                    best_score = score;
                    best_kind = Some(cap.kind);
                }
            }
        }
        best_kind.map(|kind| (kind, best_score))
    }
}

fn entity_phrase_list(_entity_name: &str, entity: &crate::schema::EntityDef) -> Vec<String> {
    let mut phrases = Vec::new();
    if let Some(hints) = &entity.discovery {
        phrases.extend(hints.names.iter().cloned());
        phrases.extend(hints.qualifier_names.iter().cloned());
    }
    // Authorship only — BM25 indexes these via capability_document_text; not a second matcher.
    dedupe_phrases(phrases)
}

fn capability_operation_phrases(cap: &CapabilitySchema) -> Vec<String> {
    let mut phrases = Vec::new();
    if let Some(hints) = &cap.discovery {
        phrases.extend(hints.operation_terms.iter().cloned());
    }
    dedupe_phrases(phrases)
}

fn capability_target_phrases(cap: &CapabilitySchema) -> Vec<String> {
    let mut phrases = Vec::new();
    if let Some(hints) = &cap.discovery {
        phrases.extend(hints.target_terms.iter().cloned());
    }
    dedupe_phrases(phrases)
}

/// BM25 index for a single catalog row (unit tests).
pub fn search_index_for_cgs(
    entry_id: &str,
    cgs: &CGS,
) -> crate::catalog_search_index::CatalogSearchIndex {
    crate::catalog_search_index::CatalogSearchIndex::build_from_pairs([(entry_id, cgs)])
}

fn dedupe_phrases(phrases: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    phrases
        .into_iter()
        .map(|phrase| phrase.trim().to_ascii_lowercase())
        .filter(|phrase| !phrase.is_empty() && seen.insert(phrase.clone()))
        .collect()
}

fn operation_terms_hit(meta: &CatalogCapabilityMeta, intent: &str) -> bool {
    meta.operation_phrases
        .iter()
        .any(|phrase| crate::catalog_search_index::phrase_tokens_covered_by_intent(phrase, intent))
}

fn target_terms_hit(meta: &CatalogCapabilityMeta, intent: &str) -> bool {
    meta.target_phrases
        .iter()
        .any(|phrase| crate::catalog_search_index::phrase_tokens_covered_by_intent(phrase, intent))
}

/// Admit mutation capabilities via BM25 score; label operation vs target with authored terms.
fn capability_bm25_match(
    meta: &CatalogCapabilityMeta,
    entry_id: &str,
    search: &crate::catalog_search_index::CatalogSearchIndex,
    intent_class: &DiscoveryIntentClass,
    intent: &str,
) -> (bool, bool) {
    let score = search.capability_score(entry_id, &meta.name, intent);
    if score == 0 {
        return (false, false);
    }
    let op_hit = operation_terms_hit(meta, intent);
    if op_hit {
        return (true, true);
    }
    if intent_class.is_mutation_family() && target_terms_hit(meta, intent) {
        return (true, false);
    }
    // BM25 alone is not enough — entity nouns in the cap document must not admit mutations.
    (false, false)
}

/// Entity admit uses authored `discovery.names` / `qualifier_names` only.
/// Description-only BM25 must not mint entities (that path owns `discover()` ranking).
fn entity_name_hit(index: &CatalogSeedIndex, entity: &str, intent: &str) -> bool {
    index.entity_phrases.get(entity).is_some_and(|phrases| {
        phrases.iter().any(|phrase| {
            crate::catalog_search_index::phrase_tokens_covered_by_intent(phrase, intent)
        })
    })
}

pub fn match_intent_to_catalog(
    intent: &str,
    index: &CatalogSeedIndex,
    search: &crate::catalog_search_index::CatalogSearchIndex,
    intent_class: &DiscoveryIntentClass,
) -> WorkflowMatch {
    let mut matched_entities = HashSet::new();
    let mut matched_mutation_entities = HashSet::new();
    let mut matched_operation_entities = HashSet::new();
    let mut matched_capabilities = Vec::new();

    for entity in index.entity_phrases.keys() {
        if entity_name_hit(index, entity, intent) {
            matched_entities.insert(entity.clone());
        }
    }

    for (entity, caps) in &index.mutation_caps {
        for cap in caps {
            let (matched, operation_hit) =
                capability_bm25_match(cap, &index.entry_id, search, intent_class, intent);
            if matched {
                matched_mutation_entities.insert(entity.clone());
                matched_entities.insert(entity.clone());
                matched_capabilities.push((entity.clone(), cap.name.clone()));
                if operation_hit {
                    matched_operation_entities.insert(entity.clone());
                }
            }
        }
    }

    for (entity, targets) in &index.outgoing {
        for rel_target in targets {
            if entity_name_hit(index, rel_target, intent) {
                matched_entities.insert(entity.clone());
                matched_entities.insert(rel_target.clone());
            }
        }
    }

    let compound_key_hit = index
        .compound_key_entities
        .iter()
        .any(|entity| matched_entities.contains(entity));

    WorkflowMatch {
        entry_id: index.entry_id.clone(),
        matched_entities,
        matched_mutation_entities,
        matched_operation_entities,
        matched_capabilities,
        compound_key_hit,
    }
}

pub fn suggests_multi_entity_workflow(
    intent_class: &DiscoveryIntentClass,
    intent: &str,
    workflow: &WorkflowMatch,
    index: &CatalogSeedIndex,
) -> bool {
    if matches!(
        intent_class,
        DiscoveryIntentClass::CatalogExploration
            | DiscoveryIntentClass::ReadListNav
            | DiscoveryIntentClass::ReadListLeafCollection
            | DiscoveryIntentClass::LocalizedMutation
            | DiscoveryIntentClass::HostCapabilityMiss { .. }
    ) {
        return false;
    }
    if workflow.matched_entities.is_empty() {
        return false;
    }
    if workflow.matched_mutation_entities.len() >= 2 {
        return true;
    }
    if workflow.compound_key_hit && intent_mentions_repo_path(intent) {
        return true;
    }
    matches!(
        intent_class,
        DiscoveryIntentClass::RepoScopedWorkflow | DiscoveryIntentClass::WorkflowMutation
    ) && workflow.matched_entities.len() >= 3
        && (!workflow.matched_mutation_entities.is_empty()
            || workflow
                .matched_entities
                .iter()
                .any(|entity| index.mutation_caps.contains_key(entity)))
}

pub fn suggests_mutation_workflow(
    intent_class: &DiscoveryIntentClass,
    workflow: &WorkflowMatch,
    index: &CatalogSeedIndex,
) -> bool {
    if matches!(
        intent_class,
        DiscoveryIntentClass::CatalogExploration
            | DiscoveryIntentClass::ReadListNav
            | DiscoveryIntentClass::ReadListLeafCollection
            | DiscoveryIntentClass::HostCapabilityMiss { .. }
    ) {
        return false;
    }
    if suggests_repo_scoped_workflow(intent_class, "", workflow, index) {
        return true;
    }
    if matches!(intent_class, DiscoveryIntentClass::LocalizedMutation) {
        return !workflow.matched_operation_entities.is_empty();
    }
    if !suggests_multi_entity_workflow(intent_class, "", workflow, index) {
        return false;
    }
    if is_single_leaf_mutation(workflow, index) {
        return false;
    }
    if matches!(
        intent_class,
        DiscoveryIntentClass::WorkflowMutation | DiscoveryIntentClass::RepoScopedWorkflow
    ) {
        return true;
    }
    workflow.matched_capabilities.len() >= 2
}

pub fn suggests_repo_scoped_workflow(
    intent_class: &DiscoveryIntentClass,
    intent: &str,
    workflow: &WorkflowMatch,
    _index: &CatalogSeedIndex,
) -> bool {
    if !matches!(intent_class, DiscoveryIntentClass::RepoScopedWorkflow) {
        return false;
    }
    if workflow.matched_operation_entities.len() >= 2 {
        return true;
    }
    // Mutation-family hits without contiguous operation phrases still scope to the repo root
    // when the class is already RepoScopedWorkflow (branded multi-step create/branch/PR intents).
    if workflow.matched_mutation_entities.len() >= 2 {
        return true;
    }
    if workflow.compound_key_hit
        && intent_mentions_repo_path(intent)
        && !workflow.matched_mutation_entities.is_empty()
    {
        return true;
    }
    workflow.matched_entities.len() >= 3 && workflow.matched_mutation_entities.len() >= 2
}

pub fn is_localized_mutation(
    intent_class: &DiscoveryIntentClass,
    workflow: &WorkflowMatch,
    index: &CatalogSeedIndex,
) -> bool {
    if !matches!(intent_class, DiscoveryIntentClass::LocalizedMutation) {
        return false;
    }
    if workflow.matched_operation_entities.is_empty() {
        return false;
    }
    // Multi-leaf / multi-operation workflows are not a single localized spine.
    if workflow.matched_mutation_entities.is_empty() {
        return false;
    }
    // Multi-leaf attach/dependent mutations are not a single localized spine.
    // Primary Create noise (Task Create beside Comment Create) must not veto.
    let leaf_mutations: HashSet<&str> = workflow
        .matched_mutation_entities
        .iter()
        .filter(|entity| {
            if matches!(
                index.entity_seed_class.get(entity.as_str()),
                Some(DiscoverySeedClass::Dependent)
            ) {
                return true;
            }
            index.relation_seed_nav.iter().any(|((from, to), nav)| {
                to == entity.as_str()
                    && *nav == DiscoverySeedNav::Attach
                    && workflow.matched_entities.contains(from)
            })
        })
        .map(String::as_str)
        .collect();
    let leaf_count = if leaf_mutations.is_empty() {
        workflow.matched_mutation_entities.len()
    } else {
        leaf_mutations.len()
    };
    if leaf_count >= 2 {
        return false;
    }
    // Primary + attach leaf both operation-matched (list PRs + reviewers) is not localized.
    if workflow.matched_operation_entities.len() >= 2 {
        return false;
    }
    if workflow.matched_capabilities.len() >= 3 && leaf_mutations.is_empty() {
        return false;
    }
    if workflow.matched_capabilities.len() >= 5 {
        return false;
    }
    if workflow.matched_operation_entities.len() >= 2 && workflow.matched_entities.len() >= 3 {
        return false;
    }
    !suggests_repo_scoped_workflow(intent_class, "", workflow, index)
}

pub fn mutation_anchor_entity(
    index: &CatalogSeedIndex,
    workflow: &WorkflowMatch,
) -> Option<String> {
    if workflow.matched_operation_entities.is_empty() {
        return None;
    }
    if workflow.matched_operation_entities.len() == 1 {
        return workflow.matched_operation_entities.iter().next().cloned();
    }
    workflow
        .matched_operation_entities
        .iter()
        .min_by_key(|entity| workflow_root_rank(entity, index, workflow))
        .cloned()
}

fn is_single_leaf_mutation(workflow: &WorkflowMatch, index: &CatalogSeedIndex) -> bool {
    if workflow.matched_mutation_entities.len() != 1 {
        return false;
    }
    let entity = workflow.matched_mutation_entities.iter().next().unwrap();
    if workflow.matched_capabilities.len() > 1 {
        return false;
    }
    let out = index
        .outgoing
        .get(entity)
        .map(|targets| targets.intersection(&workflow.matched_entities).count())
        .unwrap_or(0);
    let in_deg = index
        .incoming
        .get(entity)
        .map(|sources| sources.intersection(&workflow.matched_entities).count())
        .unwrap_or(0);
    out == 0 && in_deg == 0
}

pub fn workflow_root_rank(entity: &str, index: &CatalogSeedIndex, workflow: &WorkflowMatch) -> i32 {
    let out = index
        .outgoing
        .get(entity)
        .map(|targets| targets.intersection(&workflow.matched_entities).count())
        .unwrap_or(0) as i32;
    let mutation_count = index
        .mutation_caps
        .get(entity)
        .map(|caps| caps.len())
        .unwrap_or(0) as i32;
    let compound_bonus = if index.compound_key_entities.contains(entity) {
        -5
    } else {
        0
    };
    let scope_penalty = index
        .key_var_counts
        .get(entity)
        .map(|count| *count as i32 * 8)
        .unwrap_or(0);
    let in_deg = index
        .incoming
        .get(entity)
        .map(|sources| sources.intersection(&workflow.matched_entities).count())
        .unwrap_or(0) as i32;
    let leaf_penalty = if out == 0 && in_deg > 0 { 15 } else { 0 };
    100 - out * 10 - mutation_count * 5 + compound_bonus + leaf_penalty + scope_penalty
}

pub fn workflow_root_entity(index: &CatalogSeedIndex, workflow: &WorkflowMatch) -> Option<String> {
    let mut candidates: Vec<String> = workflow
        .matched_mutation_entities
        .iter()
        .chain(workflow.matched_entities.iter())
        .filter(|entity| index.mutation_caps.contains_key(entity.as_str()))
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if candidates.is_empty() {
        candidates = workflow.matched_entities.iter().cloned().collect();
    }
    let scoped: Vec<String> = candidates
        .iter()
        .filter(|entity| index.compound_key_entities.contains(entity.as_str()))
        .cloned()
        .collect();
    let narrowed = if scoped.is_empty() {
        candidates
    } else {
        let min_key_vars = scoped
            .iter()
            .filter_map(|entity| index.key_var_counts.get(entity))
            .min()
            .copied();
        scoped
            .into_iter()
            .filter(|entity| {
                min_key_vars.is_none_or(|min| index.key_var_counts.get(entity) == Some(&min))
            })
            .collect()
    };
    narrowed
        .into_iter()
        .min_by_key(|entity| workflow_root_rank(entity, index, workflow))
}

pub fn inject_entities_for_workflow(
    index: &CatalogSeedIndex,
    workflow: &WorkflowMatch,
) -> Vec<String> {
    let mut entities: HashSet<String> =
        workflow.matched_mutation_entities.iter().cloned().collect();
    if let Some(root) = workflow_root_entity(index, workflow) {
        entities.insert(root);
    }
    for entity in &workflow.matched_entities {
        if index.mutation_caps.contains_key(entity) {
            entities.insert(entity.clone());
        }
    }
    entities.into_iter().collect()
}

/// Intent-matched entities that expose mutation capabilities (single-step inject).
pub fn mutation_inject_entity_targets(
    index: &CatalogSeedIndex,
    workflow: &WorkflowMatch,
) -> Vec<String> {
    let mut entities: HashSet<String> =
        workflow.matched_mutation_entities.iter().cloned().collect();
    for entity in &workflow.matched_entities {
        if index.mutation_caps.contains_key(entity.as_str()) {
            entities.insert(entity.clone());
        }
    }
    entities.into_iter().collect()
}

/// Intent-matched entities with operation-term capability hits (localized inject).
pub fn operation_mutation_inject_targets(workflow: &WorkflowMatch) -> Vec<String> {
    workflow
        .matched_operation_entities
        .iter()
        .cloned()
        .collect()
}

pub fn inject_entity_targets(
    index: &CatalogSeedIndex,
    workflow: &WorkflowMatch,
    repo_scoped: bool,
) -> Vec<String> {
    if repo_scoped {
        inject_entities_for_workflow(index, workflow)
    } else {
        let mut entities: HashSet<String> = workflow
            .matched_operation_entities
            .iter()
            .cloned()
            .collect();
        if entities.is_empty() {
            entities.extend(mutation_inject_entity_targets(index, workflow));
        }
        entities.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::discovery_intent_class::DiscoveryIntentClass;
    use crate::loader::load_schema_dir;

    fn workflow_fixture_cgs() -> Option<CGS> {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/seed_workflow_matrix");
        if !dir.join("domain.yaml").is_file() {
            return None;
        }
        load_schema_dir(&dir).ok()
    }

    #[test]
    fn localized_comment_create_is_not_repo_scoped() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "Post a summary comment on the incident ticket.";
        let workflow = match_intent_to_catalog(
            intent,
            &index,
            &search,
            &DiscoveryIntentClass::LocalizedMutation,
        );
        assert!(is_localized_mutation(
            &DiscoveryIntentClass::LocalizedMutation,
            &workflow,
            &index
        ));
        assert!(!suggests_repo_scoped_workflow(
            &DiscoveryIntentClass::LocalizedMutation,
            intent,
            &workflow,
            &index
        ));
        assert_eq!(
            mutation_anchor_entity(&index, &workflow).as_deref(),
            Some("TicketNote")
        );
    }

    #[test]
    fn localized_merge_proposal_create_is_not_repo_scoped() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "Open a merge proposal from hotfix/cache into main and request reviewers.";
        let workflow = match_intent_to_catalog(
            intent,
            &index,
            &search,
            &DiscoveryIntentClass::LocalizedMutation,
        );
        assert!(is_localized_mutation(
            &DiscoveryIntentClass::LocalizedMutation,
            &workflow,
            &index
        ));
        assert!(!suggests_repo_scoped_workflow(
            &DiscoveryIntentClass::LocalizedMutation,
            intent,
            &workflow,
            &index
        ));
        assert_eq!(
            mutation_anchor_entity(&index, &workflow).as_deref(),
            Some("MergeProposal")
        );
    }

    #[test]
    fn catalog_exploration_suppresses_mutation_workflow() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "Browse capabilities for repository-scoped repo workflow: tickets, branches, merge proposals.";
        let workflow = match_intent_to_catalog(
            intent,
            &index,
            &search,
            &DiscoveryIntentClass::CatalogExploration,
        );
        assert!(!suggests_mutation_workflow(
            &DiscoveryIntentClass::CatalogExploration,
            &workflow,
            &index
        ));
        assert!(!suggests_repo_scoped_workflow(
            &DiscoveryIntentClass::CatalogExploration,
            intent,
            &workflow,
            &index
        ));
    }

    #[test]
    fn mail_message_phrase_scores_higher_than_thread() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "Show unread messages from the CFO this week.";
        assert!(
            index.entity_phrase_match_score(&search, "MailMessage", intent)
                > index.entity_phrase_match_score(&search, "MailThread", intent)
        );
    }

    #[test]
    fn multiword_operation_phrases_ignore_english_articles() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let hits = search.search("open a pull request from fix/oauth into main", 20);
        assert!(
            !hits.is_empty(),
            "BM25 DefaultTokenizer must match article paraphrase from catalog text alone"
        );
        let neg = search.search("list repository issues unrelated xyz", 5);
        // still may hit Repo entity; require the positive PR intent to rank a MergeProposal/PR-ish cap
        assert!(
            hits.iter().any(|h| {
                h.capability_name.to_ascii_lowercase().contains("merge")
                    || h.capability_name.to_ascii_lowercase().contains("pull")
                    || h.entity.to_ascii_lowercase().contains("merge")
            }),
            "expected merge/pr capability among hits: {hits:?}"
        );
        let _ = neg;
    }

    #[test]
    fn catalog_workflow_root_prefers_anchor_entity() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "In the Acme catalog monorepo: open a bug ticket, cut a feature branch, commit a readme update, open a merge proposal, and leave a ticket note with the proposal link.";
        let workflow = match_intent_to_catalog(
            intent,
            &index,
            &search,
            &DiscoveryIntentClass::RepoScopedWorkflow,
        );
        assert!(suggests_multi_entity_workflow(
            &DiscoveryIntentClass::RepoScopedWorkflow,
            intent,
            &workflow,
            &index
        ));
        assert!(suggests_mutation_workflow(
            &DiscoveryIntentClass::RepoScopedWorkflow,
            &workflow,
            &index
        ));
        assert!(suggests_repo_scoped_workflow(
            &DiscoveryIntentClass::RepoScopedWorkflow,
            intent,
            &workflow,
            &index
        ));
        assert_eq!(
            workflow_root_entity(&index, &workflow).as_deref(),
            Some("Workspace")
        );
        // Multi-operation evidence must not stop at LocalizedMutation.
        let localized_view = match_intent_to_catalog(
            intent,
            &index,
            &search,
            &DiscoveryIntentClass::LocalizedMutation,
        );
        assert!(
            !is_localized_mutation(
                &DiscoveryIntentClass::LocalizedMutation,
                &localized_view,
                &index
            ),
            "multi-mutation workflows must fall through to repo/workflow classification"
        );
    }

    #[test]
    fn read_only_ticket_list_is_not_mutation_workflow() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "List open tickets in the workspace and show tags on each ticket.";
        let workflow =
            match_intent_to_catalog(intent, &index, &search, &DiscoveryIntentClass::ReadListNav);
        assert!(!suggests_mutation_workflow(
            &DiscoveryIntentClass::ReadListNav,
            &workflow,
            &index
        ));
    }

    #[test]
    fn repo_scoped_workflow_without_owner_path() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "In the Acme monorepo: open a bug ticket, cut a feature branch, commit a readme update, open a merge proposal, and leave a ticket note with the proposal link.";
        let workflow = match_intent_to_catalog(
            intent,
            &index,
            &search,
            &DiscoveryIntentClass::RepoScopedWorkflow,
        );
        assert!(suggests_mutation_workflow(
            &DiscoveryIntentClass::RepoScopedWorkflow,
            &workflow,
            &index
        ));
        assert_eq!(
            workflow_root_entity(&index, &workflow).as_deref(),
            Some("Workspace")
        );
    }

    #[test]
    fn repo_scoped_workflow_root_is_workspace() {
        let Some(cgs) = workflow_fixture_cgs() else {
            return;
        };
        let index = build_catalog_seed_index("seed_workflow", &cgs);
        let search = search_index_for_cgs("seed_workflow", &cgs);
        let intent = "On workspace acme/tool-test: open a ticket with a bug label, create a branch, commit a small markdown file, open a merge proposal linking the ticket, and comment on the ticket with the proposal link.";
        let workflow = match_intent_to_catalog(
            intent,
            &index,
            &search,
            &DiscoveryIntentClass::RepoScopedWorkflow,
        );
        assert!(suggests_multi_entity_workflow(
            &DiscoveryIntentClass::RepoScopedWorkflow,
            intent,
            &workflow,
            &index
        ));
        assert_eq!(
            workflow_root_entity(&index, &workflow).as_deref(),
            Some("Workspace")
        );
    }
}
