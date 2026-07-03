//! Shared run artifact resolution for MCP `resources/read` and `plasm_read_run_artifact`.

use std::time::Instant;

use plasm_trace::RunArtifactArchiveRef;

use crate::run_artifacts::{
    logical_uuid_from_uri_segment, parse_plasm_execute_run_uri,
    parse_plasm_session_short_resource_uri, ArtifactPayload, RunArtifactId,
};
use crate::server_state::PlasmHostState;

use super::resource_read_trace;
use super::transport::PlasmExecBinding;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RunArtifactResolveError {
    #[error("invalid logical session in URI: use `plasm://session/l_<token>/r/...` from `plasm_context`")]
    InvalidSessionRef,
    #[error(
        "unknown resource_index {0} for this session (not meta_generation / dict_ref; use steps[].resource_index, run_id, or canonical_artifact_uri)"
    )]
    UnknownIndex(u64),
    #[error("unknown run artifact (wrong run_id or not yet stored for this session)")]
    UnknownRunId,
    #[error("unsupported resource URI: {0}")]
    UnsupportedUri(String),
    #[error("run artifact decode failed: {0}")]
    DecodeFailed(String),
    #[error("artifact URI session does not match logical_session_ref")]
    SessionMismatch,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedRunArtifact {
    pub payload: ArtifactPayload,
    pub run_id: Option<RunArtifactId>,
    pub resource_index: Option<u64>,
    pub prompt_hash: String,
    pub session_id: String,
    pub metric_kind: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) enum RunArtifactLookup {
    ShortIndex { resource_index: u64 },
    CanonicalRun { run_id: RunArtifactId },
}

#[derive(Debug, Clone)]
pub(crate) enum RunArtifactLookupArg {
    ArtifactUri(String),
    ResourceIndex(u64),
    RunId(RunArtifactId),
}

pub(crate) fn lookup_from_artifact_uri(
    uri: &str,
    binding: &PlasmExecBinding,
    logical_uuid_str: &str,
) -> Result<RunArtifactLookup, RunArtifactResolveError> {
    if let Some((segment, resource_index)) = parse_plasm_session_short_resource_uri(uri) {
        let Some(logical_uuid) = logical_uuid_from_uri_segment(&segment) else {
            return Err(RunArtifactResolveError::InvalidSessionRef);
        };
        if logical_uuid.to_string() != logical_uuid_str {
            return Err(RunArtifactResolveError::SessionMismatch);
        }
        return Ok(RunArtifactLookup::ShortIndex { resource_index });
    }
    let Some((prompt_hash, session_id, run_id)) = parse_plasm_execute_run_uri(uri) else {
        return Err(RunArtifactResolveError::UnsupportedUri(uri.to_string()));
    };
    if prompt_hash != binding.prompt_hash || session_id != binding.session_id {
        return Err(RunArtifactResolveError::SessionMismatch);
    }
    Ok(RunArtifactLookup::CanonicalRun { run_id })
}

pub(crate) fn resolve_lookup_arg(
    session_ref: &str,
    binding: &PlasmExecBinding,
    logical_uuid_str: &str,
    arg: RunArtifactLookupArg,
) -> Result<(String, RunArtifactLookup), RunArtifactResolveError> {
    match arg {
        RunArtifactLookupArg::ArtifactUri(raw) => {
            let uri = raw.trim();
            let lookup = lookup_from_artifact_uri(uri, binding, logical_uuid_str)?;
            Ok((uri.to_string(), lookup))
        }
        RunArtifactLookupArg::ResourceIndex(idx) => Ok((
            format!("plasm://session/{session_ref}/r/{idx}"),
            RunArtifactLookup::ShortIndex {
                resource_index: idx,
            },
        )),
        RunArtifactLookupArg::RunId(run_id) => Ok((
            format!(
                "plasm://execute/{}/{}/run/{}",
                binding.prompt_hash,
                binding.session_id,
                run_id.to_wire()
            ),
            RunArtifactLookup::CanonicalRun { run_id },
        )),
    }
}

