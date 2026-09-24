//! MCP-oriented tabular summaries (`tsv` fences) over [`ExecutionResult`].
//!
//! Policy (encoded cell width, reversible string encoding) is explicit via [`TsvCellPolicy`] so this stays distinct from
//! generic table formatting in the parent module.

use super::in_band_fidelity::{InBandSummaryReport, SummaryFidelityLoss};
use plasm_core::CGS;
use plasm_runtime::ExecutionResult;
use std::collections::BTreeSet;

/// Unicode-scalar cap on encoded cells for MCP ` ```tsv ` cells.
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

/// Encode a string as one JSON string literal, never as a lossy whitespace summary.
/// Over-budget cells are explicitly withheld; they are not truncated valid-looking values.
fn sanitize_tsv_cell_with_flag(s: &str, policy: &TsvCellPolicy) -> (String, bool) {
    bounded_encoded_cell(
        serde_json::to_string(s).expect("string serialization is infallible"),
        policy,
    )
}

fn bounded_encoded_cell(encoded: String, policy: &TsvCellPolicy) -> (String, bool) {
    if encoded.chars().count() <= policy.max_scalars {
        (encoded, false)
    } else {
        (super::REFERENCE_ONLY_PLACEHOLDER.into(), true)
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
    let text = format_tsv_inner(
        result,
        cgs,
        max_entity_rows,
        &mut omitted,
        &policy,
        &mut report,
        false,
    );
    (text, omitted.into_iter().collect(), report)
}

/// MCP-only TSV rendering that preserves schema `ReferenceOnly` and `Lossy` strings when the
/// caller has already admitted them under a strict aggregate byte budget.
pub(crate) fn format_result_tsv_with_full_fidelity_cgs(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
) -> (String, InBandSummaryReport) {
    let policy = TsvCellPolicy::mcp_default();
    let mut omitted = BTreeSet::new();
    let mut report = InBandSummaryReport::default();
    let text = format_tsv_inner(
        result,
        cgs,
        max_entity_rows,
        &mut omitted,
        &policy,
        &mut report,
        true,
    );
    (text, report)
}

fn format_tsv_inner(
    result: &ExecutionResult,
    cgs: Option<&CGS>,
    max_entity_rows: Option<usize>,
    omitted: &mut BTreeSet<String>,
    policy: &TsvCellPolicy,
    report: &mut InBandSummaryReport,
    full_fidelity: bool,
) -> String {
    if result.entities.is_empty() {
        return super::format_empty_result_body(result);
    }

    let columns = super::union_entity_table_columns(result, cgs, max_entity_rows);

    let mut lines: Vec<String> = Vec::new();
    let header_cells: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
    lines.push(header_cells.join("\t"));

    let row_limit = max_entity_rows.unwrap_or(usize::MAX);
    for entity in result.entities.iter().take(row_limit) {
        let row: Vec<String> = columns
            .iter()
            .map(|col| {
                let mut cell_report = InBandSummaryReport::default();
                let raw = if full_fidelity {
                    full_fidelity_tsv_cell(col, entity, cgs, omitted, &mut cell_report)
                } else {
                    super::format_summary_column_cell(
                        col.as_str(),
                        entity,
                        cgs,
                        omitted,
                        Some(&mut cell_report),
                    )
                };
                let cell_value = entity.fields.get(col).map(|v| v.to_value()).or_else(|| {
                    entity.relations.get(col).map(|refs| {
                        plasm_core::Value::Array(
                            refs.iter()
                                .map(|reference| plasm_core::Value::String(reference.to_string()))
                                .collect(),
                        )
                    })
                });
                let blob_base = col
                    .strip_suffix("_ref")
                    .or_else(|| col.strip_suffix("_mime"))
                    .filter(|base| {
                        super::field_type_is_blob(cgs, &entity.reference.entity_type, base)
                    });
                // A withheld/unavailable summary is a marker, not the field value.
                let unavailable = entity.unavailable_fields.contains(col)
                    || blob_base.is_some_and(|base| entity.unavailable_fields.contains(base));
                let encode = !unavailable && !cell_report.any_loss();
                let (cell, transport_truncated) = match cell_value {
                    Some(plasm_core::Value::String(s)) if encode => {
                        sanitize_tsv_cell_with_flag(&s, policy)
                    }
                    Some(value) if encode => bounded_encoded_cell(
                        serde_json::to_string(&value).expect("materialized value serialization"),
                        policy,
                    ),
                    _ if col == "_ref" => sanitize_tsv_cell_with_flag(&raw, policy),
                    _ if cell_report.any_loss() => {
                        omitted.insert(col.clone());
                        (super::REFERENCE_ONLY_PLACEHOLDER.into(), false)
                    }
                    _ if unavailable => (raw, false),
                    // Projected blob reference/MIME cells are textual values, not row fields.
                    _ if blob_base.is_some() => sanitize_tsv_cell_with_flag(&raw, policy),
                    // A column supplied only by another row has no value in this row.
                    _ => bounded_encoded_cell("null".into(), policy),
                };
                if transport_truncated {
                    cell_report.record(col.as_str(), SummaryFidelityLoss::TsvScalarTransportClamp);
                    omitted.insert(col.clone());
                }
                report.merge_from(&cell_report);
                cell
            })
            .collect();
        lines.push(row.join("\t"));
    }

    lines.join("\n")
}

fn full_fidelity_tsv_cell(
    col: &str,
    entity: &plasm_runtime::CachedEntity,
    cgs: Option<&CGS>,
    omitted: &mut BTreeSet<String>,
    report: &mut InBandSummaryReport,
) -> String {
    if !entity.unavailable_fields.contains(col) {
        if let Some(plasm_core::Value::String(s)) =
            entity.fields.get(col).map(|value| value.to_value())
        {
            return s;
        }
    }
    super::format_summary_column_cell(col, entity, cgs, omitted, Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn tsv_string_transport_round_trips(s in ".{0,256}") {
            let (cell, omitted) = sanitize_tsv_cell_with_flag(&s, &TsvCellPolicy::mcp_default());
            prop_assert!(!omitted);
            prop_assert!(!cell.contains('\n') && !cell.contains('\t'));
            prop_assert_eq!(serde_json::from_str::<String>(&cell).unwrap(), s);
        }
    }

    #[test]
    fn rendered_tsv_preserves_typed_cells_after_snapshot_serialization() {
        use indexmap::IndexMap;
        use plasm_core::{Ref, Value};
        use plasm_runtime::{
            CachedEntity, EntityCompleteness, ExecutionSource, ExecutionStats, OperationLedger,
            ResultCoverage,
        };
        let fields = IndexMap::from([
            (
                "text".into(),
                Value::String("# note\n\n  - nested\titem\n\\n".into()),
            ),
            ("empty".into(), Value::String(String::new())),
            ("null_text".into(), Value::String("null".into())),
            ("absent".into(), Value::Null),
            (
                "items".into(),
                Value::Array(vec![Value::String("a\tb\n".into()), Value::Integer(2)]),
            ),
        ]);
        let mut entity = CachedEntity::from_decoded(
            Ref::new("Document", "one"),
            fields.clone(),
            IndexMap::new(),
            0,
            EntityCompleteness::Complete,
        );
        entity.relations = IndexMap::from([
            (
                "related".into(),
                vec![
                    Ref::new("Document", "a\tb\nc"),
                    Ref::new("Document", "other"),
                ],
            ),
            ("empty_relation".into(), vec![]),
        ]);
        let entity: CachedEntity =
            serde_json::from_slice(&serde_json::to_vec(&entity).unwrap()).unwrap();
        let result = ExecutionResult {
            entities: vec![entity],
            count: 1,
            has_more: false,
            coverage: ResultCoverage::Complete,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
            operations: OperationLedger::empty(),
        };
        let snapshot = super::super::entity_to_json(&result.entities[0]);
        let (body, report) = format_result_tsv_with_full_fidelity_cgs(&result, None, None);
        assert!(!report.any_loss(), "{body}");
        let (summary, omitted, report) = format_result_tsv_with_cgs(&result, None, None);
        assert_eq!(summary, body);
        assert!(omitted.is_empty() && !report.any_loss());
        let lines = body.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        for (key, cell) in lines[0].split('\t').zip(lines[1].split('\t')) {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(cell).unwrap(),
                snapshot[key],
                "snapshot {key}"
            );
            if let Some(expected) = fields.get(key) {
                assert_eq!(
                    &serde_json::from_str::<Value>(cell).unwrap(),
                    expected,
                    "{key}"
                );
            }
        }
        let mut sparse = result.clone();
        sparse
            .entities
            .push(CachedEntity::new(Ref::new("Document", "empty"), 0));
        let (body, _, _) = format_result_tsv_with_cgs(&sparse, None, None);
        assert!(body
            .lines()
            .nth(2)
            .unwrap()
            .split('\t')
            .all(|cell| cell == "null"));
        let (limited, _, _) = format_result_tsv_with_cgs(&sparse, None, Some(1));
        assert_eq!(limited.lines().count(), 2);
    }

    #[test]
    fn tsv_document_transport_preserves_layout() {
        let s = "# Note\n\n- a\n  - b\t c\r\n\\n  ";
        let (cell, omitted) = sanitize_tsv_cell_with_flag(s, &TsvCellPolicy::mcp_default());
        assert!(!omitted);
        assert_eq!(serde_json::from_str::<String>(&cell).unwrap(), s);
    }
}
