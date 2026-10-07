//! MCP run markdown / `_meta` step publishing.

mod meta;
mod page;
pub use page::ResultPublicationError;
mod policy;
mod render;

use std::sync::{Arc, Mutex};

pub(crate) use meta::{build_mcp_run_tool_meta, tool_meta_from_handles};

use meta::build_ui_steps;
use policy::PublishPlan;
use render::build_inline_bodies;
use serde_json::json;

use super::{ExecuteRunToolOutput, PublishedResultStep, *};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_run_markdown::McpResultTransportPolicy;

pub fn publish_plasm_result_steps(
    store: &impl crate::execute_session::PagingContinuationStore,
    logical_session_ref: Option<&str>,
    cgs: Option<&CGS>,
    meta_index: Option<&mut PlasmMetaIndex>,
    steps: &[PublishedResultStep],
) -> Result<ExecuteRunToolOutput, ResultPublicationError> {
    publish_plasm_result_steps_with_policy(
        store,
        logical_session_ref,
        cgs,
        meta_index,
        steps,
        &McpResultTransportPolicy::default(),
    )
}

pub fn publish_plasm_result_steps_with_policy(
    store: &impl crate::execute_session::PagingContinuationStore,
    logical_session_ref: Option<&str>,
    cgs: Option<&CGS>,
    meta_index: Option<&mut PlasmMetaIndex>,
    steps: &[PublishedResultStep],
    policy: &McpResultTransportPolicy,
) -> Result<ExecuteRunToolOutput, ResultPublicationError> {
    let (steps, plan) = PublishPlan::build(store, logical_session_ref, steps, cgs, policy)?;
    let inline = build_inline_bodies(&steps, &plan, steps.len());
    let markdown = inline.sections.clone();
    let all_ui_steps = build_ui_steps(&steps, &plan, cgs);
    let paging_for_meta = (!inline.paging.is_empty()).then_some(inline.paging.as_slice());
    let mut tool_meta = build_mcp_run_tool_meta(
        meta_index,
        &all_ui_steps,
        &inline.omitted_union,
        paging_for_meta,
    );
    if let Some(meta) = tool_meta.as_mut() {
        if let Some(plasm) = meta.get_mut("plasm").and_then(|v| v.as_object_mut()) {
            plasm.insert("result_delivery".into(), json!("inline"));
        }
    }
    Ok(ExecuteRunToolOutput {
        delivered_steps: steps,
        markdown,
        tool_meta,
    })
}

/// Publish using the session-owned continuation store and shared metadata index.
pub(crate) fn publish_with_shared_meta_index(
    store: &impl crate::execute_session::PagingContinuationStore,
    logical_session_ref: Option<&str>,
    cgs: Option<&CGS>,
    meta_index: Option<Arc<Mutex<PlasmMetaIndex>>>,
    steps: &[PublishedResultStep],
    policy: &McpResultTransportPolicy,
) -> Result<ExecuteRunToolOutput, ResultPublicationError> {
    match meta_index {
        Some(arc) => {
            let mut guard = arc
                .lock()
                .map_err(|_| ResultPublicationError::MetaIndexPoisoned)?;
            publish_plasm_result_steps_with_policy(
                store,
                logical_session_ref,
                cgs,
                Some(&mut *guard),
                steps,
                policy,
            )
        }
        None => publish_plasm_result_steps_with_policy(
            store,
            logical_session_ref,
            cgs,
            None,
            steps,
            policy,
        ),
    }
}

#[cfg(test)]
mod tests {
    use plasm_runtime::{ExecutionResult, ExecutionSource, ExecutionStats};

    use super::*;
    use crate::http_execute::PublishedResultStep;
    use crate::mcp_run_markdown::McpResultTransportPolicy;
    use crate::run_artifacts::{RunArtifactHandle, RunArtifactId};
    use crate::test_support::execution_fixtures::{
        synthetic_published_result_step, synthetic_published_result_step_with_paging,
    };
    use plasm_core::PagingHandle;

