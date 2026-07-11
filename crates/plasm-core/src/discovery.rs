//! Multi-entry CGS catalog and deterministic capability discovery.

use crate::cgs_context::{CgsContext, Prefix};
use crate::discovery_presentation::CatalogRoute;
use crate::domain_lexicon;
use crate::identity::{CapabilityParamName, EntityFieldName, EntityName};
use crate::schema::{CapabilityKind, CapabilitySchema, EntityDef, InputType, RelationSchema, CGS};
use crate::symbol_tuning::{
    build_focus_set_union, ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey,
    ExposureSurface, ExposureSurfaceDelta,
};
use indexmap::IndexMap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

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
fn resolve_canonical_entity_name(cgs: &CGS, raw: &str) -> Option<String> {
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

/// In-memory catalog + discovery (lexicon-style token scoring, stable sort).
pub struct InMemoryCgsRegistry {
    entries: IndexMap<String, RegistryRow>,
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
        Self { entries: map }
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
        for tok in domain_lexicon::tokens(t) {
            set.insert(tok);
        }
    }
    for p in &query.phrases {
        for tok in domain_lexicon::tokens(p) {
            set.insert(tok);
        }
    }
    set
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
    let mut set = HashSet::new();
    for t in &query.tokens {
        for tok in domain_lexicon::tokens_keep_brands(t) {
            set.insert(tok);
        }
    }
    for p in &query.phrases {
        for tok in domain_lexicon::tokens_keep_brands(p) {
            set.insert(tok);
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
    for tok in domain_lexicon::tokens(text) {
        if query.contains(&tok) {
            score += 1;
            codes.push(format!("token:{tok}"));
        }
    }
    (score, codes)
}

pub(crate) fn score_capability(
    query: &HashSet<String>,
    cgs: &CGS,
    cap: &CapabilitySchema,
) -> (u32, Vec<String>) {
    let mut total = 0u32;
    let mut reasons = Vec::new();
    for (s, mut r) in [
        score_token_hits(query, cap.name.as_str()),
        score_token_hits(query, &cap.description),
        score_token_hits(query, cap.domain.as_str()),
    ] {
        total += s;
        reasons.append(&mut r);
    }
    if let Some(ent) = cgs.entities.get(cap.domain.as_str()) {
        let (s, mut r) = score_token_hits(query, &ent.description);
        total += s;
        reasons.append(&mut r);
        if let Some(h) = &ent.discovery {
            for phrase in &h.names {
                let (s, mut r) = score_token_hits(query, phrase.as_str());
                total += s;
                reasons.append(&mut r);
            }
            for phrase in &h.qualifier_names {
                let (s, mut r) = score_token_hits(query, phrase.as_str());
                total += s;
                reasons.append(&mut r);
            }
        }
    }
    if let Some(h) = &cap.discovery {
        for phrase in &h.operation_terms {
            let (s, mut r) = score_token_hits(query, phrase.as_str());
            total += s;
            reasons.append(&mut r);
        }
        for phrase in &h.target_terms {
            let (s, mut r) = score_token_hits(query, phrase.as_str());
            total += s;
            reasons.append(&mut r);
        }
    }
    reasons.sort();
    reasons.dedup();
    (total, reasons)
}

/// Non-zero when `rel.discovery.qualifier_terms` is empty (always admit) or intent overlaps a term.
fn score_relation_against_intent(query_tokens: &HashSet<String>, rel: &RelationSchema) -> u32 {
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
        for ht in domain_lexicon::tokens(hint) {
            for dt in domain_lexicon::tokens(domain) {
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

        if query_tokens.is_empty() && !has_explicit {
            return Err(DiscoveryError::EmptyQuery);
        }

        let catalog_route = catalog_routes_for_query(query, &self.entries);

        let mut candidates: Vec<RankedCandidate> = Vec::new();

        for (entry_id, row) in &self.entries {
            if let Some(route) = &catalog_route {
                if !route.contains(entry_id) {
                    continue;
                }
            }
            let caps: Vec<&CapabilitySchema> = row.cgs.capabilities.values().collect();
            let entry_candidates: Vec<RankedCandidate> = caps
                .par_iter()
                .filter_map(|cap| {
                    if !cap_passes_filters(query, entry_id, cap) {
                        return None;
                    }
                    let (mut score, mut reasons) = score_capability(&query_tokens, &row.cgs, cap);
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
                    if query_tokens.is_empty() && score == 0 && has_explicit {
                        reasons.push("filter:explicit_only".into());
                    }
                    if score == 0 && !query_tokens.is_empty() {
                        return None;
                    }
                    reasons.sort();
                    reasons.dedup();
                    Some(RankedCandidate {
                        entry_id: entry_id.clone(),
                        entity: cap.domain.to_string(),
                        capability_name: cap.name.to_string(),
                        score,
                        reason_codes: reasons,
                        capability_description: capability_description_for_discovery(&row.cgs, cap),
                    })
                })
                .collect();
            candidates.extend(entry_candidates);
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
}

fn fields_for_admitted_read_cap(
    cgs: &CGS,
    cap: &CapabilitySchema,
    entity_name: &str,
) -> Vec<EntityFieldName> {
    if !cap.provides.is_empty() {
        let Some(ent) = cgs.get_entity(entity_name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for pname in &cap.provides {
            if let Some((fk, _)) = ent
                .fields
                .iter()
                .find(|(k, _)| k.as_str() == pname.as_str())
            {
                out.push(fk.clone());
            }
        }
        return out;
    }
    let Some(ent) = cgs.get_entity(entity_name) else {
        return Vec::new();
    };
    match cap.kind {
        CapabilityKind::Get => {
            let mut out = vec![ent.id_field.clone()];
            for kv in &ent.key_vars {
                if kv.as_str() != ent.id_field.as_str() {
                    out.push(kv.clone());
                }
            }
            out
        }
        CapabilityKind::Query | CapabilityKind::Search => {
            vec![ent.id_field.clone()]
        }
        _ => Vec::new(),
    }
}

#[cfg(feature = "ranked_capability_gate")]
fn ranked_gate_allows_mutation(ranked_capability_names: Option<&[String]>, cap_name: &str) -> bool {
    match ranked_capability_names {
        None | Some([]) => true,
        Some(names) => names.iter().any(|n| n.as_str() == cap_name),
    }
}

/// Options for [`derive_intent_exposure_surface_batch`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExposureSurfaceOptions {
    /// When true (MCP `plasm_context` waves), seeded-entity `create` / `update` / `delete` / `action`
    /// are always admitted on the first wave. Non-seeded relation-target mutators still require
    /// intent lexicon overlap (or an explicit `ranked_capability_names` listing when enabled).
    pub read_first_seeded: bool,
}

pub(crate) fn mutating_capability_admitted(
    score: u32,
    ranked_capability_names: Option<&[String]>,
    cap_name: &str,
) -> bool {
    if score == 0 {
        return false;
    }
    #[cfg(feature = "ranked_capability_gate")]
    {
        ranked_gate_allows_mutation(ranked_capability_names, cap_name)
    }
    #[cfg(not(feature = "ranked_capability_gate"))]
    {
        let _ = ranked_capability_names;
        true
    }
}

/// Capabilities on an explicitly seeded entity that are always admitted (no intent lexicon score).
fn seeded_entity_cap_always_includes(
    read_first_seeded: bool,
    cap: &CapabilitySchema,
    entity_name: &str,
    ent: &EntityDef,
    seeded_entities: &HashSet<String>,
) -> bool {
    if cap.domain.as_str() != entity_name || !seeded_entities.contains(entity_name) {
        return false;
    }
    if matches!(
        cap.kind,
        CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
    ) {
        return true;
    }
    if ent
        .primary_read
        .as_deref()
        .is_some_and(|pr| pr == cap.name.as_str())
    {
        return true;
    }
    read_first_seeded
        && matches!(
            cap.kind,
            CapabilityKind::Create
                | CapabilityKind::Update
                | CapabilityKind::Delete
                | CapabilityKind::Action
        )
}

/// Max outgoing relation hints per entity in discover TSV (`wire→Target`).
pub const DISCOVERY_OUTGOING_RELATIONS_MAX: usize = 3;

/// Compact relation navigation chart for discover TSV (no session-local `r#`).
pub fn outgoing_relation_hints_for_entity(cgs: &CGS, entity: &str, max: usize) -> String {
    let Some(ent) = cgs.get_entity(entity) else {
        return String::new();
    };
    let mut rels: Vec<_> = ent.relations.iter().collect();
    rels.sort_by_key(|(wire, _)| wire.as_str());
    let mut parts = Vec::new();
    for (wire, rel) in rels {
        if parts.len() >= max {
            break;
        }
        if cgs.get_entity(rel.target_resource.as_str()).is_none() {
            continue;
        }
        parts.push(format!("{}→{}", wire, rel.target_resource));
    }
    parts.join("; ")
}

/// Minimal intent-filtered teaching surface for MCP `plasm_context` / incremental expand waves.
///
/// - **Seeded entities** (`entity_batch`): always admit `query` / `search` / `get` on that
///   entity’s domain, plus [`EntityDef::primary_read`] when set. With
///   [`ExposureSurfaceOptions::read_first_seeded`], seeded `create` / `update` / `delete` /
///   `action` are also always admitted on the first wave.
/// - **Non-seeded** read capabilities require a non-zero lexicon overlap score against `intent`.
/// - **Non-seeded** mutating capabilities require a non-zero score; with `ranked_capability_gate`,
///   when `ranked_capability_names` is non-empty they must also appear in that list.
/// - Relations on seeded entities are admitted only when the target appears in
///   `relation_endpoint_names` and relation intent scores > 0.
/// - Mutation closure (1-hop relation targets): create/update/delete/action on targets when intent
///   scores the capability (unchanged).
///
/// `entry_id` names the registry row for callers; exposure keys follow [`CGS::entry_id`] when set (see
/// [`crate::symbol_tuning::legacy_exposure_surface_for_entities`]).
pub fn derive_intent_exposure_surface_batch(
    cgs: &CGS,
    entry_id: &str,
    intent: &str,
    relation_endpoint_keys: &[ExposureEntityKey],
    entity_batch: &[String],
    ranked_capability_names: Option<&[String]>,
    options: ExposureSurfaceOptions,
) -> ExposureSurfaceDelta {
    let mut surface = ExposureSurface::default();
    let cid = if entry_id.is_empty() {
        cgs.entry_id.clone().unwrap_or_default()
    } else {
        entry_id.to_string()
    };
    let relation_set: BTreeSet<ExposureEntityKey> =
        relation_endpoint_keys.iter().cloned().collect();

    let mut query_tokens = HashSet::new();
    for tok in domain_lexicon::tokens(intent) {
        query_tokens.insert(tok);
    }

    let mut seeded_entities = HashSet::new();
    for raw_ent in entity_batch {
        if let Some(canonical) = resolve_canonical_entity_name(cgs, raw_ent) {
            seeded_entities.insert(canonical);
        }
    }

    for raw_ent in entity_batch {
        let Some(canonical) = resolve_canonical_entity_name(cgs, raw_ent) else {
            continue;
        };
        let ename = canonical.as_str();
        let ekey = ExposureEntityKey {
            entry_id: cid.clone(),
            entity: EntityName::from(ename),
        };
        surface.entities.insert(ekey.clone());

        let Some(ent) = cgs.get_entity(ename) else {
            continue;
        };

        surface.slots.insert(ExposureSlotKey::EntityField {
            entity: ekey.clone(),
            field: ent.id_field.clone(),
        });

        let Some(cap_names) = cgs.capability_names_by_domain().get(ename) else {
            continue;
        };
        for cap_name in cap_names {
            let Some(cap) = cgs.capabilities.get(cap_name) else {
                continue;
            };
            let (score, _) = score_capability(&query_tokens, cgs, cap);
            let include = if seeded_entity_cap_always_includes(
                options.read_first_seeded,
                cap,
                ename,
                ent,
                &seeded_entities,
            ) {
                true
            } else {
                match cap.kind {
                    CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => {
                        score > 0
                    }
                    _ => mutating_capability_admitted(
                        score,
                        ranked_capability_names,
                        cap.name.as_str(),
                    ),
                }
            };
            if !include {
                continue;
            }
            let ckey = ExposureCapabilityKey {
                entry_id: cid.clone(),
                domain: EntityName::from(ename),
                capability: cap.name.clone(),
            };
            surface.capabilities.insert(ckey.clone());

            if let Some(is) = &cap.input_schema {
                if let InputType::Object { fields, .. } = &is.input_type {
                    for f in fields {
                        surface.slots.insert(ExposureSlotKey::CapabilityParam {
                            capability: ckey.clone(),
                            param: CapabilityParamName::new(f.name.clone()),
                        });
                    }
                }
            }

            if matches!(
                cap.kind,
                CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
            ) {
                for fk in fields_for_admitted_read_cap(cgs, cap, ename) {
                    surface.slots.insert(ExposureSlotKey::EntityField {
                        entity: ekey.clone(),
                        field: fk,
                    });
                }
            }
        }

        for (rname, rel) in &ent.relations {
            let target_key = ExposureEntityKey {
                entry_id: cid.clone(),
                entity: rel.target_resource.clone(),
            };
            if relation_set.contains(&target_key)
                && score_relation_against_intent(&query_tokens, rel) > 0
            {
                surface.slots.insert(ExposureSlotKey::Relation {
                    source: ekey.clone(),
                    relation: rname.clone(),
                });
            }
        }
    }

    // Mutation closure: 1-hop relation targets may expose mutators when intent scores them.
    for raw_ent in entity_batch {
        let Some(ename) = resolve_canonical_entity_name(cgs, raw_ent) else {
            continue;
        };
        let Some(ent) = cgs.get_entity(ename.as_str()) else {
            continue;
        };
        for rel in ent.relations.values() {
            let target = rel.target_resource.as_str();
            let Some(_target_ent) = cgs.get_entity(target) else {
                continue;
            };
            let tkey = ExposureEntityKey {
                entry_id: cid.clone(),
                entity: EntityName::from(target),
            };
            surface.entities.insert(tkey.clone());
            let Some(cap_names) = cgs.capability_names_by_domain().get(target) else {
                continue;
            };
            for cap_name in cap_names {
                let Some(cap) = cgs.capabilities.get(cap_name) else {
                    continue;
                };
                if !matches!(
                    cap.kind,
                    CapabilityKind::Create
                        | CapabilityKind::Update
                        | CapabilityKind::Delete
                        | CapabilityKind::Action
                ) {
                    continue;
                }
                let (score, _) = score_capability(&query_tokens, cgs, cap);
                if seeded_entities.contains(target) && options.read_first_seeded {
                    continue;
                }
                if !mutating_capability_admitted(score, ranked_capability_names, cap.name.as_str())
                {
                    continue;
                }
                let ckey = ExposureCapabilityKey {
                    entry_id: cid.clone(),
                    domain: EntityName::from(target),
                    capability: cap.name.clone(),
                };
                surface.capabilities.insert(ckey);
            }
        }
    }

    ExposureSurfaceDelta { required: surface }
}

/// Mutating capability wire names on **non-seeded** relation targets that intent qualifies but
/// are absent from `on_surface` (entry_id, domain entity, capability wire triples).
pub fn relation_target_deferred_mutator_wires(
    cgs: &CGS,
    entry_id: &str,
    intent: &str,
    seeded_entities: &[String],
    on_surface: &HashSet<(String, String, String)>,
    ranked_capability_names: Option<&[String]>,
) -> Vec<String> {
    use std::collections::BTreeSet;

    let cid = if entry_id.is_empty() {
        cgs.entry_id.clone().unwrap_or_default()
    } else {
        entry_id.to_string()
    };
    let mut query_tokens = HashSet::new();
    for tok in domain_lexicon::tokens(intent) {
        query_tokens.insert(tok);
    }
    let seeded: HashSet<String> = seeded_entities.iter().cloned().collect();
    let mut deferred = BTreeSet::new();
    for raw_ent in seeded_entities {
        let Some(ent) = cgs.get_entity(raw_ent.as_str()) else {
            continue;
        };
        for rel in ent.relations.values() {
            let target = rel.target_resource.as_str();
            if seeded.contains(target) {
                continue;
            }
            let Some(cap_names) = cgs.capability_names_by_domain().get(target) else {
                continue;
            };
            for cap_name in cap_names {
                let Some(cap) = cgs.capabilities.get(cap_name) else {
                    continue;
                };
                if !matches!(
                    cap.kind,
                    CapabilityKind::Create
                        | CapabilityKind::Update
                        | CapabilityKind::Delete
                        | CapabilityKind::Action
                ) {
                    continue;
                }
                let (score, _) = score_capability(&query_tokens, cgs, cap);
                if !mutating_capability_admitted(score, ranked_capability_names, cap.name.as_str())
                {
                    continue;
                }
                let trip = (cid.clone(), target.to_string(), cap.name.to_string());
                if on_surface.contains(&trip) {
                    continue;
                }
                deferred.insert(cap.name.to_string());
            }
        }
    }
    deferred.into_iter().collect()
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

    fn relation_keys(entry_id: &str, names: &[&str]) -> Vec<ExposureEntityKey> {
        names
            .iter()
            .map(|n| ExposureEntityKey {
                entry_id: entry_id.to_string(),
                entity: EntityName::from(*n),
            })
            .collect()
    }

    #[test]
    fn intent_surface_omits_relation_until_relation_target_in_scope() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["Profile"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "display profiles",
            &endpoints,
            &["Profile".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !delta.required.slots.iter().any(|s| matches!(
                s,
                ExposureSlotKey::Relation { relation, .. }
                    if relation.as_str() == "recorded_matches"
            )),
            "Profile.recorded_matches targets RecordedContent; omit until that entity is in scope"
        );
    }

    #[test]
    fn intent_surface_includes_profile_relation_when_recorded_content_in_scope() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["Profile", "RecordedContent"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "profile and captured content",
            &endpoints,
            &["Profile".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta.required.slots.iter().any(|s| matches!(
                s,
                ExposureSlotKey::Relation { relation, .. }
                    if relation.as_str() == "recorded_matches"
            )),
            "expected recorded_matches when RecordedContent is an allowed relation endpoint"
        );
    }

    #[test]
    fn mutating_capability_admitted_requires_nonzero_score() {
        assert!(!mutating_capability_admitted(0, None, "langitem_create"));
        assert!(mutating_capability_admitted(1, None, "langitem_create"));
    }

    #[test]
    fn intent_surface_ranked_admits_seeded_mutator_at_zero_score() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
        let cap = cgs
            .capabilities
            .get("langitem_create")
            .expect("langitem_create");
        let zero_intent = "xyzzy qwerty plugh unrelated";
        let mut zero_tokens = HashSet::new();
        for tok in domain_lexicon::tokens(zero_intent) {
            zero_tokens.insert(tok);
        }
        let (zero_score, _) = score_capability(&zero_tokens, &cgs, cap);
        assert_eq!(
            zero_score, 0,
            "fixture intent must score zero for langitem_create"
        );
        let endpoints = relation_keys("matrix", &["LangItem"]);
        let delta_ranked = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            zero_intent,
            &endpoints,
            &["LangItem".to_string()],
            Some(&["langitem_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        assert!(
            delta_ranked
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "ranked wire name must admit seeded mutator at score zero"
        );
    }

    #[test]
    fn intent_surface_read_first_admits_seeded_mutators_on_first_wave() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
        let cap = cgs
            .capabilities
            .get("langitem_create")
            .expect("langitem_create");
        let weak_intent = "langitem browse inventory metadata";
        let strong_intent = "create new langitem title";
        let mut weak_tokens = HashSet::new();
        for tok in domain_lexicon::tokens(weak_intent) {
            weak_tokens.insert(tok);
        }
        let mut strong_tokens = HashSet::new();
        for tok in domain_lexicon::tokens(strong_intent) {
            strong_tokens.insert(tok);
        }
        let (weak_score, _) = score_capability(&weak_tokens, &cgs, cap);
        let (strong_score, _) = score_capability(&strong_tokens, &cgs, cap);
        assert!(
            weak_score > 0,
            "fixture intent should score weakly: {weak_score}"
        );
        assert!(
            strong_score >= weak_score,
            "strong intent should score at least as high as weak: {strong_score} vs {weak_score}"
        );
        let endpoints = relation_keys("matrix", &["LangItem"]);
        let delta_weak = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            weak_intent,
            &endpoints,
            &["LangItem".to_string()],
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        assert!(
            delta_weak
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "read-first should admit seeded mutators even at weak intent score"
        );
        let delta_strong = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            strong_intent,
            &endpoints,
            &["LangItem".to_string()],
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        assert!(
            delta_strong
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "read-first should admit strong-scored seeded mutator"
        );
        let delta_ranked = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            weak_intent,
            &endpoints,
            &["LangItem".to_string()],
            Some(&["langitem_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        assert!(
            delta_ranked
                .required
                .capabilities
                .iter()
                .any(|c| { c.capability.as_str() == "langitem_create" }),
            "read-first should admit ranked mutator wire name"
        );
    }

    #[test]
    fn intent_surface_seeded_prompt_run_create_requires_intent_overlap() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["PromptRun"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "list profiles read metadata only",
            &endpoints,
            &["PromptRun".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !delta
                .required
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "prompt_run_create"),
            "seeded PromptRun create must require intent overlap"
        );
        let delta_create = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["PromptRun".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta_create
                .required
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "prompt_run_create"),
            "seeded PromptRun create should appear when intent scores it"
        );
    }

    #[test]
    fn intent_surface_drops_unscored_reads_when_intent_targets_other_entity() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["Meeting", "Profile"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "organisation project profile metadata list",
            &endpoints,
            &["Profile".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta.required.capabilities.iter().any(|c| {
                c.domain.as_str() == "Profile"
                    && matches!(c.capability.as_str(), "profile_query" | "profile_get")
            }),
            "expected Profile query/get to remain when intent lexicon scores profile vocabulary"
        );
        assert!(
            !delta.required.capabilities.iter().any(|c| {
                c.domain.as_str() == "Meeting"
                    && matches!(c.capability.as_str(), "meeting_query" | "meeting_get")
            }),
            "Meeting reads should be omitted when intent does not score meeting vocabulary"
        );
    }

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_excludes_non_seeded_scored_mutation() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["PromptRun", "Profile"]);
        let ranked = vec!["prompt_run_create".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["Profile".to_string()],
            Some(&ranked),
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !surface_has_capability(&delta, "PromptRun", "prompt_run_create"),
            "PromptRun create must stay off surface when PromptRun is not seeded (ranked list alone does not add caps)"
        );
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

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_keeps_mutation_on_list() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = relation_keys("overshow", &["PromptRun"]);
        let ranked = vec!["prompt_run_create".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["PromptRun".to_string()],
            Some(&ranked),
            ExposureSurfaceOptions::default(),
        );
        assert!(
            delta.required.capabilities.iter().any(|c| {
                c.capability.as_str() == "prompt_run_create"
            }),
            "ranked gate should admit mutations present in the ranked name list when intent scores them"
        );
    }

    const FEDERATED_FIELD_LAB_INTENT: &str =
        "Federated field lab v2 — pokeapi specimen linear missions proof dossier";

    fn surface_has_capability(
        delta: &ExposureSurfaceDelta,
        domain: &str,
        capability: &str,
    ) -> bool {
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.domain.as_str() == domain && c.capability.as_str() == capability)
    }

    #[test]
    fn intent_surface_seeded_sharelink_create_requires_intent_overlap() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir(&dir).expect("proof");
        cgs.entry_id = Some("proof".into());
        let endpoints = relation_keys("proof", &["ShareLink"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            FEDERATED_FIELD_LAB_INTENT,
            &endpoints,
            &["ShareLink".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !surface_has_capability(&delta, "ShareLink", "share_link_create"),
            "seeded create must require intent overlap when intent omits share/link/create tokens"
        );
        let delta_create = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            "create share link for proof dossier",
            &endpoints,
            &["ShareLink".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            surface_has_capability(&delta_create, "ShareLink", "share_link_create"),
            "seeded create should appear when intent scores the mutation"
        );
    }

    #[test]
    fn intent_surface_seeded_sharelink_create_with_intent_lexicon_match() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir(&dir).expect("proof");
        cgs.entry_id = Some("proof".into());
        let endpoints = relation_keys("proof", &["ShareLink"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            "create share link for proof dossier",
            &endpoints,
            &["ShareLink".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            surface_has_capability(&delta, "ShareLink", "share_link_create"),
            "seeded ShareLink must expose share_link_create when intent scores create"
        );
        let session = crate::symbol_tuning::TeachingExposureSession::new_with_intent_delta(
            &cgs,
            "proof",
            &["ShareLink"],
            delta,
        );
        let map = session.to_symbol_map();
        let m = map.method_sym_for("proof", "ShareLink", "share_link_create");
        assert!(
            m.starts_with('m') && m.len() > 1,
            "seeded share_link_create must receive an m# (got {m:?}) for federated lab plasm programs"
        );
    }

    #[test]
    fn intent_surface_seeded_pokemon_reads_without_intent_lexicon_match() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("pokeapi");
        let endpoints = relation_keys("pokeapi", &["Pokemon"]);
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "pokeapi",
            FEDERATED_FIELD_LAB_INTENT,
            &endpoints,
            &["Pokemon".to_string()],
            None,
            ExposureSurfaceOptions::default(),
        );
        assert!(
            surface_has_capability(&delta, "Pokemon", "pokemon_query"),
            "seeded Pokemon must expose pokemon_query"
        );
        assert!(
            surface_has_capability(&delta, "Pokemon", "pokemon_get"),
            "seeded Pokemon must expose pokemon_get"
        );
    }

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_excludes_seeded_create_when_not_ranked() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("proof");
        let endpoints = relation_keys("proof", &["ShareLink"]);
        let ranked = vec!["__not_share_link_create__".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "proof",
            "create share link for proof dossier",
            &endpoints,
            &["ShareLink".to_string()],
            Some(&ranked),
            ExposureSurfaceOptions::default(),
        );
        assert!(
            !surface_has_capability(&delta, "ShareLink", "share_link_create"),
            "ranked gate excludes seeded-entity mutations not present in ranked_capabilities"
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
        use crate::{CapabilityName, EntityName};

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

        let mut query_tokens = HashSet::new();
        for tok in domain_lexicon::tokens("find conversation thread discussion") {
            query_tokens.insert(tok);
        }
        let (score, _) = score_capability(&query_tokens, &cgs, &cap);
        assert!(
            score > 0,
            "discovery target_terms should contribute to lexicon score"
        );

        let desc = capability_description_for_discovery(&cgs, &cap);
        assert!(
            desc.contains("conversation thread"),
            "assembled description should surface discovery hints; got {desc}"
        );
    }
}
