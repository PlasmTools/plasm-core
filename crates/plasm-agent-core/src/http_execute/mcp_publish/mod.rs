//! MCP run markdown / `_meta` step publishing.

mod meta;
mod policy;
mod render;

use std::sync::{Arc, Mutex};

pub(crate) use meta::{build_mcp_run_tool_meta, tool_meta_from_handles};

use meta::build_ui_steps;
use policy::PublishPlan;
use render::{build_inline_bodies, format_resolved_steps, render_markdown, truncated_flags};
use serde_json::json;

use super::{ExecuteRunToolOutput, PublishedResultStep, *};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_run_markdown::McpResultTransportPolicy;

pub fn publish_plasm_result_steps(
    cgs: Option<&CGS>,
    meta_index: Option<&mut PlasmMetaIndex>,
    steps: &[PublishedResultStep],
) -> ExecuteRunToolOutput {
    publish_plasm_result_steps_with_policy(
        cgs,
        meta_index,
        steps,
        &McpResultTransportPolicy::default(),
    )
}

pub fn publish_plasm_result_steps_with_policy(
    cgs: Option<&CGS>,
    meta_index: Option<&mut PlasmMetaIndex>,
    steps: &[PublishedResultStep],
    policy: &McpResultTransportPolicy,
) -> ExecuteRunToolOutput {
    let mut plan = PublishPlan::build(steps, policy);
    format_resolved_steps(steps, &mut plan, cgs);
    let inline = build_inline_bodies(steps, &plan, steps.len());
    let preview_needed = plan.preview_needed(inline.char_count, policy);
    let truncated = truncated_flags(&plan, preview_needed);
    let use_mcp_meta = meta_index.is_some();
    let markdown = render_markdown(
        &plan,
        preview_needed,
        &inline,
        &inline.omitted_union,
        use_mcp_meta,
    );
    let all_ui_steps = build_ui_steps(steps, &plan, &truncated, cgs, policy);
    let paging_for_meta = (!inline.paging.is_empty()).then_some(inline.paging.as_slice());
    let mut tool_meta = build_mcp_run_tool_meta(
        meta_index,
        &all_ui_steps,
        &inline.omitted_union,
        paging_for_meta,
    );
    if let Some(meta) = tool_meta.as_mut() {
        if let Some(plasm) = meta.get_mut("plasm").and_then(|v| v.as_object_mut()) {
            plasm.insert(
                "result_delivery".into(),
                json!(plan.result_delivery(preview_needed)),
            );
        }
    }
    ExecuteRunToolOutput {
        markdown,
        tool_meta,
    }
}

/// Lock a shared MCP meta index for one publish call (live plan worker pool path).
pub(crate) fn publish_with_shared_meta_index(
    cgs: Option<&CGS>,
    meta_index: Option<Arc<Mutex<PlasmMetaIndex>>>,
    steps: &[PublishedResultStep],
    policy: &McpResultTransportPolicy,
) -> Result<ExecuteRunToolOutput, String> {
    match meta_index {
        Some(arc) => {
            let mut guard = arc
                .lock()
                .map_err(|e| format!("meta_index lock poisoned: {e}"))?;
            Ok(publish_plasm_result_steps_with_policy(
                cgs,
                Some(&mut *guard),
                steps,
                policy,
            ))
        }
        None => Ok(publish_plasm_result_steps_with_policy(
            cgs, None, steps, policy,
        )),
    }
}

#[cfg(test)]
mod tests {
    use plasm_runtime::{ExecutionResult, ExecutionSource, ExecutionStats};

    use super::*;
    use crate::http_execute::PublishedResultStep;
    use crate::run_artifacts::{RunArtifactHandle, RunArtifactId};
    use crate::test_support::execution_fixtures::{
        synthetic_published_result_step, synthetic_published_result_step_with_paging,
    };
    use plasm_core::PagingHandle;

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
            result: Arc::new(ExecutionResult {
                count: 0,
                entities: Vec::new(),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
            }),
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

