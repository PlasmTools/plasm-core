//! Nominate reads connected to the records returned by recall. Nominations are
//! bounded candidates for judgment, never authorization or resolved identities.
use super::{
    admission::intent_terms, CapabilityRef, DiscoveryAuthorization, IntentText,
    RetrievedCapability, CGS,
};
use plasm_core::{schema::RelationMaterialization, CapabilityKind};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const SOURCE: &str = "source_read";
pub(super) const NAVIGATION: &str = "record_navigation";

pub(crate) fn path_reads(
    intent: &str,
    hits: &[RetrievedCapability],
    catalogs: &BTreeMap<String, CGS>,
    allowed: &DiscoveryAuthorization,
) -> BTreeMap<CapabilityRef, &'static str> {
    let text = IntentText::parse(intent);
    let terms = intent_terms(&text.outer);
    // A slash-qualified name is evidence for a compound identity. It is not
    // an executable binding. Exclude URLs and filesystem paths.
    let key_widths: BTreeSet<_> = text
        .outer
        .split_whitespace()
        .filter_map(|word| {
            let name =
                word.trim_matches(|c: char| matches!(c, ',' | '.' | ';' | '?' | '!' | '(' | ')'));
            let (path, ordinal) = name
                .split_once('#')
                .map_or((name, None), |(p, n)| (p, Some(n)));
            let valid = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    && s != "."
                    && s != ".."
            };
            let (owner, item) = path.split_once('/')?;
            (valid(owner)
                && valid(item)
                && ordinal.is_none_or(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())))
            .then_some(if ordinal.is_some() { 3 } else { 2 })
        })
        .collect();
    let ordered = terms.iter().any(|t| {
        matches!(
            t.as_str(),
            "latest" | "newest" | "earliest" | "oldest" | "recent"
        )
    });
    let has_literal = text.outer.split_whitespace().any(|token| {
        let w = token.trim_matches(|c: char| !c.is_alphanumeric());
        w.len() >= 3
            && w.chars().all(|c| c.is_ascii_alphanumeric())
            && w.bytes().any(|b| b.is_ascii_digit())
            && w.bytes().any(|b| b.is_ascii_alphabetic())
    });
    let distinct_sources = has_literal && (ordered || !key_widths.is_empty());
    let mut rank = BTreeMap::new();
    for (i, hit) in hits
        .iter()
        .take(super::CATALOG_OPPORTUNITY_WINDOW)
        .enumerate()
    {
        rank.entry((hit.reference.catalog.as_str(), hit.document.entity.as_str()))
            .or_insert(super::CATALOG_OPPORTUNITY_WINDOW - i);
    }
    let mut nominated = BTreeMap::new();
    let mut anchors = BTreeMap::new();
    let mut receivers = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (position, hit) in hits.iter().enumerate() {
        let entry = &hit.reference.catalog;
        let cgs = &catalogs[entry];
        let Some(entity) = cgs.entities.get(hit.document.entity.as_str()) else {
            continue;
        };
        if distinct_sources && terms.contains(&entity.name.as_str().to_lowercase()) {
            if let Some(q) = cgs.representative_query_for_projection(entity.name.as_str()) {
                let r = CapabilityRef {
                    catalog: entry.clone(),
                    capability: q.name.to_string(),
                };
                if allowed.permits(&r) {
                    nominated.insert(r, SOURCE);
                }
            }
        }
        let cap = &cgs.capabilities[hit.reference.capability.as_str()];
        if distinct_sources
            && position < super::CATALOG_OPPORTUNITY_WINDOW
            && !matches!(
                cap.kind,
                CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
            )
        {
            if let Some(receiver) = cap.receiver_entity() {
                if let Some(g) = cgs.primary_get_capability(receiver.as_str()) {
                    let r = CapabilityRef {
                        catalog: entry.clone(),
                        capability: g.name.to_string(),
                    };
                    if allowed.permits(&r) {
                        *receivers.entry(r).or_insert(0usize) +=
                            super::CATALOG_OPPORTUNITY_WINDOW - position;
                    }
                }
            }
        }
        if distinct_sources
            || ordered
            || entity.relations.is_empty()
            || !key_widths.contains(&entity.key_vars.len())
            || !seen.insert((entry, entity.name.as_str()))
            || cgs
                .resolved_primary_get_for_projection(entity.name.as_str(), entity)
                .is_none()
        {
            continue;
        }
        let score = entity
            .relations
            .values()
            .map(|rel| {
                rank.get(&(entry.as_str(), rel.target_resource.as_str()))
                    .copied()
                    .unwrap_or_default()
            })
            .sum::<usize>();
        let slot = anchors
            .entry(entity.key_vars.len())
            .or_insert((0, entry, entity));
        if score > slot.0 {
            *slot = (score, entry, entity);
        }
    }
    if let Some((r, _)) = receivers.into_iter().max_by_key(|(_, score)| *score) {
        nominated.insert(r, SOURCE);
    }
    for (_, entry, entity) in anchors.values() {
        let cgs = &catalogs[*entry];
        let mut add = |name: &str| {
            let r = CapabilityRef {
                catalog: (*entry).clone(),
                capability: name.into(),
            };
            if allowed.permits(&r) {
                nominated.entry(r).or_insert(NAVIGATION);
            }
        };
        // A two-part name can simply scope a different request. Rank its
        // parent lookup as a path only with evidence for the destination.
        if entity.key_vars.len() == 2
            && !entity.relations.values().any(|rel| {
                rel.cardinality == plasm_core::schema::Cardinality::One
                    && cgs
                        .primary_get_capability(rel.target_resource.as_str())
                        .is_some_and(|read| {
                            plasm_core::domain_lexicon::raw_words(&format!(
                                "{} {} {}",
                                rel.name, rel.target_resource, read.description
                            ))
                            .iter()
                            .any(|word| {
                                word.len() > 3
                                    && terms.contains(word)
                                    && !matches!(
                                        word.as_str(),
                                        "read"
                                            | "return"
                                            | "resolve"
                                            | "fetch"
                                            | "with"
                                            | "from"
                                            | "record"
                                            | "public"
                                            | "identified"
                                    )
                            })
                        })
            })
        {
            continue;
        }
        if let Some(g) = cgs.resolved_primary_get_for_projection(entity.name.as_str(), entity) {
            add(g.name.as_str());
        }
        for rel in entity.relations.values() {
            match &rel.materialize {
                Some(
                    RelationMaterialization::QueryScoped { capability, .. }
                    | RelationMaterialization::QueryScopedBindings { capability, .. },
                ) if entity.key_vars.len() > 2 => add(capability.as_str()),
                _ if rel.cardinality == plasm_core::schema::Cardinality::One
                    && rank.contains_key(&(entry.as_str(), rel.target_resource.as_str())) =>
                {
                    if let Some(g) = cgs.primary_get_capability(rel.target_resource.as_str()) {
                        add(g.name.as_str())
                    }
                }
                _ => {}
            }
        }
    }
    nominated
}
