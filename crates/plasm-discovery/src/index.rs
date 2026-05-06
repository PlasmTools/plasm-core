//! Phrase + lexical indexes over one CGS catalog.

use std::collections::HashMap;
use std::sync::Arc;

use inflection::{plural, singular};
use plasm_core::schema::CGS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseSource {
    DiscoveryName,
    EntityName,
    ExpressionAlias,
    CapabilityTargetTerm,
}

#[derive(Debug, Clone)]
pub struct PhraseHit {
    pub entry_id: String,
    pub entity: String,
    pub phrase: String,
    pub source: PhraseSource,
}

#[derive(Debug, Clone)]
pub struct CatalogIndex {
    pub entry_id: String,
    #[allow(dead_code)]
    pub catalog_hash: String,
    phrase_to_hits: HashMap<String, Vec<PhraseHit>>,
    pub cgs: Arc<CGS>,
}

fn norm_phrase(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `IssueType` → `issue type`, `PullRequest` → `pull request` (for substring utterance hits).
pub(crate) fn camel_case_word_spaced(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut out = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if i > 0
            && c.is_uppercase()
            && chars
                .get(i - 1)
                .is_some_and(|p| p.is_lowercase() || p.is_numeric())
        {
            out.push(' ');
        }
        out.push(c.to_ascii_lowercase());
    }
    out.contains(' ').then_some(out)
}

/// Boost when the utterance mentions a multi-word entity as a phrase (e.g. "issue types" ⊃ "issue type").
pub(crate) fn camel_entity_phrase_substring_bonus(entity: &str, utterance_lower: &str) -> f64 {
    let Some(spaced) = camel_case_word_spaced(entity) else {
        return 0.0;
    };
    let needle = norm_phrase(&spaced);
    if needle.is_empty() {
        return 0.0;
    }
    if utterance_lower.contains(&needle) {
        35.0
    } else {
        0.0
    }
}

fn token_matches_segment_word(token_lower: &str, segment_lower: &str) -> bool {
    if token_lower == segment_lower {
        return true;
    }
    let seg_pl = plural::<_, String>(segment_lower);
    let seg_sg = singular::<_, String>(segment_lower);
    if token_lower == norm_phrase(&seg_pl) || token_lower == norm_phrase(&seg_sg) {
        return true;
    }
    let tok_sg = singular::<_, String>(token_lower);
    let tok_pl = plural::<_, String>(token_lower);
    norm_phrase(&tok_sg) == segment_lower || norm_phrase(&tok_pl) == segment_lower
}

/// Small bonus when every CamelCase segment matches some utterance token (with noun inflection).
pub(crate) fn camel_entity_segment_token_bonus(entity: &str, utterance_tokens: &[String]) -> f64 {
    let Some(spaced) = camel_case_word_spaced(entity) else {
        return 0.0;
    };
    let segments: Vec<&str> = spaced.split_whitespace().collect();
    if segments.is_empty() {
        return 0.0;
    }
    for seg in segments {
        let seg_l = norm_phrase(seg);
        if seg_l.is_empty() {
            return 0.0;
        }
        let hit = utterance_tokens.iter().any(|t| {
            let tl = norm_phrase(t);
            !tl.is_empty() && token_matches_segment_word(&tl, &seg_l)
        });
        if !hit {
            return 0.0;
        }
    }
    18.0
}

/// Tokens from entity `description` for lexical recall (e.g. Jira Issue text mentions "bug").
const DESCRIPTION_TOKEN_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "for", "with", "from", "to", "of", "in", "on", "is", "are",
    "was", "were", "be", "been", "being", "it", "this", "that", "these", "those", "as", "at", "by",
    "etc",
];