    #[test]
    fn publish_small_with_snapshot_inlines_tsv() {
        let run_id = RunArtifactId::from_wire(&format!("pr{}", "b".repeat(64))).expect("wire");
        let handle = RunArtifactHandle {
            run_id,
            resource_index: 1,
            plasm_uri: crate::run_artifacts::plasm_short_resource_uri(1),
            canonical_plasm_uri: crate::run_artifacts::plasm_run_resource_uri("ph", "sid", &run_id),
            http_path: crate::run_artifacts::artifact_http_path("ph", "sid", &run_id),
            payload_len: 0,
            request_fingerprints: vec!["fp".into()],
        };
        let step = synthetic_published_result_step(3, Some(handle));
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            out.markdown.contains("```tsv"),
            "small results must inline TSV even when snapshot stored: {}",
            out.markdown
        );
        assert!(
            !out.markdown.contains("(preview)"),
            "must not metadata-preview small inline results: {}",
            out.markdown
        );
        let delivery = out
            .tool_meta
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .and_then(|p| p.get("result_delivery"))
            .and_then(|v| v.as_str());
        assert_eq!(delivery, Some("inline"), "small inline runs: {out:?}");
    }

    #[test]
    fn publish_over_cap_with_snapshot_uses_artifact_only_without_paging() {
        let run_id = RunArtifactId::from_wire(&format!("pr{}", "a".repeat(64))).expect("wire");
        let handle = RunArtifactHandle {
            run_id,
            resource_index: 1,
            plasm_uri: crate::run_artifacts::plasm_short_resource_uri(1),
            canonical_plasm_uri: crate::run_artifacts::plasm_run_resource_uri("ph", "sid", &run_id),
            http_path: crate::run_artifacts::artifact_http_path("ph", "sid", &run_id),
            payload_len: 0,
            request_fingerprints: vec!["fp".into()],
        };
        let paging = PagingHandle::parse("l_AAAAAAAAQACAAAAAAAAAAQ_pg1").expect("paging handle");
        let step =
            synthetic_published_result_step_with_paging(49, Some(handle.clone()), Some(paging));
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            !out.markdown.contains("```tsv"),
            "must not duplicate inline TSV when snapshot stored: {}",
            out.markdown
        );
        assert!(
            !out.markdown.contains("Showing 25 of 49 rows"),
            "must not emit row-limit note when artifact-only: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("resources/read"),
            "expected snapshot URI hint: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("Required:"),
            "expected imperative artifact read instruction: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains(&handle.plasm_uri),
            "expected inline snapshot URI: {}",
            out.markdown
        );
        assert!(
            !out.markdown
                .contains("run_ref: \"l_AAAAAAAAQACAAAAAAAAAAQ_pg1\""),
            "must not surface paging when snapshot holds the batch: {}",
            out.markdown
        );
        assert!(
            !out.markdown.contains("(preview)"),
            "must not use metadata-only preview for moderate over-cap: {}",
            out.markdown
        );
        let paging_meta = out
            .tool_meta
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .and_then(|p| p.get("paging"));
        assert!(
            paging_meta.is_none(),
            "paging meta should be omitted when artifact is complete: {:?}",
            out.tool_meta
        );
        let artifact_complete = out
            .tool_meta
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .and_then(|p| p.get("steps"))
            .and_then(|s| s.as_array())
            .and_then(|a| a.first())
            .and_then(|s| s.get("artifact_complete"))
            .and_then(|v| v.as_bool());
        assert_eq!(artifact_complete, Some(true));
    }

    #[test]
    fn publish_extreme_row_count_with_snapshot_uses_metadata_preview() {
        let run_id = RunArtifactId::from_wire(&format!("pr{}", "a".repeat(64))).expect("wire");
        let handle = RunArtifactHandle {
            run_id,
            resource_index: 1,
            plasm_uri: crate::run_artifacts::plasm_short_resource_uri(1),
            canonical_plasm_uri: crate::run_artifacts::plasm_run_resource_uri("ph", "sid", &run_id),
            http_path: crate::run_artifacts::artifact_http_path("ph", "sid", &run_id),
            payload_len: 0,
            request_fingerprints: vec!["fp".into()],
        };
        let step = synthetic_published_result_step(937, Some(handle.clone()));
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            out.markdown.contains("(preview)"),
            "expected compact preview markdown: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("resources/read"),
            "expected snapshot URI hint: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains(&handle.plasm_uri),
            "expected inline snapshot URI: {}",
            out.markdown
        );
        assert!(
            !out.markdown.contains("move-936"),
            "must not inline full 937-row TSV: {}",
            out.markdown
        );
        assert!(
            !out.markdown.contains("```tsv"),
            "must not fence a giant TSV: {}",
            out.markdown
        );
    }

    #[test]
    fn publish_row_cap_without_snapshot_truncates_inline_tsv() {
        let policy = McpResultTransportPolicy::default();
        let step = synthetic_published_result_step(40, None);
        let out = publish_plasm_result_steps_with_policy(
            None,
            None,
            std::slice::from_ref(&step),
            &policy,
        );
        assert!(
            out.markdown.contains("```tsv"),
            "expected capped inline TSV: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("move-24") && !out.markdown.contains("move-30"),
            "expected at most {} rows inline: {}",
            policy.in_band_entity_rows,
            out.markdown
        );
        assert!(
            out.markdown.contains("Showing 25 of 40 rows"),
            "expected row-limit note: {}",
            out.markdown
        );
    }
}
