//! MCP tool Markdown rendering for live plan returns.

use crate::mcp_plasm_meta::PlasmPagingStepMeta;
use crate::mcp_run_markdown::{
    mcp_compact_markdown_multi_line, mcp_compact_markdown_single, mcp_coverage_preview_note,
    mcp_format_execute_result_table_or_tsv, mcp_inline_run_snapshot_line,
    mcp_prepend_artifact_followup_markdown, mcp_tsv_body_to_markdown_fence,
    slim_result_section_header_label, OmittedReferenceOnlyFields,
};
use crate::output::format_operations_block;
use crate::output::LossySummaryFieldNames;
use crate::run_artifacts::RunArtifactHandle;

use super::policy::{PublishPlan, ResolvedStepPublish, StepFormatOutcome, StepInBandMode};
use super::PublishedResultStep;

pub(crate) struct InlinePublishBodies {
    pub sections: String,
    pub char_count: usize,
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
        if resolved.mode.skips_inline_format() {
            continue;
        }
        let formatted = mcp_format_execute_result_table_or_tsv(
            &step.result,
            step.cgs.as_deref().or(cgs),
            resolved.mode.max_entity_rows(),
        );
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
    if resolved.artifact.is_some() {
        return;
    }
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
    let uri = resolved.artifact.as_ref().map(|h| {
        if h.canonical_plasm_uri.is_empty() {
            h.plasm_uri.as_str()
        } else {
            h.canonical_plasm_uri.as_str()
        }
    });
    sections.push_str(&mcp_coverage_preview_note(
        shown_rows_for_mode(resolved.mode, resolved.row_count),
        resolved.row_count,
        resolved.coverage,
        resolved.artifact.is_some(),
        uri,
        resolved.continue_handle.as_deref(),
        plan.artifact_access,
    ));
}

fn shown_rows_for_mode(mode: StepInBandMode, row_count: usize) -> usize {
    match mode {
        StepInBandMode::Full => row_count,
        StepInBandMode::CappedInline { shown } => shown,
        // Snapshot-only bodies carry no inline rows.
        StepInBandMode::SnapshotOnly => 0,
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
        if let Some(handle) = &resolved.artifact {
            if resolved.append_snapshot_supplement(fmt) {
                sections.push_str(&mcp_inline_run_snapshot_line(handle, plan.artifact_access));
            }
        }
    } else if resolved.mode.skips_inline_format() {
        if let Some(handle) = &resolved.artifact {
            let uri = if handle.canonical_plasm_uri.is_empty() {
                handle.plasm_uri.as_str()
            } else {
                handle.canonical_plasm_uri.as_str()
            };
            sections.push_str(&plan.artifact_access.artifact_only_body(uri));
        }
        append_coverage_note(sections, resolved, plan);
    } else {
        // Empty / unformatted Full path: still emit coverage.
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

    let char_count = sections.chars().count();
    InlinePublishBodies {
        sections,
        char_count,
        omitted_union: omitted_union.into(),
        paging,
    }
}

pub(crate) fn truncated_flags(plan: &PublishPlan, preview_needed: bool) -> Vec<bool> {
    plan.resolved
        .iter()
        .map(|resolved| resolved.is_truncated_for_transport(preview_needed))
        .collect()
}

pub(crate) fn snapshot_handles_for_meta(
    plan: &PublishPlan,
    preview_needed: bool,
) -> Vec<RunArtifactHandle> {
    plan.resolved
        .iter()
        .filter_map(|resolved| {
            let handle = resolved.artifact.as_ref()?;
            resolved
                .is_truncated_for_transport(preview_needed)
                .then(|| handle.clone())
        })
        .collect()
}

pub(crate) fn truncated_step_refs(
    plan: &PublishPlan,
    preview_needed: bool,
) -> Vec<(usize, &RunArtifactHandle)> {
    plan.resolved
        .iter()
        .enumerate()
        .filter_map(|(i, resolved)| {
            let handle = resolved.artifact.as_ref()?;
            resolved
                .is_truncated_for_transport(preview_needed)
                .then_some((i + 1, handle))
        })
        .collect()
}

pub(crate) fn render_markdown(
    plan: &PublishPlan,
    preview_needed: bool,
    inline: &InlinePublishBodies,
    omitted: &OmittedReferenceOnlyFields,
    use_mcp_meta: bool,
) -> String {
    let lossy_preview_union = lossy_preview_union(plan, preview_needed);
    let total = plan.resolved.len();
    let markdown = if preview_needed {
        let truncated_refs = truncated_step_refs(plan, preview_needed);
        let mut md = if total <= 1 {
            let (label, count_label) = plan
                .per_step_compact
                .first()
                .cloned()
                .unwrap_or_else(|| ("result".to_string(), "0 rows".into()));
            let mut md = mcp_compact_markdown_single(
                label.as_str(),
                count_label.as_str(),
                omitted,
                &lossy_preview_union,
            );
            if let Some((_, h)) = truncated_refs.first() {
                md.push_str(&mcp_inline_run_snapshot_line(h, plan.artifact_access));
            }
            md
        } else {
            mcp_compact_markdown_multi_line(
                total,
                plan.total_entity_rows,
                &plan.per_step_compact,
                omitted,
                &lossy_preview_union,
                &truncated_refs,
                plan.artifact_access,
            )
        };
        // Compact preview must still carry expression coverage (same formatter as inline).
        for resolved in &plan.resolved {
            append_coverage_note(&mut md, resolved, plan);
        }
        md
    } else {
        inline.sections.clone()
    };
    let handles_meta = snapshot_handles_for_meta(plan, preview_needed);
    // Publish Markdown is Plasm's own surface — do not append didactic observe footers.
    mcp_prepend_artifact_followup_markdown(markdown, use_mcp_meta, &handles_meta, omitted)
}

fn lossy_preview_union(plan: &PublishPlan, preview_needed: bool) -> LossySummaryFieldNames {
    if !preview_needed {
        return LossySummaryFieldNames::default();
    }
    let mut lossy_union_set = std::collections::BTreeSet::new();
    for resolved in &plan.resolved {
        if resolved.artifact.is_none() {
            continue;
        }
        if let Some(fmt) = &resolved.format {
            for name in fmt.lossy.as_slice() {
                lossy_union_set.insert(name.clone());
            }
            for name in fmt.in_band.field_names() {
                lossy_union_set.insert(name.clone());
            }
        }
    }
    LossySummaryFieldNames::from_vec_sorted_dedup(lossy_union_set.into_iter().collect())
}
