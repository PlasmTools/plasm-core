//! Direct catalogue bindings; no transitive peer expansion or inferred semantics.
use super::{evidence::CatalogEvidence, DiscoveryMatcherError};
use plasm_core::prerequisites::{
    ArgumentSource, CapabilityRef, InputPath, InputSourceCandidate, InputSourceTarget,
    PrerequisiteEdge, RowIdentity,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MAX_CAPABILITIES: usize = 8;
pub(super) const MAX_BYTES: usize = 16_000;
type Result<T> = std::result::Result<T, DiscoveryMatcherError>;

#[derive(Debug, Clone, Serialize)]
pub(super) struct Endpoint {
    pub capability: CapabilityRef,
    #[serde(flatten)]
    pub slot: Slot,
}
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(super) enum Slot {
    Entity { entity: String },
    Field { field: String },
    Input(InputSourceTarget),
    Identity { identity: RowIdentity },
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Binding {
    MayCreateResource {
        from: Endpoint,
        to: Endpoint,
    },
    InputSource {
        from: Endpoint,
        to: Endpoint,
        collect: bool,
    },
    Membership {
        from: Endpoint,
        to: Endpoint,
    },
    Prerequisite {
        from: Endpoint,
        to: Endpoint,
        arguments: BTreeMap<String, ArgumentSource>,
        provider_inputs: BTreeMap<String, InputPath>,
    },
}
impl Binding {
    pub fn endpoints(&self) -> [&Endpoint; 2] {
        match self {
            Self::InputSource { from, to, .. }
            | Self::MayCreateResource { from, to }
            | Self::Membership { from, to }
            | Self::Prerequisite { from, to, .. } => [from, to],
        }
    }
}
pub(super) fn bindings(
    sources: Vec<InputSourceCandidate>,
    prerequisites: &[PrerequisiteEdge],
) -> Result<Vec<Binding>> {
    let mut out = Vec::new();
    for source in sources {
        for binding in source.bindings {
            out.push(Binding::InputSource {
                from: Endpoint {
                    capability: binding.provider,
                    slot: Slot::Field {
                        field: binding.output_field,
                    },
                },
                to: Endpoint {
                    capability: binding.consumer,
                    slot: Slot::Input(binding.input),
                },
                collect: binding.collect,
            });
        }
        for member in source.membership {
            out.push(Binding::Membership {
                from: Endpoint {
                    capability: member.source,
                    slot: Slot::Identity {
                        identity: member.source_identity,
                    },
                },
                to: Endpoint {
                    capability: source.provider.clone(),
                    slot: Slot::Identity {
                        identity: member.provider_identity,
                    },
                },
            });
        }
    }
    for edge in prerequisites {
        if edge.requirement.bindings.is_empty() {
            return Err(invalid(
                &edge.consumer,
                "prerequisite has no output binding",
            ));
        }
        for binding in &edge.requirement.bindings {
            let field = edge
                .provider_outputs
                .get(&binding.output)
                .ok_or_else(|| invalid(&edge.provider, "prerequisite output"))?;
            out.push(Binding::Prerequisite {
                from: Endpoint {
                    capability: edge.provider.clone(),
                    slot: Slot::Field {
                        field: field.clone(),
                    },
                },
                to: Endpoint {
                    capability: edge.consumer.clone(),
                    slot: Slot::Input(InputSourceTarget::Argument {
                        input: binding.input.clone(),
                    }),
                },
                arguments: edge.requirement.arguments.clone(),
                provider_inputs: edge.provider_inputs.clone(),
            });
        }
    }
    // Canonical serialized keys deduplicate identical authored edges, not semantic guesses.
    let mut sorted = BTreeMap::new();
    for edge in out {
        sorted.insert(encode(&edge)?, edge);
    }
    Ok(sorted.into_values().collect())
}
fn invalid(reference: &CapabilityRef, part: &'static str) -> DiscoveryMatcherError {
    DiscoveryMatcherError::InvalidWitness {
        reference: reference.clone(),
        part,
    }
}
pub(super) fn value<T: Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v)
        .map_err(crate::decision_codec::DecisionCodecError::from)
        .map_err(DiscoveryMatcherError::Encode)
}
fn encode<T: Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v)
        .map_err(crate::decision_codec::DecisionCodecError::from)
        .map_err(DiscoveryMatcherError::Encode)
}
fn required<'a>(
    v: &'a Value,
    key: &str,
    reference: &CapabilityRef,
    part: &'static str,
) -> Result<&'a Value> {
    v.get(key).ok_or_else(|| invalid(reference, part))
}

