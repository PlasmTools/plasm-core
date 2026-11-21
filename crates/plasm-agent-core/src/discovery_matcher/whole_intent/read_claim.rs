//! Ask what makes a read necessary given the access paths in this packet.
//! Alternatives inform the decision; they confer no authorization or binding.
use super::{choice_question, AccessQuestion};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

fn names(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// A candidate record key can already be used as another read's scope.
/// Keep bindings together by consumer so unrelated partial keys never combine.
pub(super) fn scope_sources<'a>(
    bindings: &'a Map<String, Value>,
    cards: &[Value],
) -> BTreeSet<&'a str> {
    let reads: BTreeSet<_> = cards
        .iter()
        .filter(|card| {
            matches!(
                card["operation"]["kind"].as_str(),
                Some("get" | "query" | "search")
            )
        })
        .filter_map(|card| card["id"].as_str())
        .collect();
    let mut by_pair = BTreeMap::<_, BTreeSet<_>>::new();
    for binding in bindings.values().filter(|b| {
        b["kind"] == "input_source"
            && b.pointer("/to/input/lane").and_then(Value::as_str) == Some("scope")
    }) {
        let (Some(source), Some(field), Some(target)) = (
            binding.pointer("/from/capability").and_then(Value::as_str),
            binding.pointer("/from/field").and_then(Value::as_str),
            binding.pointer("/to/capability").and_then(Value::as_str),
        ) else {
            continue;
        };
        if source != target && reads.contains(target) {
            by_pair.entry((source, target)).or_default().insert(field);
        }
    }
    let keys: BTreeMap<_, _> = cards
        .iter()
        .filter_map(|card| {
            Some((
                card["id"].as_str()?,
                names(card.pointer("/declaration/record_key")?),
            ))
        })
        .collect();
    by_pair
        .into_iter()
        .filter_map(|((source, _), fields)| {
            keys.get(source)
                .is_some_and(|key| !key.is_empty() && key.iter().all(|f| fields.contains(f)))
                .then_some(source)
        })
        .collect()
}

#[derive(Default)]
pub(super) struct ReadAlternatives<'a> {
    pub supplies_key_fields: bool,
    pub scoped_queries: &'a [&'a Value],
    pub has_collection: bool,
}

/// A query may take an entity reference as a single scope argument, rather than
/// consuming the individual key fields through separate graph edges.
pub(super) fn scope_records<'a>(
    cards: &'a [Value],
    values: &'a Map<String, Value>,
) -> BTreeMap<(&'a str, &'a str), Vec<&'a Value>> {
    let mut targets = BTreeMap::<_, Vec<_>>::new();
    for card in cards.iter().filter(|c| c["operation"]["kind"] == "query") {
        for input in card["declaration"]["scope"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let (Some(catalog), Some(value)) =
                (card["catalog"].as_str(), input["value_ref"].as_str())
            else {
                continue;
            };
            let Some(target) = values
                .get(&super::super::evidence::key(catalog, value))
                .and_then(|v| v.pointer("/field_type/entity_ref"))
            else {
                continue;
            };
            if let (Some(entry), Some(entity)) =
                (target["entry_id"].as_str(), target["target"].as_str())
            {
                targets.entry((entry, entity)).or_default().push(card);
            }
        }
    }
    targets
}

/// Name the owner of a shared field when the catalogue offers a scoped relation.
fn related_record_question(
    queries: &[&Value],
    properties: &[&str],
    intent: &str,
    entity: &str,
    meaning: &str,
) -> Option<String> {
    let children: BTreeSet<_> = queries
        .iter()
        .filter(|query| {
            query["declaration"]["relation_uses"]
                .as_array()
                .is_some_and(|uses| !uses.is_empty())
        })
        .filter(|query| {
            names(&query["declaration"]["provides"])
                .iter()
                .any(|field| properties.contains(field))
        })
        .filter_map(|query| query["entity"].as_str())
        .collect();
    if children.is_empty() {
        return None;
    }
    let children = children.into_iter().collect::<Vec<_>>().join(", ");
    Some(format!("Premise: {intent}\nHypothesis: In addition to any related {children} rows, the user requests this separate {entity} operation: {meaning}"))
}

