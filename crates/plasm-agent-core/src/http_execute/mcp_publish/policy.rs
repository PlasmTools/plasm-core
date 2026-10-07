//! Per-step MCP tool-response transport policy (row caps and text budgets).

use crate::mcp_run_markdown::{
    McpFormattedExecuteResult, McpResultTransportPolicy, OmittedReferenceOnlyFields,
};
use crate::output::{InBandSummaryReport, LossySummaryFieldNames};

use super::PublishedResultStep;

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
    pub source_rows: usize,
    pub delivery_range: std::ops::Range<usize>,
    pub artifact: Option<crate::run_artifacts::RunArtifactHandle>,
    pub format: Option<StepFormatOutcome>,
}

impl ResolvedStepPublish {
    pub(crate) fn is_truncated_for_transport(&self) -> bool {
        self.row_count < self.source_rows
            || self
                .format
                .as_ref()
                .is_some_and(|fmt| fmt.in_band.any_loss())
    }
}

pub(crate) struct PublishPlan {
    pub resolved: Vec<ResolvedStepPublish>,
    pub artifact_access: crate::mcp_run_markdown::ArtifactAccessMode,
}

impl PublishPlan {
    pub(crate) fn build(
        store: &impl crate::execute_session::PagingContinuationStore,
        logical_session_ref: Option<&str>,
        steps: &[PublishedResultStep],
        cgs: Option<&plasm_core::CGS>,
        policy: &McpResultTransportPolicy,
    ) -> Result<(Vec<PublishedResultStep>, Self), super::page::ResultPublicationError> {
        let mut delivered = Vec::with_capacity(steps.len());
        let mut resolved = Vec::with_capacity(steps.len());
        for source in steps {
            let page = super::page::PublishedPage::prepare(
                store,
                logical_session_ref,
                source,
                cgs,
                policy,
            )?;
            let (step, observation) = page.into_parts();
            let formatted = McpFormattedExecuteResult {
                tsv_body: observation.tsv,
                reference_only_omitted: OmittedReferenceOnlyFields::default(),
                lossy_summary_fields: LossySummaryFieldNames::default(),
                in_band_report: observation.fidelity,
            };
            resolved.push(ResolvedStepPublish {
                label: crate::mcp_run_markdown::return_label_for_step(
                    step.name.as_deref(),
                    step.node_id.as_deref(),
                ),
                row_count: observation.shown,
                source_rows: step.result.count(),
                delivery_range: step.result.collection.delivery_range(),
                count_label: format!("{} rows", observation.shown),
                coverage: observation.coverage,
                continue_handle: step
                    .result
                    .paging_handle
                    .as_ref()
                    .map(|h| h.as_str().to_owned()),
                artifact: step.artifact.clone(),
                format: Some(StepFormatOutcome {
                    omitted: formatted.reference_only_omitted.clone(),
                    lossy: formatted.lossy_summary_fields.clone(),
                    in_band: formatted.in_band_report.clone(),
                    formatted,
                }),
            });
            delivered.push(step);
        }
        Ok((
            delivered,
            Self {
                resolved,
                artifact_access: policy.artifact_access,
            },
        ))
    }
}
