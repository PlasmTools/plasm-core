//! Explicit semantic serialization: no schema object is serialized wholesale.
use super::structured::{DomainReference, StructuredCapability, StructuredDiscoveryError};
use crate::schema::{InputFieldSchema, InputFieldWire, InputType, ValueDomainSlot};
use serde::Serialize;

#[derive(Serialize)]
pub struct SemanticValue<'a> {
    reference: DomainReference<'a>,
    description: &'a str,
    domain: &'a crate::value_domain::ValueDomain,
    items: Option<Box<SemanticValue<'a>>>,
}

#[derive(Serialize)]
pub struct SemanticField<'a> {
    name: &'a str,
    description: Option<&'a str>,
    required: bool,
    shape: Shape<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape<'a> {
    Named {
        value: SemanticValue<'a>,
    },
    None,
    Scalar {
        field_type: &'a crate::FieldType,
        allowed_values: Option<&'a [String]>,
    },
    Object {
        fields: Vec<SemanticField<'a>>,
        additional_fields: bool,
    },
    Array {
        element: Box<Shape<'a>>,
        min_length: Option<usize>,
        max_length: Option<usize>,
    },
    Union {
        variants: Vec<Variant<'a>>,
    },
}

#[derive(Serialize)]
pub struct Variant<'a> {
    name: &'a str,
    description: Option<&'a str>,
    fields: Vec<SemanticField<'a>>,
}

#[derive(Serialize)]
pub struct SemanticInputs<'a> {
    scope: Vec<SemanticField<'a>>,
    selection: Vec<SemanticField<'a>>,
    controls: Vec<SemanticField<'a>>,
    arguments: Option<Shape<'a>>,
    payload: Option<Shape<'a>>,
}

#[derive(Serialize)]
struct SemanticRelation<'a> {
    source: &'a str,
    name: &'a str,
    target: &'a str,
    description: &'a str,
    cardinality: crate::schema::Cardinality,
    materialization: Materialization<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Materialization<'a> {
    Unavailable,
    Embedded {
        coverage: crate::schema::EmbeddedCollectionCoverage,
    },
    PreferEmbedded {
        fallback: Box<Materialization<'a>>,
    },
    QueryIdentity {
        capability: &'a str,
        parameter: &'a str,
    },
    QueryBindings {
        capability: &'a str,
        bindings: Vec<(&'a str, &'a str)>,
    },
    GetBindings {
        capability: &'a str,
        bindings: Vec<(&'a str, &'a str)>,
    },
    HydrateEmbedded {
        capability: &'a str,
    },
    View {
        view: &'a str,
    },
}

fn fallback(input: &crate::schema::RelationScopedFallback) -> Materialization<'_> {
    use crate::schema::RelationScopedFallback as R;
    match input {
        R::QueryScoped { capability, param } => Materialization::QueryIdentity {
            capability: capability.as_str(),
            parameter: param.as_str(),
        },
        R::QueryScopedBindings {
            capability,
            bindings,
        } => Materialization::QueryBindings {
            capability: capability.as_str(),
            bindings: bindings
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
        },
        R::HydrateFromEmbedPath { get_capability, .. } => Materialization::HydrateEmbedded {
            capability: get_capability.as_str(),
        },
    }
}

fn materialization(input: Option<&crate::schema::RelationMaterialization>) -> Materialization<'_> {
    use crate::schema::RelationMaterialization as R;
    match input {
        None | Some(R::Unavailable) => Materialization::Unavailable,
        Some(R::FromParentGet {
            collection_coverage,
            ..
        }) => Materialization::Embedded {
            coverage: *collection_coverage,
        },
        Some(R::PreferFromParentGet { fallback: f, .. }) => Materialization::PreferEmbedded {
            fallback: Box::new(fallback(f)),
        },
        Some(R::QueryScoped { capability, param }) => Materialization::QueryIdentity {
            capability: capability.as_str(),
            parameter: param.as_str(),
        },
        Some(R::QueryScopedBindings {
            capability,
            bindings,
        }) => Materialization::QueryBindings {
            capability: capability.as_str(),
            bindings: bindings
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
        },
        Some(R::GetScopedBindings {
            capability,
            bindings,
        }) => Materialization::GetBindings {
            capability: capability.as_str(),
            bindings: bindings
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
        },
        Some(R::ViewEmbed { view }) => Materialization::View { view },
    }
}

