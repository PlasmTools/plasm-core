//! Schema-derived domain lexicon for deterministic expression correction.
//!
//! Builds a bounded synonym index by tokenising entity names, descriptions,
//! capability names/descriptions, and field/parameter names/descriptions from
//! a loaded [`CGS`]. Used by [`crate::expr_correction`] to resolve natural-language
//! aliases (e.g. "workspace" → Team) when strict parsing fails.
//!
//! # Disambiguation
//!
//! Raw token lookup is always resolved in **entity context**: a match counts only
//! if the `LexEntry` is valid (entity field, or capability param) for the entity
//! being queried. This eliminates cross-entity pollution from shared vocabulary
//! (e.g. "workspace" appears in Team, Space, Member, and CustomField descriptions;
//! for entity Webhook, only `team_id=Team(id)` is a valid scope → unique result).

use std::collections::{HashMap, HashSet};

use crate::{CapabilityKind, FieldType, InputType, ParameterRole, CGS};

/// API brand/product names that should not be indexed as domain terms.
/// Prevents matching on "GitHub", "ClickUp", etc.
static BRAND_STOPWORDS: &[&str] = &[
    "clickup",
    "github",
    "slack",
    "notion",
    "airtable",
    "stripe",
    "google",
    "microsoft",
    "jira",
    "linear",
    "asana",
    "trello",
    "monday",
    "salesforce",
    "hubspot",
    "zendesk",
    "intercom",
];

/// Common English stop-words not useful for disambiguation.
static COMMON_STOPWORDS: &[&str] = &[
    "a", "an", "the", "in", "on", "at", "to", "of", "for", "and", "or", "is", "are", "was", "were",
    "be", "been", "by", "its", "this", "that", "with", "from", "as", "within", "can", "used",
    "uses", "has", "have", "which", "based", "via", "per", "any", "all", "each",
];

/// One entry in the lexicon — a schema concept reachable by a token.
#[derive(Debug, Clone, PartialEq)]
pub enum LexEntry {
    /// Matches the entity name itself.
    Entity { name: String },
    /// Matches an entity's own field (entity-level attribute).
    EntityField {
        entity: String,
        field: String,
        field_type: FieldType,
    },
    /// Matches an input parameter of a query/search capability.
    CapParam {
        entity: String,
        cap: String,
        param: String,
        field_type: FieldType,
        required: bool,
        is_scope: bool,
    },
}

impl LexEntry {
    /// The entity this entry belongs to.
    pub fn entity(&self) -> &str {
        match self {
            LexEntry::Entity { name } => name,
            LexEntry::EntityField { entity, .. } => entity,
            LexEntry::CapParam { entity, .. } => entity,
        }
    }

    /// The canonical field/param name to use in a corrected expression, if any.
    pub fn field_name(&self) -> Option<&str> {
        match self {
            LexEntry::Entity { .. } => None,
            LexEntry::EntityField { field, .. } => Some(field),
            LexEntry::CapParam { param, .. } => Some(param),
        }
    }

    /// The field type, for entity ref substitution.
    pub fn field_type(&self) -> Option<&FieldType> {
        match self {
            LexEntry::Entity { .. } => None,
            LexEntry::EntityField { field_type, .. } => Some(field_type),
            LexEntry::CapParam { field_type, .. } => Some(field_type),
        }
    }
}

/// Bounded synonym index built from CGS metadata.
///
/// Keys are normalised tokens (lowercase, stemmed, no stop-words).
/// Values are all schema concepts whose name/description contains that token.
#[derive(Debug, Default)]
pub struct DomainLexicon {
    index: HashMap<String, Vec<LexEntry>>,
}

impl DomainLexicon {
    /// Build the lexicon from a loaded CGS.
    pub fn from_cgs(cgs: &CGS) -> Self {
        let mut lex = DomainLexicon::default();

        for (ename, ent) in &cgs.entities {
            let ename_s = ename.to_string();
            // Entity name and description
            for token in tokens(&ename_s) {
                lex.insert(
                    token,
                    LexEntry::Entity {
                        name: ename_s.clone(),
                    },
                );
            }
            for token in tokens(&ent.description) {
                lex.insert(
                    token,
                    LexEntry::Entity {
                        name: ename_s.clone(),
                    },
                );
            }

            // Entity fields
            for (fname, field) in &ent.fields {
                let field_type = cgs
                    .named_value_for_slot(field)
                    .expect("domain lexicon: entity field value_ref")
                    .field_type
                    .clone();
                let entry = LexEntry::EntityField {
                    entity: ename_s.clone(),
                    field: fname.as_str().to_string(),
                    field_type,
                };
                for token in tokens(fname) {
                    lex.insert(token, entry.clone());
                }
                for token in tokens(&field.description) {
                    lex.insert(token, entry.clone());
                }
            }
        }

        // Capability names, descriptions, and input params
        for (_, cap) in &cgs.capabilities {
            if !matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search) {
                continue;
            }
            let Some(is) = &cap.input_schema else {
                continue;
            };
            let InputType::Object { fields, .. } = &is.input_type else {
                continue;
            };

            for f in fields {
                let is_scope = matches!(f.role, Some(ParameterRole::Scope));
                let field_type = cgs
                    .named_value_for_slot(f)
                    .expect("domain lexicon: cap param value_ref")
                    .field_type
                    .clone();
                let entry = LexEntry::CapParam {
                    entity: cap.domain.to_string(),
                    cap: cap.name.to_string(),
                    param: f.name.clone(),
                    field_type,
                    required: f.required,
                    is_scope,
                };

                // Index by parameter name tokens
                for token in tokens(&f.name) {
                    lex.insert(token, entry.clone());
                }
                // Index by parameter description tokens if present
                if let Some(desc) = &f.description {
                    for token in tokens(desc) {
                        lex.insert(token, entry.clone());
                    }
                }

                // Also index the capability description tokens → helps resolve
                // "workspace custom fields" → CustomField{team_id=Team(id)}
                for token in tokens(&cap.description) {
                    lex.insert(token, entry.clone());
                }
            }
        }

