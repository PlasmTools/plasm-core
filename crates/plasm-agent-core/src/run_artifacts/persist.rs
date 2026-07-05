//! Canonical persist path for execute run snapshots (store + session hot cache + spill delta).

use plasm_core::expr_parser::ParsedExpr;
use plasm_runtime::ExecutionResult;
use uuid::Uuid;

use crate::execute_session::{ExecuteSession, GraphEpoch};
use crate::server_state::PlasmHostState;
use crate::trace_sink_emit::PlasmTraceContext;

use super::{
    artifact_http_path, document_from_run, plasm_run_resource_uri, plasm_session_short_run_uri,
    plasm_short_run_uri_logical, ArtifactPayload, ArtifactPayloadMetadata, DocumentFromRun,
    RunArtifactHandle, RunArtifactId,
};

#[derive(Debug, thiserror::Error)]
pub enum PersistExecuteRunError {
    #[error("run artifact id digest failed: {0}")]
    Mint(String),
    #[error("run artifact JSON: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("run artifact persist failed: {0}")]
    Persist(String),
}

pub struct PersistExecuteRunInput<'a> {
    pub st: &'a PlasmHostState,
    pub sess: &'a ExecuteSession,
    pub session_id: &'a str,
    pub entry_id: &'a str,
    pub source_line: &'a str,
    pub display_lines: Vec<String>,
    pub parsed: &'a ParsedExpr,
    pub result: &'a ExecutionResult,
    pub trace: Option<&'a PlasmTraceContext>,
}

pub fn mint_run_artifact_id_for_session(
    sess: &ExecuteSession,
    entry_id: &str,
    source_line: &str,
    parsed: &ParsedExpr,
    fingerprints: &[String],
) -> Result<RunArtifactId, PersistExecuteRunError> {
    RunArtifactId::from_plan_bundle_inputs(
        &sess.cgs.catalog_cgs_hash_hex(),
        sess.domain_revision,
        entry_id,
        source_line,
        parsed,
        fingerprints,
    )
    .map_err(|e| PersistExecuteRunError::Mint(e.to_string()))
}

fn resolve_run_artifact_uris(
    trace: Option<&PlasmTraceContext>,
    prompt_hash: &str,
    session_id: &str,
    run_id: &RunArtifactId,
    _resource_index: u64,
) -> (String, String, String) {
    let canonical_plasm_uri = plasm_run_resource_uri(prompt_hash, session_id, run_id);
    let plasm_uri = trace
        .and_then(|c| {
            if let Some(ref seg) = c.logical_session_ref {
                Some(plasm_session_short_run_uri(seg.as_str(), run_id))
            } else {
                c.logical_session_id
                    .as_deref()
                    .and_then(|ls| Uuid::parse_str(ls).ok())
                    .map(|u| plasm_short_run_uri_logical(&u, run_id))
            }
        })
        .unwrap_or_else(|| canonical_plasm_uri.clone());
    let http_path = artifact_http_path(prompt_hash, session_id, run_id);
    (plasm_uri, canonical_plasm_uri, http_path)
}

/// Serialize, store, append session hot cache, and optional graph spill delta for one execute run.
pub async fn persist_execute_run(
    input: PersistExecuteRunInput<'_>,
) -> Result<RunArtifactHandle, PersistExecuteRunError> {
    let PersistExecuteRunInput {
        st,
        sess,
        session_id,
        entry_id,
        source_line,
        display_lines,
        parsed,
        result,
        trace,
    } = input;

    let run_id = mint_run_artifact_id_for_session(
        sess,
        entry_id,
        source_line,
        parsed,
        &result.request_fingerprints,
    )?;
    let resource_index = sess.mint_run_resource_index();
    let doc = document_from_run(DocumentFromRun {
        run_id,
        prompt_hash: sess.prompt_hash.as_str(),
        session_id,
        entry_id,
        principal: sess.principal.clone(),
        display_lines,
        parsed_preimage: parsed,
        result,
        resource_index: Some(resource_index),
    });
    let payload_bytes = serde_json::to_vec(&doc)?;
    let payload_len = payload_bytes.len();
    let payload = ArtifactPayload {
        metadata: ArtifactPayloadMetadata::json_default(),
        bytes: axum::body::Bytes::from(payload_bytes),
    };
    st.run_artifacts
        .insert_payload(
            sess.prompt_hash.as_str(),
            session_id,
            run_id,
            Some(resource_index),
            &payload,
        )
        .await
        .map_err(|e| PersistExecuteRunError::Persist(e.to_string()))?;
    crate::metrics::record_run_artifact_archive_put_ok();
    let graph_epoch = {
        let cache = sess.lock_graph_cache().await;
        GraphEpoch(cache.stats().version)
    };
    let appended = sess
        .core
        .append_run_artifact(run_id, graph_epoch, resource_index, payload)
        .await;
    if let Some(persistence) = &st.session_graph_persistence {
        if let Err(e) = persistence
            .append_delta(
                sess.prompt_hash.as_str(),
                session_id,
                appended.seq.0,
                &appended.payload,
            )
            .await
        {
            tracing::warn!(error = %e, "session graph delta append failed");
        }
    }
    let (plasm_uri, canonical_plasm_uri, http_path) = resolve_run_artifact_uris(
        trace,
        sess.prompt_hash.as_str(),
        session_id,
        &run_id,
        resource_index,
    );
    Ok(RunArtifactHandle {
        run_id,
        resource_index,
        plasm_uri,
        canonical_plasm_uri,
        http_path,
        payload_len,
        request_fingerprints: result.request_fingerprints.clone(),
    })
}
