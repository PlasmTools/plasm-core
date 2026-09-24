//! One semantic view of the typed catalogue, rendered for retrieval and judgment.
//! No wire schemas, decoder JSON, defaults, runtime values, or task text enter this view.

use super::relation_use::DeclaredCapabilityUse;
use crate::schema::{
    CapabilityKind, CapabilitySchema, EntityDef, InputFieldSchema, InputFieldWire, InputType,
    NamedValueSchema, OutputType, ValueDomainSlot,
};
use std::collections::BTreeSet;

/// Input roles are preserved independently of parameter spelling.
#[derive(Clone, Copy)]
enum InputRole {
    Scope,
    Selection,
    Control,
    Arguments,
    Payload,
}

impl InputRole {
    fn phrase(self) -> &'static str {
        match self {
            Self::Scope => "The collection is scoped by",
            Self::Selection => "Matching records are selected using",
            Self::Control => "Execution is controlled by",
            Self::Arguments => "The operation accepts",
            Self::Payload => "The requested change accepts",
        }
    }
}

/// Borrow the authoritative model rather than maintain a second semantic schema.
struct SemanticView<'a> {
    cgs: &'a crate::CGS,
    capability: &'a CapabilitySchema,
    entity: &'a EntityDef,
    inputs: crate::schema::CapabilityInputs,
}

pub(super) struct RenderedMeaning {
    pub operation: String,
    pub collection: String,
    pub text: String,
}

pub(super) fn render(
    cgs: &crate::CGS,
    cap: &CapabilitySchema,
    entity: &EntityDef,
) -> Result<RenderedMeaning, String> {
    let view = SemanticView {
        cgs,
        capability: cap,
        entity,
        inputs: super::input_projection::relevance_inputs(cgs, cap)?,
    };
    let operation = view.operation()?;
    let collection = view.collection();
    // Each evidence section appears once. IDs used for dispatch stay in the enclosing artifact.
    let text = format!(
        "{}\n{}\n{}",
        cgs.entry_id.as_deref().unwrap_or_default(),
        operation,
        collection
    );
    Ok(RenderedMeaning {
        operation,
        collection,
        text,
    })
}

