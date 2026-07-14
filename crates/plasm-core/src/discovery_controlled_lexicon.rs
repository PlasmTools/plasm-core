//! Deterministic catalog-alias matching for discovery brand lock.

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::catalog_search_index::CatalogSearchIndex;
use crate::schema::CGS;

/// Catalogs explicitly named by an entry id or registry alias in the intent.
///
/// Entity and capability matches never brand-lock discovery.
pub fn explicit_named_catalogs_from_intent(
    catalogs: &IndexMap<String, CGS>,
    intent: &str,
    allowed_entry_ids: Option<&[String]>,
) -> Vec<String> {
    let intent_lower = intent.to_ascii_lowercase();
    let intent_tokens = CatalogSearchIndex::tokenize(intent);
    // Also keep raw alphanumeric tokens so short entry ids survive stemmer/stopword quirks.
    let mut raw_tokens = intent_tokens;
    for w in intent_lower.split(|c: char| !c.is_alphanumeric()) {
        if w.len() >= 2 {
            raw_tokens.insert(w.to_string());
        }
    }
    let mut named = Vec::new();

    for (entry_id, cgs) in catalogs {
        if allowed_entry_ids
            .is_some_and(|ids| !ids.is_empty() && !ids.iter().any(|allowed| allowed == entry_id))
        {
            continue;
        }

        let mut aliases = std::iter::once(entry_id.as_str())
            .chain(cgs.registry_aliases.iter().map(String::as_str));
        if aliases.any(|alias| alias_matches_intent(alias, &raw_tokens, &intent_lower)) {
            named.push(entry_id.clone());
        }
    }

    named.sort_unstable();
    named.dedup();
    named
}

fn alias_matches_intent(alias: &str, intent_tokens: &HashSet<String>, intent_lower: &str) -> bool {
    let alias_lower = alias.to_ascii_lowercase();
    if alias.chars().any(char::is_whitespace) {
        intent_lower.contains(&alias_lower)
    } else {
        intent_tokens.contains(&alias_lower) || intent_lower.contains(&alias_lower)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use std::path::PathBuf;

    #[test]
    fn entity_terms_do_not_brand_lock() {
        let cgs = prompt_matrix();
        let mut catalogs = IndexMap::new();
        catalogs.insert("prompt_matrix".into(), cgs);

        let named = explicit_named_catalogs_from_intent(&catalogs, "list issues with labels", None);
        assert!(named.is_empty());
    }

    #[test]
    fn entry_id_does_brand_lock() {
        let cgs = prompt_matrix();
        let mut catalogs = IndexMap::new();
        catalogs.insert("prompt_matrix".into(), cgs);

        let named =
            explicit_named_catalogs_from_intent(&catalogs, "use prompt_matrix to triage", None);
        assert_eq!(named, vec!["prompt_matrix"]);
    }

    fn prompt_matrix() -> CGS {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        load_schema_dir(&dir).expect("load prompt matrix")
    }
}
