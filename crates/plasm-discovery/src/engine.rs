//! Typed decomposition, scoring, gating, and [`crate::AgentDiscovery`] implementation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use inflection::{plural, singular};
use plasm_core::schema::{CapabilityKind, CGS};
use tracing::{debug_span, info_span, Instrument};

use crate::decompose::{decompose, tokenize};
use crate::embedder::{cosine_sim, BlockingEmbedder};
use crate::index::{qualifier_supported, CatalogIndex, PhraseHit, PhraseSource};
use crate::metrics;
use crate::types::{
    evidence_codes, ClarificationAnswer, ClarificationDimension, ClarificationOption,
    ClarificationPrompt, ClarificationState, DiscoveryDecision, DiscoveryError, DiscoveryEvidence,
    DiscoveryQuery, ReadyTarget, TargetHypothesis,
};

/// Score bump when an utterance token matches a CGS relation wire name on some hypothesis entity,
/// boosting that relation's target entity (only if the target is already a lexical hypothesis).
/// Large enough to overcome typical entity-name-token bonuses on a parent entity when relation
/// vocabulary (e.g. `comments`) is present in the utterance.
const RELATION_INTENT_SCORE_BONUS: f64 = 55.0;

fn token_matches_inflected_noun(token_lower: &str, label_lower: &str) -> bool {
    if token_lower == label_lower {
        return true;
    }
    let l_pl = plural::<_, String>(label_lower);
    let l_sg = singular::<_, String>(label_lower);
    if token_lower == l_pl.as_str() || token_lower == l_sg.as_str() {
        return true;
    }
    let t_sg = singular::<_, String>(token_lower);
    let t_pl = plural::<_, String>(token_lower);
    t_sg.as_str() == label_lower || t_pl.as_str() == label_lower
}

fn relation_wire_name_matches_tokens(wire_name: &str, tokens: &[String]) -> bool {
    let parts: Vec<&str> = wire_name.split('_').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    // Every segment must match: avoids `issue_types` firing on utterances that only mention
    // `issues` (Linear/GitHub-style pivots).
    parts.iter().all(|part| {
        let label = part.to_lowercase();
        tokens
            .iter()
            .any(|t| token_matches_inflected_noun(t.as_str(), &label))
    })
}

/// Same rule as the entity `+25` bonus: whole-token entity name (ASCII alnum split), plus simple plural forms.
fn utterance_has_entity_name_token(entity: &str, utterance_lower: &str) -> bool {
    let en = entity.to_lowercase();
    for t in utterance_lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if t == en {
            return true;
        }
        if !en.ends_with('s') && t == format!("{en}s") {
            return true;
        }
        if en.ends_with('e') && t == format!("{}s", en) {
            return true;
        }
    }
    false
}

fn apply_relation_intent_boosts(
    hypotheses: &mut [TargetHypothesis],
    discovery: &TypedDiscovery,
    utterance_tokens: &[String],
    utterance_lower: &str,
) {
    let present: HashSet<(String, String)> = hypotheses
        .iter()
        .map(|h| (h.entry_id.clone(), h.entity.clone()))
        .collect();

    #[derive(Default)]
    struct Acc {
        bonus: f64,
        details: Vec<String>,
    }
    let mut boost_by_target: HashMap<(String, String), Acc> = HashMap::new();

    for h in hypotheses.iter() {
        let Some(cgs) = discovery.cgs_for_entry(&h.entry_id) else {
            continue;
        };
        let Some(ent) = cgs.entities.get(h.entity.as_str()) else {
            continue;
        };
        for rel in ent.relations.values() {
            if !relation_wire_name_matches_tokens(rel.name.as_str(), utterance_tokens) {
                continue;
            }
            let target = rel.target_resource.to_string();
            let key = (h.entry_id.clone(), target);
            if present.contains(&key) {
                let acc = boost_by_target.entry(key).or_default();
                acc.bonus += RELATION_INTENT_SCORE_BONUS;
                acc.details
                    .push(format!("{}.{}", h.entity, rel.name.as_str()));
            }
        }
    }

    for h in hypotheses.iter_mut() {
        let k = (h.entry_id.clone(), h.entity.clone());
        let Some(acc) = boost_by_target.get(&k) else {
            continue;
        };
        if acc.bonus <= 0.0 {
            continue;
        }
        // Scope pivots ("for a team") already surface the target entity via the normal entity-token
        // bonus; relation-name boosts are for reaching *nested* rows (e.g. comments) whose wire name
        // appears but whose entity slug does not.
        if utterance_has_entity_name_token(&h.entity, utterance_lower) {
            continue;
        }
        h.score += acc.bonus;
        h.evidence.push(DiscoveryEvidence::new(
            evidence_codes::RELATION_INTENT,
            acc.details.join("; "),
        ));
    }
}

