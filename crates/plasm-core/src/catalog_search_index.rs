//! In-memory Okapi BM25 catalog search (`bm25` crate + `DefaultTokenizer`).
//!
//! Sole discovery tokenizer + ranker for catalog-authored text. Graph expand and
//! closed-set LLM narrow consume hits from this index — not a parallel keyword gate.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bm25::{DefaultTokenizer, Document, Language, SearchEngine, SearchEngineBuilder, Tokenizer};
use indexmap::IndexMap;

use crate::schema::{CapabilitySchema, CGS};

/// One searchable capability document in the BM25 corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSearchHit {
    pub entry_id: String,
    pub entity: String,
    pub capability_name: String,
    /// BM25 score scaled to milli-units (`round(score * 1000)`).
    pub score: u32,
}

#[derive(Debug, Clone)]
struct DocMeta {
    entry_id: String,
    entity: String,
    capability_name: String,
}

/// Process-local BM25 index over catalog-authored capability text.
#[derive(Clone)]
pub struct CatalogSearchIndex {
    engine: Arc<SearchEngine<u32>>,
    docs: Arc<Vec<DocMeta>>,
}

impl std::fmt::Debug for CatalogSearchIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogSearchIndex")
            .field("doc_count", &self.docs.len())
            .finish_non_exhaustive()
    }
}

impl CatalogSearchIndex {
    /// Canonical discovery tokenizer (`DefaultTokenizer`: unicode, deunicode, stopwords, stem).
    pub fn tokenize(text: &str) -> HashSet<String> {
        default_tokenizer().tokenize(text).into_iter().collect()
    }

    pub fn empty() -> Self {
        let engine = SearchEngineBuilder::<u32>::with_avgdl(1.0).build();
        Self {
            engine: Arc::new(engine),
            docs: Arc::new(Vec::new()),
        }
    }

    pub fn build_from_cgs_map(catalogs: &HashMap<String, &CGS>) -> Self {
        let mut pairs: Vec<(&str, &CGS)> = catalogs
            .iter()
            .map(|(id, cgs)| (id.as_str(), *cgs))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        Self::build_from_pairs(pairs)
    }

    /// Build from an `IndexMap` of owned graphs (coverage / eval harnesses).
    pub fn build_from_index_map(catalogs: &IndexMap<String, CGS>) -> Self {
        let map: HashMap<String, &CGS> =
            catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
        Self::build_from_cgs_map(&map)
    }

