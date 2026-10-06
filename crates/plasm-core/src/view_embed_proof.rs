//! Constructor-gated proof that a view_embed relation hop has a validated view producer.

use crate::RelationMaterialization;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ViewEmbedProofError {
    #[error(
        "{context}.relation `{relation}` requires view_embed_proof for view `{view}`; execute the view root before navigating `.{relation}`"
    )]
    ProofRequired {
        context: String,
        relation: String,
        view: String,
    },
    #[error("{context}.view_embed_proof is only valid with materialize view_embed")]
    ProofWithoutViewEmbed { context: String },
    #[error(
        "{context}.view_embed_proof.view `{actual}` does not match materialize view `{expected}`"
    )]
    ViewMismatch {
        context: String,
        expected: String,
        actual: String,
    },
    #[error("{context}.view_embed_proof.relation `{actual}` does not match relation `{expected}`")]
    RelationMismatch {
        context: String,
        expected: String,
        actual: String,
    },
    #[error("{context}.view_embed_proof.producer_node `{producer}` is unknown")]
    ProducerUnknown { context: String, producer: String },
}

/// Frozen at plan lower/validation time: the view root that materialized the parent row's embed refs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedViewEmbedProof {
    pub view: String,
    pub producer_node: String,
    pub relation: String,
}

impl ValidatedViewEmbedProof {
    pub fn new(view: String, producer_node: String, relation: String) -> Self {
        Self {
            view,
            producer_node,
            relation,
        }
    }

    /// Ensures `view_embed` materialization and optional proof agree on relation wire + view key.
    pub fn require_for_materialize<'a>(
        materialize: Option<&RelationMaterialization>,
        proof: Option<&'a Self>,
        relation_wire: &str,
        context: &str,
    ) -> Result<Option<&'a Self>, ViewEmbedProofError> {
        match materialize {
            Some(RelationMaterialization::ViewEmbed { view }) => {
                let proof = proof.ok_or_else(|| ViewEmbedProofError::ProofRequired {
                    context: context.to_owned(),
                    relation: relation_wire.to_owned(),
                    view: view.to_string(),
                })?;
                proof.ensure_matches(view.as_str(), relation_wire, context)?;
                Ok(Some(proof))
            }
            _ => {
                if proof.is_some() {
                    return Err(ViewEmbedProofError::ProofWithoutViewEmbed {
                        context: context.to_owned(),
                    });
                }
                Ok(None)
            }
        }
    }

    pub fn ensure_matches(
        &self,
        view: &str,
        relation_wire: &str,
        context: &str,
    ) -> Result<(), ViewEmbedProofError> {
        if self.view.as_str() != view {
            return Err(ViewEmbedProofError::ViewMismatch {
                context: context.to_owned(),
                expected: view.to_owned(),
                actual: self.view.clone(),
            });
        }
        if self.relation.as_str() != relation_wire {
            return Err(ViewEmbedProofError::RelationMismatch {
                context: context.to_owned(),
                expected: relation_wire.to_owned(),
                actual: self.relation.clone(),
            });
        }
        Ok(())
    }

    pub fn ensure_producer_known(
        &self,
        known_node_id: impl Fn(&str) -> bool,
        context: &str,
    ) -> Result<(), ViewEmbedProofError> {
        if self.producer_node.trim().is_empty() || !known_node_id(self.producer_node.as_str()) {
            return Err(ViewEmbedProofError::ProducerUnknown {
                context: context.to_owned(),
                producer: self.producer_node.clone(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RelationMaterialization;

    #[test]
    fn require_for_materialize_rejects_orphan_proof() {
        let proof = ValidatedViewEmbedProof::new("v".into(), "n0".into(), "items".into());
        let err = ValidatedViewEmbedProof::require_for_materialize(
            None,
            Some(&proof),
            "items",
            "plan.nodes[0]",
        )
        .expect_err("proof without view_embed");
        assert!(err
            .to_string()
            .contains("only valid with materialize view_embed"));
    }

    #[test]
    fn require_for_materialize_requires_proof() {
        let mat = RelationMaterialization::ViewEmbed {
            view: "lang_work_snapshot".into(),
        };
        let err = ValidatedViewEmbedProof::require_for_materialize(
            Some(&mat),
            None,
            "items",
            "plan.nodes[1]",
        )
        .expect_err("view_embed without proof");
        assert!(err.to_string().contains("view_embed_proof"));
    }

    #[test]
    fn ensure_matches_rejects_view_mismatch() {
        let proof = ValidatedViewEmbedProof::new("other".into(), "n0".into(), "items".into());
        let err = proof
            .ensure_matches("lang_work_snapshot", "items", "ctx")
            .expect_err("view mismatch");
        assert!(err.to_string().contains("does not match materialize view"));
    }
}
