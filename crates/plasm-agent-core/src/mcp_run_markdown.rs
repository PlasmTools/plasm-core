//! MCP `plasm` tool Markdown: previews, snapshot URI lines, TSV-vs-table choice, and expression previews.
//!
//! **Projection vs transport summary:** path-expression **projection** (which fields/rows the executor
//! materializes) is separate from this module’s table/TSV/preview paths, which may still cap lossy
//! or reference-only cells or defer full JSON to `resources/read`. See repository `docs/mcp-session-reuse.md` (section 5).
//!
//! **`plasm://…` run URIs** are MCP **`resources/read`** resource identifiers — they are **not** Plasm
//! path expressions and cannot be executed via the `plasm` tool alone.

use crate::output::{
    format_result_table_with_cgs, format_result_tsv_with_cgs, lossy_summary_field_names,
    InBandSummaryReport, LossySummaryFieldNames,
};
use crate::run_artifacts::RunArtifactHandle;
use plasm_core::CGS;
use plasm_runtime::ExecutionResult;
use std::collections::BTreeSet;

/// When MCP uses session meta compaction + adaptive preview; above this Unicode scalar count, markdown omits full tables.
pub const MCP_PLASM_MARKDOWN_PREVIEW_THRESHOLD_CHARS: usize = 4_000;

/// Hard cap on entity rows rendered inline in MCP tool Markdown (TSV fence or ASCII table).
/// Derived from the canonical host first-page size ([`crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE`])
/// so the first host page fits one MCP tool response; further pages use `run_ref` on `plasm_run`.
pub const MCP_IN_BAND_ENTITY_ROW_CAP: usize = crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE;

/// Above this row count, MCP may omit inline TSV and defer to snapshot-only preview (extreme results).
pub const MCP_SNAPSHOT_ONLY_ROW_THRESHOLD: usize = 500;

/// Unified transport policy for MCP `plasm` / `plasm_run` tool bodies and `_meta` preview rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpResultTransportPolicy {
    pub in_band_entity_rows: usize,
    pub markdown_preview_chars: usize,
    pub artifact_access: ArtifactAccessMode,
}

/// How agents on this MCP transport should fetch run snapshot JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtifactAccessMode {
    /// Host exposes MCP `resources/read` to the model (default).
    #[default]
    ResourcesRead,
    /// Tool-only host (e.g. Claude API MCP connector): expose `plasm_read_run_artifact`.
    ToolFallback,
}

impl ArtifactAccessMode {
    /// Tool-only hosts (Claude Desktop / Code / API connector) ingest `structuredContent`
    /// into model context — omit the UI render lane there; keep slim `structuredContent.plasm`.
    pub fn omits_structured_ui_lane(self) -> bool {
        matches!(self, Self::ToolFallback)
    }

    pub fn exposes_read_tool(self) -> bool {
        matches!(self, Self::ToolFallback)
    }

    pub fn artifact_read_instruction(self) -> &'static str {
        match self {
            Self::ResourcesRead => "MCP `resources/read`",
            Self::ToolFallback => "MCP `plasm_read_run_artifact`",
        }
    }

    pub fn row_limit_note(self, shown: usize, total: usize) -> String {
        format!(
            "\n\n_Showing {shown} of {total} rows — use {} on the snapshot URI for the full result._\n",
            self.artifact_read_instruction()
        )
    }

    pub fn snapshot_line(self, uri: &str) -> String {
        format!(
            "\n\n_Snapshot ({}; not a Plasm expression):_ `{uri}`\n",
            self.artifact_read_instruction()
        )
    }

    pub fn artifact_only_read_instruction(self) -> String {
        format!(
            "\n\n**Required:** fetch this step's rows via {} on the snapshot URI above — no inline TSV is sent when a snapshot is stored.\n",
            self.artifact_read_instruction()
        )
    }

    pub fn artifact_only_body(self, uri: &str) -> String {
        format!(
            "{}{}",
            self.snapshot_line(uri),
            self.artifact_only_read_instruction()
        )
    }
}

