//! MCP tool Markdown rendering for live plan returns.

use crate::mcp_plasm_meta::PlasmPagingStepMeta;
use crate::mcp_run_markdown::{
    mcp_coverage_preview_note, mcp_inline_run_snapshot_line, mcp_tsv_body_to_markdown_fence,
    slim_result_section_header_label, OmittedReferenceOnlyFields,
};
use crate::output::format_operations_block;
use crate::output::LossySummaryFieldNames;

use super::policy::{PublishPlan, ResolvedStepPublish, StepFormatOutcome, StepInBandMode};
use super::PublishedResultStep;

pub(crate) struct InlinePublishBodies {
    pub sections: String,
    pub omitted_union: OmittedReferenceOnlyFields,
    pub paging: Vec<PlasmPagingStepMeta>,
}

pub(crate) fn format_resolved_steps(
    steps: &[PublishedResultStep],
    plan: &mut PublishPlan,
    cgs: Option<&plasm_core::CGS>,
) {
    for (i, step) in steps.iter().enumerate() {
        let resolved = &mut plan.resolved[i];
        let observation = crate::output::render_observation(
            step.result.as_ref(),
            step.cgs.as_deref().or(cgs),
            resolved
                .mode
                .max_entity_rows()
                .unwrap_or(resolved.row_count),
            plan.inline_text_budget_bytes,
        );
        if observation.shown < resolved.row_count {
            resolved.mode = StepInBandMode::CappedInline {
                shown: observation.shown,
            };
        }
        resolved.coverage = observation.coverage;
        let formatted = crate::mcp_run_markdown::McpFormattedExecuteResult {
            tsv_body: observation.tsv,
            reference_only_omitted: OmittedReferenceOnlyFields::default(),
            lossy_summary_fields: LossySummaryFieldNames::default(),
            in_band_report: observation.fidelity,
        };
        resolved.format = Some(StepFormatOutcome {
            omitted: formatted.reference_only_omitted.clone(),
            lossy: formatted.lossy_summary_fields.clone(),
            in_band: formatted.in_band_report.clone(),
            formatted,
        });
    }
}

fn step_section_header(i: usize, total_steps: usize, label: &str, count_label: &str) -> String {
    if total_steps <= 1 {
        slim_result_section_header_label("## ", label, count_label)
    } else if i == 0 {
        format!(
            "# Results\n\n{}",
            slim_result_section_header_label("### ", label, count_label)
        )
    } else {
        slim_result_section_header_label("### ", label, count_label)
    }
}

fn append_paging_if_needed(
    sections: &mut String,
    paging: &mut Vec<PlasmPagingStepMeta>,
    step: &PublishedResultStep,
    resolved: &ResolvedStepPublish,
    step_index: usize,
) {
    let Some(handle) = &step.result.paging_handle else {
        return;
    };
    paging.push(PlasmPagingStepMeta::Next {
        run_step: step_index + 1,
        returned_count: resolved.row_count,
        next_run_ref: handle.clone(),
    });
    sections.push_str(&format!(
        "\n\nmore pages — call `plasm_run` with `run_ref: \"{}\"`.",
        handle.as_str()
    ));
}

fn append_coverage_note(sections: &mut String, resolved: &ResolvedStepPublish, plan: &PublishPlan) {
    sections.push_str(&mcp_coverage_preview_note(
        shown_rows_for_mode(resolved.mode, resolved.row_count),
        resolved.row_count,
        resolved.coverage,
        resolved.artifact.is_some(),
        None,
        resolved.continue_handle.as_deref(),
        plan.artifact_access,
    ));
}

fn shown_rows_for_mode(mode: StepInBandMode, row_count: usize) -> usize {
    match mode {
        StepInBandMode::Full => row_count,
        StepInBandMode::CappedInline { shown } => shown,
    }
}

fn build_step_section(
    sections: &mut String,
    paging: &mut Vec<PlasmPagingStepMeta>,
    i: usize,
    step: &PublishedResultStep,
    resolved: &ResolvedStepPublish,
    plan: &PublishPlan,
    _total_steps: usize,
) {
    if let Some(fmt) = &resolved.format {
        sections.push_str(&mcp_tsv_body_to_markdown_fence(&fmt.formatted.tsv_body));
        sections.push_str(&format_operations_block(&step.result));
        append_coverage_note(sections, resolved, plan);
        if fmt.in_band.any_loss() {
            sections.push_str("\nSome cells are marked preview or omitted; these are not exact values. Use typed `@compute` over the source rows to extract the needed fields or text. Preserve the selection; do not repeat completed actions to inspect their results.\n");
        }
        if let Some(handle) = &resolved.artifact {
            if plan.artifact_access != crate::mcp_run_markdown::ArtifactAccessMode::DagCompute
                && resolved.is_truncated_for_transport()
            {
                sections.push_str(&mcp_inline_run_snapshot_line(handle, plan.artifact_access));
            }
        }
    } else {
        append_coverage_note(sections, resolved, plan);
    }
    append_paging_if_needed(sections, paging, step, resolved, i);
}

pub(crate) fn build_inline_bodies(
    steps: &[PublishedResultStep],
    plan: &PublishPlan,
    total_steps: usize,
) -> InlinePublishBodies {
    let mut omitted_union = std::collections::BTreeSet::new();
    let mut paging = Vec::new();
    let mut sections = String::new();

    for (i, step) in steps.iter().enumerate() {
        let resolved = &plan.resolved[i];
        if let Some(fmt) = &resolved.format {
            omitted_union.extend(fmt.omitted.as_ref().iter().cloned());
        }
        if step.result.entities().is_empty() && !step.result.operations.is_empty() {
            if !sections.is_empty() {
                sections.push('\n');
            }
            sections.push_str(format_operations_block(&step.result).trim());
            continue;
        }
        if i > 0 {
            sections.push_str("\n\n");
        }
        sections.push_str(&step_section_header(
            i,
            total_steps,
            &resolved.label,
            &resolved.count_label,
        ));
        build_step_section(
            &mut sections,
            &mut paging,
            i,
            step,
            resolved,
            plan,
            total_steps,
        );
    }

    InlinePublishBodies {
        sections,
        omitted_union: omitted_union.into(),
        paging,
    }
}