/// Async typed discovery over one or more loaded CGS graphs.
pub struct TypedDiscovery {
    indexes: Vec<CatalogIndex>,
    embedder: Option<Arc<BlockingEmbedder>>,
    /// Max clarification / ready options per response.
    pub max_options: usize,
}

impl TypedDiscovery {
    pub fn from_cgs_entries(entries: Vec<(String, Arc<CGS>)>, enable_embeddings: bool) -> Self {
        let t0 = Instant::now();
        let mut indexes = Vec::new();
        let mut total_ent = 0i64;
        let mut total_cap = 0i64;
        for (eid, cgs) in entries {
            let idx = CatalogIndex::build(eid, cgs);
            total_ent += idx.entity_count() as i64;
            total_cap += idx.capability_count() as i64;
            indexes.push(idx);
        }
        metrics::record_index_build("success", t0.elapsed());
        metrics::record_index_sizes(total_ent, total_cap);

        let embedder = if enable_embeddings {
            Some(Arc::new(BlockingEmbedder::new(
                fastembed::EmbeddingModel::AllMiniLML6V2,
                2,
            )))
        } else {
            None
        };

        Self {
            indexes,
            embedder,
            max_options: 8,
        }
    }

    pub fn with_max_options(mut self, n: usize) -> Self {
        self.max_options = n.clamp(1, 32);
        self
    }

    fn catalog_entry_ids(&self) -> Vec<String> {
        self.indexes.iter().map(|i| i.entry_id.clone()).collect()
    }