impl McpResultTransportPolicy {
    pub fn exceeds_in_band(&self, row_count: usize) -> bool {
        row_count > self.in_band_entity_rows
    }
}

impl Default for McpResultTransportPolicy {
    fn default() -> Self {
        Self {
            in_band_entity_rows: MCP_IN_BAND_ENTITY_ROW_CAP,
            markdown_preview_chars: MCP_PLASM_MARKDOWN_PREVIEW_THRESHOLD_CHARS,
            artifact_access: ArtifactAccessMode::default(),
        }
    }
}

/// Reserved after `## Result (preview)`; snapshot lines in the body carry the `resources/read` hint.
pub(crate) const MCP_MARKDOWN_PREVIEW_SINGLE_PROLOGUE: &str = "";

/// Reserved after `# Plasm run (preview)`; same as single preview.
pub(crate) const MCP_MARKDOWN_PREVIEW_MULTI_LINE_PROLOGUE: &str = "";

/// Sorted unique field names omitted from the in-band summary as `(in artifact)` (reference-only strings).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OmittedReferenceOnlyFields(Vec<String>);

impl OmittedReferenceOnlyFields {
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
}

impl From<BTreeSet<String>> for OmittedReferenceOnlyFields {
    fn from(set: BTreeSet<String>) -> Self {
        Self(set.into_iter().collect())
    }
}

impl AsRef<[String]> for OmittedReferenceOnlyFields {
    fn as_ref(&self) -> &[String] {
        &self.0
    }
}

/// In-band execute result plus which fields were withheld or lossy-capped in the Markdown summary.
#[derive(Debug, Clone)]
pub(crate) struct McpFormattedExecuteResult {
    pub block: McpExecuteResultBlock,
    pub reference_only_omitted: OmittedReferenceOnlyFields,
    pub lossy_summary_fields: LossySummaryFieldNames,
    /// Observed clamps / reference-only replacements while building `block` (single format pass).
    pub in_band_report: InBandSummaryReport,
}

/// In-band execute result body: either a fenced TSV block or an ASCII summary table.
#[derive(Debug, Clone)]
pub(crate) enum McpExecuteResultBlock {
    TsvFence { body: String },
    AsciiTable { body: String },
}

impl McpExecuteResultBlock {
    pub(crate) fn into_mcp_result_markdown(self) -> String {
        match self {
            McpExecuteResultBlock::TsvFence { body } => {
                let mut s = String::from("```tsv\n");
                s.push_str(&body);
                s.push_str("\n```\n");
                s
            }
            McpExecuteResultBlock::AsciiTable { body } => body,
        }
    }
}

/// Truncate long expression source lines for MCP previews and traces.
#[allow(dead_code)]
pub(crate) fn execute_expression_preview(expr: &str) -> String {
    const MAX_CHARS: usize = 400;
    let t = expr.trim();
    let n = t.chars().count();
    if n <= MAX_CHARS {
        return t.to_string();
    }
    let truncated: String = t.chars().take(MAX_CHARS).collect();
    format!("{truncated}… (truncated, total {n} chars)")
}

/// Compact preview when a stored snapshot backs an over-cap return, or the full body is huge.
pub(crate) fn mcp_preview_markdown_needed(
    artifact_backed_over_cap: bool,
    full_char_count: usize,
    policy: &McpResultTransportPolicy,
) -> bool {
    artifact_backed_over_cap || full_char_count > policy.markdown_preview_chars
}

pub(crate) fn mcp_in_band_row_limit_note(
    shown: usize,
    total: usize,
    has_snapshot: bool,
    artifact_access: ArtifactAccessMode,
) -> String {
    if shown >= total {
        return String::new();
    }
    if has_snapshot {
        artifact_access.row_limit_note(shown, total)
    } else {
        format!("\n\n_Showing {shown} of {total} rows (no run snapshot stored)._\n")
    }
}

/// One-line Markdown after an in-band result when the run snapshot must be fetched separately.
pub(crate) fn mcp_inline_run_snapshot_line(
    handle: &RunArtifactHandle,
    artifact_access: ArtifactAccessMode,
) -> String {
    artifact_access.snapshot_line(handle.plasm_uri.as_str())
}