/// The serialized metadata below comes only from typed catalogue evidence. Projecting
/// the existing wire schema preserves defaults/nulls/variant metadata verbatim.
fn input_fields(
    shape: &Value,
    path: &[String],
    context: Vec<Value>,
    reference: &CapabilityRef,
) -> Result<Vec<Value>> {
    let mut metadata = shape
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(reference, "input shape"))?;
    for key in ["fields", "variants", "element_type"] {
        metadata.remove(key);
    }
    let mut context = context;
    context.push(Value::Object(metadata));
    match shape.get("type").and_then(Value::as_str) {
        Some("object") => {
            let name = path
                .first()
                .ok_or_else(|| invalid(reference, "empty input path"))?;
            let fields = required(shape, "fields", reference, "input fields")?
                .as_array()
                .ok_or_else(|| invalid(reference, "input fields"))?;
            let Some(field) = fields
                .iter()
                .find(|f| f.get("name").and_then(Value::as_str) == Some(name))
            else {
                return Ok(vec![json!({"present":false,"context":context})]);
            };
            if path.len() == 1 {
                return Ok(vec![
                    json!({"present":true,"field":field,"context":context}),
                ]);
            }
            let mut parent = field
                .as_object()
                .cloned()
                .ok_or_else(|| invalid(reference, "input field"))?;
            let nested = parent
                .remove("input_type")
                .ok_or_else(|| invalid(reference, "nested input type"))?;
            context.push(json!({"field":parent}));
            input_fields(&nested, &path[1..], context, reference)
        }
        Some("union") => {
            let variants = required(shape, "variants", reference, "input variants")?
                .as_array()
                .ok_or_else(|| invalid(reference, "input variants"))?;
            let mut out = Vec::new();
            for variant in variants {
                let mut object = variant
                    .as_object()
                    .cloned()
                    .ok_or_else(|| invalid(reference, "input variant"))?;
                object.insert("type".into(), json!("object"));
                out.extend(input_fields(
                    &Value::Object(object),
                    path,
                    context.clone(),
                    reference,
                )?);
            }
            Ok(out)
        }
        Some("array") => input_fields(
            required(shape, "element_type", reference, "array element")?,
            if path.first().is_some_and(|p| p == "*") {
                &path[1..]
            } else {
                path
            },
            context,
            reference,
        ),
        _ => Err(invalid(reference, "unsupported input shape")),
    }
}
fn input_slot(declaration: &Value, input: &InputPath, reference: &CapabilityRef) -> Result<Value> {
    use plasm_core::prerequisites::InputLane;
    let lane = match input.lane {
        InputLane::Arguments => "arguments",
        InputLane::Payload => "payload",
        InputLane::Scope => "scope",
        InputLane::Selection => "selection",
        InputLane::Controls => "controls",
    };
    let block = required(declaration, lane, reference, "input lane")?;
    let mut result = match input.lane {
        InputLane::Arguments | InputLane::Payload => {
            let fields = input_fields(
                required(block, "input_type", reference, "input type")?,
                &input.path,
                vec![],
                reference,
            )?;
            if !fields.iter().any(|f| f["present"] == true) {
                return Err(invalid(reference, "input absent from every variant"));
            }
            let mut metadata = block
                .as_object()
                .cloned()
                .ok_or_else(|| invalid(reference, "input lane"))?;
            metadata.remove("input_type");
            json!({"input":input,"fields":fields,"lane_metadata":metadata})
        }
        _ => {
            let fields = input_fields(
                &json!({"type":"object","fields":block}),
                &input.path,
                vec![],
                reference,
            )?;
            if !fields.iter().any(|f| f["present"] == true) {
                return Err(invalid(reference, "input field"));
            }
            json!({"input":input,"fields":fields})
        }
    };
    let encoded_input = value(input)?;
    if declaration
        .get("acquired_inputs")
        .and_then(Value::as_array)
        .is_some_and(|paths| paths.contains(&encoded_input))
    {
        result["acquired"] = json!(true);
    }
    Ok(result)
}

