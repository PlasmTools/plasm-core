//! Three sync engine ports. Polars types do not escape this module.

use super::json_frame::{collect_json, ingest_json_rows, FrameState};
use super::plan_apply::apply_stored_plan;
use indexmap::IndexMap;
use plasm_core::{
    CollectReason, CollectRows, CollectedFrame, CompileRowPlan, EnginePlanId, FrameId, IngestBatch,
    IngestRows, PlasmFrameSchema, RowComputeError, RowPlan, ScanError, ScanSource, Value,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Polars-backed row engine. Handles are session-local and never stored on `PlasmComp`.
pub struct PolarsAdapter {
    frames: RefCell<HashMap<FrameId, FrameState>>,
    plans: RefCell<HashMap<EnginePlanId, RowPlan>>,
    next_frame: Cell<u64>,
    next_engine: Cell<u64>,
}

impl Default for PolarsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl PolarsAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frames: RefCell::new(HashMap::new()),
            plans: RefCell::new(HashMap::new()),
            next_frame: Cell::new(1),
            next_engine: Cell::new(1),
        }
    }

    fn json_from_values(rows: &[IndexMap<String, Value>]) -> Vec<serde_json::Value> {
        rows.iter()
            .map(|row| {
                let mut map = serde_json::Map::new();
                for (k, v) in row {
                    map.insert(k.clone(), plasm_core::plasm_value_to_json(v));
                }
                serde_json::Value::Object(map)
            })
            .collect()
    }
}

impl IngestRows for PolarsAdapter {
    fn ingest(
        &mut self,
        _source: &ScanSource,
        batch: IngestBatch<'_>,
    ) -> Result<FrameId, RowComputeError> {
        let json_rows = Self::json_from_values(batch.rows);
        let state = ingest_json_rows(&json_rows).map_err(|_| ScanError::UnboundFrame)?;
        let id = FrameId::new(self.next_frame.get());
        self.next_frame.set(id.as_u64() + 1);
        self.frames.borrow_mut().insert(id, state);
        Ok(id)
    }
}

impl CompileRowPlan for PolarsAdapter {
    fn compile(&self, plan: &RowPlan) -> Result<EnginePlanId, RowComputeError> {
        let id = EnginePlanId::new(self.next_engine.get());
        self.next_engine.set(id.as_u64() + 1);
        self.plans.borrow_mut().insert(id, plan.clone());
        Ok(id)
    }
}

impl CollectRows for PolarsAdapter {
    fn collect(
        &self,
        id: EnginePlanId,
        _reason: CollectReason,
    ) -> Result<CollectedFrame, RowComputeError> {
        let plans = self.plans.borrow();
        let plan = plans.get(&id).ok_or(ScanError::UnboundFrame)?;
        let frames = self.frames.borrow();
        let mut state = frames
            .get(&plan.source())
            .cloned()
            .ok_or(ScanError::UnboundFrame)?;
        drop(frames);
        apply_stored_plan(plan, &mut state).map_err(|_| ScanError::UnboundFrame)?;
        let rows_json = collect_json(&state).map_err(|_| ScanError::UnboundFrame)?;
        let rows = rows_json
            .into_iter()
            .map(|v| match v {
                serde_json::Value::Object(map) => map
                    .into_iter()
                    .map(|(k, val)| (k, plasm_core::json_value_to_plasm_value(&val)))
                    .collect(),
                other => {
                    let mut m = IndexMap::new();
                    m.insert(
                        "value".into(),
                        plasm_core::json_value_to_plasm_value(&other),
                    );
                    m
                }
            })
            .collect();
        Ok(CollectedFrame {
            schema: PlasmFrameSchema::opaque_object(),
            rows,
        })
    }
}