/// Union of schema-tagged lossy columns and any field names recorded while formatting in-band cells
/// (default table budget, TSV transport clamp, reference-only).
pub(crate) fn merge_snapshot_column_hints(
    schema_lossy: &LossySummaryFieldNames,
    in_band: &InBandSummaryReport,
) -> LossySummaryFieldNames {
    let mut v: Vec<String> = schema_lossy.as_ref().to_vec();
    v.extend(in_band.field_names().cloned());
    LossySummaryFieldNames::from_vec_sorted_dedup(v)
}

/// MCP `plasm` tool: use fenced TSV when there are no reference-only omissions; otherwise ASCII table with `(in artifact)`.
pub(crate) fn mcp_format_execute_result_table_or_tsv(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
) -> McpFormattedExecuteResult {
    let lossy_summary_fields = lossy_summary_field_names(result, cgs);
    let (tsv, omitted_vec, tsv_report) = format_result_tsv_with_cgs(result, cgs, max_entity_rows);
    let reference_only_omitted = OmittedReferenceOnlyFields::from_vec_sorted_dedup(omitted_vec);
    if reference_only_omitted.is_empty() {
        McpFormattedExecuteResult {
            block: McpExecuteResultBlock::TsvFence { body: tsv },
            reference_only_omitted,
            lossy_summary_fields,
            in_band_report: tsv_report,
        }
    } else {
        let (table, omitted2, table_report) =
            format_result_table_with_cgs(result, cgs, max_entity_rows);
        let omitted2 = OmittedReferenceOnlyFields::from_vec_sorted_dedup(omitted2);
        debug_assert_eq!(
            reference_only_omitted, omitted2,
            "TSV vs table omission mismatch"
        );
        McpFormattedExecuteResult {
            block: McpExecuteResultBlock::AsciiTable { body: table },
            reference_only_omitted: omitted2,
            lossy_summary_fields,
            in_band_report: table_report,
        }
    }
}

/// Return label for slim markdown headers (`name` from plan return, else binding node id).
pub(crate) fn return_label_for_step(name: Option<&str>, node_id: Option<&str>) -> String {
    name.map(str::to_string)
        .or_else(|| node_id.map(str::to_string))
        .unwrap_or_else(|| "result".to_string())
}

pub(crate) fn slim_result_section_header(level: &str, label: &str, row_count: usize) -> String {
    format!("{level}{label} ({row_count} rows)\n\n")
}

pub(crate) fn mcp_compact_markdown_single(
    label: &str,
    entity_rows: usize,
    omitted: &OmittedReferenceOnlyFields,
    lossy_summary_fields: &LossySummaryFieldNames,
) -> String {
    let mut out = slim_result_section_header("## ", &format!("{label} (preview)"), entity_rows);
    out.push_str(MCP_MARKDOWN_PREVIEW_SINGLE_PROLOGUE);
    if !omitted.is_empty() {
        out.push_str("**Omitted from summary (reference-only fields):** ");
        out.push_str(&omitted.join_comma());
        out.push('\n');
    }
    if !lossy_summary_fields.is_empty() {
        out.push_str("**Abbreviated in-band columns (full text in snapshot):** ");
        out.push_str(&lossy_summary_fields.join_comma());
        out.push('\n');
    }
    out
}