#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectedDefinitions {
    // Preserve the established wire-section order independently of JSON map order.
    #[serde(deserialize_with = "definition_map")]
    pub(super) values: Value,
    #[serde(deserialize_with = "definition_map")]
    pub(super) records: Value,
}
fn definition_map<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Value, D::Error> {
    let value = <Value as serde::Deserialize>::deserialize(deserializer)?;
    if !value.is_object() {
        return Err(serde::de::Error::custom(
            "definition dictionary must be an object",
        ));
    }
    Ok(value)
}
#[derive(Clone)]
pub(super) struct OrderedCards(Vec<(String, Value)>);
impl<'de> serde::Deserialize<'de> for OrderedCards {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct CardsVisitor;
        impl<'de> serde::de::Visitor<'de> for CardsVisitor {
            type Value = OrderedCards;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("ordered capability cards")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut cards = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    cards.push(entry);
                }
                Ok(OrderedCards(cards))
            }
        }
        deserializer.deserialize_map(CardsVisitor)
    }
}
impl Serialize for OrderedCards {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (id, card) in &self.0 {
            map.serialize_entry(id, card)?;
        }
        map.end()
    }
}
pub(super) struct Scope {
    pub cards: OrderedCards,
    pub types: ProjectedDefinitions,
    pub references: Value,
    pub bindings: Vec<Value>,
    pub count: usize,
}
pub(super) fn project(
    target: &CapabilityRef,
    evidence: &CatalogEvidence,
    ids: &BTreeMap<CapabilityRef, String>,
    definitions: &Value,
) -> Result<Scope> {
    let links: Vec<_> = evidence
        .bindings
        .iter()
        .filter(|b| match b {
            // Closure supplies this acquisition when a selected consumer needs
            // it. Its own judgment does not need every reverse dependency.
            Binding::Prerequisite { to, .. } => &to.capability == target,
            // The acquisition already declares its output identity. Keep the
            // acquisition witness on its receiver consumer, not every method
            // accepting that entity on the acquisition's own question.
            Binding::InputSource {
                from,
                to:
                    Endpoint {
                        slot: Slot::Input(InputSourceTarget::Receiver { .. }),
                        ..
                    },
                ..
            } => &b.endpoints()[1].capability == target && &from.capability != target,
            _ => b.endpoints().iter().any(|e| &e.capability == target),
        })
        .collect();
    project_links(target, evidence, ids, definitions, links)
}

pub(super) fn project_path(
    target: &CapabilityRef,
    evidence: &CatalogEvidence,
    ids: &BTreeMap<CapabilityRef, String>,
    definitions: &Value,
) -> Result<Scope> {
    project_links(
        target,
        evidence,
        ids,
        definitions,
        evidence.bindings.iter().collect(),
    )
}

