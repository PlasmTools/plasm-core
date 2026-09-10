//! Portable discovery evidence produced at catalog publication time.
//!
//! This module has no network or database access. Artifact validation is also
//! used by importers, before any generation can be activated.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const DISCOVERY_RENDERER_VERSION: u32 = 1;
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDocument {
    pub capability: String,
    pub entity: String,
    pub text: String,
    pub text_hash: String,
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
            for field in entity.fields.values() {
                let value = field.named_value(cgs).map_err(|e| e.to_string())?;
                lines.push(format!(
                    "Entity field {}: {:?}",
                    field.name, value.field_type
                ));
            }
            let mut related_entities = BTreeSet::new();
            for (wire, relation) in &entity.relations {
                lines.push(format!(
                    "Relation {wire} -> {}: {}",
                    relation.target_resource, relation.description
                ));
                related_entities.insert(relation.target_resource.to_string());
            }
            let text = lines.join("\n");
            Ok(CapabilityDocument {
                capability: cap.name.to_string(),
                entity: cap.domain.to_string(),
                text_hash: content_hash(text.as_bytes()),
                text,
                related_entities: related_entities.into_iter().collect(),
            })
        })
        .collect()
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
        InputType::Value { field_type, .. } => lines.push(format!("Input {path}: {field_type:?}")),
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
                    }
                    InputFieldWire::Inline(ty) => render_input_type(cgs, &path, ty, lines)?,
                }
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