    pub fn build_from_pairs<'a, I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a CGS)>,
    {
        let mut docs = Vec::new();
        let mut documents = Vec::new();
        for (entry_id, cgs) in entries {
            let mut caps: Vec<&CapabilitySchema> = cgs.capabilities.values().collect();
            caps.sort_by_key(|c| c.name.as_str());
            for cap in caps {
                let contents = capability_document_text(cgs, cap);
                if contents.trim().is_empty() {
                    continue;
                }
                let id = docs.len() as u32;
                docs.push(DocMeta {
                    entry_id: entry_id.to_string(),
                    entity: cap.domain.to_string(),
                    capability_name: cap.name.to_string(),
                });
                documents.push(Document { id, contents });
            }
        }
        if documents.is_empty() {
            return Self::empty();
        }
        let engine =
            SearchEngineBuilder::<u32>::with_documents(Language::English, documents).build();
        Self {
            engine: Arc::new(engine),
            docs: Arc::new(docs),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    /// Rank capability documents for a free-text intent / phrase query.
    pub fn search(&self, query: &str, limit: usize) -> Vec<CatalogSearchHit> {
        if limit == 0 || query.trim().is_empty() || self.docs.is_empty() {
            return Vec::new();
        }
        self.engine
            .search(query, limit)
            .into_iter()
            .filter_map(|result| {
                let meta = self.docs.get(result.document.id as usize)?;
                let score = bm25_to_u32(result.score);
                if score == 0 {
                    return None;
                }
                Some(CatalogSearchHit {
                    entry_id: meta.entry_id.clone(),
                    entity: meta.entity.clone(),
                    capability_name: meta.capability_name.clone(),
                    score,
                })
            })
            .collect()
    }

    /// Max BM25 milli-score for any capability on `(entry_id, entity)`.
    pub fn entity_score(&self, entry_id: &str, entity: &str, query: &str) -> u32 {
        self.search(query, self.docs.len().max(1))
            .into_iter()
            .filter(|h| h.entry_id == entry_id && h.entity == entity)
            .map(|h| h.score)
            .max()
            .unwrap_or(0)
    }

    /// Best BM25 milli-score for a specific capability.
    pub fn capability_score(&self, entry_id: &str, capability_name: &str, query: &str) -> u32 {
        self.search(query, self.docs.len().max(1))
            .into_iter()
            .find(|h| h.entry_id == entry_id && h.capability_name == capability_name)
            .map(|h| h.score)
            .unwrap_or(0)
    }

    /// Hits restricted to one catalog entry.
    pub fn search_entry(&self, entry_id: &str, query: &str, limit: usize) -> Vec<CatalogSearchHit> {
        self.search(query, self.docs.len().max(limit))
            .into_iter()
            .filter(|h| h.entry_id == entry_id)
            .take(limit)
            .collect()
    }
}

fn default_tokenizer() -> DefaultTokenizer {
    DefaultTokenizer::builder()
        .language_mode(Language::English)
        .normalization(true)
        .stopwords(true)
        .stemming(true)
        .build()
}

fn bm25_to_u32(score: f32) -> u32 {
    if !score.is_finite() || score <= 0.0 {
        return 0;
    }
    (score * 1000.0).round().clamp(0.0, u32::MAX as f32) as u32
}

/// True when every token of `phrase` appears in `intent` (catalog-authored op/target labels).
pub fn phrase_tokens_covered_by_intent(phrase: &str, intent: &str) -> bool {
    let intent_tokens = CatalogSearchIndex::tokenize(intent);
    let phrase_tokens = CatalogSearchIndex::tokenize(phrase);
    !phrase_tokens.is_empty() && phrase_tokens.iter().all(|tok| intent_tokens.contains(tok))
}

/// Catalog-authored corpus for one capability (name, descriptions, discovery phrases).
fn capability_document_text(cgs: &CGS, cap: &CapabilitySchema) -> String {
    let mut parts: Vec<&str> = Vec::new();
    // Name / operation terms repeated as field boost without a custom schema.
    for _ in 0..3 {
        parts.push(cap.name.as_str());
    }
    parts.push(cap.description.as_str());
    parts.push(cap.domain.as_str());
    if let Some(ent) = cgs.entities.get(cap.domain.as_str()) {
        parts.push(ent.description.as_str());
        if let Some(h) = &ent.discovery {
            for phrase in &h.names {
                for _ in 0..2 {
                    parts.push(phrase.as_str());
                }
            }
            for phrase in &h.qualifier_names {
                parts.push(phrase.as_str());
            }
        }
    }
    if let Some(h) = &cap.discovery {
        for phrase in &h.operation_terms {
            for _ in 0..3 {
                parts.push(phrase.as_str());
            }
        }
        for phrase in &h.target_terms {
            for _ in 0..2 {
                parts.push(phrase.as_str());
            }
        }
    }
    parts
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        CapabilityKind, CapabilityMapping, CapabilityTemplateJson, DiscoveryCapabilityHints,
        DiscoveryEntityHints, EntityDef,
    };
    use crate::{CapabilityName, EntityFieldName, EntityName};
    use indexmap::IndexMap;

    fn tiny_cgs() -> CGS {
        let mut cgs = CGS::new();
        cgs.entities.insert(
            "PullRequest".into(),
            EntityDef {
                name: EntityName::from("PullRequest"),
                description: "A pull request on a repository.".into(),
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
                    names: vec!["pull request".into(), "pr".into()],
                    qualifier_names: vec![],
                    seed_class: None,
                }),
            },
        );
        cgs.capabilities.insert(
            "create_pull_request".into(),
            CapabilitySchema {
                name: CapabilityName::from("create_pull_request"),
                description: "Open a pull request.".into(),
                kind: CapabilityKind::Create,
                domain: EntityName::from("PullRequest"),
                mapping: CapabilityMapping {
                    template: CapabilityTemplateJson(serde_json::json!({ "method": "POST" })),
                },
                input_schema: None,
                output_schema: None,
                provides: vec![],
                sanitizes: vec![],
                deterministic: None,
                scope_aggregate_key_policy: Default::default(),
                preflight: None,
                discovery: Some(DiscoveryCapabilityHints {
                    operation_terms: vec!["open pull request".into(), "open pr".into()],
                    target_terms: vec![],
                }),
                identity_key: None,
            },
        );
        cgs
    }

    #[test]
    fn bm25_matches_article_paraphrase_via_default_tokenizer() {
        let cgs = tiny_cgs();
        let mut map = HashMap::new();
        map.insert("github".to_string(), &cgs);
        let index = CatalogSearchIndex::build_from_cgs_map(&map);
        let hits = index.search("open a pull request on github", 10);
        assert!(
            hits
                .iter()
                .any(|h| h.capability_name == "create_pull_request"),
            "DefaultTokenizer must stem/stopword so annotated 'open pull request' matches; hits={hits:?}"
        );
    }
}
