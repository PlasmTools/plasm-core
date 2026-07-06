//! Per-step MCP tool-response transport policy (row caps, snapshot deferral).

use crate::mcp_run_markdown::{
    mcp_preview_markdown_needed, McpFormattedExecuteResult, McpResultTransportPolicy,
    OmittedReferenceOnlyFields, MCP_SNAPSHOT_ONLY_ROW_THRESHOLD,
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
    pub(crate) fn resolve(step: &PublishedResultStep, policy: &McpResultTransportPolicy) -> Self {
        let row_count = step.result.count;
        if step.artifact.is_some()
            && (row_count > MCP_SNAPSHOT_ONLY_ROW_THRESHOLD || policy.exceeds_in_band(row_count))
        {
            Self::SnapshotOnly
        } else if policy.exceeds_in_band(row_count) {
            Self::CappedInline {
                shown: policy.in_band_entity_rows,
            }
        } else {
            Self::Full
        }
    }

    pub(crate) fn max_entity_rows(self) -> Option<usize> {
        match self {
            StepInBandMode::Full => None,
            StepInBandMode::CappedInline { shown } => Some(shown),
            StepInBandMode::SnapshotOnly => None,
        }
    }

    pub(crate) fn skips_inline_format(self) -> bool {
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
        Self {
            label: crate::mcp_run_markdown::return_label_for_step(
                step.name.as_deref(),
                step.node_id.as_deref(),
            ),
            row_count: step.result.count,
            mode: StepInBandMode::resolve(step, policy),
            artifact: step.artifact.clone(),
            format: None,
        }
    }

    pub(crate) fn append_snapshot_supplement(&self, fmt: &StepFormatOutcome) -> bool {
        self.artifact.is_some()
            && (self.mode.skips_inline_format()
                || !fmt.omitted.is_empty()
                || !fmt.lossy.is_empty()
                || fmt.in_band.any_loss())
    }

    pub(crate) fn is_truncated_for_transport(&self, preview_needed: bool) -> bool {
        match &self.format {
            Some(_) => preview_needed || self.mode.skips_inline_format(),
            None => preview_needed || self.mode.skips_inline_format(),
        }
    }

    /// Whether to attach `_meta` preview entity rows for this step. We skip them when an inline
    /// formatted body already carries the rows (`format.is_some()` and the mode is not deferring the
    /// table to a snapshot) — duplicating rows into `_meta` would be wasteful. Otherwise we include a
    /// preview when the result is untruncated, or small enough to fit the in-band cap.
    pub(crate) fn include_preview_entities(
        &self,
        truncated: bool,
        policy: &McpResultTransportPolicy,
    ) -> bool {
        if self.format.is_some() && !self.mode.skips_inline_format() {
            return false;
        }
        if self.row_count <= policy.in_band_entity_rows {
            return true;
        }
        if self.artifact.is_some() {
            return false;
        }
        !truncated
    }
}

pub(crate) struct PublishPlan {
    pub resolved: Vec<ResolvedStepPublish>,
    pub artifact_snapshot_preview: bool,
    pub total_entity_rows: usize,
    pub per_step_compact: Vec<(String, usize)>,
    pub artifact_access: crate::mcp_run_markdown::ArtifactAccessMode,
}

impl PublishPlan {
    pub(crate) fn build(steps: &[PublishedResultStep], policy: &McpResultTransportPolicy) -> Self {
        let resolved: Vec<ResolvedStepPublish> = steps
            .iter()
            .map(|step| ResolvedStepPublish::resolve(step, policy))
            .collect();
        let artifact_snapshot_preview = resolved
            .iter()
            .any(|r| r.row_count > MCP_SNAPSHOT_ONLY_ROW_THRESHOLD && r.artifact.is_some());
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
            artifact_access: policy.artifact_access,
        }
    }

    pub(crate) fn preview_needed(
        &self,
        inline_char_count: usize,
        policy: &McpResultTransportPolicy,
    ) -> bool {
        if self.artifact_snapshot_preview {
            return true;
        }
        let all_within_cap = self
            .resolved
            .iter()
            .all(|r| r.row_count <= policy.in_band_entity_rows);
        if all_within_cap {
            return false;
        }
        mcp_preview_markdown_needed(false, inline_char_count, policy)
    }

    /// Agent delivery hint mirrored into `structuredContent.plasm.result_delivery`.
    pub(crate) fn result_delivery(&self, preview_needed: bool) -> &'static str {
        if preview_needed {
            return "snapshot_only";
        }
        if self
            .resolved
            .iter()
            .any(|r| !r.mode.skips_inline_format())
        {
            "inline"
        } else {
            "snapshot_only"
        }
    }
}
