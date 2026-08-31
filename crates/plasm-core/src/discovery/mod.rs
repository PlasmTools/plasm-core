//! Multi-entry CGS catalog and deterministic capability discovery.

use crate::cgs_context::{CgsContext, Prefix};
use crate::discovery_presentation::CatalogRoute;
use crate::schema::{CapabilityKind, CapabilitySchema, RelationSchema, CGS};
use crate::symbol_tuning::build_focus_set_union;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

mod exposure_surface;
mod mutator_admit;
pub use exposure_surface::{
    derive_intent_exposure_surface_batch, outgoing_relation_hints_for_entity,
    relation_target_deferred_mutator_wires, ExposureSurfaceOptions, MutatorAdmit,
    DISCOVERY_OUTGOING_RELATIONS_MAX,
};

/// Max length for `capability_description` and [`EntitySummary::description`] in discovery JSON.
const DISCOVERY_DESCRIPTION_MAX_CHARS: usize = 240;

fn truncate_discovery_description(s: &str) -> String {
    let t = s.trim();
    if t.len() <= DISCOVERY_DESCRIPTION_MAX_CHARS {
        return t.to_string();
    }
    let mut out: String = t.chars().take(DISCOVERY_DESCRIPTION_MAX_CHARS).collect();
    out.push('…');
    out
}

fn collect_discovery_hint_phrases(cgs: &CGS, cap: &CapabilitySchema) -> Vec<String> {
    let mut phrases = Vec::new();
    if let Some(ent) = cgs.entities.get(cap.domain.as_str()) {
        if let Some(h) = &ent.discovery {
            phrases.extend(h.names.iter().cloned());
            phrases.extend(h.qualifier_names.iter().cloned());
        }
    }
    if let Some(h) = &cap.discovery {
        phrases.extend(h.operation_terms.iter().cloned());
        phrases.extend(h.target_terms.iter().cloned());
    }
    let mut seen = HashSet::new();
    phrases.retain(|p| {
        let key = p.trim().to_ascii_lowercase();
        if key.is_empty() {
            return false;
        }
        seen.insert(key)
    });
    phrases
}

fn capability_description_for_discovery(cgs: &CGS, cap: &CapabilitySchema) -> String {
    let base = cap.description.trim();
    let hints = collect_discovery_hint_phrases(cgs, cap);
    if hints.is_empty() {
        return truncate_discovery_description(base);
    }
    let hints_joined = hints.join(", ");
    if base.is_empty() {
        truncate_discovery_description(&format!("hints: {hints_joined}"))
    } else {
        truncate_discovery_description(&format!("{base} · hints: {hints_joined}"))
    }
}

/// Resolve a user/model string to the canonical CGS entity key (case-insensitive).
pub(crate) fn resolve_canonical_entity_name(cgs: &CGS, raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    for k in cgs.entities.keys() {
        if k.eq_ignore_ascii_case(raw) {
            return Some(k.to_string());
        }
    }
    None
}

fn merge_expand_and_hint_seeds(
    cgs: &CGS,
    query: &CapabilityQuery,
    mut seeds: HashSet<String>,
) -> HashSet<String> {
    if let Some(extra) = &query.expand_entities {
        for name in extra {
            if let Some(canon) = resolve_canonical_entity_name(cgs, name) {
                seeds.insert(canon);
            }
        }
    }
    for hint in &query.entity_hints {
        if let Some(canon) = resolve_canonical_entity_name(cgs, hint) {
            seeds.insert(canon);
        }
    }
    seeds
}

fn build_schema_neighborhood_for_entry(
    entry_id: String,
    cgs: &CGS,
    query: &CapabilityQuery,
    base_seeds: HashSet<String>,
) -> Option<DiscoverySchemaNeighborhood> {
    let seeds = merge_expand_and_hint_seeds(cgs, query, base_seeds);
    let mut seed_vec: Vec<String> = seeds.into_iter().collect();
    seed_vec.sort();
    if seed_vec.is_empty() {
        return None;
    }
    let seed_refs: Vec<&str> = seed_vec.iter().map(|s| s.as_str()).collect();
    let focused_set = build_focus_set_union(cgs, &seed_refs);
    let mut focused_entities: Vec<String> = focused_set.into_iter().map(str::to_string).collect();
    focused_entities.sort();
    Some(DiscoverySchemaNeighborhood {
        entry_id,
        seed_entities: seed_vec,
        focused_entities,
    })
}

/// Metadata for one catalog row (no full [`CGS`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntryMeta {
    pub entry_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Stable digest of the loaded CGS (`CGS::catalog_cgs_hash_hex`); bumps when the graph changes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub catalog_cgs_hash: String,
}

/// Agent query: deterministic match over registered graphs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CapabilityQuery {
    #[serde(default)]
    pub tokens: Vec<String>,
    #[serde(default)]
    pub phrases: Vec<String>,
    #[serde(default)]
    pub entity_hints: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<CapabilityKind>,
    pub capability_names: Option<Vec<String>>,
    pub entry_ids: Option<Vec<String>>,
    pub pick_entry: Option<String>,
    pub pick_capabilities: Option<Vec<String>>,
    pub exclude_capabilities: Option<Vec<String>>,
    pub expand_entities: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedCandidate {
    pub entry_id: String,
    /// CGS entity / domain name for this capability (use with `entry_id` for `POST /execute` `entities`).
    pub entity: String,
    pub capability_name: String,
    pub score: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<String>,
    /// Trimmed capability description for LLM-facing discovery (no need to parse `contexts[].cgs`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub capability_description: String,
}