pub(crate) fn mcp_compact_markdown_multi_line(
    total_steps: usize,
    total_entity_rows: usize,
    per_step: &[(String, usize)],
    omitted: &OmittedReferenceOnlyFields,
    lossy_summary_union: &LossySummaryFieldNames,
    truncated_step_uris: &[(usize, &RunArtifactHandle)],
    artifact_access: ArtifactAccessMode,
) -> String {
    let mut out = String::from("# Results (preview)\n\n");
    out.push_str(MCP_MARKDOWN_PREVIEW_MULTI_LINE_PROLOGUE);
    out.push_str("**Returns:** ");
    out.push_str(&total_steps.to_string());
    out.push('\n');
    out.push_str("**Total entity rows (sum):** ");
    out.push_str(&total_entity_rows.to_string());
    out.push_str("\n\n");
    for (i, (label, nrows)) in per_step.iter().enumerate() {
        let step_no = i + 1;
        out.push_str(&slim_result_section_header("### ", label, *nrows));
        if let Some((_, h)) = truncated_step_uris.iter().find(|(s, _)| *s == step_no) {
            out.push_str(&artifact_access.snapshot_line(h.plasm_uri.as_str()));
            out.push('\n');
        }
        out.push('\n');
    }
    if !omitted.is_empty() {
        out.push_str("**Omitted from summary (reference-only fields):** ");
        out.push_str(&omitted.join_comma());
        out.push('\n');
    }
    if !lossy_summary_union.is_empty() {
        out.push_str("**Abbreviated in-band columns (full text in snapshot):** ");
        out.push_str(&lossy_summary_union.join_comma());
        out.push('\n');
    }
    out
}

