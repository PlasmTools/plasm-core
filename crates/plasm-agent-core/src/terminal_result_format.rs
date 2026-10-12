//! Terminal MCP/HTTP run formatting from stored run artifacts (TSV path shared with live execute).

use std::sync::Arc;

use plasm_core::CGS;
use plasm_runtime::{CachedEntity, ExecutionResult};

use crate::execute_session::ExecuteSession;
use crate::http_execute::{publish_plasm_result_steps, ExecuteRunToolOutput, PublishedResultStep};
use crate::operation_error::OperationError;
use crate::run_artifacts::{RunArtifactDocument, RunArtifactHandle, RunArtifactId};

fn cgs_for_entry(es: &ExecuteSession, entry_id: &str) -> Option<Arc<CGS>> {
    es.contexts_by_entry
        .get(entry_id)
        .map(|c| c.cgs.clone())
        .or_else(|| Some(Arc::clone(&es.cgs)))
}

fn execution_result_from_artifact_doc(
    doc: &RunArtifactDocument,
    _cgs: &CGS,
) -> Result<ExecutionResult, OperationError> {
    let collection =
        doc.recorded_collection()
            .map_err(|_| OperationError::ResultArtifactMissing {
                handle: String::new(),
                run_artifact_id: doc.run_id.clone(),
            })?;
    Ok(ExecutionResult {
        collection,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: doc.source,
        stats: doc.stats.clone(),
        request_fingerprints: doc.request_fingerprints.clone(),
        operations: doc.operations.clone(),
    })
}

pub fn published_step_from_artifact_doc(
    doc: &RunArtifactDocument,
    es: &ExecuteSession,
    artifact: Option<RunArtifactHandle>,
) -> Result<PublishedResultStep, OperationError> {
    let entry_id = doc.entry_id.clone();
    let cgs = cgs_for_entry(es, entry_id.as_str()).ok_or_else(|| {
        OperationError::ResultArtifactMissing {
            handle: String::new(),
            run_artifact_id: doc.run_id.clone(),
        }
    })?;
    let entity = Some(doc.entity.clone());
    let result = execution_result_from_artifact_doc(doc, cgs.as_ref())?;
    let display = doc
        .display_lines
        .first()
        .cloned()
        .unwrap_or_else(|| "result".to_string());
    Ok(PublishedResultStep {
        name: None,
        node_id: None,
        entry_id: Some(entry_id),
        entity,
        cgs: Some(cgs),
        display,
        projection: None,
        result: Arc::new(result),
        artifact,
    })
}

pub fn hydrate_plan_run_from_artifact_formatted(
    doc: &RunArtifactDocument,
    es: &ExecuteSession,
    wire_id: &str,
    logical_session_ref: Option<&str>,
) -> Result<(ExecuteRunToolOutput, Vec<serde_json::Value>), OperationError> {
    let run_id =
        RunArtifactId::from_wire(wire_id).ok_or_else(|| OperationError::ResultArtifactMissing {
            handle: String::new(),
            run_artifact_id: wire_id.to_string(),
        })?;
    let canonical_plasm_uri = crate::run_artifacts::plasm_run_resource_uri(
        doc.prompt_hash.as_str(),
        doc.session_id.as_str(),
        &run_id,
    );
    // Never invent `r/1` when the document has no resource_index (agents confuse that with
    // `meta_generation`). Prefer the canonical run URI for addressing.
    let (resource_index, plasm_uri) = match doc.resource_index {
        Some(idx) if idx > 0 => (idx, crate::run_artifacts::plasm_short_resource_uri(idx)),
        _ => (0, canonical_plasm_uri.clone()),
    };
    let artifact = RunArtifactHandle {
        run_id,
        resource_index,
        plasm_uri,
        canonical_plasm_uri,
        http_path: crate::run_artifacts::artifact_http_path(
            doc.prompt_hash.as_str(),
            doc.session_id.as_str(),
            &run_id,
        ),
        payload_len: 0,
        request_fingerprints: doc.request_fingerprints.clone(),
    };
    let step = published_step_from_artifact_doc(doc, es, Some(artifact))?;
    let out = publish_plasm_result_steps(
        es,
        logical_session_ref,
        Some(es.cgs.as_ref()),
        None,
        std::slice::from_ref(&step),
    )
    .map_err(|error| OperationError::OperationFailed {
        handle: String::new(),
        error: error.into(),
    })?;
    let node_results = out.delivered_steps[0]
        .result
        .entities()
        .iter()
        .map(CachedEntity::payload_to_json)
        .collect();
    Ok((out, node_results))
}