/// Prefer the first wider lookup on the same key, then the first collection
/// that returns all requested properties. Catalogue order breaks ties.
enum CoveringRead<'a> {
    Lookup(&'a str),
    Collection(&'a str),
}

fn covering_read<'a>(
    peers: impl Iterator<Item = &'a Value>,
    declaration: &Value,
    keys: &[&str],
    fields: &[&str],
) -> Option<CoveringRead<'a>> {
    if fields.is_empty() {
        return None;
    }
    let mut collection = None;
    for peer in peers {
        let other = &peer["declaration"];
        let provided = names(&other["provides"]);
        let meaning = peer["meaning"].as_str().unwrap_or_default();
        match peer["operation"]["kind"].as_str() {
            Some("get")
                if other["record_key"] == declaration["record_key"]
                    && provided.len() > fields.len()
                    && fields.iter().all(|f| provided.contains(f)) =>
            {
                return Some(CoveringRead::Lookup(meaning));
            }
            Some("query")
                if collection.is_none()
                    && !provided.is_empty()
                    && fields
                        .iter()
                        .all(|f| keys.contains(f) || provided.contains(f)) =>
            {
                collection = Some(CoveringRead::Collection(meaning));
            }
            _ => {}
        }
    }
    collection
}

pub(super) fn question(
    access: AccessQuestion,
    intent: &str,
    catalog: &str,
    entity: &str,
    card: &Value,
    cards: &[Value],
    alternatives: ReadAlternatives<'_>,
) -> Option<Value> {
    let peers = cards
        .iter()
        .filter(|p| p["catalog"] == catalog && p["entity"] == entity && p["id"] != card["id"]);
    let meaning = card["meaning"].as_str().unwrap_or_default();
    let distinguish_search = matches!(access, AccessQuestion::Search)
        && peers.clone().any(|p| p["operation"]["kind"] == "search");
    let (mut instructions, yes, no) = match access {
        AccessQuestion::Search if peers.clone().any(|p| p["operation"]["kind"] == "get") => (
            format!("Premise: {intent}\nHypothesis: The user wants to discover matching {entity} records in {catalog} beyond any explicitly identified {entity}. The result of this search is a set of {entity} records, not records related to one identified {entity}."),
            format!("The requested matches are {entity} records and {catalog} is their requested search source."),
            format!("The request identifies the {entity} to read, concerns its related data, or asks another source to search.")
        ),
        AccessQuestion::Search if alternatives.has_collection => (
            format!("User request: {intent}\nDoes the user request a search for matching {entity} records in {catalog} itself? Using an identified {entity} as scope for other records, or searching another source for information about it, does not request {entity} matches."),
            format!("The requested matches are {entity} records and {catalog} is their requested search source."),
            "The premise does not request or imply this access path.".into()
        ),
        AccessQuestion::Search => (
            format!("Premise: {intent}\nHypothesis: The user wants {catalog} to find additional sources or matching {entity} records. Merely retrieving material at a known or linked address would not satisfy that part of the request."),
            format!("The request asks {catalog} to find additional matches. A search need not prove its findings."), "The premise does not request or imply this access path.".into()
        ),
        AccessQuestion::Lookup => {
            let declaration = &card["declaration"];
            let keys = names(&declaration["record_key"]);
            let fields = names(&declaration["provides"]);
            let alternative = covering_read(peers, declaration, &keys, &fields);
            let non_key_fields = fields.iter().filter(|field| !keys.contains(field)).copied().collect::<Vec<_>>();
            let keys = keys.join(", ");
            let relation_question = || related_record_question(alternatives.scoped_queries, &non_key_fields, intent, entity, meaning);
            if let Some(CoveringRead::Lookup(broader)) = alternative {
                (relation_question().unwrap_or_else(|| format!("User request: {intent}\nDoes the user need an existing {catalog} {entity} as an input and specifically request the narrower representation from this operation: {meaning}? Available broader read for the same record key: {broader}")),
                "The request identifies an existing source record and specifically needs this representation, its access constraints, or another distinct property of this read.".into(),
                "The broader read already supplies the requested information, or the user does not need either read.".into())
            } else if let Some(CoveringRead::Collection(alternative)) = alternative {
                (format!("Read this request: {intent}\nDoes the user supply or independently identify a particular {catalog} {entity} for lookup by {keys}? A record picked from the result of this collection would already contain the same data: {alternative}"),
                "An independently identified record is requested, or a separate retrieval of individual records is explicitly required.".into(),
                "The request identifies members by their membership, ordering or filtering within a collection, or does not need this record.".into())
            } else if alternatives.supplies_key_fields {
                let properties = if non_key_fields.is_empty() { String::new() } else {format!(" Returned properties: {}.", non_key_fields.join(", "))};
                (format!("Premise: {intent}\nHypothesis: Information from an existing {catalog} {entity} record is required, beyond its key ({keys}). Using a supplied key to scope other records requires no lookup of the scope itself. Operation: {meaning}{properties}"),
                "This operation obtains a requested record, a requested property, or an input not already supplied.".into(),
                "The entity merely provides scope, is a destination, or its data is outside the request.".into())
            } else if !alternatives.scoped_queries.is_empty() {
                let instructions = relation_question().unwrap_or_else(|| format!("Request: {intent}\nIs the {catalog} {entity} itself the object to read? A user-supplied identifier can locate related records directly. Fetch this record only for its own requested data or to obtain a value the workflow otherwise lacks."));
                (instructions,
                format!("The workflow identifies a particular existing {entity} as a source of requested data."), format!("The workflow has no such existing source record. Mentioning or creating a destination does not designate a record for this lookup."))
            } else {
                (format!("Does the request designate an existing {catalog} {entity} to retrieve by {keys}, either directly or through an upstream reference? Determine whether this is a source record in the workflow. User request: {intent}."),
                format!("The workflow identifies a particular existing {entity} as a source of requested data."), format!("The workflow has no such existing source record. Mentioning or creating a destination does not designate a record for this lookup."))
            }
        }
        _ => return None,
    };
    if distinguish_search {
        instructions.push_str(&format!(
            " Search contract: {}.",
            card["operation"]["contract"].as_str().unwrap_or(meaning)
        ));
    }
    Some(choice_question(instructions, access, &yes, &no))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aggregate_scope_uses_the_declared_reference_target() {
        let cards = [
            json!({"catalog":"a","operation":{"kind":"query"},"declaration":{"scope":[{"value_ref":"parent"}]}}),
        ];
        let values = Map::from_iter([
            (
                "a/parent".into(),
                json!({"field_type":{"entity_ref":{"entry_id":"b","target":"Group"}}}),
            ),
            (
                "b/parent".into(),
                json!({"field_type":{"entity_ref":{"entry_id":"a","target":"Other"}}}),
            ),
        ]);
        assert_eq!(
            scope_records(&cards, &values),
            BTreeMap::from([(("b", "Group"), vec![&cards[0]])])
        );
    }

    #[test]
    fn shared_property_is_attributed_to_the_related_record_even_with_a_wider_lookup() {
        let lookup = json!({"id":"a","catalog":"matrix","entity":"Group","meaning":"Read the group note.",
            "operation":{"kind":"get"},"declaration":{"record_key":["id"],"provides":["id","note"]}});
        let mut wider = lookup.clone();
        wider["id"] = json!("b");
        wider["declaration"]["provides"] = json!(["id", "note", "owner"]);
        let members = json!({"entity":"Member","declaration":{"provides":["note"],"relation_uses":["Group.members"]}});
        let q = question(
            AccessQuestion::Lookup,
            "Return member notes.",
            "matrix",
            "Group",
            &lookup,
            &[lookup.clone(), wider],
            ReadAlternatives {
                scoped_queries: &[&members],
                ..Default::default()
            },
        )
        .unwrap();
        let claim = q["instructions"].as_str().unwrap();
        assert!(claim.contains("related Member rows"));
        assert!(claim.contains("separate Group operation: Read the group note."));
        assert!(related_record_question(&[&members], &["owner"], "", "Group", "").is_none());
    }

    #[test]
    fn scope_supplier_question_distinguishes_returned_properties_from_keys() {
        let card = json!({"id":"c0","catalog":"matrix","entity":"Group",
            "operation":{"kind":"get"},"declaration":{"record_key":["id"],"provides":["id","owner","title"]}});
        let rendered = question(
            AccessQuestion::Lookup,
            "Find the group owner and list its members.",
            "matrix",
            "Group",
            &card,
            std::slice::from_ref(&card),
            ReadAlternatives {
                supplies_key_fields: true,
                ..Default::default()
            },
        )
        .unwrap();
        let hypothesis = rendered["instructions"].as_str().unwrap();
        assert!(hypothesis.contains("key (id)"));
        assert!(hypothesis.contains("Returned properties: owner, title."));
    }

    #[test]
    fn alternative_requires_same_catalog_and_coverage_of_non_key_fields() {
        let lookup = json!({"id":"c0","catalog":"alpha","entity":"Item","meaning":"Read one item.",
            "operation":{"kind":"get"},"declaration":{"record_key":["scope","id"],"provides":["scope","id","title","detail"]}});
        let mut list = json!({"id":"c1","catalog":"alpha","entity":"Item","meaning":"List scoped items.",
            "operation":{"kind":"query"},"declaration":{"provides":["id","title"]}});
        let render = |peer| {
            question(
                AccessQuestion::Lookup,
                "Read item 3",
                "alpha",
                "Item",
                &lookup,
                &[lookup.clone(), peer],
                ReadAlternatives::default(),
            )
            .unwrap()
        };
        assert!(!render(list.clone())["instructions"]
            .as_str()
            .unwrap()
            .contains("List scoped items."));
        list["declaration"]["provides"] = json!(["id", "title", "detail"]);
        assert!(render(list.clone())["instructions"]
            .as_str()
            .unwrap()
            .contains("List scoped items."));
        list["catalog"] = json!("other");
        assert!(!render(list)["instructions"]
            .as_str()
            .unwrap()
            .contains("List scoped items."));
    }
}
