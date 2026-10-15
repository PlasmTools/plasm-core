//! Shared MCP tool-response text budget, with per-step row admission.

use crate::mcp_run_markdown::{
    McpFormattedExecuteResult, McpResultTransportPolicy, OmittedReferenceOnlyFields,
};
use crate::output::{InBandSummaryReport, LossySummaryFieldNames, RenderedObservation};

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
        let observations = allocate_observations(steps, cgs, policy);
        let mut delivered = Vec::with_capacity(steps.len());
        let mut resolved = Vec::with_capacity(steps.len());
        for (source, observation) in steps.iter().zip(observations) {
            let page = super::page::PublishedPage::prepare(
                store,
                logical_session_ref,
                source,
                observation,
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

/// Allocate table text before publication registers any continuations. Preserve
/// fitting observations and reclaim paging slack for field fidelity first.
fn allocate_observations(
    steps: &[PublishedResultStep],
    cgs: Option<&plasm_core::CGS>,
    policy: &McpResultTransportPolicy,
) -> Vec<RenderedObservation> {
    let render = |source: &PublishedResultStep, rows, budget| {
        crate::output::render_observation(
            source.result.as_ref(),
            source.cgs.as_deref().or(cgs),
            rows,
            budget,
        )
    };
    let complete: Vec<_> = steps
        .iter()
        .map(|source| render(source, policy.in_band_entity_rows, usize::MAX))
        .collect();
    let demands: Vec<_> = complete
        .iter()
        .map(|observation| observation.tsv.len())
        .collect();
    let budgets = table_budgets(&demands, policy.inline_text_budget_bytes);
    let (mut observations, mut reusable): (Vec<_>, Vec<_>) = steps
        .iter()
        .zip(complete)
        .zip(budgets)
        .map(|((source, complete), budget)| {
            if complete.tsv.len() <= budget {
                (complete, None)
            } else {
                (
                    render(source, policy.in_band_entity_rows, budget),
                    Some(complete),
                )
            }
        })
        .unzip();
    let used: usize = observations
        .iter()
        .map(|observation| observation.tsv.len())
        .sum();
    let mut spare = policy.inline_text_budget_bytes.saturating_sub(used);
    let mut restorations: Vec<_> = steps
        .iter()
        .zip(&observations)
        .enumerate()
        .filter(|(_, (_, observation))| observation.fidelity.any_loss())
        .map(|(index, (source, observation))| {
            let complete = reusable[index]
                .take()
                .filter(|complete| complete.shown == observation.shown)
                .unwrap_or_else(|| render(source, observation.shown, usize::MAX));
            (index, complete)
        })
        .collect();
    restorations.sort_by_key(|(index, complete)| {
        complete
            .tsv
            .len()
            .saturating_sub(observations[*index].tsv.len())
    });
    // Complete ordinary values take priority over enlarging lossy previews,
    // independently of where an enormous result appeared.
    for (index, complete) in restorations {
        let needed = complete
            .tsv
            .len()
            .saturating_sub(observations[index].tsv.len());
        if !complete.fidelity.any_loss() && needed <= spare {
            spare -= needed;
            observations[index] = complete;
        }
    }
    // Keep the admitted row count: omitted rows already have exact continuations.
    for (source, observation) in steps.iter().zip(&mut observations) {
        if spare == 0 || !observation.fidelity.any_loss() {
            continue;
        }
        let budget = observation.tsv.len() + spare;
        let expanded = render(source, observation.shown, budget);
        if expanded.tsv.len() <= budget {
            spare = budget - expanded.tsv.len();
            *observation = expanded;
        }
    }
    observations
}

/// Fair allocation without penalizing a small table for a large sibling.
/// Allocation is independent of result order and never exceeds the total.
fn table_budgets(demands: &[usize], total: usize) -> Vec<usize> {
    let mut order: Vec<_> = demands.iter().copied().enumerate().collect();
    order.sort_by_key(|(_, demand)| *demand);
    let mut remaining = total;
    let mut budgets = vec![0; demands.len()];
    for (position, (index, demand)) in order.into_iter().enumerate() {
        let budget = demand.min(remaining / (demands.len() - position));
        budgets[index] = budget;
        remaining -= budget;
    }
    budgets
}

#[cfg(test)]
mod tests {
    use super::table_budgets;

    #[test]
    fn small_results_leave_budget_for_large_siblings() {
        assert_eq!(table_budgets(&[10_000, 100], 12_000), [10_000, 100]);
        assert_eq!(table_budgets(&[100, 10_000], 12_000), [100, 10_000]);
        assert_eq!(
            table_budgets(&[10_000, 100, 10_000], 12_000),
            [5950, 100, 5950]
        );
        assert_eq!(table_budgets(&[10, 10], 0), [0, 0]);
        assert!(table_budgets(&[], 12_000).is_empty());
    }
}
