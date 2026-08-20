//! Engine ports. Implementations live in `plasm-runtime`. No `polars` types here.

use crate::plasm_monad::StepId;
use crate::value::Value;
use indexmap::IndexMap;

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
    pub rows: &'a [IndexMap<String, Value>],
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectedFrame {
    pub schema: PlasmFrameSchema,
    pub rows: Vec<IndexMap<String, Value>>,
}

pub trait IngestRows {
    fn ingest(
        &mut self,
        source: &ScanSource,
        batch: IngestBatch<'_>,
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

/// Convenience bound for the single phase-1 adapter (not object-safe).
pub trait RowComputeEngine: IngestRows + CompileRowPlan + CollectRows {}

impl<T> RowComputeEngine for T where T: IngestRows + CompileRowPlan + CollectRows {}

impl CollectedFrame {
    #[must_use]
    pub fn empty(schema: PlasmFrameSchema) -> Self {
        Self {
            schema,
            rows: Vec::new(),
        }
    }
}

impl CollectReason {
    #[must_use]
    pub fn program_return(step: StepId) -> Self {
        Self::ProgramReturn { step }
    }
}
