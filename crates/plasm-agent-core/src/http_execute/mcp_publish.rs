//! MCP run markdown / `_meta` step publishing.

use super::{ExecuteRunToolOutput, PublishedResultStep, *};
use crate::mcp_plasm_meta::{
    plasm_paging_json_value, PlasmMetaIndex, PlasmPagingStepMeta, RunUiStepFields,
    StepPlasmMetaFields, MCP_UI_PREVIEW_ENTITY_ROW_CAP,
};
use crate::mcp_run_markdown::{
    mcp_compact_markdown_multi_line, mcp_compact_markdown_single,
    mcp_format_execute_result_table_or_tsv, mcp_inline_run_snapshot_line,
    mcp_prepend_artifact_followup_markdown, mcp_preview_markdown_needed,
    merge_snapshot_column_hints, return_label_for_step, slim_result_section_header,
    OmittedReferenceOnlyFields,
};
use crate::output::{InBandSummaryReport, LossySummaryFieldNames};

fn plasm_meta_object(
    handles: &[RunArtifactHandle],
    omitted_from_summary: &[String],
    lossy_per_step: Option<&[LossySummaryFieldNames]>,
    run_step_numbers: Option<&[usize]>,
    paging: Option<&[PlasmPagingStepMeta]>,
    step_meta: Option<&[StepPlasmMetaFields]>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    if !handles.is_empty() {
        let steps: Vec<serde_json::Value> = handles
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let mut step = serde_json::json!({
                    "run_id": h.run_id.to_wire(),
                    "artifact_uri": h.plasm_uri,
                    "canonical_artifact_uri": h.canonical_plasm_uri,
                    "artifact_path": h.http_path,
                    "request_fingerprints": h.request_fingerprints,
                });
                if let Some(rs) = run_step_numbers {
                    if let Some(&run_step) = rs.get(i) {
                        if let Some(obj) = step.as_object_mut() {
                            obj.insert("run_step".into(), serde_json::json!(run_step));
                        }
                    }
                }
                if let Some(ls) = lossy_per_step {
                    if let Some(lossy) = ls.get(i) {
                        if !lossy.is_empty() {
                            if let Some(obj) = step.as_object_mut() {
                                obj.insert(
                                    "lossy_summary_fields".into(),
                                    serde_json::json!(lossy.as_slice()),
                                );
                            }
                        }
                    }
                }
                if let Some(meta) = step_meta {
                    if let Some(m) = meta.get(i) {
                        if let Some(obj) = step.as_object_mut() {
                            obj.insert("return_label".into(), serde_json::json!(m.return_label));
                            obj.insert("display".into(), serde_json::json!(m.display));
                            obj.insert("row_count".into(), serde_json::json!(m.row_count));
                        }
                    }
                }
                step
            })
            .collect();
        m.insert("steps".into(), serde_json::Value::Array(steps));
    }
    if !omitted_from_summary.is_empty() {
        m.insert(
            "omitted_from_summary".into(),
            serde_json::json!(omitted_from_summary),
        );
    }
    if let Some(ps) = paging {
        if let Some(v) = plasm_paging_json_value(ps) {
            m.insert("paging".into(), v);
        }
    }
    m
}

fn preview_entities_for_step(
    step: &PublishedResultStep,
    cgs: Option<&CGS>,
) -> Vec<serde_json::Value> {
    let cgs = step.cgs.as_deref().or(cgs);
    step.result
        .entities
        .iter()
        .take(MCP_UI_PREVIEW_ENTITY_ROW_CAP)
        .map(|e| {
            let mut v = entity_to_agent_row_json(e, cgs);
            strip_cache_keys_from_agent_preview_row(&mut v);
            v
        })
        .collect()
}

fn strip_cache_keys_from_agent_preview_row(v: &mut serde_json::Value) {
    if let Some(obj) = v.as_object_mut() {
        for key in ["_ref", "_version", "_last_updated", "_completeness"] {
            obj.remove(key);
        }
    }
}

fn step_result_truncated_for_ui(
    preview_needed: bool,
    i: usize,
    per_step_artifact: &[Option<RunArtifactHandle>],
    per_step_omitted: &[OmittedReferenceOnlyFields],
    per_step_lossy: &[LossySummaryFieldNames],
    per_step_in_band: &[InBandSummaryReport],
) -> bool {
    per_step_artifact[i].is_some()
        && (preview_needed
            || !per_step_omitted[i].is_empty()
            || !per_step_lossy[i].is_empty()
            || per_step_in_band[i].any_loss())
}

