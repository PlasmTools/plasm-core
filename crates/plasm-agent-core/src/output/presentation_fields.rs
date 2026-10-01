//! Field sets for execute summaries: which columns are reference-only vs lossy-capped in MCP/TSV/table output.

use super::field_presentation;
use plasm_core::{AgentPresentation, CGS};
use plasm_runtime::ExecutionResult;
use std::collections::BTreeSet;

/// Sorted unique field names rendered with a **lossy** summary cap (full string only in run snapshot JSON).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LossySummaryFieldNames(Vec<String>);

impl LossySummaryFieldNames {
    pub(crate) fn from_vec_sorted_dedup(mut v: Vec<String>) -> Self {
        v.sort();
        v.dedup();
        Self(v)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn join_comma(&self) -> String {
        self.0.join(", ")
    }

    pub(crate) fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl AsRef<[String]> for LossySummaryFieldNames {
    fn as_ref(&self) -> &[String] {
        &self.0
    }
}

/// Field names whose schema uses [`AgentPresentation::Lossy`] for in-band summaries.
pub(crate) fn lossy_summary_field_names(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
) -> LossySummaryFieldNames {
    let Some(cgs) = cgs else {
        return LossySummaryFieldNames::default();
    };
    let mut names = BTreeSet::new();
    for entity in result.entities() {
        for key in entity.fields.keys() {
            if matches!(
                field_presentation(Some(cgs), &entity.reference.entity_type, key.as_str()),
                Some(AgentPresentation::Lossy)
            ) {
                names.insert(key.clone());
            }
        }
    }
    LossySummaryFieldNames::from_vec_sorted_dedup(names.into_iter().collect())
}
