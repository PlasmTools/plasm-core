//! Identify explicit source bindings in the current request before operation selection.
use crate::discovery_matcher::whole_intent::references::IntentText;
use crate::discovery_store::DiscoveryAuthorization;
use plasm_core::discovery::CatalogEntryMeta;
use std::collections::BTreeSet;

const PUBLIC_NAMES: &str = include_str!(concat!(env!("OUT_DIR"), "/public_catalog_names.txt"));

fn key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn unavailable(
    intent: &str,
    entries: &[CatalogEntryMeta],
    policy: &DiscoveryAuthorization,
) -> Vec<String> {
    let mut known: BTreeSet<_> = PUBLIC_NAMES.lines().map(key).collect();
    let mut visible = BTreeSet::new();
    for entry in entries {
        let allowed = policy.catalogs.contains(&entry.entry_id)
            && policy
                .capabilities
                .get(&entry.entry_id)
                .is_none_or(|caps| !caps.is_empty());
        for name in [&entry.entry_id, &entry.label]
            .into_iter()
            .chain(&entry.aliases)
        {
            let name = key(name);
            known.insert(name.clone());
            if allowed {
                visible.insert(name);
            }
        }
    }
    missing_bindings(&IntentText::parse(intent).outer, &known, &visible)
}

fn missing_bindings(
    text: &str,
    known: &BTreeSet<String>,
    visible: &BTreeSet<String>,
) -> Vec<String> {
    let mut missing = Vec::new();
    let tokens: Vec<_> = text.split_whitespace().collect();
    let terminators = ['.', ';', '!', '?'];
    for words in tokens.split_inclusive(|w| w.ends_with(terminators)) {
        // Conditional, alternative and negative scopes do not establish a required source.
        if words.iter().any(|w| {
            matches!(
                w.trim_matches(['.', ',', ';', '!', '?'])
                    .to_ascii_lowercase()
                    .as_str(),
                "not"
                    | "never"
                    | "without"
                    | "except"
                    | "avoid"
                    | "skip"
                    | "if"
                    | "unless"
                    | "or"
                    | "instead"
            )
        }) {
            continue;
        }
        for (i, word) in words.iter().enumerate() {
            if !matches!(
                word.to_ascii_lowercase().as_str(),
                "read" | "get" | "fetch" | "retrieve" | "list" | "use" | "using" | "via" | "from"
            ) {
                continue;
            }
            let mut start = i + 1;
            if words
                .get(start)
                .is_some_and(|w| w.eq_ignore_ascii_case("the"))
            {
                start += 1;
            }
            let mut name = String::new();
            let mut matched = None;
            for (j, word) in words[start..].iter().enumerate() {
                let part = word.trim_end_matches(['.', ';', '!', '?', ',', ':']);
                // A URL, path, handle or field reference is an operand.
                if part.is_empty()
                    || !part
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                {
                    break;
                }
                name.push_str(&key(part));
                if known.contains(&name) {
                    matched = Some((start + j, visible.contains(&name)));
                }
                if word.len() != part.len()
                    || !known
                        .range(name.clone()..)
                        .next()
                        .is_some_and(|n| n.starts_with(&name))
                {
                    break;
                }
            }
            if let Some((end, false)) = matched {
                let source = words[start..=end]
                    .join(" ")
                    .trim_end_matches(['.', ';', '!', '?', ',', ':'])
                    .to_owned();
                if !missing.contains(&source) {
                    missing.push(source);
                }
            }
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_explicit_source_against_pinned_access() {
        let known = ["deskhub".to_owned(), "desk".to_owned()].into();
        let available = ["desk".to_owned()].into();
        let missing = |s: &str| missing_bindings(&IntentText::parse(s).outer, &known, &available);
        assert_eq!(
            missing("Read the Desk ticket and fetch Desk Hub user pg."),
            ["Desk Hub"]
        );
        assert_eq!(missing("Use Desk Hub to list records."), ["Desk Hub"]);
        for intent in [
            "Read Desk ticket 3.",
            "Get DeskHub/archive.",
            "Get DeskHub.example/archive via Desk.",
            "Search for Desk Hub.",
            "Read Desk ticket \"from Desk Hub\".",
            "Do not use Desk Hub.",
            "Use Desk Hub or Desk.",
        ] {
            assert!(missing(intent).is_empty(), "{intent}");
        }
    }
}
