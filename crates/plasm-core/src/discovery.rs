//! Multi-entry CGS catalog and deterministic capability discovery.

use crate::cgs_context::{CgsContext, Prefix};
use crate::domain_lexicon;
use crate::identity::{CapabilityParamName, EntityFieldName, EntityName};
use crate::schema::{CapabilityKind, CapabilitySchema, InputType, CGS};
use crate::symbol_tuning::{
    build_focus_set_union, ExposureCapabilityKey, ExposureEntityKey, ExposureSlotKey,
    ExposureSurface, ExposureSurfaceDelta,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    /// Sorted list suitable for `POST /execute` `entities` — mirrors the focused DOMAIN slice from `:schema` / `RenderConfig::for_eval_seeds`.
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
    cgs: Arc<CGS>,
}

fn fallback_target_entry_ids(
    query: &CapabilityQuery,
    entries: &IndexMap<String, RegistryRow>,
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
    let mut by_name: IndexMap<String, String> = IndexMap::new();
    for n in neighborhoods {
        let Some(row) = entries.get(&n.entry_id) else {
            continue;
        };
        let cgs = row.cgs.as_ref();
        for name in &n.focused_entities {
            if by_name.contains_key(name) {
                continue;
            }
            let desc = cgs
                .get_entity(name.as_str())
                .map(|e| truncate_discovery_description(&e.description))
                .unwrap_or_default();
            by_name.insert(name.clone(), desc);
        }
    }
    let mut v: Vec<EntitySummary> = by_name
        .into_iter()
        .map(|(name, description)| EntitySummary { name, description })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
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
            map.insert(id.clone(), RegistryRow { label, tags, cgs });
        }
        Self { entries: map }
    }

    /// First catalog entry's CGS in insertion order (YAML / `from_pairs` order).
    ///
    /// Used to bootstrap the CLI and execution engine when `--registry` is given without `--schema`.
    pub fn first_cgs(&self) -> Option<Arc<CGS>> {
        self.entries.first().map(|(_, row)| row.cgs.clone())
    }
}

impl CgsCatalog for InMemoryCgsRegistry {
    fn list_entries(&self) -> Vec<CatalogEntryMeta> {
        self.entries
            .iter()
            .map(|(id, row)| CatalogEntryMeta {
                entry_id: id.clone(),
                label: row.label.clone(),
                tags: row.tags.clone(),
                catalog_cgs_hash: row.cgs.catalog_cgs_hash_hex(),
            })
            .collect()
    }

