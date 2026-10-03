//! MCP `plasm` tool Markdown: previews, snapshot URI lines, TSV-vs-table choice, and expression previews.
//!
//! **Projection vs transport summary:** path-expression **projection** (which fields/rows the executor
//! materializes) is separate from this module’s table/TSV/preview paths, which may still cap lossy
//! or reference-only cells or defer full JSON to `resources/read`. See repository `docs/mcp-session-reuse.md` (section 5).
//!
//! **`plasm://…` run URIs** are MCP **`resources/read`** resource identifiers — they are **not** Plasm
//! path expressions and cannot be executed via the `plasm` tool alone.

use crate::output::{InBandSummaryReport, LossySummaryFieldNames};
use crate::run_artifacts::RunArtifactHandle;
use plasm_runtime::ExecutionResult;
use std::collections::BTreeSet;

/// Target byte budget for each returned table; at least one bounded row stays visible.
pub const MCP_INLINE_TEXT_BUDGET_BYTES: usize = 12 * 1024;

/// Hard cap on entity rows rendered inline in MCP tool Markdown (TSV fence or ASCII table).
/// Derived from the canonical host first-page size ([`crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE`])
/// so the first host page fits one MCP tool response; further pages use `run_ref` on `plasm_run`.
pub const MCP_IN_BAND_ENTITY_ROW_CAP: usize = crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE;

/// Unified transport policy for MCP `plasm` / `plasm_run` tool bodies and `_meta` preview rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpResultTransportPolicy {
    pub in_band_entity_rows: usize,
    pub inline_text_budget_bytes: usize,
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
    /// Agent computes inside the reviewed DAG; snapshots remain host-side evidence.
    DagCompute,
}

impl ArtifactAccessMode {
    /// Tool-only hosts (Claude Desktop / Code / API connector) ingest `structuredContent`
    /// into model context — [`crate::mcp_delivery::McpDeliveryProfile::ToolFallback`]
    /// therefore emits content (+ optional `_meta.ui`) only.
    pub fn exposes_read_tool(self) -> bool {
        matches!(self, Self::ToolFallback)
    }

    pub fn artifact_read_instruction(self) -> &'static str {
        match self {
            Self::ResourcesRead => "MCP `resources/read`",
            Self::ToolFallback => "MCP `plasm_read_run_artifact`",
            Self::DagCompute => "typed Python DAG computation",
        }
    }

    /// Preview line: shown/snapshot relationship plus expression coverage.
    pub fn coverage_preview_note(
        self,
        shown: usize,
        snapshot_rows: usize,
        coverage: plasm_runtime::ResultCoverage,
        snapshot_uri: Option<&str>,
        continue_handle: Option<&str>,
    ) -> String {
        format_coverage_preview_note(
            shown,
            snapshot_rows,
            coverage,
            true,
            snapshot_uri,
            continue_handle,
            self,
        )
    }

    pub fn snapshot_line(self, uri: &str) -> String {
        if self == Self::DagCompute {
            return String::new();
        }
        format!(
            "\n\n_Snapshot ({}; not a Plasm expression):_ `{uri}`\n",
            self.artifact_read_instruction()
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
            inline_text_budget_bytes: MCP_INLINE_TEXT_BUDGET_BYTES,
            artifact_access: ArtifactAccessMode::default(),
        }
    }
}

/// Sorted unique field names omitted from the in-band summary as `(in artifact)` (reference-only strings).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OmittedReferenceOnlyFields(Vec<String>);

impl OmittedReferenceOnlyFields {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
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
    /// Raw TSV body (no markdown fence).
    pub tsv_body: String,
    pub reference_only_omitted: OmittedReferenceOnlyFields,
    pub lossy_summary_fields: LossySummaryFieldNames,
    /// Observed clamps / reference-only replacements while building `tsv_body` (single format pass).
    pub in_band_report: InBandSummaryReport,
}