/// One entity’s CGS description for choosing `POST /execute` `entities` without mining full CGS JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntitySummary {
    pub entry_id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ambiguity {
    pub dimension: String,
    pub entry_ids: Vec<String>,
    pub capability_name: String,
    pub score: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClosureStats {
    pub context_count: usize,
    pub total_entities: usize,
    pub total_capabilities: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryContextJson {
    pub prefix: Prefix,
    pub cgs: CGS,
}

/// Same entity closure as the REPL `:schema <Entity>` command (via [`crate::symbol_tuning::build_focus_set`])
/// and HTTP execute seeds (via [`crate::symbol_tuning::build_focus_set_union`]): each seed plus outgoing
/// `EntityRef` / relation targets and entities with incoming `EntityRef` to a seed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoverySchemaNeighborhood {
    pub entry_id: String,
    /// Distinct [`RankedCandidate::entity`] for this catalog row, plus any `expand_entities` query names that exist in the CGS.
    pub seed_entities: Vec<String>,
    /// Sorted list suitable for `POST /execute` `entities` — mirrors the focused teaching slice from `:schema` / `RenderConfig::for_eval_seeds`.
    pub focused_entities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub contexts: Vec<DiscoveryContextJson>,
    pub candidates: Vec<RankedCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguities: Vec<Ambiguity>,
    pub applied_query_echo: CapabilityQuery,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_stats: Option<ClosureStats>,
    /// Per catalog entry: REPL-style focused entity set for opening execute sessions after discovery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schema_neighborhoods: Vec<DiscoverySchemaNeighborhood>,
    /// Short entity descriptions for every name in `schema_neighborhoods[].focused_entities` (deduped, sorted by name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_summaries: Vec<EntitySummary>,
    /// Catalog `entry_id`s matched by intent catalog routing (empty when routing scanned all catalogs).
    #[serde(default, skip_serializing_if = "CatalogRoute::is_empty")]
    pub catalog_route: crate::discovery_presentation::CatalogRoute,
}

#[derive(Error, Debug)]
pub enum DiscoveryError {
    #[error("unknown catalog entry: {0}")]
    UnknownEntry(String),
    #[error(
        "discovery query produced no selectors (add tokens, phrases, expand_entities, capability_names, or pick_*)"
    )]
    EmptyQuery,
}

/// Source of truth for registered [`CGS`] graphs.
pub trait CgsCatalog: Send + Sync {
    fn list_entries(&self) -> Vec<CatalogEntryMeta>;
    fn load_context(&self, entry_id: &str) -> Result<CgsContext, DiscoveryError>;
    /// Metadata for one catalog row without building the full [`list_entries`](Self::list_entries) vec.
    fn lookup_entry_meta(&self, entry_id: &str) -> Option<CatalogEntryMeta>;
}

/// Deterministic search and packaging of [`DiscoveryResult`].
pub trait CgsDiscovery: Send + Sync {
    fn discover(&self, query: &CapabilityQuery) -> Result<DiscoveryResult, DiscoveryError>;

    /// Process-owned BM25 corpus used by discover + seed retrieve (same index, no rebuild).
    fn search_index(&self) -> &crate::catalog_search_index::CatalogSearchIndex;
}

struct RegistryRow {
    label: String,
    tags: Vec<String>,
    aliases: Vec<String>,
    cgs: Arc<CGS>,
    catalog_cgs_hash: String,
}

