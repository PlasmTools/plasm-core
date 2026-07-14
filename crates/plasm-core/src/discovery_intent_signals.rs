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
pub fn intent_mentions_catalog_id(entry_id: &str, intent: &str) -> bool {
    let lower = intent.to_lowercase();
    let catalog = entry_id.to_lowercase();
    lower.contains(&catalog) || lower.contains(&catalog.replace('-', " "))
}
