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

fn entity_type_for_doc(doc: &RunArtifactDocument) -> Option<String> {
    doc.parsed_preimage
        .expr
        .qualified_entity_key()
        .map(|qe| qe.entity)
}

fn execution_result_from_artifact_doc(
    doc: &RunArtifactDocument,
    cgs: &CGS,
) -> Result<ExecutionResult, OperationError> {
    let entity_type =
        entity_type_for_doc(doc).ok_or_else(|| OperationError::ResultArtifactMissing {
            handle: String::new(),
            run_artifact_id: doc.run_id.clone(),
        })?;
    let mut entities = Vec::with_capacity(doc.entities.len());
    for row in &doc.entities {
        match CachedEntity::from_row_json(entity_type.as_str(), row, cgs) {
            Ok(entity) => entities.push(entity),
            Err(_) => {
                return Err(OperationError::ResultArtifactMissing {
                    handle: String::new(),
                    run_artifact_id: doc.run_id.clone(),
                });
            }
        }
    }
    Ok(ExecutionResult {
        count: entities.len(),
        entities,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: doc.source,
        stats: doc.stats.clone(),
        request_fingerprints: doc.request_fingerprints.clone(),
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
    let entity = entity_type_for_doc(doc);
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
) -> Result<(ExecuteRunToolOutput, Vec<serde_json::Value>), OperationError> {
    let run_id =
        RunArtifactId::from_wire(wire_id).ok_or_else(|| OperationError::ResultArtifactMissing {
            handle: String::new(),
            run_artifact_id: wire_id.to_string(),
        })?;
    let artifact = RunArtifactHandle {
        run_id,
        resource_index: doc.resource_index.unwrap_or(1),
        plasm_uri: crate::run_artifacts::plasm_short_resource_uri(doc.resource_index.unwrap_or(1)),
        canonical_plasm_uri: crate::run_artifacts::plasm_run_resource_uri(
            doc.prompt_hash.as_str(),
            doc.session_id.as_str(),
            &run_id,
        ),
        http_path: crate::run_artifacts::artifact_http_path(
            doc.prompt_hash.as_str(),
            doc.session_id.as_str(),
            &run_id,
        ),
        payload_len: 0,
        request_fingerprints: doc.request_fingerprints.clone(),
    };
    let step = published_step_from_artifact_doc(doc, es, Some(artifact))?;
    let node_results = doc.entities.clone();
    let out = publish_plasm_result_steps(Some(es.cgs.as_ref()), None, std::slice::from_ref(&step));
    Ok((out, node_results))
}

#[cfg(test)]
mod tests {
    use plasm_core::expr_parser::ParsedExpr;
    use plasm_core::{Expr, QueryExpr};
    use plasm_runtime::{ExecutionSource, ExecutionStats};

    use super::*;
    use crate::run_artifacts::RunArtifactDocument;

    #[test]
    fn hydrate_empty_artifact_uses_tsv_not_json() {
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
            None,
        );
        let doc = RunArtifactDocument {
            run_id: "pr".to_string() + &"a".repeat(64),
            prompt_hash: "ph".into(),
            session_id: "sid".into(),
            entry_id: "default".into(),
            resource_index: Some(1),
            principal: None,
            parsed_preimage: ParsedExpr {
                expr: Expr::Query(QueryExpr {
                    entity: "Pet".into(),
                    predicate: None,
                    projection: None,
                    pagination: None,
                    hydrate: None,
                    capability_name: None,
                    catalog_entry_id: Some("default".into()),
                }),
                projection: None,
            },
            display_lines: vec!["pets".into()],
            request_fingerprints: vec!["fp1".into()],
            entities: vec![],
            source: ExecutionSource::Live,
            stats: ExecutionStats::default(),
        };
        let (out, _) = hydrate_plan_run_from_artifact_formatted(&doc, &es, doc.run_id.as_str())
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
}