fn project_links(
    target: &CapabilityRef,
    evidence: &CatalogEvidence,
    ids: &BTreeMap<CapabilityRef, String>,
    definitions: &Value,
    links: Vec<&Binding>,
) -> Result<Scope> {
    let mut endpoints: BTreeMap<CapabilityRef, Vec<&Slot>> =
        BTreeMap::from([(target.clone(), vec![])]);
    for link in &links {
        for endpoint in link.endpoints() {
            endpoints
                .entry(endpoint.capability.clone())
                .or_default()
                .push(&endpoint.slot);
        }
    }
    let mut cards = Vec::new();
    let mut references = json!({});
    let mut types = json!({"values":{},"records":{}});
    let target_evidence = evidence
        .capabilities
        .get(target)
        .ok_or_else(|| invalid(target, "target evidence"))?;
    let target_refs = value(&target_evidence.references)?;
    for kind in ["values", "records"] {
        for name in target_refs[kind]
            .as_array()
            .ok_or_else(|| invalid(target, "target references"))?
        {
            let name = name
                .as_str()
                .ok_or_else(|| invalid(target, "reference name"))?;
            types[kind][name] = required(&definitions[kind], name, target, "definition")?.clone();
        }
    }
    for (reference, slots) in &endpoints {
        let id = ids
            .get(reference)
            .ok_or_else(|| invalid(reference, "local identity"))?;
        let ev = evidence
            .capabilities
            .get(reference)
            .ok_or_else(|| invalid(reference, "capability evidence"))?;
        let entity = &ev.entity;
        let declaration = value(&ev.declaration)?;
        let card = json!({"reference":reference,"entity":entity,"operation":ev.operation});
        let sort_key = plasm_core::catalog_discovery::content_hash(
            encode(&json!({"capability":card,"declaration":declaration}))?.as_bytes(),
        );
        if reference == target {
            cards.push((
                sort_key,
                id.clone(),
                json!({"capability":card,"declaration":declaration}),
            ));
            references[id] = target_refs.clone();
            continue;
        }
        let record = required(
            &definitions["records"],
            &super::evidence::key(&reference.catalog, entity),
            reference,
            "record",
        )?;
        let mut projected = BTreeMap::new();
        let mut names = BTreeSet::new();
        for slot in slots {
            let part = match slot {
                Slot::Entity { entity } => json!({"owning_entity":entity}),
                Slot::Field { field } => {
                    json!({"output_field":required(&record["fields"],field,reference,"output field")?})
                }
                Slot::Input(InputSourceTarget::Argument { input }) => {
                    input_slot(&declaration, input, reference)?
                }
                Slot::Input(InputSourceTarget::Receiver { entity }) => {
                    let receiver = required(
                        &definitions["records"],
                        &super::evidence::key(&reference.catalog, entity.as_str()),
                        reference,
                        "receiver",
                    )?;
                    let id = receiver["id_field"]
                        .as_str()
                        .ok_or_else(|| invalid(reference, "receiver identity"))?;
                    json!({"receiver":entity,"identity_field":required(&receiver["fields"],id,reference,"receiver identity field")?})
                }
                Slot::Identity {
                    identity: RowIdentity::Field { field },
                } => {
                    json!({"identity_field":required(&record["fields"],field.as_str(),reference,"identity field")?})
                }
                Slot::Identity {
                    identity: RowIdentity::Relation { relation },
                } => {
                    json!({"identity_relation":required(&record["relations"],relation.as_str(),reference,"identity relation")?})
                }
            };
            collect_values(
                &part,
                &reference.catalog,
                &definitions,
                &mut types,
                &mut names,
                reference,
            )?;
            projected.insert(encode(&part)?, part);
        }
        let mut reduced =
            json!({"capability":card,"slots":projected.into_values().collect::<Vec<_>>()});
        if let Some(output) = declaration.get("output") {
            reduced["declaration"] = json!({"output":output});
        }
        cards.push((sort_key, id.clone(), reduced));
        references[id] = json!({"values":names,"records":[]});
    }
    let mut bindings = Vec::new();
    let mut memberships = BTreeMap::<String, (Value, u8)>::new();
    for link in links {
        let mut wire = value(link)?;
        for (key, endpoint) in ["from", "to"].into_iter().zip(link.endpoints()) {
            wire[key]["capability"] = json!(ids
                .get(&endpoint.capability)
                .ok_or_else(|| invalid(&endpoint.capability, "binding identity"))?);
        }
        if matches!(link, Binding::Membership { .. }) {
            // Only exact reciprocal membership endpoints coalesce. Argument
            // flow and prerequisite directions are never interchangeable.
            let reverse = encode(&wire["from"])? > encode(&wire["to"])?;
            if reverse {
                let from = wire["from"].take();
                wire["from"] = wire["to"].take();
                wire["to"] = from;
            }
            let direction = if reverse { 2 } else { 1 };
            let entry = memberships.entry(encode(&wire)?).or_insert((wire, 0));
            entry.1 |= direction;
        } else {
            bindings.push(wire);
        }
    }
    for (mut wire, directions) in memberships.into_values() {
        if directions == 3 {
            wire["bidirectional"] = json!(true);
        } else if directions == 2 {
            let from = wire["from"].take();
            wire["from"] = wire["to"].take();
            wire["to"] = from;
        }
        bindings.push(wire);
    }
    compact_record_names(&mut types["records"]);
    cards.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    Ok(Scope {
        cards: OrderedCards(cards.into_iter().map(|(_, id, card)| (id, card)).collect()),
        types: ProjectedDefinitions {
            values: types["values"].take(),
            records: types["records"].take(),
        },
        references,
        bindings,
        count: endpoints.len(),
    })
}