/// Prepends a short note when `use_mcp_meta` is true and there are reference-only omissions but no
/// inline snapshot row yet. Snapshot URIs elsewhere in the body remain the authoritative pointer to full JSON.
pub(crate) fn mcp_prepend_artifact_followup_markdown(
    markdown: String,
    use_mcp_meta: bool,
    truncated_snapshot_handles: &[RunArtifactHandle],
    omitted: &OmittedReferenceOnlyFields,
) -> String {
    if !use_mcp_meta || (truncated_snapshot_handles.is_empty() && omitted.is_empty()) {
        return markdown;
    }
    // Inline `_Snapshot (`resources/read`): …` rows already carry the hint; no extra banner.
    if !truncated_snapshot_handles.is_empty() {
        return markdown;
    }
    let mut p = String::from("**Reference-only fields omitted from this summary:** ");
    p.push_str(&omitted.join_comma());
    p.push_str(".\n\n");
    format!("{p}{markdown}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::LossySummaryFieldNames;
    use crate::run_artifacts::{artifact_http_path, plasm_run_resource_uri, RunArtifactId};

    #[test]
    fn mcp_preview_markdown_needed_on_row_cap_with_snapshot() {
        let policy = McpResultTransportPolicy::default();
        assert!(mcp_preview_markdown_needed(true, 100, &policy));
        assert!(!mcp_preview_markdown_needed(false, 100, &policy));
        assert!(mcp_preview_markdown_needed(false, 5_000, &policy));
    }

    #[test]
    fn mcp_markdown_preview_prologues_are_empty() {
        assert!(MCP_MARKDOWN_PREVIEW_SINGLE_PROLOGUE.is_empty());
        assert!(MCP_MARKDOWN_PREVIEW_MULTI_LINE_PROLOGUE.is_empty());
    }

    #[test]
    fn mcp_compact_markdown_single_preview_has_no_must_read_banner() {
        let omitted = OmittedReferenceOnlyFields::from_vec_sorted_dedup(vec!["body".into()]);
        let s =
            mcp_compact_markdown_single("sorted", 2, &omitted, &LossySummaryFieldNames::default());
        assert!(s.starts_with("## sorted (preview)"));
        assert!(!s.contains("MUST"), "preview: {s}");
        assert!(!s.contains("Optional full JSON"), "preview: {s}");
        assert!(s.contains("body"), "omitted fields listed: {s}");
    }

    #[test]
    fn mcp_compact_markdown_multi_line_preview_has_no_must_read_banner() {
        let omitted = OmittedReferenceOnlyFields::from_vec_sorted_dedup(vec!["commentBody".into()]);
        let h = sample_handle();
        let s = mcp_compact_markdown_multi_line(
            2,
            5,
            &[("first".into(), 3), ("second".into(), 2)],
            &omitted,
            &LossySummaryFieldNames::default(),
            &[(1, &h)],
            ArtifactAccessMode::ResourcesRead,
        );
        assert!(s.starts_with("# Results (preview)"));
        assert!(!s.contains("MUST"), "multi-line preview: {s}");
        assert!(!s.contains("Optional full JSON"), "multi-line preview: {s}");
        assert!(s.contains("commentBody"), "multi-line preview: {s}");
        assert!(s.contains(&h.plasm_uri), "truncated step URI inline: {s}");
    }

    fn sample_handle() -> RunArtifactHandle {
        let run_id = RunArtifactId::from_bytes([0u8; 32]);
        RunArtifactHandle {
            run_id,
            resource_index: 1,
            plasm_uri: "plasm://r/1".into(),
            canonical_plasm_uri: plasm_run_resource_uri("a", "b", &run_id),
            http_path: artifact_http_path("a", "b", &run_id),
            payload_len: 1,
            request_fingerprints: vec![],
        }
    }

    #[test]
    fn mcp_inline_run_snapshot_line_labels_resources_read_not_plasm_expr() {
        let h = sample_handle();
        let line = mcp_inline_run_snapshot_line(&h, ArtifactAccessMode::ResourcesRead);
        assert!(
            line.contains("resources/read") && line.contains("not a Plasm expression"),
            "{line}"
        );
    }

    #[test]
    fn mcp_prepend_artifact_followup_noop_without_mcp_meta() {
        let h = sample_handle();
        let omitted = OmittedReferenceOnlyFields::default();
        let out =
            mcp_prepend_artifact_followup_markdown("## Result\n".into(), false, &[h], &omitted);
        assert_eq!(out, "## Result\n");
    }

    #[test]
    fn mcp_prepend_artifact_followup_no_prefix_when_no_truncated_snapshots() {
        let run_id = RunArtifactId::from_bytes([0x55; 32]);
        let h = RunArtifactHandle {
            run_id,
            resource_index: 3,
            plasm_uri: "plasm://r/3".into(),
            canonical_plasm_uri: plasm_run_resource_uri("ph", "sess", &run_id),
            http_path: artifact_http_path("ph", "sess", &run_id),
            payload_len: 100,
            request_fingerprints: vec!["abc".into()],
        };
        let omitted = OmittedReferenceOnlyFields::default();
        let out = mcp_prepend_artifact_followup_markdown("## Result\n".into(), true, &[], &omitted);
        assert_eq!(out, "## Result\n", "{out}");
        let body = format!(
            "## Result\n{}",
            mcp_inline_run_snapshot_line(&h, ArtifactAccessMode::ResourcesRead)
        );
        let out2 = mcp_prepend_artifact_followup_markdown(body.clone(), true, &[h], &omitted);
        assert_eq!(out2, body, "{out2}");
        assert!(!out2.contains("Optional full JSON"), "{out2}");
        assert!(out2.contains("plasm://r/3"), "{out2}");
    }

    #[test]
    fn mcp_prepend_artifact_followup_with_handles_returns_body_unchanged() {
        let run_id = RunArtifactId::from_bytes([0x66; 32]);
        let h = RunArtifactHandle {
            run_id,
            resource_index: 2,
            plasm_uri: "plasm://r/2".into(),
            canonical_plasm_uri: plasm_run_resource_uri("ph", "sess", &run_id),
            http_path: artifact_http_path("ph", "sess", &run_id),
            payload_len: 100,
            request_fingerprints: vec![],
        };
        let omitted = OmittedReferenceOnlyFields::from_vec_sorted_dedup(vec!["body".into()]);
        let out =
            mcp_prepend_artifact_followup_markdown("## Result\n".into(), true, &[h], &omitted);
        assert_eq!(out, "## Result\n", "{out}");
        assert!(!out.contains("MUST"), "{out}");
    }

    #[test]
    fn mcp_prepend_artifact_followup_omitted_only_without_handles() {
        let omitted = OmittedReferenceOnlyFields::from_vec_sorted_dedup(vec!["body".into()]);
        let out = mcp_prepend_artifact_followup_markdown("table\n".into(), true, &[], &omitted);
        assert!(out.contains("Reference-only fields omitted"), "{out}");
        assert!(out.contains("body"), "{out}");
        assert!(!out.contains("call **`resources/read`**"), "{out}");
        assert!(out.ends_with("table\n"), "{out}");
    }
}
