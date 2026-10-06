//! Shared policy for plan surface qualified-entity requirements (dry-run, stub materialization, render).

use crate::plasm_plan::{QualifiedEntityKey, ResultShape, ValidatedSurfaceNode};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SurfaceQualifiedEntityPolicyError {
    #[error("missing qualified_entity in a federated session")]
    MissingQualifiedEntityInFederatedSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SurfaceQualifiedEntityPolicy {
    /// Page continuation / synthetic paging — no catalog entity required.
    PageWithoutEntity,
    /// Executable surface with pinned catalog entity.
    RequiresQualifiedEntity(QualifiedEntityKey),
    /// Single-catalog sessions may omit `qualified_entity` on non-page surfaces.
    EntityOptional,
}

pub(crate) fn surface_qualified_entity_policy(
    surface: &ValidatedSurfaceNode,
    federated_session: bool,
) -> Result<SurfaceQualifiedEntityPolicy, SurfaceQualifiedEntityPolicyError> {
    if surface.result_shape == ResultShape::Page && surface.qualified_entity.is_none() {
        return Ok(SurfaceQualifiedEntityPolicy::PageWithoutEntity);
    }
    if let Some(qe) = surface.qualified_entity.clone() {
        return Ok(SurfaceQualifiedEntityPolicy::RequiresQualifiedEntity(qe));
    }
    if federated_session {
        return Err(SurfaceQualifiedEntityPolicyError::MissingQualifiedEntityInFederatedSession);
    }
    Ok(SurfaceQualifiedEntityPolicy::EntityOptional)
}

pub(crate) fn surface_qualified_entity_policy_err(
    _node_id: &str,
    surface: &ValidatedSurfaceNode,
    federated_session: bool,
) -> Result<SurfaceQualifiedEntityPolicy, SurfaceQualifiedEntityPolicyError> {
    surface_qualified_entity_policy(surface, federated_session)
}
