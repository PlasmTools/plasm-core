//! Engine ports. Implementations live in `plasm-runtime`. Values and declarations stay paired throughout execution.

use crate::plasm_monad::StepId;
use crate::ValueRow;

use super::collect::CollectReason;
use super::error::RowComputeError;
use super::ids::{EnginePlanId, FixtureScanId, FrameId, GraphSnapshotId};
use super::plan::RowPlan;
use super::schema::PlasmFrameSchema;
use crate::identity::EntityName;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanSource {
    Fixture {
        id: FixtureScanId,
        schema: PlasmFrameSchema,
    },
    Inline {
        schema: PlasmFrameSchema,
    },
    Graph {
        entity: EntityName,
        snapshot: GraphSnapshotId,
        schema: PlasmFrameSchema,
    },
}

pub struct IngestBatch<'a> {
    pub rows: &'a [ValueRow],
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectedFrame {
    pub schema: PlasmFrameSchema,
    pub rows: Vec<ValueRow>,
    /// Input occurrence retained by each output; synthesized rows have no witness.
    pub occurrences: Vec<Option<usize>>,
}

pub trait IngestRows<'a> {
    fn ingest(
        &mut self,
        source: &ScanSource,
        batch: IngestBatch<'a>,
    ) -> Result<FrameId, RowComputeError>;
}

pub trait CompileRowPlan {
    fn compile(&self, plan: &RowPlan) -> Result<EnginePlanId, RowComputeError>;
}

pub trait CollectRows {
    fn collect(
        &self,
        id: EnginePlanId,
        reason: CollectReason,
    ) -> Result<CollectedFrame, RowComputeError>;
}

/// Engine capability composition over a borrowed input lifetime.
pub trait RowComputeEngine<'a>: IngestRows<'a> + CompileRowPlan + CollectRows {}

impl<'a, T> RowComputeEngine<'a> for T where T: IngestRows<'a> + CompileRowPlan + CollectRows {}

impl CollectedFrame {
    /// Validate the correspondence returned across the row-engine boundary.
    pub fn validate_correspondence(
        &self,
        input_len: usize,
    ) -> Result<(), super::error::RowCorrespondenceError> {
        if self.rows.len() != self.occurrences.len() {
            return Err(super::error::RowCorrespondenceError::LengthMismatch);
        }
        if self
            .occurrences
            .iter()
            .flatten()
            .any(|index| *index >= input_len)
        {
            return Err(super::error::RowCorrespondenceError::InputIndexOutOfBounds);
        }
        Ok(())
    }

    #[must_use]
    pub fn empty(schema: PlasmFrameSchema) -> Self {
        Self {
            schema,
            rows: Vec::new(),
            occurrences: Vec::new(),
        }
    }
}

impl CollectReason {
    #[must_use]
    pub fn program_return(step: StepId) -> Self {
        Self::ProgramReturn { step }
    }
}