    fn cgs_for_entry<'a>(&'a self, entry_id: &str) -> Option<&'a CGS> {
        self.indexes
            .iter()
            .find(|i| i.entry_id == entry_id)
            .map(|i| i.cgs.as_ref())
    }

    fn score_hit(hit: &PhraseHit) -> f64 {
        let base = match hit.source {
            PhraseSource::DiscoveryName => 100.0,
            PhraseSource::EntityName => 90.0,
            PhraseSource::ExpressionAlias => 85.0,
            PhraseSource::CapabilityTargetTerm => 70.0,
        };
        base + hit.phrase.len() as f64 * 0.05
    }

    fn pick_capability_for_entity(
        cgs: &CGS,
        entity: &str,
        verbs: &[String],
    ) -> Option<(String, CapabilityKind)> {
        let mut caps: Vec<_> = cgs
            .capabilities
            .iter()
            .filter(|(_, c)| c.domain.as_str() == entity)
            .map(|(n, c)| (n.to_string(), c.kind, c))
            .collect();
        caps.sort_by(|a, b| a.0.cmp(&b.0));

        let wants_search = verbs.iter().any(|v| v == "search");
        let wants_get = verbs
            .iter()
            .any(|v| ["get", "fetch", "open"].contains(&v.as_str()));
        let wants_query = verbs
            .iter()
            .any(|v| ["list", "query", "show", "find", "pull"].contains(&v.as_str()));

        if wants_search {
            if let Some((n, k, _)) = caps
                .iter()
                .find(|(_, k, _)| matches!(k, CapabilityKind::Search))
            {
                return Some((n.clone(), *k));
            }
        }
        if wants_get {
            if let Some((n, k, _)) = caps
                .iter()
                .find(|(_, k, _)| matches!(k, CapabilityKind::Get))
            {
                return Some((n.clone(), *k));
            }
        }
        if wants_query {
            if let Some((n, k, _)) = caps
                .iter()
                .find(|(_, k, _)| matches!(k, CapabilityKind::Query))
            {
                return Some((n.clone(), *k));
            }
        }

        caps.first().map(|(n, k, _)| (n.clone(), *k))
    }

    fn evidence_for_hit(hit: &PhraseHit) -> DiscoveryEvidence {
        let (code, detail) = match hit.source {
            PhraseSource::DiscoveryName => (evidence_codes::DISCOVERY_NAME, hit.phrase.clone()),
            PhraseSource::EntityName => (evidence_codes::ENTITY_NAME, hit.entity.clone()),
            PhraseSource::ExpressionAlias => (evidence_codes::EXPRESSION_ALIAS, hit.phrase.clone()),
            PhraseSource::CapabilityTargetTerm => {
                (evidence_codes::CAP_TARGET_TERM, hit.phrase.clone())
            }
        };
        DiscoveryEvidence::new(code, detail)
    }

    async fn discover_inner(
        &self,
        query: &DiscoveryQuery,
    ) -> Result<DiscoveryDecision, DiscoveryError> {
        let utterance = query.utterance.trim();
        if utterance.is_empty() {
            metrics::record_request_outcome("error");
            return Err(DiscoveryError::EmptyUtterance);
        }

        let span = info_span!(
            "plasm.discovery.discover",
            model_id = self
                .embedder
                .as_ref()
                .map(|e| e.model_id())
                .unwrap_or("none"),
            entry_count = self.indexes.len() as i64,
        );
        async { self.discover_inner_body(query, utterance).await }
            .instrument(span)
            .await
    }

    async fn discover_inner_body(
        &self,
        query: &DiscoveryQuery,
        utterance: &str,
    ) -> Result<DiscoveryDecision, DiscoveryError> {
        let t_dec0 = Instant::now();
        let catalog_ids = self.catalog_entry_ids();
        for eid in &query.allowed_entry_ids {
            if !catalog_ids.iter().any(|c| c == eid) {
                metrics::record_request_outcome("error");
                return Err(DiscoveryError::UnknownEntry(eid.clone()));
            }
        }

        let decomposed = {
            let _g = debug_span!("plasm.discovery.decompose_intent").entered();
            decompose(utterance, &catalog_ids)
        };
        metrics::record_intent_decompose_duration(t_dec0.elapsed());

        let lower = utterance.to_lowercase();
        let ut_tokens = tokenize(&lower);

        let mut hits: Vec<PhraseHit> = Vec::new();
        for idx in &self.indexes {
            if !query.allowed_entry_ids.is_empty()
                && !query.allowed_entry_ids.iter().any(|e| e == &idx.entry_id)
            {
                continue;
            }
            if let Some(ref fe) = query.force_entry_id {
                if fe != &idx.entry_id {
                    continue;
                }
            }
            hits.extend(idx.scan_utterance(&lower));
            for p in &decomposed.noun_phrases {
                hits.extend(idx.lookup_phrase(p));
            }
        }

        let mut seen = HashSet::new();
        hits.retain(|h| {
            let k = (
                h.entry_id.clone(),
                h.entity.clone(),
                h.phrase.clone(),
                format!("{:?}", h.source),
            );
            seen.insert(k)
        });

        let mut hypotheses: Vec<TargetHypothesis> = Vec::new();
        for h in hits {
            let Some(idx) = self.indexes.iter().find(|i| i.entry_id == h.entry_id) else {
                continue;
            };
            if let Some(ref fe) = query.force_entity {
                if fe.to_lowercase() != h.entity.to_lowercase() {
                    continue;
                }
            }
            let Some((cap_name, cap_kind)) =
                Self::pick_capability_for_entity(&idx.cgs, &h.entity, &decomposed.operation_verbs)
            else {
                continue;
            };
            let mut score = Self::score_hit(&h);
            // Prefer the entity whose *name* appears as a tokenizer word (not a substring), plus
            // common English plurals, over description-only hits (e.g. Linear `Issue` vs `Comment`).
            let ent_token_hit = utterance_has_entity_name_token(&h.entity, &lower);
            if ent_token_hit {
                score += 25.0;
            }
            score += crate::index::camel_entity_phrase_substring_bonus(&h.entity, &lower);
            let camel_seg = crate::index::camel_entity_segment_token_bonus(&h.entity, &ut_tokens);
            if camel_seg > 0.0 {
                score += camel_seg;
            }
            let mut ev = vec![Self::evidence_for_hit(&h)];
            if camel_seg > 0.0 {
                ev.push(DiscoveryEvidence::new(
                    evidence_codes::CAMEL_SEGMENT_CONJUNCTION,
                    h.entity.clone(),
                ));
            }
            hypotheses.push(TargetHypothesis {
                entry_id: h.entry_id.clone(),
                entity: h.entity.clone(),
                capability_name: cap_name,
                capability_kind: cap_kind,
                score,
                matched_phrase: h.phrase.clone(),
                qualifiers: Vec::new(),
                evidence: ev,
            });
        }

        // One hypothesis per (catalog, entity): multiple capabilities for the same entity
        // share one lexical hit — keeping all of them floods clarification lists and hides
        // distinct entities (e.g. GitHub `Label` vs `Issue`).
        let mut best: HashMap<(String, String), TargetHypothesis> = HashMap::new();
        for hyp in hypotheses {
            let k = (hyp.entry_id.clone(), hyp.entity.clone());
            best.entry(k)
                .and_modify(|e| {
                    if hyp.score > e.score {
                        *e = hyp.clone();
                    }
                })
                .or_insert(hyp);
        }
        let mut hypotheses: Vec<TargetHypothesis> = best.into_values().collect();
        apply_relation_intent_boosts(&mut hypotheses, self, &ut_tokens, &lower);
        hypotheses.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let (true, false, Some(embedder)) = (
            query.enable_embeddings,
            hypotheses.is_empty(),
            self.embedder.as_ref().cloned(),
        ) {
            let t_embed = Instant::now();
            metrics::record_embed_cache("miss");
            let lines: Vec<String> = hypotheses
                .iter()
                .map(|h| format!("{} {} {}", h.entry_id, h.entity, h.matched_phrase))
                .collect();
            let mut all_texts = vec![utterance.to_string()];
            all_texts.extend(lines.iter().cloned());
            let t_batch = Instant::now();
            match embedder.embed_batch(all_texts).await {
                Ok(vecs) if vecs.len() == hypotheses.len() + 1 => {
                    let qv = &vecs[0];
                    for (i, h) in hypotheses.iter_mut().enumerate() {
                        let sim = cosine_sim(qv, &vecs[i + 1]);
                        h.score += 30.0 * sim;
                        if sim > 0.01 {
                            h.evidence.push(DiscoveryEvidence::new(
                                evidence_codes::EMBEDDING_SIM,
                                format!("{sim:.4}"),
                            ));
                        }
                    }
                    metrics::record_embed_batch_duration(t_batch.elapsed());
                }
                Ok(_) => metrics::record_embed_cache("error"),
                Err(e) => {
                    metrics::record_embed_cache("error");
                    tracing::warn!(error = %e, "typed discovery embedding failed; continuing lexical-only");
                }
            }
            let _ = t_embed;
        }

        hypotheses.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let max_h = self.max_options.min(query.max_options).max(1);
        hypotheses.truncate(max_h);
        metrics::record_hypothesis_count(hypotheses.len() as u64);

        if decomposed.api_hints.len() == 1 {
            let only = decomposed.api_hints[0].clone();
            hypotheses.retain(|h| h.entry_id == only);
        }

        if hypotheses.is_empty() {
            metrics::record_option_count(0);
            metrics::record_request_outcome("no_match");
            return Ok(DiscoveryDecision::NoMatch {
                evidence: vec![DiscoveryEvidence::new(
                    "no_lexical_hit",
                    "no phrase matched catalog vocabulary after filters",
                )],
            });
        }

        let t_graph = Instant::now();
        {
            let _g = debug_span!("plasm.discovery.validate_qualifiers").entered();
            let qualifiers_candidates: Vec<String> = if let Some(top) = hypotheses.first() {
                decomposed
                    .noun_phrases
                    .iter()
                    .filter(|p| {
                        !top.matched_phrase.eq_ignore_ascii_case(p)
                            && !top.entity.eq_ignore_ascii_case(p)
                    })
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            for h in &mut hypotheses {
                let Some(cgs) = self.cgs_for_entry(&h.entry_id) else {
                    continue;
                };
                let mut qv = Vec::new();
                for q in &qualifiers_candidates {
                    if q.len() < 2 {
                        continue;
                    }
                    if qualifier_supported(cgs, &h.entity, q) {
                        qv.push(q.clone());
                        h.evidence.push(DiscoveryEvidence::new(
                            evidence_codes::GRAPH_QUALIFIER_OK,
                            format!("{} supports '{}'", h.entity, q),
                        ));
                    }
                }
                h.qualifiers = qv;
            }
        }
        metrics::record_graph_validate_duration(t_graph.elapsed());

        let t_gate = Instant::now();
        let _g = debug_span!("plasm.discovery.gate_decision").entered();
        let cap = self.max_options.min(query.max_options).max(1);

        // Clarify API: same matched phrase, multiple catalogs, no API hint.
        if decomposed.api_hints.is_empty() && hypotheses.len() > 1 {
            let phrase0 = hypotheses[0].matched_phrase.to_lowercase();
            let same_phrase: Vec<_> = hypotheses
                .iter()
                .filter(|h| h.matched_phrase.to_lowercase() == phrase0)
                .collect();
            let mut eids: HashSet<String> = HashSet::new();
            for h in &same_phrase {
                eids.insert(h.entry_id.clone());
            }
            if eids.len() > 1 {
                let opts: Vec<ClarificationOption> = eids
                    .into_iter()
                    .take(cap)
                    .map(|eid| ClarificationOption {
                        label: format!("catalog `{eid}`"),
                        entry_id: Some(eid),
                        entity: None,
                        qualifier: None,
                    })
                    .collect();
                metrics::record_option_count(opts.len() as u64);
                metrics::record_clarification("api");
                metrics::record_request_outcome("clarify_api");
                metrics::record_decision_duration(t_gate.elapsed());
                return Ok(DiscoveryDecision::ClarifyApi {
                    prompt: ClarificationPrompt {
                        dimension: ClarificationDimension::Api,
                        prompt:
                            "Multiple catalogs match this phrase. Which integration should we use?"
                                .into(),
                        options: opts,
                        evidence: vec![],
                    },
                });
            }
        }

        // Clarify entity: top two close scores, different entities.
        if hypotheses.len() > 1 {
            let a = &hypotheses[0];
            let b = &hypotheses[1];
            if (a.score - b.score).abs() < 8.0 && a.entity != b.entity {
                let opts: Vec<ClarificationOption> = hypotheses
                    .iter()
                    .take(cap)
                    .map(|h| ClarificationOption {
                        label: format!("{} — {}", h.entry_id, h.entity),
                        entry_id: Some(h.entry_id.clone()),
                        entity: Some(h.entity.clone()),
                        qualifier: None,
                    })
                    .collect();
                metrics::record_option_count(opts.len() as u64);
                metrics::record_clarification("entity");
                metrics::record_request_outcome("clarify_entity");
                metrics::record_decision_duration(t_gate.elapsed());
                return Ok(DiscoveryDecision::ClarifyEntity {
                    prompt: ClarificationPrompt {
                        dimension: ClarificationDimension::Entity,
                        prompt: "Several resource types match. Which one did you mean?".into(),
                        options: opts,
                        evidence: vec![],
                    },
                });
            }
        }

        // Qualifier clarification if graph rejected extra noun phrases.
        let top = hypotheses.first().unwrap();
        let qualifiers: Vec<String> = decomposed
            .noun_phrases
            .iter()
            .filter(|p| {
                !top.matched_phrase.eq_ignore_ascii_case(p) && !top.entity.eq_ignore_ascii_case(p)
            })
            .cloned()
            .collect();
        let cgs = self.cgs_for_entry(&top.entry_id).unwrap();
        let bad_q: Vec<String> = qualifiers
            .into_iter()
            .filter(|q| q.len() >= 2 && !qualifier_supported(cgs, &top.entity, q))
            .take(3)
            .collect();
        if !bad_q.is_empty() {
            let mut opts = Vec::new();
            if let Some(ent) = cgs.entities.get(top.entity.as_str()) {
                for rel in ent.relations.values() {
                    let tgt = rel.target_resource.to_string();
                    if qualifier_supported(cgs, &tgt, &bad_q[0]) {
                        opts.push(ClarificationOption {
                            label: format!("{tgt} (via relation {})", rel.name.as_str()),
                            entry_id: Some(top.entry_id.clone()),
                            entity: Some(tgt),
                            qualifier: Some(bad_q[0].clone()),
                        });
                    }
                }
            }
            opts.push(ClarificationOption {
                label: "Ignore unsupported qualifiers".into(),
                entry_id: Some(top.entry_id.clone()),
                entity: Some(top.entity.clone()),
                qualifier: None,
            });
            opts.truncate(cap);
            if opts.len() > 1 {
                metrics::record_option_count(opts.len() as u64);
                metrics::record_clarification("qualifier");
                metrics::record_request_outcome("clarify_qualifier");
                metrics::record_decision_duration(t_gate.elapsed());
                return Ok(DiscoveryDecision::ClarifyQualifier {
                    prompt: ClarificationPrompt {
                        dimension: ClarificationDimension::Qualifier,
                        prompt: format!(
                            "The phrase {:?} is not clearly attached to `{}` on catalog `{}`. Pick a scope.",
                            bad_q, top.entity, top.entry_id
                        ),
                        options: opts,
                        evidence: vec![DiscoveryEvidence::new(
                            evidence_codes::GRAPH_QUALIFIER_BAD,
                            bad_q.join(", "),
                        )],
                    },
                });
            }
        }

        let winner = hypotheses.into_iter().next().unwrap();
        metrics::record_option_count(1);
        metrics::record_request_outcome("ready");
        metrics::record_decision_duration(t_gate.elapsed());
        Ok(DiscoveryDecision::Ready {
            target: ReadyTarget {
                entry_id: winner.entry_id,
                entity: winner.entity,
                capability_name: winner.capability_name,
                capability_kind: winner.capability_kind,
                score: winner.score,
                matched_phrase: winner.matched_phrase,
                qualifiers: winner.qualifiers,
                evidence: winner.evidence,
            },
        })
    }
}

