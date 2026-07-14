//! Closed `w#` witness corpus derived from the search/graph candidate pool.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::discovery_candidate_graph::TypedCandidateGraph;
use crate::discovery_seed_catalog::CatalogWorkflowContext;
use crate::discovery_seed_select::SeedSelectionValidationError;
use crate::discovery_seed_symbol_map::entity_aliases_for;
use crate::schema::DiscoverySeedNav;

use super::roles::{
    prefer_seed_nav, OwnEdge, OwnPairs, PoolChild, PoolLinks, SeedClassStamp, SeedNavStamp,
};

/// Max closed witnesses presented to the LLM per selection call.
/// Ranked by lexical score after brand-lock / top-catalog soft filter (not a schema dump).
pub const MAX_WITNESSES: usize = 24;

/// When `brand_lock_catalogs` is empty, keep only this many catalogs by best lexical score.
pub const MAX_WITNESS_CATALOGS_UNBRANDED: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessKind {
    DirectCapability {
        entry_id: String,
        entity: String,
        capability_id: String,
        capability_name: String,
        kind: String,
        description: String,
    },
    RelationHop {
        entry_id: String,
        from_entity: String,
        wire: String,
        target_entity: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementWitness {
    pub symbol: String,
    pub kind: WitnessKind,
    /// Candidate that owns / satisfies this witness when seeded.
    pub owner_candidate_id: String,
    pub lexical_score: u32,
    pub summary: String,
    pub entity_description: String,
    pub aliases: String,
    /// Pool-local parents / children / siblings (typed; BAML renders `graph_note`).
    pub pool: PoolLinks,
    /// Authored entity `discovery.seed_class`.
    pub seed_class: SeedClassStamp,
    /// Governing edge `discovery.seed_nav` when in-pool.
    pub seed_nav: SeedNavStamp,
    /// In-pool `own` edges involving this entity.
    pub own_pairs: OwnPairs,
}

#[derive(Debug, Clone)]
pub struct WitnessCorpus {
    pub witnesses: Vec<RequirementWitness>,
    pub bundles: Vec<EntityCandidateBundle>,
    pub brand_lock_catalogs: Vec<String>,
    pub(super) symbol_to_index: HashMap<String, usize>,
}

impl WitnessCorpus {
    pub fn witness(&self, symbol: &str) -> Option<&RequirementWitness> {
        self.symbol_to_index
            .get(symbol)
            .and_then(|idx| self.witnesses.get(*idx))
    }

    pub fn resolve_symbols(
        &self,
        symbols: &[String],
    ) -> Result<Vec<usize>, SeedSelectionValidationError> {
        let mut indices = Vec::with_capacity(symbols.len());
        let mut seen = HashSet::new();
        for symbol in symbols {
            let Some(idx) = self.resolve_one_symbol(symbol)? else {
                continue;
            };
            if !seen.insert(idx) {
                continue;
            }
            indices.push(idx);
        }
        Ok(indices)
    }

    fn resolve_one_symbol(
        &self,
        symbol: &str,
    ) -> Result<Option<usize>, SeedSelectionValidationError> {
        let trimmed = symbol.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if let Some(&idx) = self.symbol_to_index.get(trimmed) {
            return Ok(Some(idx));
        }
        // Closed-set repair: unique capability_name / capability_id match.
        let needle = trimmed.to_ascii_lowercase();
        let mut hits: Vec<usize> = Vec::new();
        for (idx, witness) in self.witnesses.iter().enumerate() {
            match &witness.kind {
                WitnessKind::DirectCapability {
                    capability_name,
                    capability_id,
                    ..
                } => {
                    if capability_name.eq_ignore_ascii_case(trimmed)
                        || capability_id.eq_ignore_ascii_case(trimmed)
                        || capability_id
                            .to_ascii_lowercase()
                            .ends_with(&format!(":{needle}"))
                    {
                        hits.push(idx);
                    }
                }
                WitnessKind::RelationHop { wire, .. } => {
                    if wire.eq_ignore_ascii_case(trimmed) {
                        hits.push(idx);
                    }
                }
            }
        }
        hits.sort_unstable();
        hits.dedup();
        match hits.as_slice() {
            [] => Err(SeedSelectionValidationError::UnknownSymbol(trimmed.into())),
            [idx] => Ok(Some(*idx)),
            _ => Err(SeedSelectionValidationError::UnknownSymbol(format!(
                "{trimmed} (ambiguous non-w# alias)"
            ))),
        }
    }
}

/// Build closed witnesses from the brand-locked candidate pool + graph edges.
pub fn build_witness_corpus(
    bundles: &[EntityCandidateBundle],
    brand_lock_catalogs: &[String],
    graph: &TypedCandidateGraph,
    catalog_context: Option<&CatalogWorkflowContext>,
) -> Option<WitnessCorpus> {
    let locked: Vec<EntityCandidateBundle> = if brand_lock_catalogs.is_empty() {
        bundles.to_vec()
    } else {
        bundles
            .iter()
            .filter(|b| brand_lock_catalogs.iter().any(|l| l == &b.entry_id))
            .cloned()
            .collect()
    };
    let locked: Vec<_> = locked
        .into_iter()
        .filter(|b| !b.entity.ends_with("Context"))
        .filter(|b| !b.capabilities.is_empty())
        .collect();
    if locked.is_empty() {
        return None;
    }

    let in_pool: HashSet<(String, String)> = locked
        .iter()
        .map(|b| (b.entry_id.clone(), b.entity.clone()))
        .collect();

    let mut drafted: Vec<RequirementWitness> = Vec::new();
    for bundle in &locked {
        let aliases = entity_aliases_for(catalog_context, &bundle.entry_id, &bundle.entity);
        let pool = pool_links_for(bundle, graph, &in_pool, &locked);
        let seed_class = seed_class_stamp(catalog_context, &bundle.entry_id, &bundle.entity);
        let entity_seed_nav =
            governing_seed_nav_for_entity(catalog_context, bundle, graph, &in_pool, &locked);
        let own_pairs = own_pairs_for(catalog_context, bundle, graph, &in_pool, &locked);
        for cap in &bundle.capabilities {
            let summary = format!(
                "{} {}.{} [{}] {}",
                cap.kind,
                bundle.entry_id,
                bundle.entity,
                cap.capability_name,
                truncate(&cap.description, 160)
            );
            drafted.push(RequirementWitness {
                symbol: String::new(),
                kind: WitnessKind::DirectCapability {
                    entry_id: bundle.entry_id.clone(),
                    entity: bundle.entity.clone(),
                    capability_id: cap.capability_id.clone(),
                    capability_name: cap.capability_name.clone(),
                    kind: cap.kind.clone(),
                    description: cap.description.clone(),
                },
                owner_candidate_id: bundle.candidate_id.clone(),
                lexical_score: cap.lexical_score.max(bundle.max_lexical_score),
                summary,
                entity_description: truncate(&bundle.entity_description, 180),
                aliases: aliases.clone(),
                pool: pool.clone(),
                seed_class,
                seed_nav: entity_seed_nav,
                own_pairs: own_pairs.clone(),
            });
        }

        if let Some(node) = graph.node(&bundle.entry_id, &bundle.entity) {
            for edge in &node.outgoing {
                if !in_pool.contains(&(bundle.entry_id.clone(), edge.target_entity.clone())) {
                    continue;
                }
                let summary = format!(
                    "relation {}.{} --{}→ {}.{}",
                    bundle.entry_id,
                    bundle.entity,
                    edge.wire,
                    bundle.entry_id,
                    edge.target_entity
                );
                let hop_nav = seed_nav_stamp(
                    catalog_context,
                    &bundle.entry_id,
                    &bundle.entity,
                    &edge.target_entity,
                );
                drafted.push(RequirementWitness {
                    symbol: String::new(),
                    kind: WitnessKind::RelationHop {
                        entry_id: bundle.entry_id.clone(),
                        from_entity: bundle.entity.clone(),
                        wire: edge.wire.clone(),
                        target_entity: edge.target_entity.clone(),
                    },
                    owner_candidate_id: bundle.candidate_id.clone(),
                    lexical_score: bundle.max_lexical_score,
                    summary,
                    entity_description: truncate(&bundle.entity_description, 180),
                    aliases: aliases.clone(),
                    pool: pool.clone(),
                    seed_class,
                    seed_nav: hop_nav,
                    own_pairs: own_pairs.clone(),
                });
            }
        }
    }

    let mut drafted = shortlist_witnesses_for_llm(drafted, brand_lock_catalogs);

    let mut symbol_to_index = HashMap::new();
    for (idx, witness) in drafted.iter_mut().enumerate() {
        witness.symbol = format!("w{}", idx + 1);
        symbol_to_index.insert(witness.symbol.clone(), idx);
    }

    // Bundles kept for plan cover: only owners still represented in the shortlist.
    let kept_owners: HashSet<&str> = drafted
        .iter()
        .map(|w| w.owner_candidate_id.as_str())
        .collect();
    let bundles = locked
        .into_iter()
        .filter(|b| kept_owners.contains(b.candidate_id.as_str()))
        .collect();

    Some(WitnessCorpus {
        witnesses: drafted,
        bundles,
        brand_lock_catalogs: brand_lock_catalogs.to_vec(),
        symbol_to_index,
    })
}

/// Brand-lock (already applied at bundle stage) + unbranded top-catalog soft cut +
/// top-[`MAX_WITNESSES`] by lexical score before BAML sees the closed set.
fn shortlist_witnesses_for_llm(
    mut drafted: Vec<RequirementWitness>,
    brand_lock_catalogs: &[String],
) -> Vec<RequirementWitness> {
    if drafted.is_empty() {
        return drafted;
    }

    if brand_lock_catalogs.is_empty() {
        drafted = filter_top_catalogs_by_score(drafted, MAX_WITNESS_CATALOGS_UNBRANDED);
    }

    // Lexical score first — never truncate by catalog order (that was flooding w1…w64 with
    // github×linear caps while noise vendors still leaked in at the bottom).
    drafted.sort_by(|a, b| {
        b.lexical_score
            .cmp(&a.lexical_score)
            .then_with(|| witness_kind_rank(a).cmp(&witness_kind_rank(b)))
            .then_with(|| witness_catalog(a).cmp(witness_catalog(b)))
            .then_with(|| witness_entity(a).cmp(witness_entity(b)))
            .then_with(|| a.summary.cmp(&b.summary))
    });
    drafted.truncate(MAX_WITNESSES);
    drafted
}

/// Keep only the N catalogs with the highest max witness lexical score.
fn filter_top_catalogs_by_score(
    drafted: Vec<RequirementWitness>,
    max_catalogs: usize,
) -> Vec<RequirementWitness> {
    if max_catalogs == 0 || drafted.is_empty() {
        return drafted;
    }
    let mut best: HashMap<String, u32> = HashMap::new();
    for w in &drafted {
        let cat = witness_catalog(w).to_string();
        let entry = best.entry(cat).or_insert(0);
        *entry = (*entry).max(w.lexical_score);
    }
    if best.len() <= max_catalogs {
        return drafted;
    }
    let mut ranked: Vec<(String, u32)> = best.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let keep: HashSet<String> = ranked
        .into_iter()
        .take(max_catalogs)
        .map(|(cat, _)| cat)
        .collect();
    drafted
        .into_iter()
        .filter(|w| keep.contains(witness_catalog(w)))
        .collect()
}

pub(super) fn witness_catalog(w: &RequirementWitness) -> &str {
    match &w.kind {
        WitnessKind::DirectCapability { entry_id, .. }
        | WitnessKind::RelationHop { entry_id, .. } => entry_id.as_str(),
    }
}

pub(super) fn witness_entity(w: &RequirementWitness) -> &str {
    match &w.kind {
        WitnessKind::DirectCapability { entity, .. } => entity.as_str(),
        WitnessKind::RelationHop { from_entity, .. } => from_entity.as_str(),
    }
}

fn witness_kind_rank(w: &RequirementWitness) -> u8 {
    match &w.kind {
        WitnessKind::DirectCapability { .. } => 0,
        WitnessKind::RelationHop { .. } => 1,
    }
}

fn pool_links_for(
    bundle: &EntityCandidateBundle,
    graph: &TypedCandidateGraph,
    in_pool: &HashSet<(String, String)>,
    locked: &[EntityCandidateBundle],
) -> PoolLinks {
    let mut parents: BTreeSet<String> = BTreeSet::new();
    let mut children: BTreeSet<PoolChild> = BTreeSet::new();

    if let Some(node) = graph.node(&bundle.entry_id, &bundle.entity) {
        for parent in &node.parents {
            if in_pool.contains(&(bundle.entry_id.clone(), parent.clone())) {
                parents.insert(parent.clone());
            }
        }
        for edge in &node.outgoing {
            if in_pool.contains(&(bundle.entry_id.clone(), edge.target_entity.clone())) {
                children.insert(PoolChild::new(&edge.wire, &edge.target_entity));
            }
        }
    }

    // Peer relation_hints fill gaps when TypedCandidateGraph has no CGS edges.
    for other in locked {
        if other.entry_id != bundle.entry_id || other.entity == bundle.entity {
            continue;
        }
        for (_wire, target) in parse_relation_hint_edges(&other.relation_hints) {
            if target == bundle.entity {
                parents.insert(other.entity.clone());
            }
        }
    }
    for (wire, target) in parse_relation_hint_edges(&bundle.relation_hints) {
        if in_pool.contains(&(bundle.entry_id.clone(), target.clone())) {
            children.insert(PoolChild::new(wire, target));
        }
    }

    let siblings: BTreeSet<String> = locked
        .iter()
        .filter(|other| other.entry_id == bundle.entry_id && other.entity != bundle.entity)
        .map(|other| other.entity.clone())
        .collect();

    PoolLinks {
        parents,
        children,
        siblings,
    }
}

fn seed_class_stamp(
    catalog_context: Option<&CatalogWorkflowContext>,
    entry_id: &str,
    entity: &str,
) -> SeedClassStamp {
    SeedClassStamp::from_catalog(
        catalog_context.and_then(|ctx| ctx.entity_seed_class(entry_id, entity)),
    )
}

fn seed_nav_stamp(
    catalog_context: Option<&CatalogWorkflowContext>,
    entry_id: &str,
    from_entity: &str,
    target_entity: &str,
) -> SeedNavStamp {
    SeedNavStamp::from_catalog(
        catalog_context.and_then(|ctx| ctx.relation_seed_nav(entry_id, from_entity, target_entity)),
    )
}

/// Directed in-pool edge with authored `seed_nav` touching `bundle.entity`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct InPoolNavEdge {
    from: String,
    to: String,
    nav: DiscoverySeedNav,
}

/// Collect authored in-pool `seed_nav` edges incident on this entity (graph + hint fallback).
fn in_pool_nav_edges(
    catalog_context: Option<&CatalogWorkflowContext>,
    bundle: &EntityCandidateBundle,
    graph: &TypedCandidateGraph,
    in_pool: &HashSet<(String, String)>,
    locked: &[EntityCandidateBundle],
) -> BTreeSet<InPoolNavEdge> {
    let Some(ctx) = catalog_context else {
        return BTreeSet::new();
    };
    let mut edges = BTreeSet::new();
    let mut push = |from: &str, to: &str| {
        if from == to {
            return;
        }
        if !in_pool.contains(&(bundle.entry_id.clone(), from.to_string()))
            || !in_pool.contains(&(bundle.entry_id.clone(), to.to_string()))
        {
            return;
        }
        if let Some(nav) = ctx.relation_seed_nav(&bundle.entry_id, from, to) {
            edges.insert(InPoolNavEdge {
                from: from.to_string(),
                to: to.to_string(),
                nav,
            });
        }
    };

    if let Some(node) = graph.node(&bundle.entry_id, &bundle.entity) {
        for edge in &node.outgoing {
            push(&bundle.entity, &edge.target_entity);
        }
        for parent in &node.parents {
            push(parent, &bundle.entity);
        }
    }
    for (wire, target) in parse_relation_hint_edges(&bundle.relation_hints) {
        let _ = wire;
        push(&bundle.entity, &target);
    }
    for other in locked {
        if other.entry_id != bundle.entry_id || other.entity == bundle.entity {
            continue;
        }
        for (_wire, target) in parse_relation_hint_edges(&other.relation_hints) {
            if target == bundle.entity {
                push(&other.entity, &bundle.entity);
            }
        }
    }
    edges
}

/// For a DirectCapability entity, prefer an in-pool incoming edge's `seed_nav`
/// (attach/own/locate). Precedence among multiple parents: attach > own > locate.
fn governing_seed_nav_for_entity(
    catalog_context: Option<&CatalogWorkflowContext>,
    bundle: &EntityCandidateBundle,
    graph: &TypedCandidateGraph,
    in_pool: &HashSet<(String, String)>,
    locked: &[EntityCandidateBundle],
) -> SeedNavStamp {
    let mut best: Option<DiscoverySeedNav> = None;
    for edge in in_pool_nav_edges(catalog_context, bundle, graph, in_pool, locked) {
        if edge.to != bundle.entity {
            continue;
        }
        best = Some(match best {
            None => edge.nav,
            Some(prev) => prefer_seed_nav(prev, edge.nav),
        });
    }
    SeedNavStamp::from_catalog(best)
}

fn own_pairs_for(
    catalog_context: Option<&CatalogWorkflowContext>,
    bundle: &EntityCandidateBundle,
    graph: &TypedCandidateGraph,
    in_pool: &HashSet<(String, String)>,
    locked: &[EntityCandidateBundle],
) -> OwnPairs {
    OwnPairs::new(
        in_pool_nav_edges(catalog_context, bundle, graph, in_pool, locked)
            .into_iter()
            .filter(|e| e.nav == DiscoverySeedNav::Own)
            .map(|e| OwnEdge::new(e.from, e.to)),
    )
}

fn parse_relation_hint_edges(hints: &str) -> Vec<(String, String)> {
    if hints.is_empty() || hints == "(none)" {
        return Vec::new();
    }
    hints
        .split(|c| c == ';' || c == ',')
        .filter_map(|part| {
            let part = part.trim();
            let (wire, target) = part.split_once('→').or_else(|| part.split_once("->"))?;
            let wire = wire.trim();
            let target = target.trim();
            if wire.is_empty() || target.is_empty() {
                None
            } else {
                Some((wire.to_string(), target.to_string()))
            }
        })
        .collect()
}

pub(super) fn truncate(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.chars().count() >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}
