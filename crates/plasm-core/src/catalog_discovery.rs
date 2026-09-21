//! Portable discovery evidence produced at catalog publication time.
//!
//! This module has no network or database access. Artifact validation is also
//! used by importers, before any generation can be activated.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const DISCOVERY_RENDERER_VERSION: u32 = 7;
pub const EMBEDDING_DIMENSIONS: usize = 1536;
pub const EMBEDDING_MODEL: &str = "openai/text-embedding-3-small";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingProfile {
    pub model: String,
    pub dimensions: usize,
    pub encoding_format: String,
}

impl Default for EmbeddingProfile {
    fn default() -> Self {
        Self {
            model: EMBEDDING_MODEL.into(),
            dimensions: EMBEDDING_DIMENSIONS,
            encoding_format: "float".into(),
        }
    }
}

impl EmbeddingProfile {
    pub fn validate(&self) -> Result<(), String> {
        if self != &Self::default() {
            return Err("unsupported embedding profile; repack with the active profile".into());
        }
        Ok(())
    }
}

/// Operation applicability, independent of the collection used to find its receiver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationEvidence {
    pub kind: crate::schema::CapabilityKind,
    pub receiver: Option<crate::CapabilityReceiver>,
    pub contract: String,
}

/// The universe and relationships a read can establish; never an operation precondition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionEvidence {
    pub meaning: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDocument {
    pub capability: String,
    pub entity: String,
    pub text: String,
    pub text_hash: String,
    pub operation: OperationEvidence,
    pub collection: CollectionEvidence,
    pub related_entities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedCapability {
    pub document: CapabilityDocument,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogDiscoveryArtifact {
    pub entry_id: String,
    pub cgs_hash: String,
    pub renderer_version: u32,
    pub profile: EmbeddingProfile,
    pub capabilities: Vec<EmbeddedCapability>,
    pub prerequisites: crate::prerequisites::PrerequisiteCatalog,
}

pub fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Stable per-document cache identity, including the exact embedding request.
pub fn embedding_cache_key(document: &CapabilityDocument, profile: &EmbeddingProfile) -> String {
    let key = serde_json::to_vec(&(DISCOVERY_RENDERER_VERSION, profile, &document.text_hash))
        .expect("embedding profile and document hashes are serializable");
    content_hash(&key)
}

impl CatalogDiscoveryArtifact {
    pub fn validate(&self, cgs: &crate::CGS) -> Result<(), String> {
        self.profile.validate()?;
        if self.renderer_version != DISCOVERY_RENDERER_VERSION {
            return Err("unsupported discovery renderer version".into());
        }
        if cgs.entry_id.as_deref() != Some(&self.entry_id)
            || self.cgs_hash != cgs.catalog_cgs_hash_hex()
        {
            return Err("discovery artifact does not match its CGS revision".into());
        }
        if self.prerequisites != cgs.prerequisites {
            return Err("compiled prerequisite declarations disagree with CGS".into());
        }
        self.prerequisites.validate(cgs)?;
        let expected = capability_documents(cgs)?;
        if expected.len() != self.capabilities.len() {
            return Err("discovery artifact must contain every capability exactly once".into());
        }
        for (document, embedded) in expected.iter().zip(&self.capabilities) {
            if document != &embedded.document {
                return Err(format!(
                    "discovery document mismatch for {}",
                    document.capability
                ));
            }
            validate_embedding(&embedded.embedding, self.profile.dimensions)?;
        }
        Ok(())
    }
}

pub fn validate_embedding(embedding: &[f32], dimensions: usize) -> Result<(), String> {
    if embedding.len() != dimensions {
        return Err(format!(
            "embedding dimension mismatch: expected {dimensions}, got {}",
            embedding.len()
        ));
    }
    if embedding.iter().any(|v| !v.is_finite()) {
        return Err("embedding contains a non-finite value".into());
    }
    if !embedding.iter().any(|v| *v != 0.0) {
        return Err("zero embedding has undefined cosine distance".into());
    }
    Ok(())
}

/// Render only catalog semantics: never mappings, runtime values, or examples.
pub fn capability_documents(cgs: &crate::CGS) -> Result<Vec<CapabilityDocument>, String> {
    let mut capabilities: Vec<_> = cgs.capabilities.values().collect();
    capabilities.sort_by(|a, b| a.name.cmp(&b.name));
    capabilities
        .into_iter()
        .map(|cap| {
            let entity = cgs
                .entities
                .get(&cap.domain)
                .ok_or_else(|| format!("missing entity {}", cap.domain))?;
            let mut lines = vec![
                format!("Catalog: {}", cgs.entry_id.as_deref().unwrap_or_default()),
                format!("Entity: {}", cap.domain),
                format!("Entity purpose: {}", entity.description),
                format!("Capability: {} ({:?})", cap.name, cap.kind),
                format!("Purpose: {}", cap.description),
            ];
            if let Some(hints) = &entity.discovery {
                let names: BTreeSet<_> = hints.names.iter().chain(&hints.qualifier_names).collect();
                lines.push(format!(
                    "Names: {}",
                    names.into_iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            if let Some(hints) = &cap.discovery {
                let aliases: BTreeSet<_> = hints
                    .operation_terms
                    .iter()
                    .chain(&hints.target_terms)
                    .collect();
                lines.push(format!(
                    "Capability aliases: {}",
                    aliases.into_iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            let contract_start = lines.len();
            lines.push(format!("Receiver: {:?}", cap.inputs.receiver));
            for (lane, fields) in [
                ("scope", &cap.inputs.scope.0),
                ("selection", &cap.inputs.selection.0),
                ("controls", &cap.inputs.controls.0),
            ] {
                for field in fields {
                    let value = field.named_value(cgs).map_err(|e| e.to_string())?;
                    lines.push(format!(
                        "Input {lane}.{}: {:?}; required={}",
                        field.name, value.field_type, field.required
                    ));
                    render_value_evidence(
                        cgs,
                        &format!("Input {lane}.{}", field.name),
                        value,
                        &mut lines,
                    )?;
                    render_slot_description(
                        &format!("Input {lane}.{}", field.name),
                        field.description.as_deref(),
                        &mut lines,
                    );
                }
            }
            for (lane, schema) in [
                ("arguments", &cap.inputs.arguments),
                ("payload", &cap.inputs.payload),
            ] {
                if let Some(schema) = schema {
                    // Type rendering intentionally excludes examples and default values.
                    render_input_type(cgs, lane, &schema.input_type, &mut lines)?;
                }
            }
            if let Some(output) = &cap.output_schema {
                // Only the typed projection is evidence, never its transport decoder.
                match &output.output_type {
                    crate::schema::OutputType::SideEffect { description } => {
                        lines.push(format!("Output: side effect; {description}"))
                    }
                    crate::schema::OutputType::Entity { entity_type } => {
                        lines.push(format!("Output: single entity {entity_type}"))
                    }
                    crate::schema::OutputType::Collection {
                        entity_type,
                        max_count,
                    } => lines.push(format!(
                        "Output: collection {entity_type}; max_count={max_count:?}"
                    )),
                    crate::schema::OutputType::Status { .. } => lines.push("Output: status".into()),
                    crate::schema::OutputType::Custom { .. } => {
                        lines.push("Output: custom structure".into())
                    }
                }
            }
            let mut provided: Vec<_> = cap.provides.iter().map(|field| field.as_str()).collect();
            provided.sort_unstable();
            lines.push(format!("Populates: {}", provided.join(", ")));
            let operation = OperationEvidence {
                kind: cap.kind,
                receiver: cap.inputs.receiver.clone(),
                contract: format!(
                    "Purpose: {}\n{}",
                    cap.description,
                    lines[contract_start..].join("\n")
                ),
            };
            let collection_start = lines.len();
            for field in entity.fields.values() {
                let value = field.named_value(cgs).map_err(|e| e.to_string())?;
                lines.push(format!(
                    "Entity field {}: {:?}",
                    field.name, value.field_type
                ));
                render_value_evidence(
                    cgs,
                    &format!("Entity field {}", field.name),
                    value,
                    &mut lines,
                )?;
                render_slot_description(
                    &format!("Entity field {}", field.name),
                    Some(&field.description),
                    &mut lines,
                );
            }
            let mut related_entities = BTreeSet::new();
            for (wire, relation) in &entity.relations {
                lines.push(format!(
                    "Relation {wire} -> {}: {}",
                    relation.target_resource, relation.description
                ));
                related_entities.insert(relation.target_resource.to_string());
            }
            let collection = CollectionEvidence {
                meaning: format!(
                    "Entity: {}\nEntity purpose: {}\n{}",
                    cap.domain,
                    entity.description,
                    lines[collection_start..].join("\n")
                ),
            };
            let text = lines.join("\n");
            Ok(CapabilityDocument {
                capability: cap.name.to_string(),
                entity: cap.domain.to_string(),
                text_hash: content_hash(text.as_bytes()),
                text,
                related_entities: related_entities.into_iter().collect(),
                operation,
                collection,
            })
        })
        .collect()
}

/// Semantic evidence from CGS only; JSON quoting preserves metadata line boundaries.
fn render_slot_description(label: &str, description: Option<&str>, lines: &mut Vec<String>) {
    if let Some(description) = description.filter(|text| !text.trim().is_empty()) {
        lines.push(format!(
            "{label} meaning: {}",
            serde_json::json!(description)
        ));
    }
}

fn render_members(label: &str, members: Option<&[String]>, lines: &mut Vec<String>) {
    if let Some(members) = members.filter(|members| !members.is_empty()) {
        lines.push(format!("{label} members: {}", serde_json::json!(members)));
    }
}

fn render_value_evidence(
    cgs: &crate::CGS,
    label: &str,
    value: &crate::schema::NamedValueSchema,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    let mut value = value;
    let mut label = label.to_string();
    let mut seen = BTreeSet::new();
    loop {
        render_slot_description(&label, Some(&value.description), lines);
        render_members(&label, value.allowed_values.as_deref(), lines);
        let Some(items) = &value.array_items else {
            break;
        };
        use crate::schema::ValueDomainSlot;
        let key = items.value_domain_key().as_str();
        if !seen.insert(key) {
            return Err(format!("cyclic discovery array value domain: {key}"));
        }
        value = cgs
            .named_value_for_slot(items)
            .map_err(|error| error.to_string())?;
        label.push_str("[]");
        lines.push(format!("{label}: {:?}", value.field_type));
    }
    Ok(())
}

fn render_input_type(
    cgs: &crate::CGS,
    path: &str,
    ty: &crate::InputType,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    use crate::schema::{InputFieldWire, InputType};
    match ty {
        InputType::None => {}
        InputType::Value {
            field_type,
            allowed_values,
        } => {
            lines.push(format!("Input {path}: {field_type:?}"));
            render_members(&format!("Input {path}"), allowed_values.as_deref(), lines);
        }
        InputType::Object { fields, .. } => {
            for field in fields {
                let path = format!("{path}.{}", field.name);
                match &field.wire {
                    InputFieldWire::Registry(_) => {
                        let value = field.named_value(cgs).map_err(|e| e.to_string())?;
                        lines.push(format!(
                            "Input {path}: {:?}; required={}",
                            value.field_type, field.required
                        ));
                        render_value_evidence(cgs, &format!("Input {path}"), value, lines)?;
                    }
                    InputFieldWire::Inline(ty) => render_input_type(cgs, &path, ty, lines)?,
                }
                render_slot_description(
                    &format!("Input {path}"),
                    field.description.as_deref(),
                    lines,
                );
            }
        }
        InputType::Array { element_type, .. } => {
            render_input_type(cgs, &format!("{path}[]"), element_type, lines)?
        }
        InputType::Union { variants } => {
            for variant in variants {
                render_input_type(
                    cgs,
                    &format!("{path}.{}", variant.name),
                    &InputType::Object {
                        fields: variant.fields.clone(),
                        additional_fields: false,
                    },
                    lines,
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role_fixture() -> crate::CGS {
        crate::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/discovery_value_roles"),
        )
        .expect("abstract discovery role fixture")
    }

    proptest::proptest! {
        #[test]
        fn collection_semantics_cannot_rewrite_operation_contract(meaning in "[a-zA-Z ]{1,120}") {
            let mut cgs = role_fixture();
            let before = capability_documents(&cgs).unwrap();
            for entity in cgs.entities.values_mut() {
                entity.description = meaning.clone();
            }
            let after = capability_documents(&cgs).unwrap();
            for (old, new) in before.iter().zip(&after) {
                proptest::prop_assert_eq!(&old.operation, &new.operation);
                proptest::prop_assert_ne!(&old.collection, &new.collection);
                let decoded: CapabilityDocument = serde_json::from_str(&serde_json::to_string(new).unwrap()).unwrap();
                proptest::prop_assert_eq!(new, &decoded);
            }
        }
    }

    #[test]
    fn operation_restrictions_survive_projection_and_old_documents_are_rejected() {
        let mut cgs = role_fixture();
        cgs.capabilities
            .get_mut("record_create")
            .unwrap()
            .description = "Create a record only in an open collection".into();
        let documents = capability_documents(&cgs).unwrap();
        let doc = documents
            .iter()
            .find(|d| d.capability == "record_create")
            .unwrap();
        assert!(doc
            .operation
            .contract
            .contains("only in an open collection"));
        let mut wire = serde_json::to_value(doc).unwrap();
        wire.as_object_mut().unwrap().remove("operation");
        assert!(serde_json::from_value::<CapabilityDocument>(wire).is_err());
    }

    #[test]
    fn value_role_documents_preserve_semantics_across_input_lanes_and_array_fields() {
        let cgs = role_fixture();
        let documents = capability_documents(&cgs).expect("documents");
        let query = documents
            .iter()
            .find(|doc| doc.capability == "record_query")
            .unwrap();
        assert!(query
            .text
            .contains("Input scope.owner_email meaning: \"Owner of the collection being read\""));
        assert!(query
            .text
            .contains("Input scope.owner_email meaning: \"Select whose collection is read\""));
        assert!(query.text.contains(
            "Input selection.relationship members: [\"colleague\",\"relative\",\"neighbor\"]"
        ));
        assert!(query
            .text
            .contains("Input controls.mode members: [\"fresh\",\"cached\"]"));
        assert!(query
            .text
            .contains("Entity field roles[] members: [\"colleague\",\"relative\",\"neighbor\"]"));
        let get = documents
            .iter()
            .find(|doc| doc.capability == "record_get")
            .unwrap();
        assert!(get.text.contains(
            "Input arguments.owner_email meaning: \"Owner of the collection being read\""
        ));
        let create = documents
            .iter()
            .find(|doc| doc.capability == "record_create")
            .unwrap();
        assert!(create.text.contains(
            "Input payload.relationships[] members: [\"colleague\",\"relative\",\"neighbor\"]"
        ));
        assert!(documents
            .iter()
            .all(|doc| !doc.text.contains("private-default@example.com")
                && !doc.text.contains("fixture.invalid")));
    }

    #[test]
    fn value_role_document_hash_changes_with_semantic_evidence() {
        let mut cgs = role_fixture();
        let before = capability_documents(&cgs).unwrap();
        cgs.values.get_mut("owner").unwrap().description = "Different collection owner role".into();
        let after = capability_documents(&cgs).unwrap();
        let before = before
            .iter()
            .find(|doc| doc.capability == "record_query")
            .unwrap();
        let after = after
            .iter()
            .find(|doc| doc.capability == "record_query")
            .unwrap();
        assert_ne!(before.text_hash, after.text_hash);
        assert_ne!(
            embedding_cache_key(before, &EmbeddingProfile::default()),
            embedding_cache_key(after, &EmbeddingProfile::default())
        );
        assert_eq!(after.text_hash, content_hash(after.text.as_bytes()));
    }

    #[test]
    fn value_role_documents_reject_previous_renderer_artifacts() {
        let cgs = role_fixture();
        let artifact = CatalogDiscoveryArtifact {
            entry_id: "role_fixture".into(),
            cgs_hash: cgs.catalog_cgs_hash_hex(),
            renderer_version: DISCOVERY_RENDERER_VERSION - 1,
            profile: EmbeddingProfile::default(),
            capabilities: vec![],
            prerequisites: cgs.prerequisites.clone(),
        };
        assert_eq!(
            artifact.validate(&cgs).unwrap_err(),
            "unsupported discovery renderer version"
        );
    }

    #[test]
    fn value_role_evidence_quotes_multiline_metadata() {
        let mut lines = Vec::new();
        render_slot_description("Input owner", Some("owner\nsecond line"), &mut lines);
        assert_eq!(lines, vec!["Input owner meaning: \"owner\\nsecond line\""]);
    }

    #[test]
    fn rejects_invalid_vectors_before_import() {
        assert!(validate_embedding(&[1.0], 2).is_err());
        assert!(validate_embedding(&[f32::NAN], 1).is_err());
        assert!(validate_embedding(&[0.0], 1).is_err());
        assert!(validate_embedding(&[1.0, -1.0], 2).is_ok());
    }

    #[test]
    fn embedding_cache_separates_model_profiles() {
        let doc = CapabilityDocument {
            capability: "read".into(),
            entity: "Record".into(),
            text: "Read records".into(),
            operation: OperationEvidence {
                kind: crate::schema::CapabilityKind::Query,
                receiver: None,
                contract: "Read records".into(),
            },
            collection: CollectionEvidence {
                meaning: "Records".into(),
            },
            text_hash: content_hash(b"Read records"),
            related_entities: vec![],
        };
        let first = EmbeddingProfile::default();
        let mut second = first.clone();
        second.model = "different-model".into();
        assert_ne!(
            embedding_cache_key(&doc, &first),
            embedding_cache_key(&doc, &second)
        );
    }
}
