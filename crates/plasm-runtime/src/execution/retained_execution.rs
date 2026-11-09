//! Shared accounting for state retained by suspended execution.
use super::effect_continuation::EffectContinuationError;
use std::sync::{Arc, Mutex};

/// Aggregate accounted bytes retained by one active execution.
pub const MAX_RETAINED_EXECUTION_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct RetainedExecutionBudget {
    used: Mutex<usize>,
}

/// A reservation releases its charge when the owning execution frame is dropped.
pub struct RetainedExecutionReservation {
    budget: Option<Arc<RetainedExecutionBudget>>,
    bytes: usize,
}

impl RetainedExecutionReservation {
    pub(crate) fn new(budget: Option<Arc<RetainedExecutionBudget>>) -> Self {
        Self { budget, bytes: 0 }
    }
}

// Called only after charge_value_budget has validated the value's finite shape
// and bounded its depth. Charge backing allocations in addition to its logical
// payload, which may be much smaller than the capacity retained by the frame.
fn charge_selection_capacity(
    value: &plasm_core::Value,
    remaining: &mut usize,
) -> Result<(), plasm_core::ValueBudgetError> {
    use plasm_core::{Value, ValueBudgetError};
    let bytes = match value {
        Value::String(s) => s.capacity().saturating_sub(s.len()),
        Value::Array(values) => values
            .capacity()
            .saturating_mul(std::mem::size_of::<Value>()),
        Value::Object(values) => values
            .capacity()
            .saturating_mul(std::mem::size_of::<(String, Value)>().saturating_add(16))
            .saturating_mul(2),
        _ => 0,
    };
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(ValueBudgetError::BytesExceeded)?;
    match value {
        Value::Array(values) => {
            for value in values {
                charge_selection_capacity(value, remaining)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                *remaining = remaining
                    .checked_sub(key.capacity().saturating_sub(key.len()))
                    .ok_or(ValueBudgetError::BytesExceeded)?;
                charge_selection_capacity(value, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}

impl RetainedExecutionReservation {
    /// Charge a parsed expression and the allocations retained by its fields.
    pub fn add_parsed_expr(
        &mut self,
        parsed: &plasm_core::expr_parser::ParsedExpr,
    ) -> Result<(), EffectContinuationError> {
        if self.budget.is_none() {
            return Ok(());
        }
        self.add_retained(parsed)?;
        let projection = parsed.projection.as_ref().map_or(0, |fields| {
            fields.iter().fold(
                fields
                    .capacity()
                    .saturating_mul(std::mem::size_of::<String>()),
                |sum, field| sum.saturating_add(field.capacity()),
            )
        });
        self.add_capacity_bytes(
            std::mem::size_of::<plasm_core::expr_parser::ParsedExpr>()
                .saturating_add(super::expression_retention_capacity_bytes(&parsed.expr))
                .saturating_add(projection)
                .saturating_add(
                    parsed
                        .field_dot_extract
                        .as_ref()
                        .map_or(0, String::capacity),
                ),
        )
    }

    /// Add one resolved value retained by an execution frame.
    pub fn add_value(&mut self, value: &plasm_core::Value) -> Result<(), EffectContinuationError> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        let mut used = budget.used.lock().unwrap_or_else(|e| e.into_inner());
        let mut remaining = MAX_RETAINED_EXECUTION_BYTES - *used;
        let initial = remaining;
        plasm_core::charge_value_budget(value, &mut remaining)
            .map_err(EffectContinuationError::RetainedBudget)?;
        charge_selection_capacity(value, &mut remaining)
            .map_err(EffectContinuationError::RetainedBudget)?;
        let bytes = initial - remaining;
        *used += bytes;
        self.bytes += bytes;
        Ok(())
    }
    /// Add retained rows to this reservation without copying their payloads.
    pub fn add_selection(
        &mut self,
        rows: &[plasm_core::ValueRow],
    ) -> Result<(), EffectContinuationError> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        let mut used = budget.used.lock().unwrap_or_else(|e| e.into_inner());
        let mut remaining = MAX_RETAINED_EXECUTION_BYTES - *used;
        let initial = remaining;
        for row in rows {
            plasm_core::charge_value_budget(row, &mut remaining)
                .map_err(EffectContinuationError::RetainedBudget)?;
            charge_selection_capacity(row, &mut remaining)
                .map_err(EffectContinuationError::RetainedBudget)?;
        }
        let bytes = initial - remaining;
        *used += bytes;
        self.bytes += bytes;
        Ok(())
    }

    /// Charge known retained allocation capacity in addition to encoded payload.
    /// On failure the reservation and the shared budget are unchanged.
    pub fn add_capacity_bytes(&mut self, bytes: usize) -> Result<(), EffectContinuationError> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        let mut used = budget.used.lock().unwrap_or_else(|e| e.into_inner());
        if bytes > MAX_RETAINED_EXECUTION_BYTES.saturating_sub(*used) {
            return Err(EffectContinuationError::RetainedBudget(
                plasm_core::ValueBudgetError::BytesExceeded,
            ));
        }
        *used += bytes;
        self.bytes += bytes;
        Ok(())
    }
    /// Add a newly retained payload to this reservation without copying it.
    pub fn add_retained<T: serde::Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), EffectContinuationError> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        struct Counter {
            remaining: usize,
            exceeded: bool,
        }
        impl std::io::Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(remaining) = self.remaining.checked_sub(bytes.len()) {
                    self.remaining = remaining;
                    Ok(bytes.len())
                } else {
                    self.exceeded = true;
                    Err(std::io::Error::other("continuation budget exceeded"))
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut used = budget.used.lock().unwrap_or_else(|e| e.into_inner());
        let initial = MAX_RETAINED_EXECUTION_BYTES - *used;
        let mut counter = Counter {
            remaining: initial,
            exceeded: false,
        };
        if let Err(error) = serde_json::to_writer(&mut counter, value) {
            return Err(if counter.exceeded {
                EffectContinuationError::RetainedBudget(plasm_core::ValueBudgetError::BytesExceeded)
            } else {
                EffectContinuationError::RetainedSerialization(error)
            });
        }
        let bytes = initial - counter.remaining;
        *used += bytes;
        self.bytes += bytes;
        Ok(())
    }
}