fn fallback_target_entry_ids(
    query: &CapabilityQuery,
    entries: &IndexMap<String, RegistryRow>,
    catalog_route: Option<&HashSet<String>>,
) -> Vec<String> {
    if let Some(p) = &query.pick_entry {
        return vec![p.clone()];
    }
    if let Some(ids) = &query.entry_ids {
        return ids.clone();
    }
    let Some(exp) = &query.expand_entities else {
        return vec![];
    };
    if exp.is_empty() {
        return vec![];
    }
    let mut out: Vec<String> = Vec::new();
    for (eid, row) in entries {
        if let Some(route) = catalog_route {
            if !route.contains(eid) {
                continue;
            }
        }
        let cgs = row.cgs.as_ref();
        if exp
            .iter()
            .any(|raw| resolve_canonical_entity_name(cgs, raw).is_some())
        {
            out.push(eid.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn build_entity_summaries(
    entries: &IndexMap<String, RegistryRow>,
    neighborhoods: &[DiscoverySchemaNeighborhood],
) -> Vec<EntitySummary> {
    let mut by_key: IndexMap<(String, String), String> = IndexMap::new();
    for n in neighborhoods {
        let Some(row) = entries.get(&n.entry_id) else {
            continue;
        };
        let cgs = row.cgs.as_ref();
        for name in &n.focused_entities {
            let key = (n.entry_id.clone(), name.clone());
            if by_key.contains_key(&key) {
                continue;
            }
            let desc = cgs
                .get_entity(name.as_str())
                .map(|e| truncate_discovery_description(&e.description))
                .unwrap_or_default();
            by_key.insert(key, desc);
        }
    }
    let mut v: Vec<EntitySummary> = by_key
        .into_iter()
        .map(|((entry_id, name), description)| EntitySummary {
            entry_id,
            name,
            description,
        })
        .collect();
    v.sort_by(|a, b| {
        a.entry_id
            .cmp(&b.entry_id)
            .then_with(|| a.name.cmp(&b.name))
    });
    v
}

/// One registry row for [`InMemoryCgsRegistry::from_pairs`]:
/// `(entry_id, label, tags, cgs)`. HTTP origin is [`CGS::http_backend`].
pub type RegistryEntryPair = (String, String, Vec<String>, Arc<CGS>);

/// In-memory catalog + discovery (Okapi BM25 via [`CatalogSearchIndex`]).
pub struct InMemoryCgsRegistry {
    entries: IndexMap<String, RegistryRow>,
    search_index: crate::catalog_search_index::CatalogSearchIndex,
}

impl InMemoryCgsRegistry {
    pub fn from_pairs(pairs: Vec<RegistryEntryPair>) -> Self {
        let mut map = IndexMap::new();
        for (id, label, tags, cgs) in pairs {
            let catalog_cgs_hash = cgs.catalog_cgs_hash_hex();
            let aliases = cgs.registry_aliases.clone();
            map.insert(
                id.clone(),
                RegistryRow {
                    label,
                    tags,
                    aliases,
                    cgs,
                    catalog_cgs_hash,
                },
            );
        }
        let search_pairs: Vec<(&str, &CGS)> = map
            .iter()
            .map(|(id, row)| (id.as_str(), row.cgs.as_ref()))
            .collect();
        let search_index =
            crate::catalog_search_index::CatalogSearchIndex::build_from_pairs(search_pairs);
        Self {
            entries: map,
            search_index,
        }
    }

    /// Process-local BM25 index rebuilt with [`Self::from_pairs`] / catalog reload.
    pub fn search_index(&self) -> &crate::catalog_search_index::CatalogSearchIndex {
        &self.search_index
    }

    pub fn entry_ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// All loaded catalog graphs keyed by registry `entry_id`.
    pub fn catalog_arcs(&self) -> IndexMap<String, Arc<CGS>> {
        self.entries
            .iter()
            .map(|(id, row)| (id.clone(), Arc::clone(&row.cgs)))
            .collect()
    }

    /// Borrow the loaded CGS for a canonical registry `entry_id` (no clone of the full map).
    pub fn cgs_arc(&self, entry_id: &str) -> Option<Arc<CGS>> {
        self.entries.get(entry_id).map(|row| Arc::clone(&row.cgs))
    }

    /// Resolve a raw catalog id (entry_id, alias, label, or tag) to the canonical registry `entry_id`.
    ///
    /// When `allowed_entry_ids` is non-empty, only those catalogs are considered (tenant MCP scope).
    pub fn resolve_entry_id(
        &self,
        raw: &str,
        allowed_entry_ids: Option<&[String]>,
    ) -> Result<String, DiscoveryError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(DiscoveryError::UnknownEntry(String::new()));
        }

        let allowed: Option<std::collections::HashSet<&str>> = allowed_entry_ids.map(|ids| {
            ids.iter()
                .map(|s| s.as_str())
                .collect::<std::collections::HashSet<_>>()
        });

        let is_allowed = |id: &str| allowed.as_ref().is_none_or(|a| a.contains(id));

        if self.entries.contains_key(raw) && is_allowed(raw) {
            return Ok(raw.to_string());
        }

        let mut matches: Vec<String> = Vec::new();
        for (id, row) in &self.entries {
            if !is_allowed(id.as_str()) {
                continue;
            }
            let hit = id.eq_ignore_ascii_case(raw)
                || row.label.eq_ignore_ascii_case(raw)
                || row.tags.iter().any(|t| t.eq_ignore_ascii_case(raw))
                || row.aliases.iter().any(|a| a.eq_ignore_ascii_case(raw));
            if hit {
                matches.push(id.clone());
            }
        }
        matches.sort();
        matches.dedup();

        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => {
                let hint = suggest_entry_id(raw, self, allowed.as_ref());
                Err(DiscoveryError::UnknownEntry(if hint.is_empty() {
                    raw.to_string()
                } else {
                    format!("{raw} ({hint})")
                }))
            }
            _ => Err(DiscoveryError::UnknownEntry(format!(
                "{raw} (ambiguous: {})",
                matches.join(", ")
            ))),
        }
    }

    /// First catalog entry's CGS in insertion order (YAML / `from_pairs` order).
    ///
    /// Used to bootstrap the CLI and execution engine when `--registry` is given without `--schema`.
    pub fn first_cgs(&self) -> Option<Arc<CGS>> {
        self.entries.first().map(|(_, row)| row.cgs.clone())
    }
}

fn suggest_entry_id(
    raw: &str,
    reg: &InMemoryCgsRegistry,
    allowed: Option<&std::collections::HashSet<&str>>,
) -> String {
    let raw_l = raw.to_ascii_lowercase();
    let mut best: Option<(u32, String)> = None;
    for (id, row) in &reg.entries {
        if allowed.is_some_and(|a| !a.contains(id.as_str())) {
            continue;
        }
        let candidates = [id.as_str(), row.label.as_str()]
            .into_iter()
            .chain(row.aliases.iter().map(|s| s.as_str()))
            .chain(row.tags.iter().map(|s| s.as_str()));
        for cand in candidates {
            let dist = levenshtein_ascii(&raw_l, &cand.to_ascii_lowercase());
            if dist <= 3 && best.as_ref().is_none_or(|(d, _)| dist < *d) {
                best = Some((dist, id.clone()));
            }
        }
    }
    best.map(|(_, id)| format!("did you mean `{id}`?"))
        .unwrap_or_default()
}

fn levenshtein_ascii(a: &str, b: &str) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len() as u32;
    }
    if b.is_empty() {
        return a.len() as u32;
    }
    let mut prev: Vec<u32> = (0..=b.len()).map(|i| i as u32).collect();
    let mut cur = vec![0u32; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = (i + 1) as u32;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

impl CgsCatalog for InMemoryCgsRegistry {
    fn list_entries(&self) -> Vec<CatalogEntryMeta> {
        self.entries
            .iter()
            .map(|(id, row)| CatalogEntryMeta {
                entry_id: id.clone(),
                label: row.label.clone(),
                tags: row.tags.clone(),
                aliases: row.aliases.clone(),
                catalog_cgs_hash: row.catalog_cgs_hash.clone(),
            })
            .collect()
    }

    fn lookup_entry_meta(&self, entry_id: &str) -> Option<CatalogEntryMeta> {
        self.entries.get(entry_id).map(|row| CatalogEntryMeta {
            entry_id: entry_id.to_string(),
            label: row.label.clone(),
            tags: row.tags.clone(),
            aliases: row.aliases.clone(),
            catalog_cgs_hash: row.catalog_cgs_hash.clone(),
        })
    }

    fn load_context(&self, entry_id: &str) -> Result<CgsContext, DiscoveryError> {
        let row = self
            .entries
            .get(entry_id)
            .ok_or_else(|| DiscoveryError::UnknownEntry(entry_id.to_string()))?;
        Ok(CgsContext::entry(entry_id, row.cgs.clone()))
    }
}

fn collect_query_tokens(query: &CapabilityQuery) -> HashSet<String> {
    let mut set = HashSet::new();
    for t in &query.tokens {
        set.extend(crate::catalog_search_index::CatalogSearchIndex::tokenize(t));
    }
    for p in &query.phrases {
        set.extend(crate::catalog_search_index::CatalogSearchIndex::tokenize(p));
    }
    set
}

/// Free-text query string for BM25 (`phrases` + `tokens` joined).
fn collect_query_text(query: &CapabilityQuery) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for t in &query.tokens {
        let u = t.trim();
        if !u.is_empty() {
            parts.push(u);
        }
    }
    for p in &query.phrases {
        let u = p.trim();
        if !u.is_empty() {
            parts.push(u);
        }
    }
    parts.join(" ")
}