#[async_trait]
impl crate::AgentDiscovery for TypedDiscovery {
    async fn discover(&self, query: DiscoveryQuery) -> Result<DiscoveryDecision, DiscoveryError> {
        self.discover_inner(&query).await
    }

    async fn answer_clarification(
        &self,
        state: ClarificationState,
        answer: ClarificationAnswer,
    ) -> Result<DiscoveryDecision, DiscoveryError> {
        let opt = state
            .options
            .get(answer.selected_index)
            .ok_or(DiscoveryError::InvalidClarificationAnswer)?;
        let mut q = DiscoveryQuery {
            utterance: state.utterance.clone(),
            allowed_entry_ids: state.allowed_entry_ids.clone(),
            prior_state: None,
            max_options: state.max_options,
            enable_embeddings: state.enable_embeddings,
            force_entry_id: None,
            force_entity: None,
        };
        if let Some(e) = &opt.entry_id {
            q.force_entry_id = Some(e.clone());
        }
        if let Some(e) = &opt.entity {
            q.force_entity = Some(e.clone());
        }
        self.discover_inner(&q).await
    }
}

#[cfg(test)]
mod relation_intent_rank_tests {
    use super::*;
    use crate::AgentDiscovery;
    use plasm_core::schema::{
        CapabilityMapping, CapabilitySchema, Cardinality, DiscoveryCapabilityHints, FieldSchema,
        FieldValueKind, NamedValueSchema, RelationSchema, ResourceSchema, ValueDomainKey,
    };
    use plasm_core::{CapabilityName, EntityFieldName, EntityName, FieldType, RelationName};

