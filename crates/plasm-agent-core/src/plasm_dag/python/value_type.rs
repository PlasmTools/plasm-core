//! Materialized value contracts for dependency sealing.
use super::*;
use crate::plasm_plan::PlanDataInput;
use plasm_core::value_contract::ValueContract as T;
impl Lower<'_> {
    pub(super) fn value_type(
        &self,
        value: &PlasmDataValue,
        inputs: &BTreeMap<String, PlanDataInput>,
    ) -> Result<T, String> {
        text::derive_contract(
            self.es,
            &self.state,
            self.frame.row.as_deref().unwrap_or(""),
            value,
            &inputs.values().cloned().collect::<Vec<_>>(),
            0,
        )
    }
}
