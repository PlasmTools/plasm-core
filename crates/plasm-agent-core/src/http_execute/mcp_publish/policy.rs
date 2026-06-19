//! Per-step MCP tool-response transport policy (row caps, snapshot deferral).

use crate::mcp_run_markdown::{
    mcp_preview_markdown_needed, McpFormattedExecuteResult, McpResultTransportPolicy,
    OmittedReferenceOnlyFields,
};
use crate::output::{InBandSummaryReport, LossySummaryFieldNames};

use super::PublishedResultStep;

/// How one return step is rendered in MCP tool Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepInBandMode {
    Full,
    CappedInline { shown: usize },
    SnapshotOnly,
}

impl StepInBandMode {
    pub(crate) fn max_entity_rows(self) -> Option<usize> {
        match self {
            StepInBandMode::Full => None,
            StepInBandMode::CappedInline { shown } => Some(shown),
            StepInBandMode::SnapshotOnly => None,
        }
    }

    pub(crate) fn defers_inline_table(self) -> bool {
        matches!(self, StepInBandMode::SnapshotOnly)
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
    pub mode: StepInBandMode,
    pub artifact: Option<crate::run_artifacts::RunArtifactHandle>,
    pub format: Option<StepFormatOutcome>,
}

impl ResolvedStepPublish {
    pub(crate) fn resolve(step: &PublishedResultStep, policy: &McpResultTransportPolicy) -> Self {
        let label = crate::mcp_run_markdown::return_label_for_step(
            step.name.as_deref(),
            step.node_id.as_deref(),
        );
        let row_count = step.result.count;
        let has_artifact = step.artifact.is_some();
        let mode = if crate::mcp_run_markdown::mcp_step_exceeds_in_band_row_cap(row_count, policy)
            && has_artifact
        {
            StepInBandMode::SnapshotOnly
        } else if crate::mcp_run_markdown::mcp_step_exceeds_in_band_row_cap(row_count, policy) {
            StepInBandMode::CappedInline {
                shown: policy.in_band_entity_rows,
            }
        } else {
            StepInBandMode::Full
        };
        Self {
            label,
            row_count,
            mode,
            artifact: step.artifact.clone(),
            format: None,
        }
    }

    pub(crate) fn requires_snapshot_read(&self, fmt: &StepFormatOutcome) -> bool {
        self.artifact.is_some()
            && (self.mode.defers_inline_table()
                || matches!(self.mode, StepInBandMode::CappedInline { .. })
                || !fmt.omitted.is_empty()
                || !fmt.lossy.is_empty()
                || fmt.in_band.any_loss())
    }

    pub(crate) fn include_preview_entities(&self, truncated: bool, policy: &McpResultTransportPolicy) -> bool {
        !truncated || self.row_count <= policy.in_band_entity_rows
    }
}

pub(crate) struct PublishPlan {
    pub resolved: Vec<ResolvedStepPublish>,
    pub artifact_snapshot_preview: bool,
    pub total_entity_rows: usize,
    pub per_step_compact: Vec<(String, usize)>,
}

impl PublishPlan {
    pub(crate) fn build(steps: &[PublishedResultStep], policy: &McpResultTransportPolicy) -> Self {
        let resolved: Vec<ResolvedStepPublish> = steps
            .iter()
            .map(|step| ResolvedStepPublish::resolve(step, policy))
            .collect();
        let artifact_snapshot_preview = resolved
            .iter()
            .any(|r| matches!(r.mode, StepInBandMode::SnapshotOnly));
        let total_entity_rows = resolved.iter().map(|r| r.row_count).sum();
        let per_step_compact = resolved
            .iter()
            .map(|r| (r.label.clone(), r.row_count))
            .collect();
        Self {
            resolved,
            artifact_snapshot_preview,
            total_entity_rows,
            per_step_compact,
        }
    }

    pub(crate) fn preview_needed(&self, inline_char_count: usize, policy: &McpResultTransportPolicy) -> bool {
        mcp_preview_markdown_needed(self.artifact_snapshot_preview, inline_char_count, policy)
    }
}