impl SemanticView<'_> {
    /// One narrative contract for retrieval and judgment. Input roles come from
    /// the model; descriptions are never inferred from identifiers or task text.
    fn operation(&self) -> Result<String, String> {
        let cap = self.capability;
        let mut prose = Narrative::default();
        prose.push(cap.description.clone());
        if cap.description.is_empty() {
            let verb = match cap.kind {
                CapabilityKind::Query => "List matching",
                CapabilityKind::Search => "Search by text and rank matching",
                CapabilityKind::Get => "Read an identified",
                CapabilityKind::Create => "Create a",
                CapabilityKind::Update => "Update an identified",
                CapabilityKind::Delete => "Delete an identified",
                CapabilityKind::Action => "Act on a",
            };
            prose.push(format!("{verb} {} record", cap.domain));
        }
        // Query and ranked search are distinct even when their authored glosses coincide.
        if cap.kind == CapabilityKind::Search {
            prose.push("Matches are ranked by search relevance".into());
        }
        for relation in self.relation_uses() {
            prose.push(relation);
        }
        for (role, fields) in [
            (InputRole::Scope, &self.inputs.scope.0),
            (InputRole::Selection, &self.inputs.selection.0),
            (InputRole::Control, &self.inputs.controls.0),
        ] {
            if !fields.is_empty() {
                prose.push(format!(
                    "{} {}",
                    role.phrase(),
                    input_fields(self.cgs, fields)?
                ));
            }
        }
        for (role, schema) in [
            (InputRole::Arguments, &self.inputs.arguments),
            (InputRole::Payload, &self.inputs.payload),
        ] {
            if let Some(schema) = schema {
                if let Some(description) = &schema.description {
                    prose.push(description.clone());
                }
                prose.push(format!(
                    "{} {}",
                    role.phrase(),
                    match &schema.input_type {
                        InputType::Object {
                            fields,
                            additional_fields: false,
                        } => input_fields(self.cgs, fields)?,
                        other => input_shape(self.cgs, other)?,
                    }
                ));
            }
        }
        match cap.output_schema.as_ref().map(|s| &s.output_type) {
            Some(OutputType::Entity { entity_type }) => {
                prose.push(format!("Returns one {entity_type} record"))
            }
            Some(OutputType::Collection {
                entity_type,
                max_count,
            }) => prose.push(format!(
                "Returns {entity_type} records{}",
                max_count
                    .map(|n| format!(", at most {n}"))
                    .unwrap_or_default()
            )),
            Some(OutputType::Status { .. }) => {
                prose.push("Returns a status acknowledgement".into())
            }
            Some(OutputType::Custom { .. }) => {
                prose.push("Returns a custom structured response".into())
            }
            Some(OutputType::SideEffect { description }) => prose.push(description.clone()),
            None if matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search) => {
                prose.push(format!("Returns matching {} records", cap.domain))
            }
            None if cap.kind == CapabilityKind::Get => {
                prose.push(format!("Returns the identified {} record", cap.domain))
            }
            None => {}
        }
        let mut observed = Narrative::default();
        for name in &cap.provides {
            // A relevance projection describes only declared observations with
            // authored meaning. The full typed output contract remains in CGS.
            if let Some(field) = self.entity.fields.get(name.as_str()) {
                let value = field.named_value(self.cgs).map_err(|e| e.to_string())?;
                if !field.description.is_empty() || !value.description.is_empty() {
                    observed.push(value_meaning(
                        self.cgs,
                        value,
                        Some(&field.description),
                        &mut BTreeSet::new(),
                    )?);
                }
            }
        }
        if !observed.0.is_empty() {
            prose.push(format!(
                "The result exposes the following information. {}",
                observed.finish()
            ));
        }
        Ok(prose.finish())
    }

    /// A relation's named materializer establishes a use of this exact capability.
    /// Entity/type similarity alone does not establish that association. No paths,
    /// scope bindings or wire metadata belong in the semantic document.
    fn relation_uses(&self) -> BTreeSet<String> {
        let mut uses = BTreeSet::new();
        for source in self.cgs.entities.values() {
            for relation in source.relations.values() {
                if relation.target_resource != self.capability.domain {
                    continue;
                }
                let Some(use_) = relation
                    .materialize
                    .as_ref()
                    .and_then(DeclaredCapabilityUse::declared_capability_use)
                else {
                    continue;
                };
                let role = use_.role.meaning();
                if use_.capability == &self.capability.name {
                    uses.insert(format!(
                        "{role} through the {} relationship from {} to {}. {}",
                        relation.name, source.name, relation.target_resource, relation.description
                    ));
                }
            }
        }
        uses
    }

    fn collection(&self) -> String {
        let mut prose = Narrative::default();
        prose.push(format!("Works with {} records", self.capability.domain));
        prose.push(self.entity.description.clone());
        prose.finish()
    }
}