pub(crate) async fn resolve_run_artifact_for_binding(
    plasm: &PlasmHostState,
    binding: &PlasmExecBinding,
    lookup: RunArtifactLookup,
    ls_key: Option<&str>,
    read_source: Option<&str>,
    started: Instant,
    uri_for_trace: &str,
) -> Result<ResolvedRunArtifact, RunArtifactResolveError> {
    let (metric_kind, resource_index, run_id_hint) = match lookup {
        RunArtifactLookup::ShortIndex { resource_index } => {
            ("logical_short", Some(resource_index), None)
        }
        RunArtifactLookup::CanonicalRun { run_id } => ("canonical", None, Some(run_id)),
    };

    let fetched = fetch_run_artifact_payload(plasm, binding, resource_index, run_id_hint).await;

    match fetched {
        Ok((payload, run_id, resolved_index)) => {
            let archive = run_id.map(|rid| RunArtifactArchiveRef {
                prompt_hash: binding.prompt_hash.clone(),
                session_id: binding.session_id.clone(),
                run_id: rid.to_wire(),
                resource_index: resolved_index,
            });
            resource_read_trace::McpResourceReadTrace::success(
                ls_key,
                read_source,
                started,
                uri_for_trace,
                archive,
                &payload,
            )
            .emit(plasm)
            .await;
            Ok(ResolvedRunArtifact {
                payload,
                run_id,
                resource_index: resolved_index,
                prompt_hash: binding.prompt_hash.clone(),
                session_id: binding.session_id.clone(),
                metric_kind,
            })
        }
        Err(e) => {
            let (reason, archive) = match &e {
                RunArtifactResolveError::DecodeFailed(_) => {
                    ("decode_failed", archive_for_lookup(binding, &lookup))
                }
                RunArtifactResolveError::UnknownIndex(_) => ("unknown_artifact", None),
                RunArtifactResolveError::UnknownRunId => {
                    ("unknown_artifact", archive_for_lookup(binding, &lookup))
                }
                _ => ("unknown_artifact", None),
            };
            resource_read_trace::McpResourceReadTrace::error(
                ls_key,
                read_source,
                started,
                uri_for_trace,
                archive,
                reason,
            )
            .emit(plasm)
            .await;
            Err(e)
        }
    }
}

fn archive_for_lookup(
    binding: &PlasmExecBinding,
    lookup: &RunArtifactLookup,
) -> Option<RunArtifactArchiveRef> {
    match lookup {
        RunArtifactLookup::ShortIndex { resource_index } => Some(RunArtifactArchiveRef {
            prompt_hash: binding.prompt_hash.clone(),
            session_id: binding.session_id.clone(),
            run_id: String::new(),
            resource_index: Some(*resource_index),
        }),
        RunArtifactLookup::CanonicalRun { run_id } => Some(RunArtifactArchiveRef {
            prompt_hash: binding.prompt_hash.clone(),
            session_id: binding.session_id.clone(),
            run_id: run_id.to_wire(),
            resource_index: None,
        }),
    }
}

async fn fetch_run_artifact_payload(
    plasm: &PlasmHostState,
    binding: &PlasmExecBinding,
    resource_index: Option<u64>,
    run_id: Option<RunArtifactId>,
) -> Result<(ArtifactPayload, Option<RunArtifactId>, Option<u64>), RunArtifactResolveError> {
    let live_sess = plasm
        .get_execute_session(binding.prompt_hash.as_str(), binding.session_id.as_str())
        .await;

    let (live_payload, live_run_id, _live_index) = if let Some(ref sess) = live_sess {
        if let Some(idx) = resource_index {
            let art = sess.core.get_run_artifact_by_resource_index(idx).await;
            (
                art.as_ref().map(|a| a.payload.clone()),
                art.as_ref().map(|a| a.run_id),
                Some(idx),
            )
        } else if let Some(rid) = run_id {
            let art = sess.core.get_run_artifact(rid).await;
            (art.as_ref().map(|a| a.payload.clone()), Some(rid), None)
        } else {
            (None, None, None)
        }
    } else {
        (None, run_id, resource_index)
    };

    if live_payload.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("hot");
    }

    let persisted_payload = if live_payload.is_none() {
        if let Some(idx) = resource_index {
            match plasm
                .run_artifacts
                .get_payload_result_by_resource_index(
                    binding.prompt_hash.as_str(),
                    binding.session_id.as_str(),
                    idx,
                )
                .await
            {
                Ok(v) => v,
                Err(e) => return Err(RunArtifactResolveError::DecodeFailed(e.to_string())),
            }
        } else if let Some(rid) = run_id {
            match plasm
                .run_artifacts
                .get_payload_result(&binding.prompt_hash, &binding.session_id, rid)
                .await
            {
                Ok(v) => v,
                Err(e) => return Err(RunArtifactResolveError::DecodeFailed(e.to_string())),
            }
        } else {
            None
        }
    } else {
        None
    };

    if live_payload.is_none() && persisted_payload.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("archive");
    }

    let payload = live_payload.or(persisted_payload);
    let Some(payload) = payload else {
        return if let Some(idx) = resource_index {
            Err(RunArtifactResolveError::UnknownIndex(idx))
        } else {
            Err(RunArtifactResolveError::UnknownRunId)
        };
    };

    let resolved_run_id = match (live_run_id, run_id, resource_index) {
        (Some(rid), _, _) => Some(rid),
        (None, Some(rid), _) => Some(rid),
        (None, None, Some(idx)) => {
            plasm
                .run_artifacts
                .resolve_run_id_for_resource_index(
                    binding.prompt_hash.as_str(),
                    binding.session_id.as_str(),
                    idx,
                )
                .await
        }
        (None, None, None) => None,
    };

    Ok((payload, resolved_run_id, resource_index))
}