/// A keyed field/relation already carries its name. Retain mismatches verbatim;
/// never traverse defaults, examples or other authored business data.
fn compact_record_names(records: &mut Value) {
    let Some(records) = records.as_object_mut() else {
        return;
    };
    for record in records.values_mut() {
        for section in ["fields", "relations"] {
            let Some(entries) = record.get_mut(section).and_then(Value::as_object_mut) else {
                continue;
            };
            for (key, entry) in entries {
                if entry.get("name").and_then(Value::as_str) == Some(key.as_str()) {
                    if let Some(object) = entry.as_object_mut() {
                        object.remove("name");
                    }
                }
            }
        }
    }
}
fn collect_values(
    part: &Value,
    catalog: &str,
    definitions: &Value,
    types: &mut Value,
    names: &mut BTreeSet<String>,
    reference: &CapabilityRef,
) -> Result<()> {
    match part {
        Value::Object(map) => {
            if let Some(name) = map.get("value_ref").and_then(Value::as_str) {
                let key = super::evidence::key(catalog, name);
                if names.insert(key.clone()) {
                    let definition = required(
                        &definitions["values"],
                        &key,
                        reference,
                        "slot value definition",
                    )?;
                    types["values"][&key] = definition.clone();
                    if let Some(items) = definition.get("array_items") {
                        collect_values(items, catalog, definitions, types, names, reference)?;
                    }
                }
            }
            // Traverse only schema-bearing fields, never arbitrary defaults or
            // examples whose business data may itself contain a value_ref key.
            for key in [
                "output_field",
                "identity_field",
                "field",
                "fields",
                "input_type",
                "variants",
                "element_type",
            ] {
                if let Some(v) = map.get(key) {
                    collect_values(v, catalog, definitions, types, names, reference)?;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_values(item, catalog, definitions, types, names, reference)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_name_compaction_preserves_authored_metadata_and_business_objects() {
        let mut records = json!({"matrix/Item": {"name":"Item","fields": {
            "optional": {"name":"optional","required":false,"default":null,"description":"Keep absence distinct.","value_ref":"text"},
            "different": {"name":"authored_alias","required":true,"default":{"name":"different","fields":{"x":{"name":"x"}}}},
            "absent": {"value_ref":"opaque"}
        },"relations":{"children":{"name":"children","cardinality":"many","description":"Owned children","materialize":{"path":[{"key":"children"}],"collection_coverage":"complete"}}}}});
        let original = records.clone();
        compact_record_names(&mut records);
        assert!(records["matrix/Item"]["fields"]["optional"]
            .get("name")
            .is_none());
        assert!(records["matrix/Item"]["relations"]["children"]
            .get("name")
            .is_none());
        records["matrix/Item"]["fields"]["optional"]["name"] = json!("optional");
        records["matrix/Item"]["relations"]["children"]["name"] = json!("children");
        assert_eq!(records, original);
    }
    fn reference() -> CapabilityRef {
        CapabilityRef {
            catalog: "matrix".into(),
            capability: "consume".into(),
        }
    }

    #[test]
    fn nested_variant_projection_preserves_absence_null_and_lane_validation() {
        let field = json!({"name":"token","value_ref":"credential","required":false,"default":null,"description":"Optional credential"});
        let declaration = json!({"payload":{"input_type":{"type":"union","variants":[
            {"name":"present","wire":{"field":"kind","value":"p"},"fields":[{"name":"nested","required":true,"input_type":{"type":"object","additional_fields":false,"fields":[field.clone(),{"name":"noise","value_ref":"unused"}]}}]},
            {"name":"absent","wire":{"field":"kind","value":"a"},"fields":[]}
        ]},"validation":{"allow_null":true},"description":"Payload contract"}});
        let path = InputPath {
            lane: plasm_core::prerequisites::InputLane::Payload,
            path: vec!["nested".into(), "token".into()],
        };
        let projected = input_slot(&declaration, &path, &reference()).unwrap();
        assert_eq!(projected["fields"][0]["field"], field);
        assert_eq!(projected["fields"][1]["present"], false);
        assert!(projected["fields"][1].get("field").is_none());
        assert_eq!(projected["lane_metadata"]["validation"]["allow_null"], true);
        assert!(!encode(&projected).unwrap().contains("unused"));
        assert!(encode(&projected).unwrap().contains("\"name\":\"absent\""));
    }

    #[test]
    fn nested_arrays_preserve_limits_and_unknown_paths_fail_closed() {
        let shape = json!({"type":"array","min_length":1,"max_length":4,"element_type":{"type":"object","additional_fields":true,"fields":[{"name":"value","value_ref":"raw","required":true}]}});
        let projected =
            input_fields(&shape, &["*".into(), "value".into()], vec![], &reference()).unwrap();
        assert_eq!(projected[0]["context"][0]["min_length"], 1);
        assert_eq!(projected[0]["context"][0]["max_length"], 4);
        assert!(input_fields(
            &json!({"type":"value"}),
            &["value".into()],
            vec![],
            &reference()
        )
        .is_err());
    }

    #[test]
    fn named_array_cycles_are_deduplicated_and_defaults_are_not_schema() {
        let definitions = json!({"values":{"matrix/a":{"array_items":{"value_ref":"b"}},"matrix/b":{"array_items":{"value_ref":"a"}}}});
        let mut types = json!({"values":{}});
        let mut names = BTreeSet::new();
        let slot = json!({"output_field":{"value_ref":"a","default":{"value_ref":"not_a_schema_reference"}}});
        collect_values(
            &slot,
            "matrix",
            &definitions,
            &mut types,
            &mut names,
            &reference(),
        )
        .unwrap();
        assert_eq!(
            names,
            BTreeSet::from(["matrix/a".to_owned(), "matrix/b".to_owned()])
        );
        assert_eq!(types["values"], definitions["values"]);
    }
}
