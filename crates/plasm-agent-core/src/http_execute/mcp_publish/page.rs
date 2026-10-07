//! Admission from execution observations to bounded, resumable delivery pages.
use std::sync::Arc;

use crate::execute_session::{PagingContinuationStore, SyntheticPageCursor, SyntheticPageKind};
use crate::mcp_run_markdown::McpResultTransportPolicy;
use crate::output::RenderedObservation;
use plasm_core::CGS;

use super::PublishedResultStep;

#[derive(Debug, thiserror::Error)]
pub enum ResultPublicationError {
    #[error("presentation paging requires catalog ownership for step {step}")]
    OwnershipMissing { step: String },
    #[error("presentation page violates collection laws: {0}")]
    Collection(#[from] plasm_core::collection_codec::CollectionFault),
    #[error("meta_index lock poisoned")]
    MetaIndexPoisoned,
}

impl From<ResultPublicationError> for plasm_runtime::ExecutionFailure {
    fn from(error: ResultPublicationError) -> Self {
        match error {
            ResultPublicationError::Collection(fault) => fault.into(),
            other => Self::new(
                plasm_runtime::FailureCause::Runtime,
                "result_publication_failed",
                other.to_string(),
            ),
        }
    }
}

/// The renderer receives only admitted pages. Its row selection and TSV body
/// are established together; it cannot independently discard more rows.
pub(super) struct PublishedPage {
    step: PublishedResultStep,
    observation: RenderedObservation,
}

impl PublishedPage {
    pub(super) fn prepare(
        store: &impl PagingContinuationStore,
        logical_session_ref: Option<&str>,
        source: &PublishedResultStep,
        cgs: Option<&CGS>,
        policy: &McpResultTransportPolicy,
    ) -> Result<Self, ResultPublicationError> {
        let range = source.result.collection.delivery_range();
        if source.result.count() != 0
            && (range.is_empty()
                || (range.end < source.result.count() && source.result.paging_handle.is_none()))
        {
            return Err(plasm_core::collection_codec::CollectionFault::NotResident.into());
        }
        // Reuse the source-independent RowObservation formatter to measure the
        // physical delivery budget. Neither row limits nor byte limits change
        // the logical collection used by execution and downstream computation.
        let observation = crate::output::render_observation(
            source.result.as_ref(),
            source.cgs.as_deref().or(cgs),
            policy.in_band_entity_rows,
            policy.inline_text_budget_bytes,
        );
        let source_rows = source.result.entities().len();
        let mut step = source.clone();
        if observation.shown < source_rows {
            let qualified_entity = source
                .entry_id
                .as_ref()
                .zip(source.entity.as_ref())
                .map(|(entry_id, entity)| crate::plasm_plan::QualifiedEntityKey {
                    entry_id: entry_id.clone(),
                    entity: entity.clone(),
                })
                .ok_or_else(|| ResultPublicationError::OwnershipMissing {
                    step: source
                        .node_id
                        .clone()
                        .unwrap_or_else(|| source.display.clone()),
                })?;
            let mut result = source.result.as_ref().clone();
            let cursor = SyntheticPageCursor {
                kind: SyntheticPageKind::Delivery {
                    continuation: result.paging_handle.clone(),
                },
                node_id: source
                    .node_id
                    .clone()
                    .unwrap_or_else(|| source.display.clone()),
                qualified_entity,
                collection: result.collection.clone(),
                offset: observation.shown,
                page_size: observation.shown,
                request_fingerprints: result.request_fingerprints.clone(),
            };
            result.collection = result.collection.delivery(0..observation.shown)?;
            result.paging_handle =
                Some(store.register_synthetic_paging_continuation(cursor, logical_session_ref));
            result.has_more = true;
            step.result = Arc::new(result);
        }
        Ok(Self { step, observation })
    }

    pub(super) fn into_parts(self) -> (PublishedResultStep, RenderedObservation) {
        (self.step, self.observation)
    }
}