/// Preserve authored text exactly, adding only sentence boundaries. Identical
/// complete clauses may share one occurrence; no fuzzy compression or truncation.
#[derive(Default)]
struct Narrative(Vec<String>);
impl Narrative {
    fn push(&mut self, text: String) {
        if !text.is_empty() && !self.0.contains(&text) {
            self.0.push(text);
        }
    }
    fn finish(self) -> String {
        self.0
            .into_iter()
            .map(|text| {
                if text.trim_end().ends_with(['.', '!', '?']) {
                    text
                } else {
                    format!("{text}.")
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn input_fields(cgs: &crate::CGS, fields: &[InputFieldSchema]) -> Result<String, String> {
    fields
        .iter()
        .map(|field| input_field(cgs, field))
        .collect::<Result<Vec<_>, _>>()
        .map(|meanings| meanings.join("; "))
}

fn input_field(cgs: &crate::CGS, field: &InputFieldSchema) -> Result<String, String> {
    let meaning = match &field.wire {
        InputFieldWire::Registry(_) => value_meaning(
            cgs,
            field.named_value(cgs).map_err(|e| e.to_string())?,
            field.description.as_deref(),
            &mut BTreeSet::new(),
        )?,
        InputFieldWire::Inline(ty) => format!(
            "{}{}",
            field
                .description
                .as_ref()
                .map(|d| format!("{d} "))
                .unwrap_or_default(),
            input_shape(cgs, ty)?
        ),
    };
    Ok(format!(
        "{meaning}{}",
        if field.required {
            " (required)"
        } else {
            " (optional)"
        }
    ))
}

fn value_meaning(
    cgs: &crate::CGS,
    value: &NamedValueSchema,
    description: Option<&str>,
    seen: &mut BTreeSet<String>,
) -> Result<String, String> {
    let mut parts = Vec::new();
    for text in description
        .into_iter()
        .chain(std::iter::once(value.description.as_str()))
    {
        if !text.is_empty() && !parts.contains(&text.to_string()) {
            parts.push(text.to_string());
        }
    }
    // Preserve declared type semantics, without exposing private registry names.
    if parts.is_empty() {
        parts.push(input_shape(
            cgs,
            &InputType::Value {
                field_type: value.field_type.clone(),
                allowed_values: None,
            },
        )?);
    }
    if let Some(members) = &value.allowed_values {
        if !members.is_empty() {
            let glosses = value
                .domain
                .enum_membership
                .as_ref()
                .and_then(|m| m.glosses());
            let meanings = members
                .iter()
                .map(|token| match glosses.and_then(|g| g.get(token)) {
                    Some(gloss) if !gloss.trim().is_empty() => format!("{token}: {gloss}"),
                    _ => token.clone(),
                })
                .collect::<Vec<_>>();
            parts.push(format!("Allowed values: {}.", meanings.join(", ")));
        }
    }
    if let Some(items) = &value.array_items {
        let key = items.value_domain_key().as_str().to_string();
        if !seen.insert(key.clone()) {
            return Err(format!("cyclic discovery array value domain: {key}"));
        }
        let item = cgs.named_value_for_slot(items).map_err(|e| e.to_string())?;
        parts.push(format!(
            "Each element describes {}",
            value_meaning(cgs, item, None, seen)?
        ));
        seen.remove(&key);
    }
    Ok(parts.join(" "))
}

fn input_shape(cgs: &crate::CGS, input: &InputType) -> Result<String, String> {
    Ok(match input {
        InputType::None => "no input".into(),
        InputType::Value {
            field_type,
            allowed_values,
        } => {
            let mut text = match field_type {
                crate::FieldType::Boolean => "a boolean".into(),
                crate::FieldType::Number => "a number".into(),
                crate::FieldType::Integer => "an integer".into(),
                crate::FieldType::Uuid => "a UUID".into(),
                crate::FieldType::DigitId => "a digit-string identifier".into(),
                crate::FieldType::Blob => "a binary or large payload".into(),
                crate::FieldType::String => "text".into(),
                crate::FieldType::Select => "one selected value".into(),
                crate::FieldType::MultiSelect => "multiple selected values".into(),
                crate::FieldType::Date => "a date".into(),
                crate::FieldType::Array => "an array".into(),
                crate::FieldType::Json => "a structured value".into(),
                crate::FieldType::Money => "a monetary amount".into(),
                crate::FieldType::EntityRef { target, .. } => {
                    format!("a reference to a {target} record")
                }
            };
            if let Some(values) = allowed_values {
                text.push_str(&format!(" restricted to {}", values.join(", ")));
            }
            text
        }
        InputType::Object {
            fields,
            additional_fields,
        } => {
            let names = fields
                .iter()
                .map(|f| input_field(cgs, f))
                .collect::<Result<Vec<_>, _>>()?
                .join("; ");
            let mut text = if names.is_empty() {
                "a structured record".into()
            } else {
                format!("a record containing {names}")
            };
            if *additional_fields {
                text.push_str(" and additional fields");
            }
            text
        }
        InputType::Array {
            element_type,
            min_length,
            max_length,
        } => {
            let mut text = format!(
                "an array whose elements are {}",
                input_shape(cgs, element_type)?
            );
            if let Some(minimum) = min_length {
                text.push_str(&format!(", with at least {minimum} elements"));
            }
            if let Some(maximum) = max_length {
                text.push_str(&format!(", with at most {maximum} elements"));
            }
            text
        }
        InputType::Union { variants } => {
            let alternatives = variants
                .iter()
                .map(|v| {
                    let fields = v
                        .fields
                        .iter()
                        .map(|f| input_field(cgs, f))
                        .collect::<Result<Vec<_>, _>>()?
                        .join("; ");
                    let mut text = v.name.clone();
                    if let Some(description) = &v.description {
                        text.push_str(&format!(": {description}"));
                    }
                    if !fields.is_empty() {
                        text.push_str(&format!(" with {fields}"));
                    }
                    Ok(text)
                })
                .collect::<Result<Vec<_>, String>>()?;
            format!(
                "one of these alternative variants: {}",
                alternatives.join("; ")
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{RelationMaterialization, RelationScopedFallback};

    fn fixture() -> crate::CGS {
        crate::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/discovery_value_roles"),
        )
        .unwrap()
    }

    proptest::proptest! {
        #[test]
        fn relation_use_preserves_binding_role_and_description_across_serialization(
            mode in 0usize..10, description in "[a-zA-Z ]{1,180}",
            named in proptest::bool::ANY, target_matches in proptest::bool::ANY,
        ) {
            use crate::schema::{Cardinality, RelationSchema, EmbedOnMissPolicy};
            let mut cgs = fixture();
            let capability = if named { "record_query" } else { "record_get" }.into();
            let materialize = match mode {
                0 => Some(RelationMaterialization::QueryScoped { capability, param: "scope_seat".into() }),
                1 => Some(RelationMaterialization::QueryScopedBindings { capability, bindings: Default::default() }),
                2 => Some(RelationMaterialization::GetScopedBindings { capability, bindings: Default::default() }),
                3..=5 => Some(RelationMaterialization::PreferFromParentGet {
                    path: vec![], on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
                    fallback: match mode {
                        3 => RelationScopedFallback::QueryScoped { capability, param: "scope_seat".into() },
                        4 => RelationScopedFallback::QueryScopedBindings { capability, bindings: Default::default() },
                        _ => RelationScopedFallback::HydrateFromEmbedPath { get_capability: capability, path: vec![] },
                    },
                }),
                6 => Some(RelationMaterialization::FromParentGet { path: vec![] }),
                7 => Some(RelationMaterialization::ViewEmbed { view: "private_view".into() }),
                8 => Some(RelationMaterialization::Unavailable),
                _ => None,
            };
            cgs.entities.get_mut("Record").unwrap().relations.insert("related".into(), RelationSchema {
                name: "related".into(), description: description.clone(),
                target_resource: if target_matches { "Record" } else { "Other" }.into(),
                cardinality: Cardinality::Many, materialize, discovery: None,
            });
            let decoded: crate::CGS = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
            let actual = render(&cgs, &cgs.capabilities["record_query"], &cgs.entities["Record"]).unwrap();
            let wire = render(&decoded, &decoded.capabilities["record_query"], &decoded.entities["Record"]).unwrap();
            proptest::prop_assert_eq!(&actual.operation, &wire.operation);
            proptest::prop_assert_eq!(&actual.text, &wire.text);
            let linked = mode < 6 && named && target_matches;
            proptest::prop_assert_eq!(actual.operation.contains("the related relationship from Record to Record."), linked);
            if linked {
                let fact = format!("the related relationship from Record to Record. {description}");
                proptest::prop_assert!(actual.operation.contains(&fact));
                proptest::prop_assert!(actual.text.contains(&fact));
                proptest::prop_assert_eq!(actual.operation.contains("when embedded targets"), mode >= 3);
                proptest::prop_assert_eq!(actual.operation.contains("Hydrates already identified"), mode == 5);
            }
            for hidden in ["scope_seat", "private_view", "query_scoped", "hydrate_from_embed_path"] {
                proptest::prop_assert!(!actual.operation.contains(hidden));
            }
        }

        #[test]
        fn receiver_and_return_contract_survive_schema_roundtrip(
            bound in proptest::bool::ANY, maximum in 0usize..1000,
            variant in 0usize..5, operation in 0usize..7,
        ) {
            let cgs = fixture();
            let mut cap = cgs.capabilities["record_create"].clone();
            cap.kind = [CapabilityKind::Query, CapabilityKind::Search, CapabilityKind::Get,
                CapabilityKind::Create, CapabilityKind::Update, CapabilityKind::Delete, CapabilityKind::Action][operation];
            cap.inputs.receiver = Some(if bound {
                crate::schema::CapabilityReceiver::Entity { entity: "Record".into() }
            } else { crate::schema::CapabilityReceiver::None });
            let output_type = match variant {
                0 => OutputType::Entity { entity_type: "Receipt".into() },
                1 => OutputType::Collection { entity_type: "Receipt".into(), max_count: Some(maximum) },
                2 => OutputType::Status { success_indicators: vec!["private-wire-field".into()] },
                3 => OutputType::Custom { schema: serde_json::json!({"private_schema": true}) },
                _ => OutputType::SideEffect { description: "Records the declared change.".into() },
            };
            cap.output_schema = Some(crate::schema::OutputSchema {
                output_type, decoder: serde_json::json!({"wire_secret": true}), idempotent: false, reconcile: None,
            });
            let roundtrip: CapabilitySchema = serde_json::from_slice(&serde_json::to_vec(&cap).unwrap()).unwrap();
            let actual = render(&cgs, &cap, &cgs.entities["Record"]).unwrap();
            let decoded = render(&cgs, &roundtrip, &cgs.entities["Record"]).unwrap();
            proptest::prop_assert_eq!(&actual.text, &decoded.text);
            proptest::prop_assert_eq!(&actual.operation, &decoded.operation);
            proptest::prop_assert_eq!(cap.receiver_entity(), roundtrip.receiver_entity());
            let output = match variant {
                0 => "Returns one Receipt record.".into(),
                1 => format!("Returns Receipt records, at most {maximum}."),
                2 => "Returns a status acknowledgement.".into(),
                3 => "Returns a custom structured response.".into(),
                _ => "Records the declared change.".into(),
            };
            proptest::prop_assert!(actual.text.contains(&output));
            for secret in ["private-wire-field", "wire_secret", "private_schema"] {
                proptest::prop_assert!(!actual.text.contains(secret));
            }
        }

        #[test]
        fn descriptions_stay_attached_to_roles_independent_of_identifiers(
            selector in "[a-z][a-z_]{0,24}", control in "[a-z][a-z_]{0,24}",
            description in ".{1,120}", search in proptest::bool::ANY,
        ) {
            let mut cgs = fixture();
            cgs.values.get_mut("mode").unwrap().description = description.clone();
            let mut cap = cgs.capabilities["record_query"].clone();
            cap.inputs.selection.0[0].name = selector.clone();
            cap.inputs.controls.0[0].name = control.clone();
            cap.inputs.controls.0[0].description = None;
            cap.kind = if search { CapabilityKind::Search } else { CapabilityKind::Query };
            let actual = render(&cgs, &cap, &cgs.entities["Record"]).unwrap();
            let selection = "Matching records are selected using Recorded relationship";
            let control = format!("Execution is controlled by {description}");
            proptest::prop_assert!(actual.text.contains(selection));
            proptest::prop_assert!(actual.text.contains(&control));
            proptest::prop_assert!(actual.text.contains("The collection is scoped by Select whose collection is read Owner of the collection being read (optional)"));
            proptest::prop_assert_eq!(actual.text.contains("Matches are ranked by search relevance"), search);
            proptest::prop_assert!(actual.operation.contains(&description));
            proptest::prop_assert!(actual.operation.contains("Recorded relationship to the account holder"));
        }
    }

    #[test]
    fn argument_shape_preserves_alternatives_bounds_and_member_meanings() {
        let cgs = fixture();
        let ty: InputType = serde_json::from_value(serde_json::json!({
            "type":"array", "min_length":1, "max_length":3,
            "element_type":{"type":"union", "variants":[
                {"name":"assign", "description":"Assign collection ownership",
                 "wire":{"field":"private_tag","value":"a"},
                 "fields":[{"name":"owner","value_ref":"owner","required":true,"default":"secret-default"}]},
                {"name":"clear","wire":{"field":"private_tag","value":"c"},"fields":[]}
            ]}
        })).unwrap();
        let text = input_shape(&cgs, &ty).unwrap();
        for required in [
            "one of these alternative variants",
            "Owner of the collection being read (required)",
            "Assign collection ownership",
            "at least 1 elements",
            "at most 3 elements",
            "; clear",
        ] {
            assert!(text.contains(required), "{text}");
        }
        assert!(!text.contains("private_tag"));
        assert!(!text.contains("secret-default"));
        assert!(!text.ends_with("with "));
    }

    #[test]
    fn control_spelling_cannot_invent_ordering_or_selection() {
        let cgs = fixture();
        let mut cap = cgs.capabilities["record_query"].clone();
        cap.inputs.selection.0.clear();
        cap.inputs.controls.0[0].name = "sort_by".into();
        let text = render(&cgs, &cap, &cgs.entities["Record"]).unwrap().text;
        assert!(
            text.contains("Execution is controlled by Read consistency mode Allowed values: fresh, cached. (optional)")
        );
        assert!(!text.contains("Matching records are selected using"));
        assert!(!text.contains("ascending"));
        assert!(!text.contains("descending"));
    }

    #[test]
    fn narrative_preserves_roles_and_requiredness() {
        let cgs = fixture();
        let mut cap = cgs.capabilities["record_query"].clone();
        let mut second = cap.inputs.selection.0[0].clone();
        second.name = "other_relationship".into();
        second.required = true;
        cap.inputs.selection.0.push(second);
        let rendered = render(&cgs, &cap, &cgs.entities["Record"]).unwrap();
        assert!(rendered
            .operation
            .contains("Recorded relationship to the account holder Allowed values: colleague, relative, neighbor. (required)"));
        assert!(rendered.operation.contains("The collection is scoped by"));
        assert!(rendered.operation.contains("Execution is controlled by"));
        assert!(rendered
            .operation
            .contains("Allowed values: colleague, relative, neighbor."));
        assert!(!rendered.operation.contains("Invoke without a receiver"));
        assert!(!rendered.text.contains("Invoke without a receiver"));
    }

    #[test]
    fn identical_operation_and_effect_are_rendered_once() {
        let cgs = fixture();
        let mut cap = cgs.capabilities["record_create"].clone();
        cap.output_schema = Some(crate::schema::OutputSchema {
            output_type: OutputType::SideEffect {
                description: cap.description.clone(),
            },
            decoder: serde_json::json!({}),
            idempotent: false,
            reconcile: None,
        });
        let text = render(&cgs, &cap, &cgs.entities["Record"]).unwrap().text;
        assert_eq!(text.matches(&cap.description).count(), 1);
    }

    #[test]
    fn narrative_preserves_inputs_and_outputs_without_unrelated_entity_attributes() {
        let mut cgs = fixture();
        cgs.entities
            .get_mut("Record")
            .unwrap()
            .fields
            .get_mut("roles")
            .unwrap()
            .description = "UNRELATED_ENTITY_ATTRIBUTE".into();
        let rendered = render(
            &cgs,
            &cgs.capabilities["record_create"],
            &cgs.entities["Record"],
        )
        .unwrap();
        assert!(rendered.text.contains("Recorded relationship labels"));
        assert!(rendered.text.contains("Record identity"));
        assert!(!rendered.text.contains("UNRELATED_ENTITY_ATTRIBUTE"));
        assert!(!rendered.text.contains("Payload:"));
        assert!(rendered.text.contains(&rendered.operation));
        assert!(rendered.text.contains(&rendered.collection));
    }

    #[test]
    fn entity_context_is_not_a_hydration_promise() {
        let cgs = fixture();
        let text = render(
            &cgs,
            &cgs.capabilities["record_create"],
            &cgs.entities["Record"],
        )
        .unwrap();
        assert!(!text.collection.contains("Recorded relationship labels"));
        assert!(text
            .operation
            .contains("The result exposes the following information. Record identity"));
        assert!(!text.operation.contains(
            "The result exposes the following information. Recorded relationship labels"
        ));
    }

    #[test]
    fn mutators_never_claim_query_or_ranked_search_semantics() {
        let cgs = fixture();
        let mut cap = cgs.capabilities["record_create"].clone();
        cap.description.clear();
        for kind in [
            CapabilityKind::Get,
            CapabilityKind::Create,
            CapabilityKind::Update,
            CapabilityKind::Delete,
            CapabilityKind::Action,
        ] {
            cap.kind = kind;
            let text = render(&cgs, &cap, &cgs.entities["Record"])
                .unwrap()
                .operation;
            assert!(!text.contains("List matching"));
            assert!(!text.contains("Search by text and rank matching"));
        }
    }
}
