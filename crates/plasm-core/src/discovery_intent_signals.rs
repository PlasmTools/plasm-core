//! Shared intent/requirement structural helpers for discovery (non-NLP).

/// `owner/repo`-style path token in intent (not a URL).
pub fn intent_mentions_repo_path(intent: &str) -> bool {
    intent.split_whitespace().any(|token| {
        let parts: Vec<&str> = token.split('/').collect();
        parts.len() == 2
            && !parts[0].is_empty()
            && !parts[1].is_empty()
            && !token.starts_with("http")
    })
}

pub fn is_auxiliary_entity_for_mutation(entity: &str) -> bool {
    entity.contains("Snapshot") || entity.ends_with("Context")
}

/// Whether intent text names a catalog entry_id (structural brand hint for routing UX).
///
/// Matches whole alphanumeric tokens only — never raw substrings — so short ids like
/// `phone` do not fire inside unrelated words such as `headphones`.
pub fn intent_mentions_catalog_id(entry_id: &str, intent: &str) -> bool {
    let catalog = entry_id.to_lowercase();
    let intent_lower = intent.to_lowercase();
    let mut intent_tokens = std::collections::HashSet::new();
    for w in intent_lower.split(|c: char| !c.is_alphanumeric()) {
        if w.len() >= 2 {
            intent_tokens.insert(w.to_string());
        }
    }
    if intent_tokens.contains(&catalog) {
        return true;
    }
    let spaced = catalog.replace('-', " ");
    if spaced != catalog && intent_lower.contains(&spaced) {
        return true;
    }
    let parts: Vec<&str> = catalog
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .collect();
    !parts.is_empty() && parts.iter().all(|part| intent_tokens.contains(*part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_id_does_not_match_inside_headphones() {
        assert!(!intent_mentions_catalog_id(
            "phone",
            "Login to Amazon and search products for headphones"
        ));
        assert!(intent_mentions_catalog_id(
            "amazon",
            "Login to Amazon and search products for headphones"
        ));
        assert!(intent_mentions_catalog_id(
            "phone",
            "Login to Phone and list contacts"
        ));
    }
}