fn catalog_route_probe_lower(query: &CapabilityQuery) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for t in &query.tokens {
        let u = t.trim();
        if !u.is_empty() {
            parts.push(u);
        }
    }
    for p in &query.phrases {
        let u = p.trim();
        if !u.is_empty() {
            parts.push(u);
        }
    }
    parts.join(" ").to_ascii_lowercase()
}

fn catalog_route_tokens_from_query(query: &CapabilityQuery) -> HashSet<String> {
    // Brand / entry-id tokens must survive stopwording — use raw alphanumeric splits.
    let mut set = HashSet::new();
    let probe = catalog_route_probe_lower(query);
    for w in probe.split(|c: char| !c.is_alphanumeric()) {
        if w.len() >= 2 {
            set.insert(w.to_string());
        }
    }
    set
}

fn probe_word_hit(probe_lower: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    probe_lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == word)
}

fn entry_matches_catalog_route(
    entry_id: &str,
    label: &str,
    tags: &[String],
    aliases: &[String],
    probe_lower: &str,
    route_tokens: &HashSet<String>,
) -> bool {
    let eid_lower = entry_id.to_ascii_lowercase();

    if eid_lower.len() >= 4
        && (route_tokens.contains(&eid_lower) || probe_word_hit(probe_lower, &eid_lower))
    {
        return true;
    }

    let segments: Vec<&str> = entry_id
        .split(|c| ['_', '-'].contains(&c))
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() >= 2 {
        let mut all = true;
        let mut saw_required = false;
        for seg in &segments {
            let sl = seg.to_ascii_lowercase();
            if sl.len() < 3 {
                continue;
            }
            saw_required = true;
            if !(route_tokens.contains(&sl) || probe_word_hit(probe_lower, &sl)) {
                all = false;
                break;
            }
        }
        if all && saw_required {
            return true;
        }
    }

    let lab = label.trim().to_ascii_lowercase();
    if lab.len() >= 4 && probe_lower.contains(&lab) {
        return true;
    }

    if eid_lower.contains('-') || eid_lower.contains('_') {
        let normalized = eid_lower.replace(['-', '_'], " ");
        if normalized.len() >= 4 && probe_lower.contains(&normalized) {
            return true;
        }
    }

    for tag in tags {
        let t = tag.trim().to_ascii_lowercase();
        if t.len() >= 4
            && (probe_lower.contains(&t)
                || route_tokens.contains(&t)
                || probe_word_hit(probe_lower, &t))
        {
            return true;
        }
    }

    for alias in aliases {
        let a = alias.trim().to_ascii_lowercase();
        if a.len() >= 4 && (route_tokens.contains(&a) || probe_word_hit(probe_lower, &a)) {
            return true;
        }
    }

    false
}

fn catalog_routes_for_query(
    query: &CapabilityQuery,
    entries: &IndexMap<String, RegistryRow>,
) -> Option<HashSet<String>> {
    if query.pick_entry.is_some() || query.entry_ids.is_some() {
        return None;
    }
    let probe_lower = catalog_route_probe_lower(query);
    let route_tokens = catalog_route_tokens_from_query(query);
    if probe_lower.is_empty() && route_tokens.is_empty() {
        return None;
    }
    let mut matched = HashSet::new();
    for (eid, row) in entries {
        if entry_matches_catalog_route(
            eid,
            &row.label,
            &row.tags,
            &row.aliases,
            &probe_lower,
            &route_tokens,
        ) {
            matched.insert(eid.clone());
        }
    }
    if matched.is_empty() {
        None
    } else {
        Some(matched)
    }
}