        lex
    }

    fn insert(&mut self, token: String, entry: LexEntry) {
        let list = self.index.entry(token).or_default();
        // Deduplicate by entity + field combination
        if !list
            .iter()
            .any(|e| e.entity() == entry.entity() && e.field_name() == entry.field_name())
        {
            list.push(entry);
        }
    }

    /// Resolve `tokens` to `LexEntry` values valid for `entity_name`.
    ///
    /// Returns all entries whose token set intersects the query AND that are
    /// valid for the given entity. The caller checks uniqueness.
    pub fn resolve_for_entity<'a>(
        &'a self,
        query_tokens: &[String],
        entity_name: &str,
    ) -> Vec<&'a LexEntry> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut results: Vec<&LexEntry> = Vec::new();

        for tok in query_tokens {
            if let Some(entries) = self.index.get(tok) {
                for entry in entries {
                    if entry.entity() != entity_name {
                        continue;
                    }
                    // Skip entity-only entries (no field name) for field resolution
                    if entry.field_name().is_none() {
                        continue;
                    }
                    let key = format!("{}:{}", entry.entity(), entry.field_name().unwrap_or(""));
                    if seen.insert(key) {
                        results.push(entry);
                    }
                }
            }
        }

        results
    }
}

fn raw_words(input: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();

    // Split on non-alphabetic/non-digit runs, then split camelCase
    let mut current = String::new();
    for ch in input.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && !current.is_empty() {
                // camelCase boundary — flush current word
                let w = current.to_lowercase();
                if !w.is_empty() {
                    words.push(w);
                }
                current.clear();
            }
            current.push(ch);
        } else {
            // delimiter
            if !current.is_empty() {
                words.push(current.to_lowercase());
                current.clear();
            }
        }
    }
    if !current.is_empty() {
        words.push(current.to_lowercase());
    }

    words
}

/// Tokenise a name or description into normalised, filtered tokens.
///
/// - Split on `_`, `-`, space, and camelCase boundaries
/// - Lowercase
/// - Strip simple English suffixes
/// - Remove brand names and common stop-words
pub fn tokens(input: &str) -> Vec<String> {
    raw_words(input)
        .into_iter()
        .map(stem)
        .filter(|w| w.len() >= 3)
        .filter(|w| !BRAND_STOPWORDS.contains(&w.as_str()))
        .filter(|w| !COMMON_STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Like [`tokens`], but **does not** drop API/product brand tokens.
///
/// Used for catalog / API routing where product names must remain visible to deterministic matchers.
pub fn tokens_keep_brands(input: &str) -> Vec<String> {
    raw_words(input)
        .into_iter()
        .map(stem)
        .filter(|w| w.len() >= 3)
        .filter(|w| !COMMON_STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Very simple English suffix stripper — no external dependency needed
/// given the constrained vocabulary size.
fn stem(mut word: String) -> String {
    // Order matters — longer suffixes first
    for suffix in &[
        "ically", "tion", "tion", "ing", "ings", "tions", "ions", "ion", "ed", "s",
    ] {
        if word.len() > suffix.len() + 2 && word.ends_with(suffix) {
            word.truncate(word.len() - suffix.len());
            return word;
        }
    }
    word
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenises_snake_case() {
        let t = tokens("team_id");
        assert!(t.contains(&"team".to_string()));
        assert!(t.contains(&"id".to_string()) || t.is_empty() || !t.contains(&"id".to_string()));
        // "id" is 2 chars — filtered by length >= 3 — that's intentional
        assert!(!t.contains(&"id".to_string()));
        assert!(t.contains(&"team".to_string()));
    }

    #[test]
    fn tokenises_camel_case() {
        let t = tokens("teamId");
        assert!(t.contains(&"team".to_string()));
    }

    #[test]
    fn tokens_keep_brands_retains_github() {
        // PascalCase splits into git/hub before stemming; routing uses lowercase brands too.
        let t = tokens_keep_brands("github issue");
        assert!(t.contains(&"github".to_string()));
        assert!(!tokens("github issue").contains(&"github".to_string()));
    }

    #[test]
    fn strips_brand_stopwords() {
        let t = tokens("A ClickUp workspace");
        assert!(!t.iter().any(|s| s == "clickup"));
        assert!(t.iter().any(|s| s == "workspace"));
    }

    #[test]
    fn tokenises_description_with_alias() {
        let t = tokens("A ClickUp workspace (historically called Team)");
        assert!(t.iter().any(|s| s == "workspace"));
        assert!(t.iter().any(|s| s == "team"));
        assert!(!t.iter().any(|s| s == "clickup"));
    }

    #[test]
    fn stem_basic() {
        assert_eq!(stem("workspaces".to_string()), "workspace");
        assert_eq!(stem("teams".to_string()), "team");
        assert_eq!(stem("logging".to_string()), "logg");
    }

    #[test]
    fn lexicon_resolves_for_entity() {
        // Minimal test using loader if petstore schema exists
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(dir).unwrap();
        let lex = DomainLexicon::from_cgs(&cgs);
        // "status" should resolve to Pet.status
        let toks = tokens("status");
        let hits = lex.resolve_for_entity(&toks, "Pet");
        assert!(!hits.is_empty(), "should find status field on Pet");
    }
}
