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
pub(super) struct ReadAlternatives {
    pub supplies_key_fields: bool,
    pub used_as_scope_record: bool,
    pub has_collection: bool,
}

/// A query may take an entity reference as a single scope argument, rather than
/// consuming the individual key fields through separate graph edges.
pub(super) fn scope_records<'a>(
    cards: &[Value],
    values: &'a Map<String, Value>,
) -> BTreeSet<(&'a str, &'a str)> {
    let mut targets = BTreeSet::new();
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
                targets.insert((entry, entity));
            }
        }
    }
    targets
}

pub(super) fn question(
    access: AccessQuestion,
    intent: &str,
    catalog: &str,
    entity: &str,
    card: &Value,
    cards: &[Value],
    alternatives: ReadAlternatives,
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
            format!("The request asks {catalog} to find additional matches. The query topic may be supplied by another outcome."), "The premise does not request or imply this access path.".into()
        ),
        AccessQuestion::Lookup => {
            let declaration = &card["declaration"];
            let keys = names(&declaration["record_key"]);
            let fields = names(&declaration["provides"]);
            let mut collection = None;
            let mut wider = None;
            for peer in peers {
                let other = &peer["declaration"];
                let provided = names(&other["provides"]);
                if fields.is_empty() || provided.is_empty() { continue; }
                let description = peer["meaning"].as_str().unwrap_or_default();
                if peer["operation"]["kind"] == "query" && fields.iter().all(|f| provided.contains(f) || keys.contains(f)) {
                    collection.get_or_insert(description);
                }
                if peer["operation"]["kind"] == "get" && other["record_key"] == declaration["record_key"] && provided.len() > fields.len() && fields.iter().all(|f|provided.contains(f)) {
                    wider.get_or_insert(description);
                }
            }
            let keys = keys.join(", ");
            if let Some(broader) = wider {
                (format!("User request: {intent}\nDoes the user need an existing {catalog} {entity} as an input and specifically request the narrower representation from this operation: {meaning}? Available broader read for the same record key: {broader}"),
                "The request identifies an existing source record and specifically needs this representation, its access constraints, or another distinct property of this read.".into(),
                "The broader read already supplies the requested information, or the user does not need either read.".into())
            } else if let Some(alternative) = collection {
                (format!("Read this request: {intent}\nDoes the user supply or independently identify a particular {catalog} {entity} for lookup by {keys}? A record picked from the result of this collection would already contain the same data: {alternative}"),
                "An independently identified record is requested, or a separate retrieval of individual records is explicitly required.".into(),
                "The request identifies members by their membership, ordering or filtering within a collection, or does not need this record.".into())
            } else if alternatives.supplies_key_fields {
                (format!("Premise: {intent}\nHypothesis: Information from an existing {catalog} {entity} record is required, beyond its key ({keys}). Using a supplied key to scope other records requires no lookup of the scope itself. Operation: {meaning}"),
                "This operation obtains a requested record, a requested property, or an input not already supplied.".into(),
                "The entity merely provides scope, is a destination, or its data is outside the request.".into())
            } else if alternatives.used_as_scope_record {
                (format!("Request: {intent}\nIs the {catalog} {entity} itself the object to read? A user-supplied identifier can locate related records directly. Fetch this record only for its own requested data or to obtain a value the workflow otherwise lacks."),
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
            BTreeSet::from([("b", "Group")])
        );
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
