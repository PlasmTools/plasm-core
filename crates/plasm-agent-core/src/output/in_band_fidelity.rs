//! Evidence of what the tabular / TSV summary withheld relative to full run snapshot JSON.
//!
//! Schema-tagged [`plasm_core::AgentPresentation::Lossy`] field names are tracked separately;
//! this ledger records **observed** clamps (default table
//! budget, TSV transport cap, reference-only placeholders).

use std::collections::BTreeMap;

/// Strongest-wins per field when multiple format stages touch the same column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SummaryFidelityLoss {
    /// CGS `Lossy` path: 72-scalar cap in summary cells.
    SchemaLossyCap,
    /// Default [`plasm_core::ValueTableCellBudget`] string clamp (e.g. 160 UTF-8 bytes + ellipsis).
    DefaultTableBudgetClamp,
    /// [`super::summary::sanitize_tsv_cell`] Unicode-scalar cap for MCP ` ```tsv ` fences.
    TsvScalarTransportClamp,
    /// Cell replaced with `(in artifact)`; full string only in snapshot JSON.
    ReferenceOnlyWithheld,
    /// Cell shows reserved `__plasm_attachment` summary (`uri (mime)`); bytes are not inlined.
    AttachmentRefSummary,
    /// Materialized data could not be projected through its declared public contract.
    InvalidObservation,
}

/// Columns where in-band text is not authoritative vs run artifact JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InBandSummaryReport {
    losses: BTreeMap<String, SummaryFidelityLoss>,
    projection_errors: BTreeMap<String, Vec<plasm_core::value_contract::ValueContractError>>,
}

impl InBandSummaryReport {
    pub fn record(&mut self, field: impl Into<String>, loss: SummaryFidelityLoss) {
        let field = field.into();
        self.losses
            .entry(field)
            .and_modify(|existing| {
                if loss > *existing {
                    *existing = loss;
                }
            })
            .or_insert(loss);
    }

    /// Keep typed host evidence even when the agent receives only a safe marker.
    pub fn record_invalid_observation(
        &mut self,
        field: impl Into<String>,
        error: plasm_core::value_contract::ValueContractError,
    ) {
        let field = field.into();
        self.record(field.clone(), SummaryFidelityLoss::InvalidObservation);
        self.projection_errors.entry(field).or_default().push(error);
    }

    pub fn projection_errors_for(
        &self,
        field: &str,
    ) -> &[plasm_core::value_contract::ValueContractError] {
        self.projection_errors
            .get(field)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.losses.is_empty()
    }

    /// Any observed in-band loss (including reference-only placeholders).
    pub fn any_loss(&self) -> bool {
        !self.losses.is_empty()
    }

    pub fn merge_from(&mut self, other: &InBandSummaryReport) {
        for (k, v) in &other.losses {
            self.record(k.clone(), *v);
        }
        for (field, errors) in &other.projection_errors {
            self.projection_errors
                .entry(field.clone())
                .or_default()
                .extend(errors.iter().cloned());
        }
    }

    pub(crate) fn field_names(&self) -> impl Iterator<Item = &String> {
        self.losses.keys()
    }
}

#[cfg(test)]
impl InBandSummaryReport {
    pub(crate) fn loss_for(&self, field: &str) -> Option<SummaryFidelityLoss> {
        self.losses.get(field).copied()
    }
}

pub(super) fn record_attachment_ref_summary(field_name: &str, report: &mut InBandSummaryReport) {
    report.record(field_name, SummaryFidelityLoss::AttachmentRefSummary);
}

pub(super) fn record_value_cell_fidelity(
    v: &plasm_core::Value,
    presentation: Option<plasm_core::AgentPresentation>,
    field_name: &str,
    display: &str,
    report: &mut InBandSummaryReport,
) {
    use plasm_core::AgentPresentation;
    use plasm_core::Value;

    match presentation {
        Some(AgentPresentation::ReferenceOnly) => {
            report.record(field_name, SummaryFidelityLoss::ReferenceOnlyWithheld);
        }
        Some(AgentPresentation::Lossy) => {
            if let Value::String(s) = v {
                if display != s.as_str() {
                    report.record(field_name, SummaryFidelityLoss::SchemaLossyCap);
                }
            }
        }
        Some(AgentPresentation::Default) | None => {
            if let Value::String(s) = v {
                if display != s.as_str() {
                    report.record(field_name, SummaryFidelityLoss::DefaultTableBudgetClamp);
                }
            }
        }
    }
}
