//! Candidate admission and ordering before bounded catalogue evidence is assembled.
use super::{
    CapabilityRef, DiscoveryAuthorization, IntentText, RetrievedCapability,
    CATALOG_OPPORTUNITY_WINDOW, CGS,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn intent_terms(intent: &str) -> BTreeSet<String> {
    intent
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub(super) fn names_entity(terms: &BTreeSet<String>, name: &str) -> bool {
    let name = name.to_lowercase();
    terms.contains(&name) || terms.contains(&format!("{name}s"))
}

/// Nominate a catalogue collection for assessment when the current request
/// names its plural subject. Quoted arguments cannot nominate operations.
pub(crate) fn named_collection_queries(
    request: &str,
    catalogs: &BTreeMap<String, CGS>,
    allowed: &DiscoveryAuthorization,
) -> BTreeSet<CapabilityRef> {
    let outer = IntentText::parse(request);
    let words = QualifiedSubjects::new(&outer.outer).words;
    let mut queries = BTreeSet::new();
    for (catalog, cgs) in catalogs {
        for entity in cgs.entities.values() {
            let mut phrase = plasm_core::domain_lexicon::raw_words(entity.name.as_str());
            let Some(noun) = phrase.last_mut() else {
                continue;
            };
            if noun.ends_with('y')
                && noun
                    .chars()
                    .rev()
                    .nth(1)
                    .is_some_and(|c| !"aeiou".contains(c))
            {
                noun.pop();
                noun.push_str("ies");
            } else if ["s", "sh", "ch", "x", "z"]
                .iter()
                .any(|ending| noun.ends_with(ending))
            {
                noun.push_str("es");
            } else {
                noun.push('s');
            }
            if !words.windows(phrase.len()).any(|span| span == phrase) {
                continue;
            }
            if let Some(query) = entity
                .primary_query
                .as_deref()
                .and_then(|id| cgs.capabilities.get(id))
                .or_else(|| cgs.representative_query_for_projection(entity.name.as_str()))
            {
                let reference = CapabilityRef {
                    catalog: catalog.clone(),
                    capability: query.name.to_string(),
                };
                if allowed.permits(&reference) {
                    queries.insert(reference);
                }
            }
        }
    }
    queries
}

/// Stronger than an unqualified entity mention: the request names the catalogue
/// immediately before its subject. Keep the following literal for keyed access.
pub(super) struct QualifiedSubjects {
    words: Vec<String>,
}

impl QualifiedSubjects {
    pub(super) fn new(intent: &str) -> Self {
        Self {
            words: intent
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(str::to_lowercase)
                .collect(),
        }
    }

    fn subjects<'a>(&'a self, catalog: &str) -> impl Iterator<Item = (&'a str, Option<&'a str>)> {
        let catalog = catalog
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        (0..self.words.len()).filter_map(move |start| {
            let mut remaining = catalog.as_str();
            for (i, word) in self.words.iter().enumerate().skip(start) {
                remaining = remaining.strip_prefix(word.as_str())?;
                if remaining.is_empty() {
                    return Some((
                        self.words.get(i + 1)?.as_str(),
                        self.words.get(i + 2).map(String::as_str),
                    ));
                }
            }
            None
        })
    }

    pub(super) fn qualified_roles<'a>(
        &'a self,
        candidate: &'a RetrievedCapability,
    ) -> impl Iterator<Item = &'a str> {
        let contract = candidate
            .document
            .operation
            .contract
            .split('.')
            .next()
            .unwrap_or_default();
        self.subjects(&candidate.reference.catalog)
            .filter_map(move |(subject, _)| {
                let matches = candidate.document.operation.kind == plasm_core::CapabilityKind::Get
                    && contract
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|term| term.eq_ignore_ascii_case(subject));
                matches.then_some(subject)
            })
    }

    pub(super) fn names_numbered_record(&self, candidate: &RetrievedCapability) -> bool {
        candidate.document.operation.kind == plasm_core::CapabilityKind::Get
            && self
                .subjects(&candidate.reference.catalog)
                .any(|(subject, literal)| {
                    subject.eq_ignore_ascii_case(&candidate.document.entity)
                        && literal.is_some_and(|key| key.bytes().all(|b| b.is_ascii_digit()))
                })
    }
}

