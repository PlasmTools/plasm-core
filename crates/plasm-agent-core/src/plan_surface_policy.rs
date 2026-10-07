//! Shared policy for plan surface qualified-entity requirements (dry-run, stub materialization, render).

use crate::plasm_plan::{QualifiedEntityKey, ResultShape, ValidatedSurfaceNode};
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum SurfaceQualifiedEntityPolicyError {
    #[error("missing qualified_entity in a federated session")]
    MissingQualifiedEntityInFederatedSession,
    #[error("page continuation {handle} is unavailable")]
    PageUnavailable { handle: plasm_core::PagingHandle },
    #[error("page continuation origin does not match plan qualification")]
    PageOriginMismatch,
    #[error("page surface must contain a verified page expression")]
    PageExpressionMissing,
    #[error("stored page violates collection laws: {0}")]
    Collection(#[from] plasm_core::collection_codec::CollectionFault),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SurfaceQualifiedEntityPolicy {
    StoredPage(QualifiedEntityKey),
    /// Executable surface with pinned catalog entity.
    RequiresQualifiedEntity(QualifiedEntityKey),
    /// Single-catalog sessions may omit `qualified_entity` on non-page surfaces.
    EntityOptional,
}

impl From<SurfaceQualifiedEntityPolicyError> for plasm_runtime::ExecutionFailure {
    fn from(error: SurfaceQualifiedEntityPolicyError) -> Self {
        match error {
            SurfaceQualifiedEntityPolicyError::Collection(fault) => fault.into(),
            SurfaceQualifiedEntityPolicyError::PageUnavailable { handle } => {
                crate::http_execute::PagingHandleFault::Unavailable { handle }.into()
            }
            other => Self::new(
                plasm_runtime::FailureCause::Program,
                "page_surface_admission",
                other.to_string(),
            ),
        }
    }
}

pub(crate) fn surface_qualified_entity_policy(
    session: &impl crate::execute_session::PagingContinuationStore,
    surface: &ValidatedSurfaceNode,
    federated_session: bool,
) -> Result<SurfaceQualifiedEntityPolicy, SurfaceQualifiedEntityPolicyError> {
    use crate::execute_session::{ContinuationAdmission, PagingContinuation};
    if let Some(plasm_core::Expr::Page(page)) = surface.ir.as_ref().map(|ir| &ir.expr) {
        if surface.result_shape != ResultShape::Page
            || surface.effect_class != plasm_core::plasm_monad::EffectClass::Read
        {
            return Err(SurfaceQualifiedEntityPolicyError::PageExpressionMissing);
        }
        let continuation = session
            .resolve_paging_continuation(&page.handle)
            .ok_or_else(|| SurfaceQualifiedEntityPolicyError::PageUnavailable {
                handle: page.handle.clone(),
            })?;
        let (origin, stored) = match continuation.admission() {
            ContinuationAdmission::Provider { origin, .. } => (origin, false),
            ContinuationAdmission::Stored { origin, collection } => {
                // Validate through the same codec used by runtime delivery. This is read-only.
                collection.delivery(0..collection.resident_entities().len())?;
                (origin, true)
            }
        };
        if surface
            .qualified_entity
            .as_ref()
            .is_some_and(|actual| actual != origin)
        {
            return Err(SurfaceQualifiedEntityPolicyError::PageOriginMismatch);
        }
        return Ok(if stored {
            SurfaceQualifiedEntityPolicy::StoredPage(origin.clone())
        } else {
            SurfaceQualifiedEntityPolicy::RequiresQualifiedEntity(origin.clone())
        });
    }
    if surface.result_shape == ResultShape::Page {
        return Err(SurfaceQualifiedEntityPolicyError::PageExpressionMissing);
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
    session: &impl crate::execute_session::PagingContinuationStore,
    _node_id: &str,
    surface: &ValidatedSurfaceNode,
    federated_session: bool,
) -> Result<SurfaceQualifiedEntityPolicy, SurfaceQualifiedEntityPolicyError> {
    surface_qualified_entity_policy(session, surface, federated_session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute_session::{
        PagingContinuationStore, PagingResume, SyntheticPageCursor, SyntheticPageKind,
    };
    use crate::plasm_plan::{PlanNodeId, PlanNodeKind, ValidatedPlanExprIr};
    use plasm_core::{Expr, PagingHandle};

    struct Store(Option<PagingResume>);
    impl PagingContinuationStore for Store {
        fn resolve_paging_continuation(&self, _: &PagingHandle) -> Option<PagingResume> {
            self.0.clone()
        }
        fn register_synthetic_paging_continuation(
            &self,
            _: SyntheticPageCursor,
            _: Option<&str>,
        ) -> PagingHandle {
            panic!("admission must not mutate the continuation store")
        }
    }

    fn surface() -> ValidatedSurfaceNode {
        ValidatedSurfaceNode {
            id: PlanNodeId::new("page").unwrap(),
            kind: PlanNodeKind::Query,
            qualified_entity: Some(QualifiedEntityKey {
                entry_id: "fixture".into(),
                entity: "Projected".into(),
            }),
            ir: Some(ValidatedPlanExprIr {
                expr: Expr::Page(plasm_core::expr::PageExpr {
                    handle: PagingHandle::mint_monotonic(1),
                    limit: None,
                }),
                projection: None,
            }),
            ir_template: None,
            effect_class: plasm_core::plasm_monad::EffectClass::Read,
            result_shape: ResultShape::Page,
            projection: vec![],
            predicates: vec![],
            depends_on: vec![],
            uses_result: vec![],
            approval: None,
            page_size: None,
            pushed_read_budget: None,
        }
    }

    #[test]
    fn page_shape_does_not_authorize_an_unverified_continuation() {
        let mut page = surface();
        assert!(matches!(
            surface_qualified_entity_policy(&Store(None), &page, true),
            Err(SurfaceQualifiedEntityPolicyError::PageUnavailable { .. })
        ));
        page.ir = None;
        assert!(matches!(
            surface_qualified_entity_policy(&Store(None), &page, true),
            Err(SurfaceQualifiedEntityPolicyError::PageExpressionMissing)
        ));
    }

    #[test]
    fn collection_faults_retain_the_codec_failure_class_through_dry_admission() {
        let stage = crate::program_diagnostic::ProgramStageError::plan(
            crate::plasm_plan_run::DryPlanValidationError::SurfacePolicy {
                index: 0,
                source: SurfaceQualifiedEntityPolicyError::Collection(
                    plasm_core::collection_codec::CollectionFault::Conservation,
                ),
            },
        );
        let failure: plasm_runtime::ExecutionFailure = stage.into();
        assert_eq!(failure.cause, plasm_runtime::FailureCause::ResponseContract);
        assert_eq!(failure.code, "collection_conservation");
        assert!(failure.effects.is_empty());
        assert!(failure.dispatches.is_empty());
    }

    #[test]
    fn stored_page_admission_preserves_collection_and_rejects_forged_origin() {
        let mut page = surface();
        let source =
            crate::test_support::execution_fixtures::synthetic_published_result_step(3, None);
        let window = source.result.collection.delivery(1..3).unwrap();
        assert_eq!(window.delivery_range(), 1..3);
        let store = Store(Some(PagingResume::Synthetic(SyntheticPageCursor {
            kind: SyntheticPageKind::Delivery { continuation: None },
            node_id: "computed".into(),
            qualified_entity: page.qualified_entity.clone().unwrap(),
            collection: window.clone(),
            offset: 1,
            page_size: 1,
            request_fingerprints: vec![],
        })));
        assert_eq!(
            surface_qualified_entity_policy(&store, &page, true).unwrap(),
            SurfaceQualifiedEntityPolicy::StoredPage(page.qualified_entity.clone().unwrap())
        );
        page.qualified_entity.as_mut().unwrap().entry_id = "unrelated".into();
        assert!(matches!(
            surface_qualified_entity_policy(&store, &page, true),
            Err(SurfaceQualifiedEntityPolicyError::PageOriginMismatch)
        ));
        let PagingResume::Synthetic(cursor) = store.0.unwrap() else {
            unreachable!()
        };
        assert_eq!(cursor.offset, 1);
        assert_eq!(cursor.collection.count(), 3);
        assert_eq!(cursor.collection.delivery_range(), window.delivery_range());
        assert_eq!(cursor.collection.resident_entities().len(), 2);
    }
}