/// Wrap a raw TSV body in a markdown ` ```tsv ` fence for MCP tool content.
pub(crate) fn mcp_tsv_body_to_markdown_fence(body: &str) -> String {
    let mut s = String::from("```tsv\n");
    s.push_str(body);
    s.push_str("\n```\n");
    s
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

pub(crate) fn mcp_coverage_preview_note(
    shown: usize,
    snapshot_rows: usize,
    coverage: plasm_runtime::ResultCoverage,
    has_snapshot: bool,
    snapshot_uri: Option<&str>,
    continue_handle: Option<&str>,
    artifact_access: ArtifactAccessMode,
) -> String {
    format_coverage_preview_note(
        shown,
        snapshot_rows,
        coverage,
        has_snapshot,
        snapshot_uri,
        continue_handle,
        artifact_access,
    )
}

/// Pure formatter: shown/snapshot relationship + mandatory expression coverage sentence.
///
/// Live [`ExecutionResult`](plasm_runtime::ExecutionResult) paths always pass a concrete
/// [`ResultCoverage`](plasm_runtime::ResultCoverage) — never `Option` / `None` shim.
#[must_use]
pub(crate) fn format_coverage_preview_note(
    shown: usize,
    snapshot_rows: usize,
    coverage: plasm_runtime::ResultCoverage,
    has_snapshot: bool,
    snapshot_uri: Option<&str>,
    continue_handle: Option<&str>,
    artifact_access: ArtifactAccessMode,
) -> String {
    let mut line = if shown < snapshot_rows {
        format!(
            "{shown}/{snapshot_rows} rows shown · {} coverage.",
            coverage.as_str()
        )
    } else {
        format!("{snapshot_rows} rows · {} coverage.", coverage.as_str())
    };
    if shown < snapshot_rows {
        if artifact_access == ArtifactAccessMode::DagCompute {
            line.push_str(" Preview only; use typed `@compute` over all rows before deciding.");
        } else {
            line.push_str(" Preview only; inspect the full result before deciding.");
        }
    }
    if artifact_access != ArtifactAccessMode::DagCompute && has_snapshot {
        if let Some(uri) = snapshot_uri {
            line.push_str(&format!(
                " Details: {} `{uri}`.",
                artifact_access.artifact_read_instruction()
            ));
        }
    } else if !has_snapshot && shown < snapshot_rows {
        line.push_str(" (no run snapshot stored)");
    }
    if let Some(handle) = continue_handle {
        // A handle may continue backend acquisition or page a stored snapshot.
        line.push_str(&format!(" Continue: `{handle}`."));
    }
    format!("\n\n_{line}_\n")
}

/// One-line Markdown after an in-band result when the run snapshot must be fetched separately.
pub(crate) fn mcp_inline_run_snapshot_line(
    handle: &RunArtifactHandle,
    artifact_access: ArtifactAccessMode,
) -> String {
    let uri = if handle.canonical_plasm_uri.is_empty() {
        handle.plasm_uri.as_str()
    } else {
        handle.canonical_plasm_uri.as_str()
    };
    artifact_access.snapshot_line(uri)
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

/// Return label for slim markdown headers (`name` from plan return, else binding node id).
pub(crate) fn return_label_for_step(name: Option<&str>, node_id: Option<&str>) -> String {
    name.map(str::to_string)
        .or_else(|| node_id.map(str::to_string))
        .unwrap_or_else(|| "result".to_string())
}

pub(crate) fn slim_result_count_label(result: &ExecutionResult) -> String {
    format!("{} rows", result.count())
}

pub(crate) fn slim_result_section_header_label(
    level: &str,
    label: &str,
    count_label: &str,
) -> String {
    format!("{level}{label} ({count_label})\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_artifacts::{artifact_http_path, plasm_run_resource_uri, RunArtifactId};

    #[test]
    fn coverage_preview_describes_snapshot_and_expression() {
        let note = ArtifactAccessMode::ResourcesRead.coverage_preview_note(
            10,
            25,
            plasm_runtime::ResultCoverage::Partial,
            Some("plasm://r/1"),
            Some("l_page1"),
        );
        assert!(
            note.contains("10/25 rows shown · partial coverage."),
            "{note}"
        );
        assert!(note.contains("Details:"), "{note}");
        assert!(note.contains("plasm://r/1"), "{note}");
        assert!(note.contains("Continue: `l_page1`"), "{note}");

        let complete = ArtifactAccessMode::ResourcesRead.coverage_preview_note(
            10,
            80,
            plasm_runtime::ResultCoverage::Complete,
            Some("plasm://r/2"),
            None,
        );
        assert!(
            complete.contains("10/80 rows shown · complete coverage."),
            "{complete}"
        );
        assert!(!complete.contains("Continue:"), "{complete}");
    }

    #[test]
    fn coverage_preview_always_includes_coverage_sentence() {
        for cov in [
            plasm_runtime::ResultCoverage::Complete,
            plasm_runtime::ResultCoverage::Partial,
            plasm_runtime::ResultCoverage::Unknown,
        ] {
            let full = format_coverage_preview_note(
                3,
                3,
                cov,
                false,
                None,
                None,
                ArtifactAccessMode::ResourcesRead,
            );
            assert!(
                full.contains(&format!("{} coverage.", cov.as_str())),
                "Full/empty path missing coverage: {full}"
            );
            let capped = format_coverage_preview_note(
                2,
                10,
                cov,
                false,
                None,
                None,
                ArtifactAccessMode::ResourcesRead,
            );
            assert!(
                capped.contains(&format!("{} coverage.", cov.as_str())),
                "Capped path missing coverage: {capped}"
            );
            let snap = format_coverage_preview_note(
                0,
                40,
                cov,
                true,
                Some("plasm://r/x"),
                None,
                ArtifactAccessMode::ResourcesRead,
            );
            assert!(
                snap.contains(&format!("{} coverage.", cov.as_str())),
                "SnapshotOnly path missing coverage: {snap}"
            );
        }
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
    fn full_fidelity_restores_default_long_string_under_budget() {
        use crate::output::{REFERENCE_ONLY_PLACEHOLDER, SUMMARY_DEFAULT_STRING_OMIT_CHARS};
        use indexmap::IndexMap;
        use plasm_compile::DecodedRelation;
        use plasm_core::{EntityKey, Ref, Value};
        use plasm_runtime::{
            CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource, ExecutionStats,
        };

        let long = "x".repeat(SUMMARY_DEFAULT_STRING_OMIT_CHARS + 1);
        let r = Ref {
            entity_type: "Note".into(),
            key: EntityKey::Simple("1".into()),
        };
        let mut fields = IndexMap::new();
        fields.insert("body".into(), Value::String(long.clone()));
        let entity = CachedEntity::from_decoded(
            r,
            fields,
            IndexMap::<String, DecodedRelation>::new(),
            0,
            EntityCompleteness::Complete,
        );
        let result = ExecutionResult {
            collection: crate::test_support::execution_fixtures::collection(
                vec![entity],
                plasm_runtime::ResultCoverage::Unknown,
            ),
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: vec![],
            operations: plasm_runtime::OperationLedger::empty(),
        };
        let formatted = crate::output::render_observation(&result, None, 25, 12 * 1024);
        let body = &formatted.tsv;
        assert!(
            body.contains(&long),
            "default long string must restore in full under budget, got: {body}"
        );
        assert!(
            !body.contains(REFERENCE_ONLY_PLACEHOLDER),
            "must not claim full fidelity while leaving placeholder: {body}"
        );
        assert!(!formatted.fidelity.any_loss());
    }
}

#[cfg(test)]
mod dag_compute_delivery_tests {
    use super::*;
    #[test]
    fn dag_delivery_keeps_snapshots_out_of_agent_tools() {
        let mode = ArtifactAccessMode::DagCompute;
        assert!(!mode.exposes_read_tool());
        assert!(mode.snapshot_line("plasm://test").is_empty());
        for text in [mode.coverage_preview_note(
            0,
            600,
            plasm_runtime::ResultCoverage::Complete,
            Some("plasm://test"),
            None,
        )] {
            assert!(text.contains("DAG") || text.contains("@compute"));
            for absent in [
                "plasm_read_run_artifact",
                "plasm_artefact_transform",
                "resources/read",
                "plasm://test",
                "TypeScript",
            ] {
                assert!(!text.contains(absent), "{text}");
            }
        }
    }
}