/// Reserve the highest-ranked complementary read for a named collection in each catalogue.
pub(super) fn admission_preferences(
    intent: &str,
    candidates: &[RetrievedCapability],
    catalogs: &BTreeMap<String, CGS>,
    mut preferred: BTreeSet<CapabilityRef>,
) -> BTreeSet<CapabilityRef> {
    let qualified = QualifiedSubjects::new(intent);
    let terms = intent_terms(intent);
    let mut roles = BTreeMap::<_, Vec<_>>::new();
    let mut named_collections = BTreeMap::<_, BTreeSet<_>>::new();
    let mut reads = Vec::new();
    for candidate in candidates {
        // An explicit search names its catalogue before supplying the query.
        // Prioritise its Search contracts for assessment, never for teaching.
        if candidate.document.operation.kind == plasm_core::CapabilityKind::Search
            && qualified.words.windows(3).any(|words| {
                words[0] == "search"
                    && words[1].eq_ignore_ascii_case(&candidate.reference.catalog)
                    && words[2] == "for"
            })
        {
            preferred.insert(candidate.reference.clone());
        }
        for role in qualified
            .qualified_roles(candidate)
            .collect::<BTreeSet<_>>()
        {
            roles
                .entry((&candidate.reference.catalog, role))
                .or_default()
                .push(candidate);
        }
        if !matches!(
            candidate.document.operation.kind,
            plasm_core::CapabilityKind::Get
                | plasm_core::CapabilityKind::Query
                | plasm_core::CapabilityKind::Search
        ) {
            continue;
        }
        let Some(key) = record_identity(candidate, catalogs) else {
            continue;
        };
        reads.push((candidate, key.clone()));
        if candidate.document.operation.kind == plasm_core::CapabilityKind::Query
            && names_entity(&terms, &candidate.document.entity)
        {
            named_collections
                .entry(key)
                .or_default()
                .insert(&candidate.document.entity);
        }
    }
    for ((_, role), matches) in roles {
        if let [candidate] = matches.as_slice() {
            if !candidate.document.entity.eq_ignore_ascii_case(role) {
                preferred.insert(candidate.reference.clone());
            }
        }
    }
    // A domain reused by several other entities cannot identify which of
    // their reads complements this collection. Leave those in recall order.
    named_collections.retain(|(catalog, domain), collections| {
        catalogs[catalog]
            .entities
            .values()
            .filter(|entity| {
                !collections
                    .iter()
                    .any(|name| name.as_str() == entity.name.as_str())
                    && entity
                        .fields
                        .get(&entity.id_field)
                        .is_some_and(|field| field.kind.registry_key().as_str() == domain)
            })
            .take(2)
            .count()
            == 1
    });
    let mut represented = BTreeSet::new();
    for (candidate, key) in reads {
        if named_collections.get(&key).is_some_and(|entities| {
            entities
                .iter()
                .any(|entity| *entity != &candidate.document.entity)
        }) && represented.insert(&candidate.reference.catalog)
        {
            preferred.insert(candidate.reference.clone());
        }
    }
    preferred
}

pub(super) fn record_identity(
    candidate: &RetrievedCapability,
    catalogs: &BTreeMap<String, CGS>,
) -> Option<(String, String)> {
    let catalog = &candidate.reference.catalog;
    let entity = catalogs
        .get(catalog)?
        .entities
        .get(candidate.document.entity.as_str())?;
    let field = entity.fields.get(&entity.id_field)?;
    Some((
        catalog.clone(),
        field.kind.registry_key().as_str().to_owned(),
    ))
}

