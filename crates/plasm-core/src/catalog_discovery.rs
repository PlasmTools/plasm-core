//! Portable discovery evidence produced at catalog publication time.
//!
//! This module has no network or database access. Artifact validation is also
//! used by importers, before any generation can be activated.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod input_projection;
mod relation_use;
mod retrieval_text;

pub const DISCOVERY_RENDERER_VERSION: u32 = 18;
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
    cgs.prerequisites.validate(cgs)?;
    let mut capabilities: Vec<_> = cgs.capabilities.values().collect();
    capabilities.sort_by(|a, b| a.name.cmp(&b.name));
    capabilities
        .into_iter()
        .map(|cap| {
            let entity = cgs
                .entities
                .get(&cap.domain)
                .ok_or_else(|| format!("missing entity {}", cap.domain))?;
            let rendered = retrieval_text::render(cgs, cap, entity)?;
            let operation = OperationEvidence {
                kind: cap.kind,
                receiver: cap.inputs.receiver.clone(),
                contract: rendered.operation,
            };
            let collection = CollectionEvidence {
                meaning: rendered.collection,
            };
            let text = rendered.text;
            Ok(CapabilityDocument {
                capability: cap.name.to_string(),
                entity: cap.domain.to_string(),
                text_hash: content_hash(text.as_bytes()),
                text,
                operation,
                collection,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role_fixture() -> crate::CGS {
        let mut cgs = crate::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/discovery_value_roles"),
        )
        .expect("abstract discovery role fixture");
        cgs.bind_registry_entry_id("role_fixture");
        cgs
    }

    proptest::proptest! {
        #[test]
        fn entity_meaning_changes_index_without_rewriting_operation_contract(meaning in ".{0,150}") {
            let mut cgs = role_fixture();
            let before = capability_documents(&cgs).unwrap();
            for entity in cgs.entities.values_mut() {
                proptest::prop_assume!(entity.description != meaning);
                entity.description = meaning.clone();
            }
            let after = capability_documents(&cgs).unwrap();
            for (old, new) in before.iter().zip(&after) {
                proptest::prop_assert_eq!(&old.operation, &new.operation);
                proptest::prop_assert_ne!(&old.collection, &new.collection);
                proptest::prop_assert_ne!(&old.text, &new.text);
                proptest::prop_assert_ne!(&old.text_hash, &new.text_hash);
                proptest::prop_assert!(new.text.contains(&meaning));
                let decoded: CapabilityDocument = serde_json::from_str(&serde_json::to_string(new).unwrap()).unwrap();
                proptest::prop_assert_eq!(new, &decoded);
            }
        }
    }

    #[test]
    fn retrieval_text_contains_only_declared_capability_meanings() {
        let cgs = role_fixture();
        let documents = capability_documents(&cgs).unwrap();
        let query = documents
            .iter()
            .find(|d| d.capability == "record_query")
            .unwrap();
        assert!(query
            .text
            .contains("Works with Record records. Relationship-classified record"));
        assert!(query.text.contains(
            "Matching records are selected using Recorded relationship to the account holder"
        ));
        assert!(query
            .text
            .contains("Recorded relationship to the account holder"));
        assert!(query.text.contains("Recorded relationship labels"));
        assert!(!query.text.starts_with('{'));
        assert!(query
            .operation
            .contract
            .contains("Recorded relationship to the account holder"));
        assert!(query.operation.contract.contains("Read consistency mode"));
        assert!(query
            .collection
            .meaning
            .contains("Relationship-classified record"));
        assert_eq!(query.text_hash, content_hash(query.text.as_bytes()));
    }

    proptest::proptest! {
        #[test]
        fn input_descriptions_survive_indexing_and_serialization(description in ".{1,150}") {
            let mut cgs = role_fixture();
            let before = capability_documents(&cgs).unwrap();
            cgs.capabilities.get_mut("record_get").unwrap().inputs.arguments.as_mut().unwrap().description = Some(description.clone());
            cgs.capabilities.get_mut("record_create").unwrap().inputs.payload.as_mut().unwrap().description = Some(description.clone());
            let after = capability_documents(&cgs).unwrap();
            for (name, lane) in [("record_get", "arguments_description"), ("record_create", "payload_description")] {
                let old = before.iter().find(|d| d.capability == name).unwrap();
                let new = after.iter().find(|d| d.capability == name).unwrap();
                proptest::prop_assert_ne!(&old.text_hash, &new.text_hash);
                let decoded: CapabilityDocument = serde_json::from_slice(&serde_json::to_vec(new).unwrap()).unwrap();
                let _ = lane;
                proptest::prop_assert!(decoded.operation.contract.contains(&description));
                proptest::prop_assert_eq!(new, &decoded);
            }
        }

        #[test]
        fn declared_controls_change_retrieval_and_cache_identity(description in "[a-zA-Z ]{1,120}") {
            let mut cgs = role_fixture();
            proptest::prop_assume!(description != cgs.values["mode"].description);
            let before = capability_documents(&cgs).unwrap();
            cgs.values.get_mut("mode").unwrap().description = description;
            let after = capability_documents(&cgs).unwrap();
            let old = before.iter().find(|d| d.capability == "record_query").unwrap();
            let new = after.iter().find(|d| d.capability == "record_query").unwrap();
            proptest::prop_assert_ne!(&old.text, &new.text);
            proptest::prop_assert_ne!(&old.text_hash, &new.text_hash);
            proptest::prop_assert_eq!(&old.collection, &new.collection);
        }

        #[test]
        fn unobserved_entity_attributes_do_not_change_mutator_index(description in "[a-zA-Z ]{1,120}") {
            let mut cgs = role_fixture();
            let before = capability_documents(&cgs).unwrap();
            cgs.values.get_mut("mode").unwrap().description = description.clone();
            cgs.entities.get_mut("Record").unwrap().fields.get_mut("roles").unwrap().description = description;
            let after = capability_documents(&cgs).unwrap();
            let old = before.iter().find(|d| d.capability == "record_create").unwrap();
            let new = after.iter().find(|d| d.capability == "record_create").unwrap();
            proptest::prop_assert_eq!(&old.text, &new.text);
            proptest::prop_assert_eq!(&old.operation, &new.operation);
            proptest::prop_assert_eq!(&old.collection, &new.collection);
            let old_query = before.iter().find(|d| d.capability == "record_query").unwrap();
            let new_query = after.iter().find(|d| d.capability == "record_query").unwrap();
            proptest::prop_assert_ne!(&old_query.text, &new_query.text);
        }

        #[test]
        fn retrieval_meanings_preserve_exact_authored_text(description in ".{0,150}") {
            let mut cgs = role_fixture();
            cgs.values.get_mut("owner").unwrap().description = description.clone();
            let doc = capability_documents(&cgs).unwrap().into_iter().find(|d| d.capability == "record_query").unwrap();
            let decoded: CapabilityDocument = serde_json::from_slice(&serde_json::to_vec(&doc).unwrap()).unwrap();
            proptest::prop_assert_eq!(&doc, &decoded);
            proptest::prop_assert!(decoded.text.contains(&description));
            proptest::prop_assert_eq!(decoded.text_hash, content_hash(decoded.text.as_bytes()));
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
        assert!(query.operation.contract.contains("The collection is scoped by Select whose collection is read Owner of the collection being read"));
        assert!(query.operation.contract.contains("Matching records are selected using Recorded relationship to the account holder Allowed values: colleague, relative, neighbor."));
        assert!(query.operation.contract.contains(
            "Execution is controlled by Read consistency mode Allowed values: fresh, cached."
        ));
        assert!(query.operation.contract.contains("Each element describes Recorded relationship to the account holder Allowed values: colleague, relative, neighbor."));
        let get = documents
            .iter()
            .find(|d| d.capability == "record_get")
            .unwrap();
        assert!(get
            .operation
            .contract
            .contains("The operation accepts Owner of the collection being read"));
        let create = documents
            .iter()
            .find(|d| d.capability == "record_create")
            .unwrap();
        assert!(create.operation.contract.contains("Each element describes Recorded relationship to the account holder Allowed values: colleague, relative, neighbor."));
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
    fn value_role_evidence_preserves_multiline_metadata() {
        let mut cgs = role_fixture();
        cgs.values.get_mut("owner").unwrap().description = "owner\nsecond line".into();
        let docs = capability_documents(&cgs).unwrap();
        let doc = docs
            .iter()
            .find(|d| d.capability == "record_query")
            .unwrap();
        assert!(doc.text.contains("owner\nsecond line"));
        let wire = serde_json::to_vec(doc).unwrap();
        assert_eq!(
            *doc,
            serde_json::from_slice::<CapabilityDocument>(&wire).unwrap()
        );
    }

    #[test]
    fn rejects_invalid_vectors_before_import() {
        assert!(validate_embedding(&[1.0], 2).is_err());
        assert!(validate_embedding(&[f32::NAN], 1).is_err());
        assert!(validate_embedding(&[0.0], 1).is_err());
        assert!(validate_embedding(&[1.0, -1.0], 2).is_ok());
    }

    #[test]
    fn enum_member_meanings_survive_projection_and_codec() {
        let mut cgs = role_fixture();
        let before = capability_documents(&cgs).unwrap();
        let role = cgs.values.get_mut("role").unwrap();
        role.domain.enum_membership = Some(
            crate::value_domain::EnumMembership::try_new(
                vec!["colleague".into(), "relative".into(), "neighbor".into()],
                Some(indexmap::IndexMap::from([(
                    "colleague".into(),
                    "Person employed alongside the account holder".into(),
                )])),
            )
            .unwrap(),
        );
        let after = capability_documents(&cgs).unwrap();
        let prior = before
            .iter()
            .find(|d| d.capability == "record_query")
            .unwrap();
        let current = after
            .iter()
            .find(|d| d.capability == "record_query")
            .unwrap();
        assert!(current
            .text
            .contains("colleague: Person employed alongside the account holder"));
        assert_ne!(prior.text_hash, current.text_hash);
        let decoded: crate::CGS =
            serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
        assert_eq!(after, capability_documents(&decoded).unwrap());
        let mut stale = serde_json::to_value(current).unwrap();
        stale["related_entities"] = serde_json::json!([]);
        assert!(serde_json::from_value::<CapabilityDocument>(stale).is_err());
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
