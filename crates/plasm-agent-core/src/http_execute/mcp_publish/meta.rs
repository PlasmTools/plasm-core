//! `_meta.plasm` assembly for MCP `plasm` / `plasm_run` tool results.

use crate::mcp_plasm_meta::{
    plasm_paging_json_value, PlasmMetaIndex, PlasmPagingStepMeta, RunUiStepFields,
    StepPlasmMetaFields,
};
use crate::mcp_run_markdown::{merge_snapshot_column_hints, OmittedReferenceOnlyFields};
use crate::run_artifacts::RunArtifactHandle;
use plasm_runtime::entity_to_agent_row_json;

pub(crate) fn plasm_meta_object(
    handles: &[RunArtifactHandle],
    omitted_from_summary: &[String],
    lossy_per_step: Option<&[crate::output::LossySummaryFieldNames]>,
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

pub(crate) fn preview_entities_for_step(
    step: &super::PublishedResultStep,
    cgs: Option<&plasm_core::CGS>,
    max_rows: usize,
) -> Vec<serde_json::Value> {
    let cgs = step.cgs.as_deref().or(cgs);
    step.result
        .entities
        .iter()
        .take(max_rows)
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

pub(crate) fn build_ui_steps(
    steps: &[super::PublishedResultStep],
    plan: &super::policy::PublishPlan,
    truncated_flags: &[bool],
    cgs: Option<&plasm_core::CGS>,
    policy: &crate::mcp_run_markdown::McpResultTransportPolicy,
) -> Vec<RunUiStepFields> {
    steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let resolved = &plan.resolved[i];
            let truncated = truncated_flags[i];
            let fmt = resolved.format.as_ref();
            let step_cgs = step.cgs.as_deref().or(cgs);
            let column_schema = crate::run_ui_column_schema::build_run_step_column_schema(
                &step.result,
                step_cgs,
                step.entry_id.as_deref(),
                step.entity.as_deref(),
            )
            .map(|s| crate::run_ui_column_schema::column_schema_json(&s));
            let lossy = fmt
                .map(|f| merge_snapshot_column_hints(&f.lossy, &f.in_band))
                .unwrap_or_default();
            RunUiStepFields {
                run_step: i + 1,
                return_label: resolved.label.clone(),
                display: step.display.clone(),
                row_count: resolved.row_count,
                node_id: step.node_id.clone(),
                preview_entities: if resolved.include_preview_entities(truncated, policy) {
                    Some(preview_entities_for_step(
                        step,
                        cgs,
                        policy.in_band_entity_rows,
                    ))
                } else {
                    None
                },
                artifact: resolved.artifact.clone(),
                lossy_summary_fields: lossy,
                column_schema,
            }
        })
        .collect()
}