    fn lookup_entry_meta(&self, entry_id: &str) -> Option<CatalogEntryMeta> {
        self.entries.get(entry_id).map(|row| CatalogEntryMeta {
            entry_id: entry_id.to_string(),
            label: row.label.clone(),
            tags: row.tags.clone(),
            catalog_cgs_hash: row.cgs.catalog_cgs_hash_hex(),
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
    }
    reasons.sort();
    reasons.dedup();
    (total, reasons)
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

        let mut candidates: Vec<RankedCandidate> = Vec::new();

        for (entry_id, row) in &self.entries {
            for cap in row.cgs.capabilities.values() {
                if !cap_passes_filters(query, entry_id, cap) {
                    continue;
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
                    capability_description: truncate_discovery_description(&cap.description),
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
            let fb = fallback_target_entry_ids(query, &self.entries);
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

        Ok(DiscoveryResult {
            contexts,
            candidates,
            ambiguities,
            applied_query_echo: query.clone(),
            closure_stats: Some(closure_stats),
            schema_neighborhoods,
            entity_summaries,
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

/// Minimal intent-filtered DOMAIN surface for MCP `plasm_context` / incremental expand waves.
///
/// - Read capabilities (`query` / `search` / `get`) on each batched entity are always admitted.
/// - Mutating capabilities require a non-zero lexicon overlap score against `intent`.
/// - With the default `ranked_capability_gate` feature, when `ranked_capability_names` is non-empty,
///   mutating capabilities must also appear in that list (typically discovery-ranked picks).
/// - Relations are admitted only when the target entity name appears in `relation_endpoint_names`.
/// - Entity fields are driven by admitted read-capability projections plus `id_field`.
///
/// `entry_id` names the registry row for callers; exposure keys follow [`CGS::entry_id`] when set (see
/// [`crate::symbol_tuning::legacy_exposure_surface_for_entities`]).
pub fn derive_intent_exposure_surface_batch(
    cgs: &CGS,
    _entry_id: &str,
    intent: &str,
    relation_endpoint_names: &[String],
    entity_batch: &[String],
    ranked_capability_names: Option<&[String]>,
) -> ExposureSurfaceDelta {
    let mut surface = ExposureSurface::default();
    let cid = cgs.entry_id.clone().unwrap_or_default();
    let relation_set: HashSet<String> = relation_endpoint_names.iter().cloned().collect();

    let mut query_tokens = HashSet::new();
    for tok in domain_lexicon::tokens(intent) {
        query_tokens.insert(tok);
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
            let include = match cap.kind {
                CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get => true,
                _ => {
                    let (score, _) = score_capability(&query_tokens, cgs, cap);
                    if score == 0 {
                        false
                    } else {
                        #[cfg(feature = "ranked_capability_gate")]
                        {
                            ranked_gate_allows_mutation(ranked_capability_names, cap.name.as_str())
                        }
                        #[cfg(not(feature = "ranked_capability_gate"))]
                        {
                            let _ = ranked_capability_names;
                            true
                        }
                    }
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
            if relation_set.contains(rel.target_resource.as_str()) {
                surface.slots.insert(ExposureSlotKey::Relation {
                    source: ekey.clone(),
                    relation: rname.clone(),
                });
            }
        }
    }

    ExposureSurfaceDelta { required: surface }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::Prefix;
    use std::path::Path;

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
    fn intent_surface_omits_relation_until_relation_target_in_scope() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = vec!["Profile".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "display profiles",
            &endpoints,
            &["Profile".to_string()],
            None,
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
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let mut endpoints = vec!["Profile".to_string(), "RecordedContent".to_string()];
        endpoints.sort_unstable();
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "profile and captured content",
            &endpoints,
            &["Profile".to_string()],
            None,
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
    fn intent_surface_drops_unscored_mutations_on_prompt_run() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = vec!["PromptRun".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "list profiles read metadata only",
            &endpoints,
            &["PromptRun".to_string()],
            None,
        );
        assert!(
            !delta
                .required
                .capabilities
                .iter()
                .any(|c| c.capability.as_str() == "prompt_run_create"),
            "narrow intent should omit PromptRun create when lexicon does not score it"
        );
    }

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_excludes_scored_mutation_not_in_list() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = vec!["PromptRun".to_string()];
        let ranked = vec!["__not_prompt_run_create__".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["PromptRun".to_string()],
            Some(&ranked),
        );
        assert!(
            !delta.required.capabilities.iter().any(|c| {
                c.capability.as_str() == "prompt_run_create"
            }),
            "ranked gate should drop scored mutations absent from the ranked name list"
        );
    }

    #[cfg(feature = "ranked_capability_gate")]
    #[test]
    fn intent_surface_ranked_gate_keeps_mutation_on_list() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("overshow_tools");
        let endpoints = vec!["PromptRun".to_string()];
        let ranked = vec!["prompt_run_create".to_string()];
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "overshow",
            "create and execute a new prompt run",
            &endpoints,
            &["PromptRun".to_string()],
            Some(&ranked),
        );
        assert!(
            delta.required.capabilities.iter().any(|c| {
                c.capability.as_str() == "prompt_run_create"
            }),
            "ranked gate should admit mutations present in the ranked name list when intent scores them"
        );
    }
}