impl Drop for RetainedExecutionReservation {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            let mut used = budget.used.lock().unwrap_or_else(|e| e.into_inner());
            *used -= self.bytes;
        }
    }
}

pub(super) fn reserve_pagination(
    budget: Option<Arc<RetainedExecutionBudget>>,
    resume: &super::QueryPaginationResumeData,
) -> Result<RetainedExecutionReservation, EffectContinuationError> {
    let mut retained = RetainedExecutionReservation::new(budget);
    retained.add_retained(&(
        &resume.query,
        &resume.residual,
        &resume.catalog_hash,
        &resume.compiled_catalog_hash,
        &resume.capability_name,
        &resume.env,
        &resume.template,
        &resume.config,
        &resume.state,
        resume.state.progress_guard.retained_components(),
    ))?;
    let mut capacity = crate::materialization::value_map_capacity(&resume.env, 0)
        .saturating_add(std::mem::size_of::<super::QueryPaginationResumeData>())
        .saturating_add(resume.catalog_hash.capacity())
        .saturating_add(resume.compiled_catalog_hash.capacity())
        .saturating_add(resume.residual.as_ref().map_or(
            0,
            super::retained_mutation::predicate_retention_capacity_bytes,
        ))
        .saturating_add(resume.capability_name.capacity())
        .saturating_add(
            resume
                .state
                .next_absolute_url
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(resume.state.progress_guard.retention_capacity_bytes())
        .saturating_add(
            resume
                .state
                .param_values
                .capacity()
                .saturating_mul(std::mem::size_of::<(String, Option<serde_json::Value>)>()),
        );
    for (name, value) in &resume.state.param_values {
        capacity = capacity.saturating_add(name.capacity()).saturating_add(
            value.as_ref().map_or(0, |value| {
                crate::materialization::json_retention_capacity(value, 0)
            }),
        );
    }
    retained.add_capacity_bytes(capacity)?;
    Ok(retained)
}

pub(super) fn reserve_selection_evidence(
    budget: Arc<RetainedExecutionBudget>,
    selection: &crate::execution_failure::EffectSelectionEvidence,
    frame_size: usize,
) -> Result<RetainedExecutionReservation, EffectContinuationError> {
    let mut guard = RetainedExecutionReservation::new(Some(budget));
    guard.add_retained(selection)?;
    let capacity = selection
        .scope_path
        .capacity()
        .saturating_mul(std::mem::size_of::<String>())
        .saturating_add(
            selection
                .occurrence_path
                .capacity()
                .saturating_mul(std::mem::size_of::<usize>()),
        )
        .saturating_add(frame_size)
        .saturating_add(selection.scope_path.iter().fold(0usize, |total, value| {
            total.saturating_add(value.capacity())
        }));
    guard.add_capacity_bytes(capacity)?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_expression_charges_spare_projection_capacity() {
        let budget = Arc::new(RetainedExecutionBudget::default());
        let mut field = String::with_capacity(MAX_RETAINED_EXECUTION_BYTES + 1);
        field.push('x');
        let parsed = plasm_core::expr_parser::ParsedExpr {
            expr: plasm_core::Expr::Query(plasm_core::QueryExpr::all("Item")),
            projection: Some(vec![field]),
            field_dot_extract: None,
        };
        let mut reservation = RetainedExecutionReservation::new(Some(budget.clone()));
        assert!(matches!(
            reservation.add_parsed_expr(&parsed),
            Err(EffectContinuationError::RetainedBudget(
                plasm_core::ValueBudgetError::BytesExceeded
            ))
        ));
        drop(reservation);
        let mut next = RetainedExecutionReservation::new(Some(budget));
        assert!(next
            .add_capacity_bytes(MAX_RETAINED_EXECUTION_BYTES)
            .is_ok());
    }
}
