//! Borrowed-value implementation of the typed row engine ports.

use super::eval::apply_stored_plan;
use super::rows::{collect_rows, ingest_rows, FrameState};
use plasm_core::{
    CollectReason, CollectRows, CollectedFrame, CompileRowPlan, EnginePlanId, FrameId, IngestBatch,
    IngestRows, PlasmFrameSchema, RowComputeError, RowPlan, ScanError, ScanSource,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Typed row engine. Handles are session-local and never stored on `PlasmComp`.
pub struct ValueRowEngine<'a> {
    frames: RefCell<HashMap<FrameId, FrameState<'a>>>,
    plans: RefCell<HashMap<EnginePlanId, RowPlan>>,
    next_frame: Cell<u64>,
    next_engine: Cell<u64>,
}

impl Default for ValueRowEngine<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl ValueRowEngine<'_> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frames: RefCell::new(HashMap::new()),
            plans: RefCell::new(HashMap::new()),
            next_frame: Cell::new(1),
            next_engine: Cell::new(1),
        }
    }
}

impl<'a> IngestRows<'a> for ValueRowEngine<'a> {
    fn ingest(
        &mut self,
        source: &ScanSource,
        batch: IngestBatch<'a>,
    ) -> Result<FrameId, RowComputeError> {
        let schema = match source {
            ScanSource::Inline { schema }
            | ScanSource::Fixture { schema, .. }
            | ScanSource::Graph { schema, .. } => schema,
        };
        let mut state = ingest_rows(batch.rows, schema.contract());
        state.shape = schema.shape().clone();
        let id = FrameId::new(self.next_frame.get());
        self.next_frame.set(id.as_u64() + 1);
        self.frames.borrow_mut().insert(id, state);
        Ok(id)
    }
}

impl CompileRowPlan for ValueRowEngine<'_> {
    fn compile(&self, plan: &RowPlan) -> Result<EnginePlanId, RowComputeError> {
        let frames = self.frames.borrow();
        let frame = frames.get(&plan.source()).ok_or(ScanError::UnboundFrame)?;
        let mut contract = frame.contract.clone();
        for (_, node) in plan.nodes().iter() {
            contract = plasm_core::row_plan::contracts::output_contract(&contract, node)
                .map_err(RowComputeError::Contract)?;
        }
        let id = EnginePlanId::new(self.next_engine.get());
        self.next_engine.set(id.as_u64() + 1);
        self.plans.borrow_mut().insert(id, plan.clone());
        Ok(id)
    }
}

impl CollectRows for ValueRowEngine<'_> {
    fn collect(
        &self,
        id: EnginePlanId,
        reason: CollectReason,
    ) -> Result<CollectedFrame, RowComputeError> {
        let plans = self.plans.borrow();
        let plan = plans.get(&id).ok_or(ScanError::UnboundFrame)?;
        if &reason != plan.collect() {
            return Err(plasm_core::row_plan::CollectError::CollectNotAtBarrier.into());
        }
        let frames = self.frames.borrow();
        let mut state = frames
            .get(&plan.source())
            .cloned()
            .ok_or(ScanError::UnboundFrame)?;
        drop(frames);
        apply_stored_plan(plan, &mut state)?;
        let rows = collect_rows(&state);
        let occurrences = state.rows.iter().map(|row| row.1).collect();
        Ok(CollectedFrame {
            occurrences,
            schema: PlasmFrameSchema::new(state.shape, state.contract)
                .map_err(RowComputeError::Schema)?,
            rows,
        })
    }
}