fn score_token_hits(query: &HashSet<String>, text: &str) -> (u32, Vec<String>) {
    let mut codes = Vec::new();
    let mut score = 0u32;
    for tok in crate::catalog_search_index::CatalogSearchIndex::tokenize(text) {
        if query.contains(&tok) {
            score += 1;
            codes.push(format!("token:{tok}"));
        }
    }
    (score, codes)
}

/// BM25 milli-score for one capability against free-text intent (ephemeral single-catalog index).
#[cfg(test)]
pub(crate) fn score_capability_bm25(
    cgs: &CGS,
    entry_id: &str,
    cap: &CapabilitySchema,
    query_text: &str,
) -> u32 {
    if query_text.trim().is_empty() {
        return 0;
    }
    let index =
        crate::catalog_search_index::CatalogSearchIndex::build_from_pairs([(entry_id, cgs)]);
    index.capability_score(entry_id, cap.name.as_str(), query_text)
}

/// Non-zero when `rel.discovery.qualifier_terms` is empty (always admit) or intent overlaps a term.
pub(crate) fn score_relation_against_intent(
    query_tokens: &HashSet<String>,
    rel: &RelationSchema,
) -> u32 {
    let Some(h) = &rel.discovery else {
        return 1;
    };
    if h.qualifier_terms.is_empty() {
        return 1;
    }
    let mut total = 0u32;
    for term in &h.qualifier_terms {
        total = total.saturating_add(score_token_hits(query_tokens, term.as_str()).0);
    }
    total
}

fn entity_hint_matches(hints: &[String], domain: &str) -> bool {
    if hints.is_empty() {
        return true;
    }
    let domain_lower = domain.to_ascii_lowercase();
    for hint in hints {
        if hint.eq_ignore_ascii_case(domain) {
            return true;
        }
        let h = hint.to_ascii_lowercase();
        if domain_lower.contains(&h) || h.contains(&domain_lower) {
            return true;
        }
        for ht in crate::catalog_search_index::CatalogSearchIndex::tokenize(hint) {
            for dt in crate::catalog_search_index::CatalogSearchIndex::tokenize(domain) {
                if ht == dt {
                    return true;
                }
            }
        }
    }
    false
}

fn cap_passes_filters(query: &CapabilityQuery, entry_id: &str, cap: &CapabilitySchema) -> bool {
    if let Some(ids) = &query.entry_ids {
        if !ids.iter().any(|x| x == entry_id) {
            return false;
        }
    }
    if let Some(pick) = &query.pick_entry {
        if pick != entry_id {
            return false;
        }
    }
    if !query.kinds.is_empty() && !query.kinds.contains(&cap.kind) {
        return false;
    }
    if let Some(names) = &query.capability_names {
        if !names.iter().any(|n| n == cap.name.as_str()) {
            return false;
        }
    }
    if let Some(pick) = &query.pick_capabilities {
        if !pick.iter().any(|n| n == cap.name.as_str()) {
            return false;
        }
    }
    if let Some(ex) = &query.exclude_capabilities {
        if ex.iter().any(|n| n == cap.name.as_str()) {
            return false;
        }
    }
    if !entity_hint_matches(&query.entity_hints, cap.domain.as_str()) {
        return false;
    }
    true
}