#[cfg(test)]
mod tests {
    use plasm_core::expr_parser::ParsedExpr;
    use plasm_core::{Expr, QueryExpr};
    use plasm_runtime::{ExecutionSource, ExecutionStats};

    use super::*;
    use crate::run_artifacts::RunArtifactDocument;

    fn empty_artifact() -> (ExecuteSession, RunArtifactDocument) {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(plasm_core::CgsContext::entry("default", cgs.clone())),
        );
        let es = ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            "hash".into(),
            None,
        );
        let doc = RunArtifactDocument {
            run_id: "pr".to_string() + &"a".repeat(64),
            prompt_hash: "ph".into(),
            session_id: "sid".into(),
            entry_id: "default".into(),
            entity: "Pet".into(),
            resource_index: Some(1),
            principal: None,
            parsed_preimage: ParsedExpr::from_expr(Expr::Query(QueryExpr {
                entity: "Pet".into(),
                predicate: None,
                projection: None,
                pagination: None,
                hydrate: None,
                capability_name: None,
                catalog_entry_id: plasm_core::CatalogEntryStamp::some(
                    plasm_core::RegistryEntryId::from("default"),
                ),
            })),
            display_lines: vec!["pets".into()],
            request_fingerprints: vec!["fp1".into()],
            entities: vec![].into(),
            collection: crate::test_support::execution_fixtures::checkpoint(
                vec![],
                plasm_runtime::ResultCoverage::Unknown,
            ),
            source: ExecutionSource::Live,
            stats: ExecutionStats::default(),
            operations: plasm_runtime::OperationLedger::empty(),
        };
        (es, doc)
    }

    #[test]
    fn hydrate_empty_artifact_uses_tsv_not_json() {
        let (es, doc) = empty_artifact();
        let (out, _) =
            hydrate_plan_run_from_artifact_formatted(&doc, &es, doc.run_id.as_str(), None)
                .expect("format");
        assert!(out.markdown.contains("```tsv"));
        assert!(!out.markdown.contains("```json"));
        let steps = out
            .tool_meta
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .and_then(|p| p.get("steps"))
            .and_then(|s| s.as_array())
            .expect("artifact steps in meta");
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0].get("run_id").and_then(|v| v.as_str()),
            Some(doc.run_id.as_str())
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(128))]
        #[test]
        fn rowset_properties_artifact_roundtrip_matches_live_context(
            count in 0usize..120, text in ".{0,1200}", complete in proptest::bool::ANY,
        ) {
            let (es, mut doc) = empty_artifact();
            let coverage = if complete { plasm_runtime::ResultCoverage::Complete } else { plasm_runtime::ResultCoverage::Unknown };
            let rows: Vec<_> = (0..count).map(|i| {
                let mut row = CachedEntity::new(plasm_core::Ref::new("Pet", i.to_string()), 0);
                row.fields.insert("computed_name".into(), plasm_core::Value::String(format!("{text}:{i}")).into());
                row.fields.insert("ordinal".into(), plasm_core::Value::Integer(i as i64).into());
                row
            }).collect();
            doc.entities = rows.clone().into();
            doc.collection = crate::test_support::execution_fixtures::checkpoint(rows, coverage);
            let decoded: RunArtifactDocument = serde_json::from_slice(&serde_json::to_vec(&doc).unwrap()).unwrap();
            let live_step = published_step_from_artifact_doc(&doc, &es, None).unwrap();
            let live = publish_plasm_result_steps(&es, None, Some(&es.cgs), None, &[live_step]).unwrap();
            let (stored, returned) = hydrate_plan_run_from_artifact_formatted(&decoded, &es, &doc.run_id, None).unwrap();
            fn table(text: &str) -> &str { text.split("```tsv\n").nth(1).unwrap().split("```").next().unwrap() }
            proptest::prop_assert_eq!(table(&live.markdown), table(&stored.markdown));
            proptest::prop_assert_eq!(live.delivered_steps[0].result.entities().len(), returned.len());
            proptest::prop_assert_eq!(decoded.entities.len(), count);
            proptest::prop_assert_eq!(stored.delivered_steps[0].result.coverage(), coverage);
            proptest::prop_assert!(!stored.markdown.contains("```json"));
            if count != 0 { proptest::prop_assert!(table(&stored.markdown).lines().next().unwrap().contains("computed_name")); }
        }
    }
}