#[derive(Serialize)]
pub struct SemanticCapability<'a> {
    name: &'a str,
    entity: &'a str,
    description: &'a str,
    operation_kind: crate::schema::CapabilityKind,
    inputs: SemanticInputs<'a>,
    observations: Vec<SemanticField<'a>>,
    relationships: Vec<SemanticRelation<'a>>,
}

fn value<'a>(
    view: &'a StructuredCapability<'_>,
    key: &'a crate::ValueDomainKey,
    seen: &mut Vec<&'a str>,
) -> Result<SemanticValue<'a>, StructuredDiscoveryError> {
    let (reference, schema) = view.value_by_key(key)?;
    if seen.contains(&reference.key) {
        return Err(StructuredDiscoveryError::CyclicValueDomainItems {
            key: reference.key.to_owned(),
        });
    }
    seen.push(reference.key);
    let items = schema
        .array_items
        .as_ref()
        .map(|item| value(view, item.value_domain_key(), seen).map(Box::new))
        .transpose()?;
    seen.pop();
    Ok(SemanticValue {
        reference,
        description: &schema.description,
        domain: &schema.domain,
        items,
    })
}

fn fields<'a>(
    view: &'a StructuredCapability<'_>,
    fields: &'a [InputFieldSchema],
) -> Result<Vec<SemanticField<'a>>, StructuredDiscoveryError> {
    fields
        .iter()
        .map(|field| {
            Ok(SemanticField {
                name: &field.name,
                description: field.description.as_deref(),
                required: field.required,
                shape: match &field.wire {
                    InputFieldWire::Registry(key) => Shape::Named {
                        value: value(view, key, &mut Vec::new())?,
                    },
                    InputFieldWire::Inline(input) => shape(view, input)?,
                },
            })
        })
        .collect()
}

fn shape<'a>(
    view: &'a StructuredCapability<'_>,
    input: &'a InputType,
) -> Result<Shape<'a>, StructuredDiscoveryError> {
    Ok(match input {
        InputType::None => Shape::None,
        InputType::Value {
            field_type,
            allowed_values,
        } => Shape::Scalar {
            field_type,
            allowed_values: allowed_values.as_deref(),
        },
        InputType::Object {
            fields: input_fields,
            additional_fields,
        } => Shape::Object {
            fields: fields(view, input_fields)?,
            additional_fields: *additional_fields,
        },
        InputType::Array {
            element_type,
            min_length,
            max_length,
        } => Shape::Array {
            element: Box::new(shape(view, element_type)?),
            min_length: *min_length,
            max_length: *max_length,
        },
        InputType::Union { variants } => Shape::Union {
            variants: variants
                .iter()
                .map(|v| {
                    Ok(Variant {
                        name: &v.name,
                        description: v.description.as_deref(),
                        fields: fields(view, &v.fields)?,
                    })
                })
                .collect::<Result<_, StructuredDiscoveryError>>()?,
        },
    })
}