fn description_tokens(description: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in description.split(|c: char| !c.is_ascii_alphanumeric()) {
        let t = raw.trim().to_lowercase();
        if t.len() < 3 || DESCRIPTION_TOKEN_STOPWORDS.contains(&t.as_str()) {
            continue;
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Tokens that duplicate the catalog slug (e.g. `github` in every GitHub entity blurb) add
/// spurious hits and hide entity-specific phrases like `label` vs `issue`.
fn is_catalog_slug_token(tok: &str, entry_id: &str) -> bool {
    let t = tok.to_lowercase();
    let e = entry_id.to_lowercase();
    if t == e {
        return true;
    }
    e.split('-').any(|p| p.len() >= 3 && p == t)
}

fn insert_phrase(map: &mut HashMap<String, Vec<PhraseHit>>, key: String, hit: PhraseHit) {
    map.entry(key).or_default().push(hit);
}

/// English noun singular/plural aliases (already lowercased keys). Multi-word phrases only inflect
/// the last token (e.g. `pull request` → `pull requests`).
fn inflection_alias_keys(key: &str) -> Vec<String> {
    let key = norm_phrase(key);
    if key.is_empty() {
        return Vec::new();
    }
    let tokens: Vec<&str> = key.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    if tokens.len() == 1 {
        let w = tokens[0];
        let p = norm_phrase(&plural::<_, String>(w));
        if !p.is_empty() && p != key {
            out.push(p);
        }
        let s = norm_phrase(&singular::<_, String>(w));
        if !s.is_empty() && s != key {
            out.push(s);
        }
        return out;
    }

    let head = tokens[..tokens.len() - 1].join(" ");
    let last = tokens[tokens.len() - 1];
    for infl in [plural::<_, String>(last), singular::<_, String>(last)] {
        let infl = norm_phrase(&infl);
        if infl.is_empty() || infl == last {
            continue;
        }
        let candidate = norm_phrase(&format!("{head} {infl}"));
        if candidate != key {
            out.push(candidate);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Inserts `key` plus singular/plural noun variants where they differ (no Porter-style stem mutilation).
fn insert_phrase_with_inflection_alias(
    map: &mut HashMap<String, Vec<PhraseHit>>,
    key: String,
    hit: PhraseHit,
) {
    insert_phrase(map, key.clone(), hit.clone());
    for alt in inflection_alias_keys(&key) {
        insert_phrase(map, alt, hit.clone());
    }
}

impl CatalogIndex {
    pub fn build(entry_id: String, cgs: Arc<CGS>) -> Self {
        let catalog_hash = cgs.catalog_cgs_hash_hex();
        let mut phrase_to_hits: HashMap<String, Vec<PhraseHit>> = HashMap::new();
        let eid = entry_id.clone();

        for (ename, ent) in cgs.entities.iter() {
            let entity_s = ename.to_string();
            let key = norm_phrase(&entity_s);
            if !key.is_empty() {
                insert_phrase_with_inflection_alias(
                    &mut phrase_to_hits,
                    key.clone(),
                    PhraseHit {
                        entry_id: eid.clone(),
                        entity: entity_s.clone(),
                        phrase: entity_s.clone(),
                        source: PhraseSource::EntityName,
                    },
                );
            }
            if let Some(spaced) = camel_case_word_spaced(&entity_s) {
                let sk = norm_phrase(&spaced);
                if !sk.is_empty() && sk != key {
                    insert_phrase_with_inflection_alias(
                        &mut phrase_to_hits,
                        sk,
                        PhraseHit {
                            entry_id: eid.clone(),
                            entity: entity_s.clone(),
                            phrase: entity_s.clone(),
                            source: PhraseSource::EntityName,
                        },
                    );
                }
            }
            for a in &ent.expression_aliases {
                let k = norm_phrase(a);
                if k.is_empty() {
                    continue;
                }
                insert_phrase_with_inflection_alias(
                    &mut phrase_to_hits,
                    k.clone(),
                    PhraseHit {
                        entry_id: eid.clone(),
                        entity: entity_s.clone(),
                        phrase: a.clone(),
                        source: PhraseSource::ExpressionAlias,
                    },
                );
            }
            if let Some(d) = &ent.discovery {
                for n in &d.names {
                    let k = norm_phrase(n);
                    if k.is_empty() {
                        continue;
                    }
                    insert_phrase_with_inflection_alias(
                        &mut phrase_to_hits,
                        k.clone(),
                        PhraseHit {
                            entry_id: eid.clone(),
                            entity: entity_s.clone(),
                            phrase: n.clone(),
                            source: PhraseSource::DiscoveryName,
                        },
                    );
                }
            }

            let entity_key = norm_phrase(&entity_s);
            for tok in description_tokens(ent.description.as_str()) {
                if tok == entity_key || is_catalog_slug_token(&tok, &eid) {
                    continue;
                }
                insert_phrase_with_inflection_alias(
                    &mut phrase_to_hits,
                    tok.clone(),
                    PhraseHit {
                        entry_id: eid.clone(),
                        entity: entity_s.clone(),
                        phrase: tok.clone(),
                        source: PhraseSource::DiscoveryName,
                    },
                );
            }
        }

        for cap in cgs.capabilities.values() {
            let domain = cap.domain.to_string();
            if let Some(d) = &cap.discovery {
                for t in &d.target_terms {
                    let k = norm_phrase(t);
                    if k.is_empty() {
                        continue;
                    }
                    insert_phrase_with_inflection_alias(
                        &mut phrase_to_hits,
                        k.clone(),
                        PhraseHit {
                            entry_id: eid.clone(),
                            entity: domain.clone(),
                            phrase: t.clone(),
                            source: PhraseSource::CapabilityTargetTerm,
                        },
                    );
                }
            }
        }

        Self {
            entry_id,
            catalog_hash,
            phrase_to_hits,
            cgs,
        }
    }

    pub fn lookup_phrase(&self, phrase: &str) -> Vec<PhraseHit> {
        let k = norm_phrase(phrase);
        self.phrase_to_hits.get(&k).cloned().unwrap_or_default()
    }

    /// Any phrase key contained as substring in `utterance` (normalized), longest keys first.
    pub fn scan_utterance(&self, utterance_lower: &str) -> Vec<PhraseHit> {
        let u = norm_phrase(utterance_lower);
        let mut keys: Vec<&String> = self.phrase_to_hits.keys().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        let mut seen = HashMap::<(String, String, String), ()>::new();
        let mut out = Vec::new();
        for k in keys {
            if u.contains(k.as_str()) {
                for h in self.phrase_to_hits.get(k).into_iter().flatten() {
                    let sig = (h.entry_id.clone(), h.entity.clone(), h.phrase.clone());
                    if seen.insert(sig, ()).is_none() {
                        out.push(h.clone());
                    }
                }
            }
        }
        out
    }

    pub fn entity_count(&self) -> usize {
        self.cgs.entities.len()
    }

    pub fn capability_count(&self) -> usize {
        self.cgs.capabilities.len()
    }
}

#[cfg(test)]
mod inflection_tests {
    use super::*;

    #[test]
    fn noun_inflection_links_issue_and_issues() {
        assert!(inflection_alias_keys("issue").contains(&"issues".to_string()));
        assert!(inflection_alias_keys("issues").contains(&"issue".to_string()));
    }

    #[test]
    fn noun_inflection_pluralizes_last_token_in_phrases() {
        let alts = inflection_alias_keys("pull request");
        assert!(alts.contains(&"pull requests".to_string()));
    }
}

/// Whether `qualifier` is plausibly attached to `entity` via fields, entity discovery hints, or one hop relations.
pub fn qualifier_supported(cgs: &CGS, entity: &str, qualifier: &str) -> bool {
    let q = norm_phrase(qualifier);
    if q.is_empty() {
        return true;
    }
    let Some(ent) = cgs.entities.get(entity) else {
        return false;
    };

    for (fname, _) in ent.fields.iter() {
        let fk = norm_phrase(fname.to_string().as_str());
        if fk.contains(&q) || q.contains(&fk) {
            return true;
        }
    }

    if let Some(d) = &ent.discovery {
        for qn in &d.qualifier_names {
            if norm_phrase(qn) == q {
                return true;
            }
        }
    }

    for rel in ent.relations.values() {
        if let Some(rd) = &rel.discovery {
            for t in &rd.qualifier_terms {
                if norm_phrase(t) == q {
                    return true;
                }
            }
        }
        let tgt = rel.target_resource.to_string();
        let tk = norm_phrase(&tgt);
        if tk == q || tk.contains(&q) || q.contains(&tk) {
            return true;
        }
        if let Some(neigh) = cgs.entities.get(tgt.as_str()) {
            if let Some(d) = &neigh.discovery {
                for n in &d.names {
                    let nk = norm_phrase(n);
                    if nk == q || nk.contains(&q) || q.contains(&nk) {
                        return true;
                    }
                }
            }
        }
    }

    false
}