impl CgsDiscovery for InMemoryCgsRegistry {
    fn discover(&self, query: &CapabilityQuery) -> Result<DiscoveryResult, DiscoveryError> {
        let span = crate::spans::discovery_discover();
        let _guard = span.enter();
        let query_text = collect_query_text(query);
        let query_tokens = collect_query_tokens(query);
        let has_explicit_expand = query
            .expand_entities
            .as_ref()
            .is_some_and(|v| !v.is_empty());
        let has_explicit = query.capability_names.is_some()
            || query.pick_capabilities.is_some()
            || query.pick_entry.is_some()
            || query.entry_ids.is_some()
            || has_explicit_expand;

        if query_text.trim().is_empty() && query_tokens.is_empty() && !has_explicit {
            return Err(DiscoveryError::EmptyQuery);
        }

        let catalog_route = catalog_routes_for_query(query, &self.entries);
        let mut score_by_key: HashMap<(String, String), u32> = HashMap::new();
        if !query_text.trim().is_empty() {
            let limit = self.search_index.doc_count().max(1);
            for hit in self.search_index.search(&query_text, limit) {
                if let Some(route) = &catalog_route {
                    if !route.contains(&hit.entry_id) {
                        continue;
                    }
                }
                score_by_key.insert((hit.entry_id, hit.capability_name), hit.score);
            }
        }

        let mut candidates: Vec<RankedCandidate> = Vec::new();

        for (entry_id, row) in &self.entries {
            if let Some(route) = &catalog_route {
                if !route.contains(entry_id) {
                    continue;
                }
            }
            for cap in row.cgs.capabilities.values() {
                if !cap_passes_filters(query, entry_id, cap) {
                    continue;
                }
                let mut score = score_by_key
                    .get(&(entry_id.clone(), cap.name.to_string()))
                    .copied()
                    .unwrap_or(0);
                let mut reasons = vec!["bm25".to_string()];
                if has_explicit
                    && query
                        .capability_names
                        .as_ref()
                        .is_some_and(|n| n.iter().any(|x| x == cap.name.as_str()))
                {
                    score = score.saturating_add(1000);
                    reasons.push("filter:capability_name".into());
                }
                if has_explicit
                    && query
                        .pick_capabilities
                        .as_ref()
                        .is_some_and(|n| n.iter().any(|x| x == cap.name.as_str()))
                {
                    score = score.saturating_add(500);
                    reasons.push("filter:pick_capabilities".into());
                }
                if query_text.trim().is_empty() && score == 0 && has_explicit {
                    reasons.push("filter:explicit_only".into());
                }
                if score == 0 && !query_text.trim().is_empty() {
                    continue;
                }
                if score == 0 && query_text.trim().is_empty() && !has_explicit {
                    continue;
                }
                reasons.sort();
                reasons.dedup();
                candidates.push(RankedCandidate {
                    entry_id: entry_id.clone(),
                    entity: cap.domain.to_string(),
                    capability_name: cap.name.to_string(),
                    score,
                    reason_codes: reasons,
                    capability_description: capability_description_for_discovery(&row.cgs, cap),
                });
            }
        }

        candidates.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.entry_id.cmp(&b.entry_id))
                .then_with(|| a.capability_name.cmp(&b.capability_name))
        });

        let mut ambiguities = Vec::new();
        if candidates.len() >= 2 {
            let top = candidates[0].score;
            let mut by_cap: HashMap<String, Vec<&RankedCandidate>> = HashMap::new();
            for c in candidates.iter().filter(|c| c.score == top) {
                by_cap.entry(c.capability_name.clone()).or_default().push(c);
            }
            for (cap_name, group) in by_cap {
                if group.len() < 2 {
                    continue;
                }
                let mut eids: Vec<String> = group.iter().map(|c| c.entry_id.clone()).collect();
                eids.sort();
                eids.dedup();
                if eids.len() >= 2 {
                    ambiguities.push(Ambiguity {
                        dimension: "same_capability_name_top_score".into(),
                        entry_ids: eids,
                        capability_name: cap_name,
                        score: top,
                    });
                }
            }
        }

        let mut schema_neighborhoods: Vec<DiscoverySchemaNeighborhood> = Vec::new();

        if !candidates.is_empty() {
            let mut entry_ids: Vec<String> =
                candidates.iter().map(|c| c.entry_id.clone()).collect();
            entry_ids.sort();
            entry_ids.dedup();
            for eid in entry_ids {
                let row = self
                    .entries
                    .get(eid.as_str())
                    .expect("candidate entry_id must exist");
                let base_seeds: HashSet<String> = candidates
                    .iter()
                    .filter(|c| c.entry_id == eid)
                    .map(|c| c.entity.clone())
                    .collect();
                if let Some(n) = build_schema_neighborhood_for_entry(
                    eid.clone(),
                    row.cgs.as_ref(),
                    query,
                    base_seeds,
                ) {
                    schema_neighborhoods.push(n);
                }
            }
        } else {
            let fb = fallback_target_entry_ids(query, &self.entries, catalog_route.as_ref());
            for eid in fb {
                let Some(row) = self.entries.get(&eid) else {
                    continue;
                };
                let base_seeds = HashSet::new();
                if let Some(n) = build_schema_neighborhood_for_entry(
                    eid.clone(),
                    row.cgs.as_ref(),
                    query,
                    base_seeds,
                ) {
                    schema_neighborhoods.push(n);
                }
            }
            schema_neighborhoods.sort_by(|a, b| a.entry_id.cmp(&b.entry_id));
        }

        let mut seen_entry: HashSet<String> = HashSet::new();
        for c in &candidates {
            seen_entry.insert(c.entry_id.clone());
        }
        for n in &schema_neighborhoods {
            seen_entry.insert(n.entry_id.clone());
        }

        let mut contexts = Vec::new();
        let mut ctx_ids: Vec<String> = seen_entry.iter().cloned().collect();
        ctx_ids.sort();
        for eid in ctx_ids {
            let row = self
                .entries
                .get(eid.as_str())
                .expect("context entry_id must exist");
            contexts.push(DiscoveryContextJson {
                prefix: Prefix::Entry { id: eid.clone() },
                cgs: (*row.cgs).clone(),
            });
        }
        contexts.sort_by(|a, b| match (&a.prefix, &b.prefix) {
            (Prefix::Entry { id: ia }, Prefix::Entry { id: ib }) => ia.cmp(ib),
            _ => std::cmp::Ordering::Equal,
        });

        let closure_stats = ClosureStats {
            context_count: contexts.len(),
            total_entities: contexts.iter().map(|c| c.cgs.entities.len()).sum(),
            total_capabilities: contexts.iter().map(|c| c.cgs.capabilities.len()).sum(),
        };

        let entity_summaries = build_entity_summaries(&self.entries, &schema_neighborhoods);

        tracing::debug!(
            candidate_count = candidates.len(),
            schema_neighborhood_count = schema_neighborhoods.len(),
            entity_summary_count = entity_summaries.len(),
            context_count = contexts.len(),
            ambiguity_count = ambiguities.len(),
            "cgs discovery completed"
        );

        tracing::Span::current().record("candidate_count", candidates.len());
        tracing::Span::current().record("result_count", candidates.len());

        let catalog_route = catalog_route
            .as_ref()
            .map(|set| {
                let mut v: Vec<String> = set.iter().cloned().collect();
                v.sort();
                CatalogRoute::from(v)
            })
            .unwrap_or_default();

        Ok(DiscoveryResult {
            contexts,
            candidates,
            ambiguities,
            applied_query_echo: query.clone(),
            closure_stats: Some(closure_stats),
            schema_neighborhoods,
            entity_summaries,
            catalog_route,
        })
    }

    fn search_index(&self) -> &crate::catalog_search_index::CatalogSearchIndex {
        &self.search_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::Prefix;
    use std::collections::HashSet;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn discover_fixture_by_token() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs,
        )]);
        let q = CapabilityQuery {
            tokens: vec!["profile".into()],
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        assert!(!r.candidates.is_empty());
        assert!(r
            .candidates
            .iter()
            .any(|c| c.capability_name.contains("profile")));
        assert!(r.candidates.iter().any(|c| c.entity == "Profile"));

        let n = r
            .schema_neighborhoods
            .iter()
            .find(|n| n.entry_id == "overshow")
            .expect("schema_neighborhoods includes overshow");
        assert!(n.seed_entities.contains(&"Profile".to_string()));
        assert!(
            n.focused_entities
                .contains(&"RecordedContent".to_string()),
            "REPL-style :schema Profile neighbourhood includes RecordedContent relation/ref; got {:?}",
            n.focused_entities
        );
        let cap = r
            .candidates
            .iter()
            .find(|c| c.capability_name == "recorded_content_query_by_profile")
            .expect("described profile-scoped capability");
        assert!(
            !cap.capability_description.is_empty(),
            "candidate should carry truncated capability_description"
        );
        assert!(
            r.entity_summaries.iter().any(|s| s.name == "Profile"),
            "entity_summaries should include Profile; got {:?}",
            r.entity_summaries
        );
    }

    /// No capability rows match, but `pick_entry` + `expand_entities` still yields neighbourhoods + summaries.
    #[test]
    fn discover_fallback_schema_neighborhood_when_candidates_empty() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs,
        )]);
        let q = CapabilityQuery {
            pick_entry: Some("overshow".into()),
            capability_names: Some(vec!["__no_such_capability__".into()]),
            expand_entities: Some(vec!["profile".into()]),
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        assert!(r.candidates.is_empty());
        assert_eq!(r.contexts.len(), 1);
        assert_eq!(
            r.contexts[0].prefix,
            Prefix::Entry {
                id: "overshow".into()
            }
        );
        let n = r
            .schema_neighborhoods
            .iter()
            .find(|n| n.entry_id == "overshow")
            .expect("fallback neighbourhood");
        assert!(n.seed_entities.contains(&"Profile".to_string()));
        assert!(n.focused_entities.contains(&"Profile".to_string()));
        let profile_sum = r
            .entity_summaries
            .iter()
            .find(|s| s.name == "Profile")
            .expect("Profile summary");
        assert!(!profile_sum.description.is_empty());
    }

    #[test]
    fn discover_catalog_route_vendor_brand_long_intent() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![
            (
                "vendor_firewall".into(),
                "Cloudflare".into(),
                vec![],
                cgs.clone(),
            ),
            ("clickup".into(), "ClickUp".into(), vec![], cgs.clone()),
            ("github".into(), "GitHub".into(), vec![], cgs.clone()),
        ]);
        let phrase =
            "Update Cloudflare zone WAF rules and comment moderation labels for security issues";
        let q = CapabilityQuery {
            phrases: vec![phrase.into()],
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        assert!(
            r.candidates.iter().all(|c| c.entry_id == "vendor_firewall"),
            "expected only vendor_firewall candidates; got {:?}",
            r.candidates.iter().map(|c| &c.entry_id).collect::<Vec<_>>()
        );
        assert!(
            r.schema_neighborhoods
                .iter()
                .all(|n| n.entry_id == "vendor_firewall"),
            "expected neighborhoods only for vendor_firewall"
        );
        assert!(
            r.contexts.iter().all(|ctx| {
                matches!(&ctx.prefix, Prefix::Entry { id } if id == "vendor_firewall")
            }),
            "expected single vendor_firewall context"
        );
    }

    #[test]
    fn discover_catalog_route_google_sheets_phrase() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![
            ("github".into(), "GitHub".into(), vec![], cgs.clone()),
            (
                "google-sheets".into(),
                "Google Sheets".into(),
                vec![],
                cgs.clone(),
            ),
        ]);
        let q = CapabilityQuery {
            phrases: vec!["sync google sheets rows".into()],
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        assert!(r.candidates.iter().all(|c| c.entry_id == "google-sheets"));
    }

    #[test]
    fn discover_explicit_entry_ids_overrides_catalog_route() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![
            (
                "vendor_firewall".into(),
                "Cloudflare".into(),
                vec![],
                cgs.clone(),
            ),
            ("github".into(), "GitHub".into(), vec![], cgs.clone()),
        ]);
        let q = CapabilityQuery {
            phrases: vec!["Cloudflare DNS records".into()],
            entry_ids: Some(vec!["github".into()]),
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        assert!(r.candidates.iter().all(|c| c.entry_id == "github"));
    }

    #[test]
    fn discover_pick_entry_overrides_catalog_route_inference() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![
            (
                "vendor_firewall".into(),
                "Cloudflare".into(),
                vec![],
                cgs.clone(),
            ),
            ("github".into(), "GitHub".into(), vec![], cgs.clone()),
        ]);
        let q = CapabilityQuery {
            phrases: vec!["Cloudflare zones".into()],
            pick_entry: Some("github".into()),
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        assert!(r.candidates.iter().all(|c| c.entry_id == "github"));
    }

    #[test]
    fn discover_generic_intent_scans_all_catalogs() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![
            ("alpha".into(), "Alpha".into(), vec![], cgs.clone()),
            ("beta".into(), "Beta".into(), vec![], cgs.clone()),
        ]);
        let q = CapabilityQuery {
            phrases: vec!["organisation project profile metadata list".into()],
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        let eids: HashSet<_> = r.candidates.iter().map(|c| c.entry_id.as_str()).collect();
        assert!(
            eids.contains("alpha") && eids.contains("beta"),
            "generic intent should scan every catalog; got {:?}",
            eids
        );
    }

    #[test]
    fn discover_catalog_route_union_when_two_apis_named() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![
            (
                "vendor_firewall".into(),
                "Cloudflare".into(),
                vec![],
                cgs.clone(),
            ),
            ("github".into(), "GitHub".into(), vec![], cgs.clone()),
            ("clickup".into(), "ClickUp".into(), vec![], cgs.clone()),
        ]);
        let q = CapabilityQuery {
            // Schema overlap for scoring (brand tokens are stripped from lexicon scoring).
            tokens: vec!["profile".into()],
            phrases: vec!["Compare Cloudflare WAF with GitHub issue labels".into()],
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        let eids: HashSet<_> = r.candidates.iter().map(|c| c.entry_id.as_str()).collect();
        assert!(
            eids.contains("vendor_firewall")
                && eids.contains("github")
                && !eids.contains("clickup"),
            "expected vendor_firewall+github only; got {:?}",
            eids
        );
    }

    #[test]
    fn pokeapi_domain_yaml_registry_aliases_load() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("pokeapi");
        assert!(
            cgs.registry_aliases.iter().any(|a| a == "pokemon"),
            "pokeapi CGS must ship registry_aliases pokemon: {:?}",
            cgs.registry_aliases
        );
        assert!(
            cgs.registry_aliases.iter().any(|a| a == "poke-api"),
            "pokeapi CGS must ship registry_aliases poke-api: {:?}",
            cgs.registry_aliases
        );
    }

    #[test]
    fn resolve_entry_id_alias_pokemon_to_pokeapi() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        if !dir.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir(&dir).expect("pokeapi");
        cgs.entry_id = Some("pokeapi".into());
        cgs.registry_aliases = vec!["pokemon".into(), "poke-api".into()];
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "pokeapi".into(),
            "PokeAPI".into(),
            vec![],
            Arc::new(cgs),
        )]);
        assert_eq!(
            reg.resolve_entry_id("pokemon", None).expect("resolve"),
            "pokeapi"
        );
    }

    #[test]
    fn discover_compound_pokemon_proof_intent_routes_both_catalogs() {
        let poke_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let proof_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !poke_dir.is_dir() || !proof_dir.is_dir() {
            return;
        }
        let mut poke_cgs = load_schema_dir(&poke_dir).expect("pokeapi");
        poke_cgs.entry_id = Some("pokeapi".into());
        let mut proof_cgs = load_schema_dir(&proof_dir).expect("proof");
        proof_cgs.entry_id = Some("proof".into());
        let reg = InMemoryCgsRegistry::from_pairs(vec![
            (
                "pokeapi".into(),
                "PokeAPI".into(),
                vec![],
                Arc::new(poke_cgs),
            ),
            ("proof".into(), "Proof".into(), vec![], Arc::new(proof_cgs)),
        ]);
        let intent = "research electric type pokemon capabilities and write a proof document with evidenced findings";
        let q = CapabilityQuery {
            tokens: vec![intent.into()],
            ..Default::default()
        };
        let r = reg.discover(&q).expect("discover");
        assert!(
            r.catalog_route.iter().any(|id| id == "pokeapi")
                && r.catalog_route.iter().any(|id| id == "proof"),
            "expected routed pokeapi+proof; got {:?}",
            r.catalog_route
        );
        let eids: HashSet<_> = r.candidates.iter().map(|c| c.entry_id.as_str()).collect();
        assert!(
            eids.contains("pokeapi") && eids.contains("proof"),
            "expected candidates from both catalogs; got {:?}",
            eids
        );
    }

    #[test]
    fn discovery_hints_score_and_surface_in_capability_description() {
        use crate::schema::{
            CapabilityMapping, CapabilityTemplateJson, DiscoveryCapabilityHints,
            DiscoveryEntityHints, EntityDef,
        };
        use crate::{CapabilityName, EntityFieldName, EntityName};

        let mut cgs = CGS::new();
        cgs.entities.insert(
            "Thread".into(),
            EntityDef {
                name: EntityName::from("Thread"),
                description: "Email conversation thread.".into(),
                id_field: EntityFieldName::from("id"),
                id_format: None,
                id_from: None,
                fields: IndexMap::new(),
                relations: IndexMap::new(),
                expression_aliases: vec![],
                implicit_request_identity: false,
                key_vars: vec![],
                abstract_entity: false,
                domain_projection_examples: true,
                primary_read: None,
                discovery: Some(DiscoveryEntityHints {
                    names: vec!["conversation thread".into(), "email thread".into()],
                    qualifier_names: vec![],
                    seed_class: None,
                }),
            },
        );
        let cap = CapabilitySchema {
            name: CapabilityName::from("thread_list"),
            description: "List mailbox threads.".into(),
            kind: CapabilityKind::Query,
            domain: EntityName::from("Thread"),
            mapping: CapabilityMapping {
                template: CapabilityTemplateJson(serde_json::json!({ "method": "GET" })),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            sanitizes: vec![],
            deterministic: None,
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: Some(DiscoveryCapabilityHints {
                operation_terms: vec![],
                target_terms: vec!["conversation thread".into(), "discussion".into()],
            }),
            identity_key: None,
        };
        cgs.capabilities
            .insert(CapabilityName::from("thread_list"), cap.clone());

        let score =
            score_capability_bm25(&cgs, "gmail", &cap, "find conversation thread discussion");
        assert!(
            score > 0,
            "discovery target_terms should contribute to BM25 score"
        );

        let desc = capability_description_for_discovery(&cgs, &cap);
        assert!(
            desc.contains("conversation thread"),
            "assembled description should surface discovery hints; got {desc}"
        );
    }
}