impl<'a> SemanticCapability<'a> {
    pub fn project(view: &'a StructuredCapability<'_>) -> Result<Self, StructuredDiscoveryError> {
        let cap = view.capability();
        let inputs = view.inputs();
        Ok(Self {
            name: cap.name.as_str(),
            entity: cap.domain.as_str(),
            description: &cap.description,
            operation_kind: cap.kind,
            inputs: SemanticInputs {
                scope: fields(view, &inputs.scope.0)?,
                selection: fields(view, &inputs.selection.0)?,
                controls: fields(view, &inputs.controls.0)?,
                arguments: inputs
                    .arguments
                    .as_ref()
                    .map(|s| shape(view, &s.input_type))
                    .transpose()?,
                payload: inputs
                    .payload
                    .as_ref()
                    .map(|s| shape(view, &s.input_type))
                    .transpose()?,
            },
            relationships: view
                .relationships()
                .map(|(source, r)| SemanticRelation {
                    source,
                    name: r.name.as_str(),
                    target: r.target_resource.as_str(),
                    description: &r.description,
                    cardinality: r.cardinality,
                    materialization: materialization(r.materialize.as_ref()),
                })
                .collect(),
            observations: view
                .observations()
                .map(|o| {
                    Ok(SemanticField {
                        name: o.field.name.as_str(),
                        description: Some(&o.field.description),
                        required: o.field.required,
                        shape: Shape::Named {
                            value: value(view, o.field.value_domain_key(), &mut Vec::new())?,
                        },
                    })
                })
                .collect::<Result<_, StructuredDiscoveryError>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_wire_nested_inputs_keep_shape_not_transport() {
        let mut cgs = crate::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        cgs.entry_id = Some("abstract".into());
        let mut field = cgs.capabilities["read"].inputs.selection.0[0].clone();
        field.required = false;
        field.wire_json_path = Some(vec!["SECRET_PATH".into()]);
        field.wire_array_element_key = Some("SECRET_ELEMENT".into());
        let input = InputType::Array {
            min_length: Some(1),
            max_length: Some(3),
            element_type: Box::new(InputType::Union {
                variants: vec![crate::schema::InputVariantSchema {
                    name: "named".into(),
                    description: Some("Logical branch".into()),
                    constructor_symbol: None,
                    fields: vec![field],
                    wire: crate::schema::WireVariantDiscriminator {
                        field: "SECRET_TAG".into(),
                        value: "SECRET_VALUE".into(),
                    },
                }],
            }),
        };
        let view = StructuredCapability::new(&cgs, "read").unwrap();
        let json = serde_json::to_value(shape(&view, &input).unwrap()).unwrap();
        assert_eq!(json["min_length"], 1);
        assert_eq!(json["max_length"], 3);
        assert_eq!(
            json["element"]["variants"][0]["fields"][0]["required"],
            false
        );
        assert!(!json.to_string().contains("SECRET"));
    }

    #[test]
    fn semantic_wire_conditional_relation_keeps_bindings_not_paths() {
        let relation = crate::schema::RelationMaterialization::PreferFromParentGet {
            path: vec![crate::schema::JsonPathSegment::Key {
                key: "SECRET_PATH".into(),
            }],
            collection_coverage: Default::default(),
            on_embed_miss: Default::default(),
            fallback: crate::schema::RelationScopedFallback::QueryScopedBindings {
                capability: "read_children".into(),
                bindings: [("parent".into(), "id".into())].into_iter().collect(),
            },
        };
        let json = serde_json::to_value(materialization(Some(&relation))).unwrap();
        assert_eq!(json["kind"], "prefer_embedded");
        assert_eq!(
            json["fallback"]["bindings"][0],
            serde_json::json!(["parent", "id"])
        );
        assert!(!json.to_string().contains("SECRET"));
    }

    #[test]
    fn semantic_wire_preserves_fields_and_excludes_transport() {
        let mut cgs = crate::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap();
        cgs.entry_id = Some("abstract".into());
        cgs.entities
            .get_mut("BusinessRecord")
            .unwrap()
            .fields
            .get_mut("id")
            .unwrap()
            .wire_path = Some(vec!["SECRET_TRANSPORT_PATH".into()]);
        let view = StructuredCapability::new(&cgs, "read").unwrap();
        let json = serde_json::to_value(SemanticCapability::project(&view).unwrap()).unwrap();
        assert_eq!(json["observations"][0]["name"], "id");
        assert_eq!(json["inputs"]["selection"][0]["name"], "credential");
        assert_eq!(
            json["observations"][0]["shape"]["value"]["reference"]["catalog"],
            "abstract"
        );
        assert!(!json.to_string().contains("SECRET_TRANSPORT_PATH"));
    }
}