    fn query_mapping_body() -> serde_json::Value {
        serde_json::json!({
            "method": "POST",
            "path": [
                {"type": "literal", "value": "query"},
            ],
            "body": {"type": "object", "fields": []}
        })
    }

    fn minimal_parent_child_comments_cgs() -> CGS {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "tid".to_string(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        let id_key = ValueDomainKey::new("tid").unwrap();
        let id_field = FieldSchema {
            name: EntityFieldName::from("id"),
            kind: FieldValueKind::Registry(id_key),
            description: String::new(),
            required: true,
            agent_presentation: None,
            mime_type_hint: None,
            attachment_media: None,
            wire_path: None,
            derive: None,
        };
        cgs.add_resource(ResourceSchema {
            name: EntityName::from("Parent"),
            description: "Stores files in the system".into(),
            id_field: EntityFieldName::from("id"),
            id_format: None,
            id_from: None,
            fields: vec![id_field.clone()],
            relations: vec![RelationSchema {
                name: RelationName::from("comments"),
                description: String::new(),
                target_resource: EntityName::from("Child"),
                cardinality: Cardinality::Many,
                materialize: None,
                discovery: None,
            }],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: Some(plasm_core::schema::DiscoveryEntityHints {
                names: vec!["thing".into()],
                qualifier_names: vec![],
            }),
        })
        .unwrap();
        cgs.add_resource(ResourceSchema {
            name: EntityName::from("Child"),
            description: "Reply threads for nested views".into(),
            id_field: EntityFieldName::from("id"),
            id_format: None,
            id_from: None,
            fields: vec![id_field],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        let tmpl: CapabilityMapping = CapabilityMapping {
            template: query_mapping_body().into(),
        };
        cgs.add_capability(CapabilitySchema {
            name: CapabilityName::from("query_parent"),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: EntityName::from("Parent"),
            mapping: tmpl.clone(),
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: CapabilityName::from("query_child"),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: EntityName::from("Child"),
            mapping: tmpl,
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: Some(DiscoveryCapabilityHints {
                operation_terms: vec![],
                target_terms: vec!["comment".into()],
            }),
        })
        .unwrap();
        cgs
    }

    #[tokio::test]
    async fn relation_intent_boost_prefers_relation_target_over_parent_token_match() {
        let cgs = Arc::new(minimal_parent_child_comments_cgs());
        let discovery = TypedDiscovery::from_cgs_entries(vec![("demo".into(), cgs)], false);
        let q = DiscoveryQuery {
            utterance: "get thing comments".into(),
            allowed_entry_ids: vec![],
            prior_state: None,
            max_options: 8,
            enable_embeddings: false,
            force_entry_id: None,
            force_entity: None,
        };
        let decision = discovery.discover(q).await.unwrap();
        match decision {
            DiscoveryDecision::Ready { target } => assert_eq!(target.entity, "Child"),
            other => panic!("expected Ready, got {other:?}"),
        }
    }
}
