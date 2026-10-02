//! Per-step MCP tool-response transport policy (row caps and text budgets).

use crate::mcp_run_markdown::{
    McpFormattedExecuteResult, McpResultTransportPolicy, OmittedReferenceOnlyFields,
};
use crate::output::{InBandSummaryReport, LossySummaryFieldNames};

use super::PublishedResultStep;

/// How one return step is rendered in MCP tool Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepInBandMode {
    Full,
    CappedInline { shown: usize },
}

impl StepInBandMode {
    pub(crate) fn resolve(step: &PublishedResultStep, policy: &McpResultTransportPolicy) -> Self {
        let row_count = step.result.count();
        if policy.exceeds_in_band(row_count) {
            Self::CappedInline {
                shown: policy.in_band_entity_rows.max(1),
            }
        } else {
            Self::Full
        }
    }

    pub(crate) fn max_entity_rows(self) -> Option<usize> {
        match self {
            StepInBandMode::Full => None,
            StepInBandMode::CappedInline { shown } => Some(shown),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StepFormatOutcome {
    pub formatted: McpFormattedExecuteResult,
    pub omitted: OmittedReferenceOnlyFields,
    pub lossy: LossySummaryFieldNames,
    pub in_band: InBandSummaryReport,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedStepPublish {
    pub label: String,
    pub row_count: usize,
    pub count_label: String,
    pub coverage: plasm_runtime::ResultCoverage,
    pub continue_handle: Option<String>,
    pub mode: StepInBandMode,
    pub artifact: Option<crate::run_artifacts::RunArtifactHandle>,
    pub format: Option<StepFormatOutcome>,
}

impl ResolvedStepPublish {
    pub(crate) fn resolve(step: &PublishedResultStep, policy: &McpResultTransportPolicy) -> Self {
        Self {
            label: crate::mcp_run_markdown::return_label_for_step(
                step.name.as_deref(),
                step.node_id.as_deref(),
            ),
            row_count: step.result.count(),
            count_label: crate::mcp_run_markdown::slim_result_count_label(&step.result),
            coverage: step.result.coverage(),
            continue_handle: step
                .result
                .paging_handle
                .as_ref()
                .map(|h| h.as_str().to_string()),
            mode: StepInBandMode::resolve(step, policy),
            artifact: step.artifact.clone(),
            format: None,
        }
    }

    pub(crate) fn is_truncated_for_transport(&self) -> bool {
        !matches!(self.mode, StepInBandMode::Full)
            || self
                .format
                .as_ref()
                .is_some_and(|fmt| fmt.in_band.any_loss())
    }
}

pub(crate) struct PublishPlan {
    pub resolved: Vec<ResolvedStepPublish>,
    pub artifact_access: crate::mcp_run_markdown::ArtifactAccessMode,
    pub inline_text_budget_bytes: usize,
}

impl PublishPlan {
    pub(crate) fn build(steps: &[PublishedResultStep], policy: &McpResultTransportPolicy) -> Self {
        Self {
            resolved: steps
                .iter()
                .map(|step| ResolvedStepPublish::resolve(step, policy))
                .collect(),
            artifact_access: policy.artifact_access,
            inline_text_budget_bytes: policy.inline_text_budget_bytes,
        }
    }
}