    fn paging_session() -> crate::execute_session::ExecuteSession {
        let cgs = Arc::new(
            plasm_core::load_schema_dir(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .unwrap(),
        );
        crate::test_support::graph_fixtures::test_execute_session(cgs, "publish-pages")
    }

    fn publish_plasm_result_steps(
        cgs: Option<&CGS>,
        index: Option<&mut PlasmMetaIndex>,
        steps: &[PublishedResultStep],
    ) -> ExecuteRunToolOutput {
        super::publish_plasm_result_steps(&paging_session(), None, cgs, index, steps).unwrap()
    }

    fn publish_plasm_result_steps_with_policy(
        cgs: Option<&CGS>,
        index: Option<&mut PlasmMetaIndex>,
        steps: &[PublishedResultStep],
        policy: &McpResultTransportPolicy,
    ) -> ExecuteRunToolOutput {
        super::publish_plasm_result_steps_with_policy(
            &paging_session(),
            None,
            cgs,
            index,
            steps,
            policy,
        )
        .unwrap()
    }

    #[test]
    fn row_26_has_a_presentation_continuation() {
        let step = synthetic_published_result_step(27, None);
        let out = publish_plasm_result_steps(None, None, &[step]);
        assert!(out.markdown.contains("more pages"), "{}", out.markdown);
        assert!(out.tool_meta.unwrap()["plasm"]["paging"][0]["next_run_ref"].is_string());
    }

    #[test]
    fn an_orphaned_delivery_window_is_a_typed_publication_failure() {
        let session = paging_session();
        let mut source = synthetic_published_result_step(27, None);
        let mut result = source.result.as_ref().clone();
        result.collection = result.collection.delivery(0..25).unwrap();
        source.result = Arc::new(result);
        let error =
            super::publish_plasm_result_steps(&session, None, None, None, &[source]).unwrap_err();
        assert!(matches!(
            error,
            ResultPublicationError::Collection(
                plasm_core::collection_codec::CollectionFault::NotResident
            )
        ));
    }

    #[test]
    fn presentation_pages_preserve_membership_and_reach_row_26() {
        let session = paging_session();
        let source = synthetic_published_result_step(27, None);
        let identity = source.result.collection.membership().identity().clone();
        let first = super::publish_plasm_result_steps(
            &session,
            None,
            None,
            None,
            std::slice::from_ref(&source),
        )
        .unwrap();
        assert_eq!(source.result.entities().len(), 27);
        assert_eq!(first.delivered_steps[0].result.entities().len(), 25);
        let handle = first.delivered_steps[0]
            .result
            .paging_handle
            .as_ref()
            .unwrap();
        let cursor = session.peek_synthetic_paging_resume(handle).unwrap();
        let second =
            super::super::run_line::synthetic_page_result(&session, handle, cursor, None).unwrap();
        assert_eq!(second.entities().len(), 2);
        assert_eq!(
            second.entities()[0].fields["id"].to_value(),
            plasm_core::Value::String("m25".into())
        );
        assert_eq!(second.collection.membership().identity(), &identity);
        assert_eq!(second.coverage(), source.result.coverage());
        assert_eq!(second.collection.delivery_range(), 25..27);
        assert!(second.paging_handle.is_none());
        assert_eq!(second.stats.network_requests, 0);
        assert!(second.operations.is_empty());
        assert!(
            second
                .collection
                .materialize(plasm_core::collection_codec::Demand::Observed)
                .is_err(),
            "a delivery window must not masquerade as a fully resident computation input"
        );
    }

    #[tokio::test]
    async fn exhausted_delivery_tail_can_be_rebudgeted_through_public_execution() {
        use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
        use crate::server_state::CatalogBootstrap;
        use crate::test_support::graph_fixtures::{
            berry_entity, load_pokeapi_mini_cgs, test_execute_session,
        };
        use plasm_core::collection_codec::Observation;
        use plasm_core::discovery::CgsRegistry;
        use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

        let cgs = load_pokeapi_mini_cgs();
        let session = test_execute_session(cgs.clone(), "tail-rebudget");
        let host = build_plasm_host_state(PlasmHostBootstrap {
            engine: ExecutionEngine::new(ExecutionConfig::default()).unwrap(),
            mode: ExecutionMode::Live,
            registry: Arc::new(CgsRegistry::from_pairs(vec![(
                "default".into(),
                "Matrix".into(),
                vec![],
                cgs.clone(),
            )])),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
        .unwrap();
        let mut source = synthetic_published_result_step(27, None);
        let mut result = source.result.as_ref().clone();
        result.collection = plasm_runtime::execution::ExecutionCollection::observe(
            result.collection.membership().identity().clone(),
            (0..27)
                .map(|i| berry_entity(&format!("berry-{i}")))
                .collect(),
            Observation::ExactOutput { decoded: 27 },
        )
        .unwrap();
        source.result = Arc::new(result);
        source.entry_id = Some("default".into());
        source.entity = Some("Berry".into());
        source.cgs = Some(cgs);
        source.display = "Berry[name]".into();
        source.projection = Some(vec!["name".into()]);
        let first =
            super::publish_plasm_result_steps(&session, None, None, None, &[source]).unwrap();
        let handle = first.delivered_steps[0]
            .result
            .paging_handle
            .as_ref()
            .unwrap();
        let cursor = session.peek_synthetic_paging_resume(handle).unwrap();
        let tail =
            super::super::run_line::synthetic_page_result(&session, handle, cursor, None).unwrap();
        assert!(session.peek_synthetic_paging_resume(handle).is_none());
        assert!(tail.paging_handle.is_none());
        assert_eq!(tail.collection.delivery_range(), 25..27);
        let tail_step = PublishedResultStep {
            result: Arc::new(tail),
            ..first.delivered_steps[0].clone()
        };
        let clipped = super::publish_plasm_result_steps_with_policy(
            &session,
            None,
            None,
            None,
            &[tail_step],
            &McpResultTransportPolicy {
                inline_text_budget_bytes: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            clipped.delivered_steps[0]
                .result
                .collection
                .delivery_range(),
            25..26
        );
        let next = clipped.delivered_steps[0]
            .result
            .paging_handle
            .as_ref()
            .unwrap();
        let bundle = crate::mcp_server::compile_page_continuation(&session, next, 0).unwrap();
        let final_page = crate::plasm_plan_run::run_plasm_comp(
            &session,
            &host,
            &session.prompt_hash,
            "tail-rebudget-session",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let final_result = &final_page.return_steps[0].result;
        assert_eq!(final_result.collection.delivery_range(), 26..27);
        assert_eq!(
            final_result.entities()[0].reference,
            berry_entity("berry-26").reference
        );
        assert_eq!(
            final_result
                .collection
                .computation_source()
                .resident_entities()
                .len(),
            27
        );
        assert_eq!(final_result.stats.network_requests, 0);
        assert!(final_result.operations.is_empty());
        assert!(final_result.paging_handle.is_none());
    }

    #[test]
    fn byte_budget_pages_drain_before_the_backend_continuation() {
        let session = paging_session();
        let backend = PagingHandle::mint_monotonic(900);
        let source = synthetic_published_result_step_with_paging(27, None, Some(backend.clone()));
        let policy = McpResultTransportPolicy {
            inline_text_budget_bytes: 1,
            ..Default::default()
        };
        let mut out = super::publish_plasm_result_steps_with_policy(
            &session,
            None,
            None,
            None,
            &[source],
            &policy,
        )
        .unwrap();
        let mut ids = Vec::new();
        for _ in 0..30 {
            let delivered = &out.delivered_steps[0];
            ids.extend(
                delivered
                    .result
                    .entities()
                    .iter()
                    .map(|row| row.reference.clone()),
            );
            let next = delivered
                .result
                .paging_handle
                .as_ref()
                .expect("backend continuation must survive");
            if next == &backend {
                break;
            }
            let cursor = session.peek_synthetic_paging_resume(next).unwrap();
            let result =
                super::super::run_line::synthetic_page_result(&session, next, cursor, None)
                    .unwrap();
            assert_eq!(result.stats.network_requests, 0);
            assert!(result.operations.is_empty());
            let step = PublishedResultStep {
                result: Arc::new(result),
                ..delivered.clone()
            };
            out = super::publish_plasm_result_steps_with_policy(
                &session,
                None,
                None,
                None,
                &[step],
                &policy,
            )
            .unwrap();
        }
        assert_eq!(ids.len(), 27);
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            27
        );
        assert_eq!(
            out.delivered_steps[0].result.paging_handle.as_ref(),
            Some(&backend)
        );
    }

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
                collection: crate::test_support::execution_fixtures::collection(
                    Vec::new(),
                    plasm_runtime::ResultCoverage::Unknown,
                ),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
                operations: plasm_runtime::OperationLedger::empty(),
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
    fn publish_over_cap_with_snapshot_preserves_continuation() {
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
            out.markdown.contains("```tsv"),
            "must retain inline TSV when a snapshot is stored: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("25/49 rows delivered"),
            "must report the actual displayed prefix: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("resources/read"),
            "expected snapshot URI hint: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains(&handle.canonical_plasm_uri),
            "expected canonical snapshot URI: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("more pages"),
            "a stored snapshot must not hide continuation: {}",
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
            paging_meta.is_some(),
            "a complete snapshot must preserve paging metadata: {:?}",
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
    fn publish_extreme_row_count_with_snapshot_keeps_rows() {
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
            out.markdown.contains("```tsv"),
            "expected bounded row preview: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("resources/read"),
            "expected snapshot URI hint: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains(&handle.canonical_plasm_uri),
            "expected canonical snapshot URI: {}",
            out.markdown
        );
        assert!(
            !out.markdown.contains("move-936"),
            "must not inline full 937-row TSV: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("```tsv"),
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
            out.markdown.contains("25/40 rows delivered"),
            "expected row-limit note: {}",
            out.markdown
        );
    }

    #[test]
    fn observe_fm0_hook_is_cut_over_publish_returns_raw_tsv() {
        // Product publish must not append didactic F_m0 footers onto Plasm Markdown.
        let src = "## files (11 rows)\n```tsv\npath\tflag\n/a\ttrue\n/b\ttrue\n/c\ttrue\n/d\ttrue\n/e\ttrue\n/f\ttrue\n/g\ttrue\n/h\ttrue\n/i\ttrue\n/j\ttrue\n/k\ttrue\n```\n";
        let md = crate::observe_process_using::append_tsv_process_using_footers(src);
        assert_eq!(md, src, "observe footers are cut over — identity only");
        assert!(!md.contains("not N applies"));
    }

    #[test]
    fn publish_partial_failure_is_agent_facing_not_private_stats() {
        let mut operations =
            plasm_runtime::OperationLedger::from_ack(plasm_runtime::OperationAck {
                entry_id: "langmatrix".into(),
                capability: "langitem_delete".into(),
                entity: "LangItem".into(),
                logical_invocations: 3,
                completed: 2,
                failed: 1,
                source: ExecutionSource::Live,
                description: "Delete LangItem".into(),
                outcomes: Vec::new(),
            });
        operations.merge_ack(plasm_runtime::OperationAck {
            entry_id: "langmatrix".into(),
            capability: "langitem_ping".into(),
            entity: "LangItem".into(),
            logical_invocations: 1,
            completed: 1,
            failed: 0,
            source: ExecutionSource::Live,
            description: "Records a ping against the item (matrix conformance).".into(),
            outcomes: Vec::new(),
        });
        let step = PublishedResultStep {
            name: Some("effects".into()),
            node_id: None,
            entry_id: Some("langmatrix".into()),
            entity: Some("LangItem".into()),
            cgs: None,
            display: "effects".into(),
            projection: None,
            result: Arc::new(ExecutionResult {
                collection: crate::test_support::execution_fixtures::collection(
                    Vec::new(),
                    plasm_runtime::ResultCoverage::Unknown,
                ),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
                operations,
            }),
            artifact: None,
        };
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(!out.markdown.contains("(no results)"), "{}", out.markdown);
        assert!(!out.markdown.contains("0 rows"), "{}", out.markdown);
        assert!(
            out.markdown.contains("`langmatrix/langitem_delete`"),
            "{}",
            out.markdown
        );
        assert!(
            out.markdown.contains("`langmatrix/langitem_ping`"),
            "{}",
            out.markdown
        );
        assert!(out.markdown.contains("2 completed"), "{}", out.markdown);
        assert!(out.markdown.contains("1 failed"), "{}", out.markdown);
        assert!(
            out.markdown.contains("does not imply rollback"),
            "{}",
            out.markdown
        );
        assert!(!out.markdown.contains("applied"), "{}", out.markdown);
        let wire = crate::output::http_execute_results_value(&step.result);
        assert_eq!(wire["rows"], serde_json::json!([]));
        assert_eq!(wire["operations"].as_array().map(|a| a.len()), Some(2));
    }

    #[test]
    fn dag_compute_hidden_cell_keeps_row_and_effect_receipt() {
        let mut step = synthetic_published_result_step(1, None);
        let result = Arc::make_mut(&mut step.result);
        let mut row = result.entities()[0].clone();
        row.fields.insert(
            "name".into(),
            plasm_core::Value::String("x".repeat(100_000)).into(),
        );
        result.collection = result.collection.replace(0, row).unwrap();
        result.operations = plasm_runtime::OperationLedger::from_ack(plasm_runtime::OperationAck {
            entry_id: "matrix".into(),
            capability: "record_update".into(),
            entity: "Record".into(),
            logical_invocations: 1,
            completed: 1,
            failed: 0,
            source: ExecutionSource::Live,
            description: "Update record".into(),
            outcomes: Vec::new(),
        });
        let policy = McpResultTransportPolicy {
            artifact_access: crate::mcp_run_markdown::ArtifactAccessMode::DagCompute,
            ..McpResultTransportPolicy::default()
        };
        let out = publish_plasm_result_steps_with_policy(None, None, &[step], &policy);
        assert!(!out.markdown.contains("(in artifact)"), "{}", out.markdown);
        assert!(!out.markdown.contains("resources/read"), "{}", out.markdown);
        assert!(out.markdown.contains("```tsv"), "{}", out.markdown);
        assert!(out.markdown.contains("\"m0\""), "{}", out.markdown);
        assert!(out.markdown.contains("record_update"), "{}", out.markdown);
        assert!(out.markdown.contains("1 completed"), "{}", out.markdown);
        assert!(out.markdown.contains("compute"), "{}", out.markdown);
        let meta = serde_json::to_string(&out.tool_meta).unwrap();
        assert!(
            !meta.contains("preview_entities"),
            "inline rows must not be duplicated in metadata"
        );
        assert!(!meta.contains("snapshot_only"));
    }

    #[test]
    fn dag_compute_large_result_keeps_bounded_rows() {
        let policy = McpResultTransportPolicy {
            artifact_access: crate::mcp_run_markdown::ArtifactAccessMode::DagCompute,
            in_band_entity_rows: 10,
            inline_text_budget_bytes: 12 * 1024,
        };
        let step = synthetic_published_result_step(600, None);
        let out = publish_plasm_result_steps_with_policy(None, None, &[step], &policy);
        assert!(out.markdown.contains("\"m0\""), "{}", out.markdown);
        assert!(!out.markdown.contains("\"m10\""), "{}", out.markdown);
        assert!(
            out.markdown.contains("10/600 rows delivered"),
            "{}",
            out.markdown
        );
        assert!(out.markdown.contains("more pages"), "{}", out.markdown);
    }

    #[test]
    fn observations_survive_every_transport_artifact_and_multi_return_limit() {
        let policy = McpResultTransportPolicy {
            artifact_access: crate::mcp_run_markdown::ArtifactAccessMode::DagCompute,
            ..McpResultTransportPolicy::default()
        };
        let handle = RunArtifactHandle {
            run_id: RunArtifactId::from_bytes([7; 32]),
            resource_index: 1,
            plasm_uri: "plasm://r/1".into(),
            canonical_plasm_uri: "plasm://test".into(),
            http_path: "/test".into(),
            payload_len: 1,
            request_fingerprints: vec![],
        };
        for mode in [
            crate::mcp_run_markdown::ArtifactAccessMode::DagCompute,
            crate::mcp_run_markdown::ArtifactAccessMode::ResourcesRead,
            crate::mcp_run_markdown::ArtifactAccessMode::ToolFallback,
        ] {
            let policy = McpResultTransportPolicy {
                artifact_access: mode,
                ..policy
            };
            for count in [0, 1, 40, 41, 600] {
                for artifact in [None, Some(handle.clone())] {
                    let mut step = synthetic_published_result_step(count, artifact);
                    let result = Arc::make_mut(&mut step.result);
                    for index in 0..count {
                        let mut row = result.entities()[index].clone();
                        row.fields.insert(
                            "name".into(),
                            plasm_core::Value::String("é".repeat(500)).into(),
                        );
                        result.collection = result.collection.replace(index, row).unwrap();
                    }
                    let out = publish_plasm_result_steps_with_policy(
                        None,
                        None,
                        &[step.clone(), step],
                        &policy,
                    );
                    if mode == crate::mcp_run_markdown::ArtifactAccessMode::DagCompute {
                        assert!(!out.markdown.contains("resources/read"), "{}", out.markdown);
                    }
                    if count > 0 {
                        assert_eq!(out.markdown.matches("\"m0\"").count(), 2);
                        assert!(!out.markdown.contains(&format!("0/{count} rows delivered")));
                        // A string that fits a cell is exact, even when the aggregate table is too large.
                        assert!(out.markdown.contains(&"é".repeat(500)));
                    } else {
                        assert!(out.markdown.contains("0 rows"));
                    }
                }
            }
        }
    }

    #[test]
    fn publish_empty_query_has_no_operations_block() {
        let step = PublishedResultStep {
            name: Some("items".into()),
            node_id: None,
            entry_id: Some("langmatrix".into()),
            entity: Some("LangItem".into()),
            cgs: None,
            display: "items".into(),
            projection: None,
            result: Arc::new(ExecutionResult {
                collection: crate::test_support::execution_fixtures::collection(
                    Vec::new(),
                    plasm_runtime::ResultCoverage::Unknown,
                ),
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
                operations: plasm_runtime::OperationLedger::empty(),
            }),
            artifact: None,
        };
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(out.markdown.contains("(no results)"), "{}", out.markdown);
        assert!(!out.markdown.contains("operations:"), "{}", out.markdown);
        let wire = crate::output::http_execute_results_value(&step.result);
        assert_eq!(wire["operations"], serde_json::json!([]));
    }

    fn with_coverage(
        mut step: PublishedResultStep,
        coverage: plasm_runtime::ResultCoverage,
    ) -> PublishedResultStep {
        let mut result = (*step.result).clone();
        result.collection = crate::test_support::execution_fixtures::collection(
            result.entities().iter().cloned().collect(),
            coverage,
        );
        step.result = Arc::new(result);
        step
    }

    #[test]
    fn markdown_full_mode_mentions_coverage() {
        let step = with_coverage(
            synthetic_published_result_step(3, None),
            plasm_runtime::ResultCoverage::Complete,
        );
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            out.markdown
                .contains("complete coverage for this expression."),
            "Full mode must mention coverage: {}",
            out.markdown
        );
    }

    #[test]
    fn markdown_capped_inline_mentions_coverage() {
        let step = with_coverage(
            synthetic_published_result_step(10, None),
            plasm_runtime::ResultCoverage::Partial,
        );
        let policy = McpResultTransportPolicy {
            in_band_entity_rows: 2,
            ..McpResultTransportPolicy::default()
        };
        let out = publish_plasm_result_steps_with_policy(
            None,
            None,
            std::slice::from_ref(&step),
            &policy,
        );
        assert!(
            out.markdown
                .contains("partial coverage for this expression."),
            "CappedInline must mention coverage: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("2/10 rows delivered"),
            "CappedInline should report shown/total: {}",
            out.markdown
        );
    }

    #[test]
    fn markdown_snapshot_backed_preview_mentions_coverage() {
        let run_id = RunArtifactId::from_wire(&format!("pr{}", "d".repeat(64))).expect("wire");
        let handle = RunArtifactHandle {
            run_id,
            resource_index: 1,
            plasm_uri: crate::run_artifacts::plasm_short_resource_uri(1),
            canonical_plasm_uri: crate::run_artifacts::plasm_run_resource_uri("ph", "sid", &run_id),
            http_path: crate::run_artifacts::artifact_http_path("ph", "sid", &run_id),
            payload_len: 0,
            request_fingerprints: vec!["fp".into()],
        };
        let step = with_coverage(
            synthetic_published_result_step(49, Some(handle)),
            plasm_runtime::ResultCoverage::Partial,
        );
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            out.markdown
                .contains("partial coverage for this expression."),
            "SnapshotOnly must mention coverage: {}",
            out.markdown
        );
    }

    #[test]
    fn markdown_empty_result_mentions_coverage() {
        let step = with_coverage(
            synthetic_published_result_step(0, None),
            plasm_runtime::ResultCoverage::Complete,
        );
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            out.markdown
                .contains("complete coverage for this expression."),
            "empty Full result must mention coverage: {}",
            out.markdown
        );
    }

    /// Host page filled exactly (25 of 25) without a continuation driver → Unknown must still
    /// appear in agent-visible Markdown (the AppWorld person-search silence class).
    #[test]
    fn markdown_host_page_full_unknown_mentions_coverage() {
        let step = with_coverage(
            synthetic_published_result_step(25, None),
            plasm_runtime::ResultCoverage::Unknown,
        );
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            out.markdown.contains("### moves (25 rows)")
                || out.markdown.contains("## moves (25 rows)"),
            "expected 25-row section header: {}",
            out.markdown
        );
        assert!(
            out.markdown
                .contains("unknown coverage for this expression."),
            "exactly-full host page must stamp Unknown coverage: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("25 rows ·"),
            "shown/total must remain even when equal: {}",
            out.markdown
        );
    }

    #[test]
    fn markdown_host_page_partial_mentions_coverage() {
        let step = with_coverage(
            synthetic_published_result_step(25, None),
            plasm_runtime::ResultCoverage::Partial,
        );
        let out = publish_plasm_result_steps(None, None, std::slice::from_ref(&step));
        assert!(
            out.markdown
                .contains("partial coverage for this expression."),
            "Partial host page must stamp coverage: {}",
            out.markdown
        );
    }

    #[test]
    fn markdown_multi_step_full_mentions_coverage_per_return() {
        let alex = {
            let mut s = with_coverage(
                synthetic_published_result_step(25, None),
                plasm_runtime::ResultCoverage::Unknown,
            );
            s.name = Some("alex".into());
            s
        };
        let morgan = {
            let mut s = with_coverage(
                synthetic_published_result_step(25, None),
                plasm_runtime::ResultCoverage::Partial,
            );
            s.name = Some("morgan".into());
            s
        };
        let out = publish_plasm_result_steps(None, None, &[alex, morgan]);
        assert!(
            out.markdown.contains("# Results"),
            "multi-return uses Results heading: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("### alex (25 rows)"),
            "alex section missing: {}",
            out.markdown
        );
        assert!(
            out.markdown.contains("### morgan (25 rows)"),
            "morgan section missing: {}",
            out.markdown
        );
        let unknown_hits = out
            .markdown
            .matches("unknown coverage for this expression.")
            .count();
        let partial_hits = out
            .markdown
            .matches("partial coverage for this expression.")
            .count();
        assert_eq!(
            unknown_hits, 1,
            "expected one Unknown stamp: {}",
            out.markdown
        );
        assert_eq!(
            partial_hits, 1,
            "expected one Partial stamp: {}",
            out.markdown
        );
    }

    #[test]
    fn markdown_bounded_preview_mentions_coverage() {
        // Row limits preserve visible evidence and its acquisition coverage.
        let step = with_coverage(
            synthetic_published_result_step(10, None),
            plasm_runtime::ResultCoverage::Unknown,
        );
        let policy = McpResultTransportPolicy {
            in_band_entity_rows: 2,
            inline_text_budget_bytes: 12 * 1024,
            ..McpResultTransportPolicy::default()
        };
        let out = publish_plasm_result_steps_with_policy(
            None,
            None,
            std::slice::from_ref(&step),
            &policy,
        );
        assert!(
            out.markdown.contains("more pages"),
            "expected bounded resumable page: {}",
            out.markdown
        );
        assert!(
            out.markdown
                .contains("unknown coverage for this expression."),
            "bounded preview must still stamp coverage: {}",
            out.markdown
        );
    }
}