fn plasm_run_ui_meta_object(
    all_steps: &[RunUiStepFields],
    omitted_from_summary: &[String],
    paging: Option<&[PlasmPagingStepMeta]>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    if !all_steps.is_empty() {
        let steps: Vec<serde_json::Value> = all_steps
            .iter()
            .map(|spec| {
                let mut step = serde_json::json!({
                    "run_step": spec.run_step,
                    "return_label": spec.return_label,
                    "display": spec.display,
                    "row_count": spec.row_count,
                });
                if let Some(ref node_id) = spec.node_id {
                    if let Some(obj) = step.as_object_mut() {
                        obj.insert("node_id".into(), serde_json::json!(node_id));
                    }
                }
                if let Some(ref preview) = spec.preview_entities {
                    if !preview.is_empty() {
                        if let Some(obj) = step.as_object_mut() {
                            obj.insert("preview_entities".into(), serde_json::json!(preview));
                        }
                    }
                }
                if !spec.lossy_summary_fields.is_empty() {
                    if let Some(obj) = step.as_object_mut() {
                        obj.insert(
                            "lossy_summary_fields".into(),
                            serde_json::json!(spec.lossy_summary_fields.as_slice()),
                        );
                    }
                }
                if let Some(ref schema) = spec.column_schema {
                    if let Some(obj) = step.as_object_mut() {
                        obj.insert("column_schema".into(), schema.clone());
                    }
                }
                if let Some(ref h) = spec.artifact {
                    if let Some(obj) = step.as_object_mut() {
                        obj.insert("run_id".into(), serde_json::json!(h.run_id.to_wire()));
                        obj.insert("artifact_uri".into(), serde_json::json!(h.plasm_uri));
                        obj.insert(
                            "canonical_artifact_uri".into(),
                            serde_json::json!(h.canonical_plasm_uri),
                        );
                        obj.insert("artifact_path".into(), serde_json::json!(h.http_path));
                        obj.insert(
                            "request_fingerprints".into(),
                            serde_json::json!(h.request_fingerprints),
                        );
                    }
                }
                step
            })
            .collect();
        m.insert("steps".into(), serde_json::Value::Array(steps));
    }
    if !omitted_from_summary.is_empty() {
        m.insert(
            "omitted_from_summary".into(),
            serde_json::json!(omitted_from_summary),
        );
    }
    if let Some(ps) = paging {
        if let Some(v) = plasm_paging_json_value(ps) {
            m.insert("paging".into(), v);
        }
    }
    m
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_mcp_run_tool_meta(
    meta_index: Option<&mut PlasmMetaIndex>,
    all_steps: &[RunUiStepFields],
    omitted_from_summary: &OmittedReferenceOnlyFields,
    paging: Option<&[PlasmPagingStepMeta]>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if all_steps.is_empty() && omitted_from_summary.is_empty() && paging.is_none() {
        return None;
    }
    match meta_index {
        Some(idx) => {
            let plasm =
                idx.build_plasm_run_ui_meta(all_steps, omitted_from_summary.as_ref(), paging);
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".into(), serde_json::Value::Object(plasm));
            Some(meta)
        }
        None => {
            let plasm = plasm_run_ui_meta_object(all_steps, omitted_from_summary.as_ref(), paging);
            if plasm.is_empty() {
                return None;
            }
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".into(), serde_json::Value::Object(plasm));
            Some(meta)
        }
    }
}

pub(crate) fn tool_meta_from_handles(
    handles: &[RunArtifactHandle],
    omitted_from_summary: &[String],
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let plasm = plasm_meta_object(handles, omitted_from_summary, None, None, None, None);
    if plasm.is_empty() {
        return None;
    }
    let mut meta = serde_json::Map::new();
    meta.insert("plasm".into(), serde_json::Value::Object(plasm));
    Some(meta)
}

pub fn publish_plasm_result_steps(
    cgs: Option<&CGS>,
    meta_index: Option<&mut PlasmMetaIndex>,
    steps: &[PublishedResultStep],
) -> ExecuteRunToolOutput {
    let total = steps.len();
    let mut per_step_body: Vec<String> = Vec::with_capacity(total);
    let mut per_step_omitted: Vec<OmittedReferenceOnlyFields> = Vec::with_capacity(total);
    let mut per_step_lossy: Vec<LossySummaryFieldNames> = Vec::with_capacity(total);
    let mut per_step_in_band: Vec<InBandSummaryReport> = Vec::with_capacity(total);
    let mut per_step_artifact: Vec<Option<RunArtifactHandle>> = Vec::with_capacity(total);
    let mut per_step_compact: Vec<(String, usize)> = Vec::with_capacity(total);
    let mut paging: Vec<PlasmPagingStepMeta> = Vec::new();
    let mut omitted_union: BTreeSet<String> = BTreeSet::new();
    let mut total_entity_rows: usize = 0;

    for (i, step) in steps.iter().enumerate() {
        let label = return_label_for_step(step.name.as_deref(), step.node_id.as_deref());
        total_entity_rows = total_entity_rows.saturating_add(step.result.count);
        per_step_compact.push((label.clone(), step.result.count));
        let formatted =
            mcp_format_execute_result_table_or_tsv(&step.result, step.cgs.as_deref().or(cgs));
        omitted_union.extend(formatted.reference_only_omitted.as_ref().iter().cloned());
        per_step_omitted.push(formatted.reference_only_omitted);
        per_step_lossy.push(formatted.lossy_summary_fields.clone());
        per_step_in_band.push(formatted.in_band_report.clone());
        per_step_artifact.push(step.artifact.clone());

        let header = if total <= 1 {
            slim_result_section_header("## ", &label, step.result.count)
        } else if i == 0 {
            format!(
                "# Results\n\n{}",
                slim_result_section_header("### ", &label, step.result.count)
            )
        } else {
            slim_result_section_header("### ", &label, step.result.count)
        };
        let mut sec = header;
        sec.push_str(&formatted.block.into_mcp_result_markdown());
        if let Some(handle) = &step.artifact {
            let truncated = !per_step_omitted[i].is_empty()
                || !per_step_lossy[i].is_empty()
                || per_step_in_band[i].any_loss();
            if truncated {
                sec.push_str(&mcp_inline_run_snapshot_line(handle));
            }
        }
        if let Some(handle) = &step.result.paging_handle {
            paging.push(PlasmPagingStepMeta::Next {
                run_step: i + 1,
                returned_count: step.result.count,
                next_page_handle: handle.clone(),
            });
            sec.push_str(&format!(
                "\n\nmore pages available - use `page({})` for the next page.",
                handle.as_str()
            ));
        }
        per_step_body.push(sec);
    }

    let omitted_for_steps: OmittedReferenceOnlyFields = omitted_union.into();
    let mut full_sections = String::new();
    for (i, b) in per_step_body.iter().enumerate() {
        if i > 0 {
            full_sections.push_str("\n\n");
        }
        full_sections.push_str(b);
    }
    let preview_needed = mcp_preview_markdown_needed(meta_index.is_some(), &full_sections);
    let mut truncated_steps: Vec<(usize, RunArtifactHandle)> = Vec::new();
    for i in 0..total {
        let step_no = i + 1;
        let Some(h) = per_step_artifact[i].as_ref() else {
            continue;
        };
        let truncated = preview_needed
            || !per_step_omitted[i].is_empty()
            || !per_step_lossy[i].is_empty()
            || per_step_in_band[i].any_loss();
        if truncated {
            truncated_steps.push((step_no, h.clone()));
        }
    }
    let handles_meta: Vec<RunArtifactHandle> =
        truncated_steps.iter().map(|(_, h)| h.clone()).collect();
    let truncated_refs: Vec<(usize, &RunArtifactHandle)> =
        truncated_steps.iter().map(|(s, h)| (*s, h)).collect();

    let all_ui_steps: Vec<RunUiStepFields> = (0..total)
        .map(|i| {
            let step = &steps[i];
            let truncated = step_result_truncated_for_ui(
                preview_needed,
                i,
                &per_step_artifact,
                &per_step_omitted,
                &per_step_lossy,
                &per_step_in_band,
            );
            let step_cgs = step.cgs.as_deref().or(cgs);
            let column_schema = crate::run_ui_column_schema::build_run_step_column_schema(
                &step.result,
                step_cgs,
                step.entry_id.as_deref(),
                step.entity.as_deref(),
            )
            .map(|s| crate::run_ui_column_schema::column_schema_json(&s));
            RunUiStepFields {
                run_step: i + 1,
                return_label: return_label_for_step(step.name.as_deref(), step.node_id.as_deref()),
                display: step.display.clone(),
                row_count: step.result.count,
                node_id: step.node_id.clone(),
                preview_entities: if truncated {
                    if step.result.count <= MCP_UI_PREVIEW_ENTITY_ROW_CAP {
                        Some(preview_entities_for_step(step, cgs))
                    } else {
                        None
                    }
                } else {
                    Some(preview_entities_for_step(step, cgs))
                },
                artifact: per_step_artifact[i].clone(),
                lossy_summary_fields: merge_snapshot_column_hints(
                    &per_step_lossy[i],
                    &per_step_in_band[i],
                ),
                column_schema,
            }
        })
        .collect();

    let mut lossy_union_set: BTreeSet<String> = BTreeSet::new();
    if preview_needed {
        for i in 0..total {
            if per_step_artifact[i].is_some() {
                for name in per_step_lossy[i].as_slice() {
                    lossy_union_set.insert(name.clone());
                }
                for name in per_step_in_band[i].field_names() {
                    lossy_union_set.insert(name.clone());
                }
            }
        }
    }
    let lossy_preview_union =
        LossySummaryFieldNames::from_vec_sorted_dedup(lossy_union_set.into_iter().collect());

    let markdown = if preview_needed {
        if total <= 1 {
            let (label, rows) = per_step_compact
                .first()
                .cloned()
                .unwrap_or_else(|| ("result".to_string(), 0));
            let mut md =
                mcp_compact_markdown_single(&label, rows, &omitted_for_steps, &lossy_preview_union);
            if let Some((_, h)) = truncated_refs.first() {
                md.push_str(&mcp_inline_run_snapshot_line(h));
            }
            md
        } else {
            mcp_compact_markdown_multi_line(
                total,
                total_entity_rows,
                &per_step_compact,
                &omitted_for_steps,
                &lossy_preview_union,
                &truncated_refs,
            )
        }
    } else {
        full_sections
    };
    let markdown = mcp_prepend_artifact_followup_markdown(
        markdown,
        meta_index.is_some(),
        &handles_meta,
        &omitted_for_steps,
    );
    let paging_for_meta = (!paging.is_empty()).then_some(paging.as_slice());
    let tool_meta = build_mcp_run_tool_meta(
        meta_index,
        &all_ui_steps,
        &omitted_for_steps,
        paging_for_meta,
    );
    ExecuteRunToolOutput {
        markdown,
        tool_meta,
    }
}

#[cfg(test)]
mod tests {
    use plasm_runtime::{ExecutionResult, ExecutionSource, ExecutionStats};

    use super::*;
    use crate::http_execute::PublishedResultStep;
    use crate::run_artifacts::{RunArtifactHandle, RunArtifactId};

    #[test]
    fn publish_includes_artifact_meta_when_result_not_truncated() {
        let run_id = RunArtifactId::from_wire(&format!("pr{}", "c".repeat(64))).expect("wire");
        let handle = RunArtifactHandle {
            run_id,
            resource_index: 1,
            plasm_uri: crate::run_artifacts::plasm_short_resource_uri(1),
            canonical_plasm_uri: crate::run_artifacts::plasm_run_resource_uri("ph", "sid", &run_id),
            http_path: crate::run_artifacts::artifact_http_path("ph", "sid", &run_id),
            payload_len: 0,
            request_fingerprints: vec!["fp".into()],
        };
        let step = PublishedResultStep {
            name: None,
            node_id: None,
            entry_id: Some("default".into()),
            entity: Some("Pet".into()),
            cgs: None,
            display: "pets".into(),
            projection: None,
            result: ExecutionResult {
                count: 0,
                entities: Vec::new(),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
            },
            artifact: Some(handle),
        };
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        let steps = out
            .tool_meta
            .and_then(|m| m.get("plasm").cloned())
            .and_then(|p| p.get("steps").cloned())
            .and_then(|s| s.as_array().cloned())
            .expect("steps meta");
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0].get("run_id").and_then(|v| v.as_str()),
            Some(run_id.to_wire().as_str())
        );
    }
}
