//! MCP-oriented tabular summaries (`tsv` fences) over [`ExecutionResult`].
//!
//! Policy (cell width, tab stripping) is explicit via [`TsvCellPolicy`] so this stays distinct from
//! generic table formatting in the parent module.

use super::in_band_fidelity::{InBandSummaryReport, SummaryFidelityLoss};
use plasm_core::CGS;
use plasm_runtime::ExecutionResult;
use std::collections::BTreeSet;

/// Unicode-scalar cap and sanitisation rules for MCP ` ```tsv ` cells.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TsvCellPolicy {
    pub max_scalars: usize,
}

impl TsvCellPolicy {
    pub(crate) const fn mcp_default() -> Self {
        Self {
            max_scalars: 16_384,
        }
    }
}

/// Single-line TSV cell: strip tabs/newlines, collapse whitespace, truncate (Unicode scalars).
pub(crate) fn sanitize_tsv_cell(s: &str, policy: &TsvCellPolicy) -> String {
    sanitize_tsv_cell_with_flag(s, policy).0
}

fn sanitize_tsv_cell_with_flag(s: &str, policy: &TsvCellPolicy) -> (String, bool) {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let no_tabs = collapsed.replace('\t', " ");
    let n = no_tabs.chars().count();
    if n <= policy.max_scalars {
        (no_tabs, false)
    } else {
        let head: String = no_tabs
            .chars()
            .take(policy.max_scalars.saturating_sub(1))
            .collect();
        (format!("{head}…"), true)
    }
}

/// Tab-separated rows (header + data) with the same reference-only rules as
/// [`super::format_result_with_cgs`] table mode. Intended for MCP when omissions are empty so the
/// fence is fully summarisable without `(in artifact)` placeholders.
pub(crate) fn format_result_tsv_with_cgs(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
) -> (String, Vec<String>, InBandSummaryReport) {
    let policy = TsvCellPolicy::mcp_default();
    let mut omitted = BTreeSet::new();
    let mut report = InBandSummaryReport::default();
    let text = format_tsv_inner(result, cgs, max_entity_rows, &mut omitted, &policy, &mut report);
    (text, omitted.into_iter().collect(), report)
}

fn format_tsv_inner(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
    omitted: &mut BTreeSet<String>,
    policy: &TsvCellPolicy,
    report: &mut InBandSummaryReport,
) -> String {
    if result.entities.is_empty() {
        return "(no results)".into();
    }

    let columns = super::union_entity_table_columns(result, cgs, max_entity_rows);

    let mut lines: Vec<String> = Vec::new();
    let header_cells: Vec<String> = columns
        .iter()
        .map(|c| sanitize_tsv_cell(c.as_str(), policy))
        .collect();
    lines.push(header_cells.join("\t"));

    let row_limit = max_entity_rows.unwrap_or(usize::MAX);
    for entity in result.entities.iter().take(row_limit) {
        let row: Vec<String> = columns
            .iter()
            .map(|col| {
                let raw = super::format_summary_column_cell(
                    col.as_str(),
                    entity,
                    cgs,
                    omitted,
                    Some(report),
                );
                let (cell, transport_truncated) = sanitize_tsv_cell_with_flag(&raw, policy);
                if transport_truncated && col.as_str() != "_ref" {
                    report.record(col.as_str(), SummaryFidelityLoss::TsvScalarTransportClamp);
                }
                cell
            })
            .collect();
        lines.push(row.join("\t"));
    }

    lines.join("\n")
}