/// Preserve recall's whole-intent ranking after giving each recalled catalogue
/// one initial opportunity. Exposed and repeatedly deferred references advance
/// without becoming relevance judgments.
pub(super) fn order_candidates<'a>(
    candidates: &'a [RetrievedCapability],
    intent: &str,
    deferred: &BTreeMap<CapabilityRef, usize>,
    exposed: &[CapabilityRef],
    preferred: &BTreeSet<CapabilityRef>,
) -> Vec<&'a RetrievedCapability> {
    let mut ranked: Vec<_> = candidates.iter().collect();
    let terms = intent_terms(intent);
    ranked.sort_by_cached_key(|candidate| {
        let named = names_entity(&terms, &candidate.document.entity);
        (
            !exposed.contains(&candidate.reference),
            deferred.get(&candidate.reference).copied().unwrap_or(0) < 3,
            !named,
        )
    });
    let mut ordered = Vec::with_capacity(ranked.len());
    for priority in [true, false] {
        let mut seen = BTreeSet::new();
        let mut read_families = BTreeSet::new();
        let mut first = Vec::new();
        let mut rest = Vec::new();
        for (rank, candidate) in ranked.iter().copied().enumerate().filter(|(_, candidate)| {
            let important = preferred.contains(&candidate.reference)
                || exposed.contains(&candidate.reference)
                || deferred.get(&candidate.reference).copied().unwrap_or(0) >= 3;
            important == priority
        }) {
            if rank < CATALOG_OPPORTUNITY_WINDOW
                && matches!(
                    candidate.document.operation.kind,
                    plasm_core::CapabilityKind::Query | plasm_core::CapabilityKind::Search
                )
            {
                read_families.insert((
                    candidate.reference.catalog.clone(),
                    candidate.document.entity.clone(),
                ));
            }
            if rank < CATALOG_OPPORTUNITY_WINDOW && seen.insert(&candidate.reference.catalog) {
                first.push(candidate);
            } else {
                rest.push(candidate);
            }
        }
        // A keyed and a collection read of one entity are distinct operations.
        // Keep their evidence together so the model can judge each separately.
        let (siblings, others): (Vec<_>, Vec<_>) = rest.into_iter().partition(|candidate| {
            candidate.document.operation.kind == plasm_core::CapabilityKind::Get
                && read_families.contains(&(
                    candidate.reference.catalog.clone(),
                    candidate.document.entity.clone(),
                ))
        });
        first.extend(siblings);
        first.extend(others);
        ordered.extend(first);
    }
    // In a federated request, reserve packet space for distinct access paths
    // in the named catalogues. Use the original retrieval rank within each
    // (catalogue, kind), before the keyed-sibling preference above can turn
    // several opportunities into variants of the same lookup.
    let named: BTreeSet<_> = candidates
        .iter()
        .map(|c| &c.reference.catalog)
        .filter(|name| terms.contains(&name.to_lowercase()))
        .collect();
    if named.len() > 1 {
        let mut kinds = BTreeSet::new();
        let mut opportunities = BTreeMap::new();
        for (rank, c) in candidates
            .iter()
            .take(CATALOG_OPPORTUNITY_WINDOW)
            .enumerate()
        {
            if named.contains(&c.reference.catalog)
                && kinds.insert((&c.reference.catalog, c.document.operation.kind.as_str()))
            {
                opportunities.insert(&c.reference, rank);
            }
        }
        ordered.sort_by_key(|c| {
            opportunities
                .get(&c.reference)
                .copied()
                .unwrap_or(usize::MAX)
        });
    }
    // A named operation deserves assessment before heuristic catalogue diversity.
    // Use only the outer request. Quoted payloads can contain catalogue identifiers.
    let identifiers: BTreeSet<_> = intent
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '.' | ':')))
        .map(|word| word.trim_matches(['.', ':']))
        .filter(|word| word.contains('/') || word.contains('_'))
        .collect();
    if !identifiers.is_empty() {
        let mut counts = BTreeMap::new();
        for candidate in candidates {
            *counts
                .entry(candidate.reference.capability.as_str())
                .or_insert(0usize) += 1;
        }
        ordered.sort_by_cached_key(|candidate| {
            let cap = candidate.reference.capability.as_str();
            let qualified = format!("{}/{cap}", candidate.reference.catalog);
            !(identifiers.contains(qualified.as_str())
                || (cap.contains('_') && counts.get(cap) == Some(&1) && identifiers.contains(cap)))
        });
    }
    ordered
}
