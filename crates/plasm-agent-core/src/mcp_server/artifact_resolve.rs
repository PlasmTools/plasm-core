//! Shared run artifact resolution for MCP `resources/read` and `plasm_read_run_artifact`.

use std::time::Instant;

use plasm_trace::RunArtifactArchiveRef;

use crate::run_artifacts::{
    logical_uuid_from_uri_segment, parse_plasm_execute_plan_uri, parse_plasm_execute_run_uri,
    parse_plasm_session_short_plan_uri, parse_plasm_session_short_resource_uri,
    parse_plasm_session_short_run_uri, parse_plasm_short_resource_uri, verify_payload_run_id,
    ArtifactPayload, RunArtifactId,
};
use crate::server_state::PlasmHostState;

use super::resource_read_trace;
use super::transport::PlasmExecBinding;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RunArtifactResolveError {
    #[error(
        "invalid logical session in URI: use `plasm://session/l_<token>/run/pr…` from `plasm_run`"
    )]
    InvalidSessionRef,
    #[error("unknown run artifact (wrong run_id or not yet stored for this session)")]
    UnknownRunId,
    #[error("unknown code plan (wrong plan index/id or not yet stored for this session)")]
    UnknownPlan,
    #[error(
        "legacy resource index URI `plasm://…/r/{{n}}` is ambiguous — use `run_id` or `plasm://…/run/pr…` from the same `plasm_run` step"
    )]
    LegacyResourceIndexUri(u64),
    #[error("artifact integrity check failed: {0}")]
    Integrity(String),
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
    pub prompt_hash: String,
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedCodePlanArtifact {
    pub payload: ArtifactPayload,
    pub prompt_hash: String,
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub(crate) enum RunArtifactLookup {
    CanonicalRun { run_id: RunArtifactId },
}

#[derive(Debug, Clone)]
pub(crate) enum RunArtifactLookupArg {
    ArtifactUri(String),
    RunId(RunArtifactId),
}

pub(crate) fn logical_uuid_from_session_scoped_uri(
    uri: &str,
) -> Result<uuid::Uuid, RunArtifactResolveError> {
    let segment = if let Some((segment, _)) = parse_plasm_session_short_run_uri(uri) {
        segment
    } else if let Some((segment, _)) = parse_plasm_session_short_resource_uri(uri) {
        segment
    } else if let Some((segment, _)) = parse_plasm_session_short_plan_uri(uri) {
        segment
    } else {
        return Err(RunArtifactResolveError::UnsupportedUri(uri.to_string()));
    };
    logical_uuid_from_uri_segment(&segment).ok_or(RunArtifactResolveError::InvalidSessionRef)
}

pub(crate) fn lookup_from_artifact_uri(
    uri: &str,
    binding: &PlasmExecBinding,
    logical_uuid_str: &str,
) -> Result<RunArtifactLookup, RunArtifactResolveError> {
    if let Some((segment, run_id)) = parse_plasm_session_short_run_uri(uri) {
        let Some(logical_uuid) = logical_uuid_from_uri_segment(&segment) else {
            return Err(RunArtifactResolveError::InvalidSessionRef);
        };
        if logical_uuid.to_string() != logical_uuid_str {
            return Err(RunArtifactResolveError::SessionMismatch);
        }
        return Ok(RunArtifactLookup::CanonicalRun { run_id });
    }
    if let Some((segment, resource_index)) = parse_plasm_session_short_resource_uri(uri) {
        let Some(logical_uuid) = logical_uuid_from_uri_segment(&segment) else {
            return Err(RunArtifactResolveError::InvalidSessionRef);
        };
        if logical_uuid.to_string() != logical_uuid_str {
            return Err(RunArtifactResolveError::SessionMismatch);
        }
        return Err(RunArtifactResolveError::LegacyResourceIndexUri(
            resource_index,
        ));
    }
    let Some((prompt_hash, session_id, run_id)) = parse_plasm_execute_run_uri(uri) else {
        if let Some(idx) = parse_plasm_short_resource_uri(uri) {
            return Err(RunArtifactResolveError::LegacyResourceIndexUri(idx));
        }
        return Err(RunArtifactResolveError::UnsupportedUri(uri.to_string()));
    };
    if prompt_hash != binding.prompt_hash || session_id != binding.session_id {
        return Err(RunArtifactResolveError::SessionMismatch);
    }
    Ok(RunArtifactLookup::CanonicalRun { run_id })
}

pub(crate) fn resolve_lookup_arg(
    _session_ref: &str,
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

async fn fetch_payload_by_run_id(
    plasm: &PlasmHostState,
    binding: &PlasmExecBinding,
    run_id: RunArtifactId,
) -> Result<(ArtifactPayload, Option<u64>), RunArtifactResolveError> {
    let live_sess = plasm
        .get_execute_session(binding.prompt_hash.as_str(), binding.session_id.as_str())
        .await;

    let live_payload = if let Some(ref sess) = live_sess {
        sess.core
            .get_run_artifact(run_id)
            .await
            .map(|a| (a.payload.clone(), Some(a.resource_index)))
    } else {
        None
    };

    if live_payload.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("hot");
    }

    let persisted = if live_payload.is_none() {
        match plasm
            .run_artifacts
            .get_payload_result(&binding.prompt_hash, &binding.session_id, run_id)
            .await
        {
            Ok(v) => v,
            Err(e) => return Err(RunArtifactResolveError::DecodeFailed(e.to_string())),
        }
    } else {
        None
    };

    if live_payload.is_none() && persisted.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("archive");
    }

    let Some((payload, resource_index)) = live_payload.or_else(|| persisted.map(|p| (p, None)))
    else {
        return Err(RunArtifactResolveError::UnknownRunId);
    };

    verify_payload_run_id(&payload, run_id).map_err(|e| match e {
        crate::run_artifacts::RunArtifactError::Integrity(msg) => {
            RunArtifactResolveError::Integrity(msg)
        }
        other => RunArtifactResolveError::DecodeFailed(other.to_string()),
    })?;

    Ok((payload, resource_index))
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
    let RunArtifactLookup::CanonicalRun { run_id } = lookup;

    match fetch_payload_by_run_id(plasm, binding, run_id).await {
        Ok((payload, resolved_index)) => {
            let archive = Some(RunArtifactArchiveRef {
                prompt_hash: binding.prompt_hash.clone(),
                session_id: binding.session_id.clone(),
                run_id: run_id.to_wire(),
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
                run_id: Some(run_id),
                prompt_hash: binding.prompt_hash.clone(),
                session_id: binding.session_id.clone(),
            })
        }
        Err(e) => {
            let (reason, archive) = match &e {
                RunArtifactResolveError::DecodeFailed(_) => {
                    ("decode_failed", archive_for_run_id(binding, run_id))
                }
                RunArtifactResolveError::UnknownRunId => {
                    ("unknown_artifact", archive_for_run_id(binding, run_id))
                }
                RunArtifactResolveError::Integrity(_) => {
                    ("integrity_failed", archive_for_run_id(binding, run_id))
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

pub(crate) async fn resolve_code_plan_for_binding(
    plasm: &PlasmHostState,
    binding: &PlasmExecBinding,
    plan_index: Option<u64>,
    plan_id: Option<uuid::Uuid>,
    ls_key: Option<&str>,
    read_source: Option<&str>,
    started: Instant,
    uri_for_trace: &str,
) -> Result<ResolvedCodePlanArtifact, RunArtifactResolveError> {
    let payload = if let Some(plan_index) = plan_index {
        plasm
            .run_artifacts
            .get_code_plan_payload_result_by_index(
                binding.prompt_hash.as_str(),
                binding.session_id.as_str(),
                plan_index,
            )
            .await
            .map_err(|e| RunArtifactResolveError::DecodeFailed(e.to_string()))?
    } else if let Some(plan_id) = plan_id {
        plasm
            .run_artifacts
            .get_code_plan_payload_result(
                binding.prompt_hash.as_str(),
                binding.session_id.as_str(),
                plan_id,
            )
            .await
            .map_err(|e| RunArtifactResolveError::DecodeFailed(e.to_string()))?
    } else {
        None
    };
    let Some(payload) = payload else {
        resource_read_trace::McpResourceReadTrace::error(
            ls_key,
            read_source,
            started,
            uri_for_trace,
            None,
            "unknown_plan",
        )
        .emit(plasm)
        .await;
        return Err(RunArtifactResolveError::UnknownPlan);
    };
    resource_read_trace::McpResourceReadTrace::success(
        ls_key,
        read_source,
        started,
        uri_for_trace,
        None,
        &payload,
    )
    .emit(plasm)
    .await;
    Ok(ResolvedCodePlanArtifact {
        payload,
        prompt_hash: binding.prompt_hash.clone(),
        session_id: binding.session_id.clone(),
    })
}

fn archive_for_run_id(
    binding: &PlasmExecBinding,
    run_id: RunArtifactId,
) -> Option<RunArtifactArchiveRef> {
    Some(RunArtifactArchiveRef {
        prompt_hash: binding.prompt_hash.clone(),
        session_id: binding.session_id.clone(),
        run_id: run_id.to_wire(),
        resource_index: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_logical_ref::parse_logical_session_wire_ref;
    use crate::run_artifacts::{plasm_session_short_run_uri, RunArtifactId};

    fn sample_run_id(byte: u8) -> RunArtifactId {
        RunArtifactId::from_bytes([byte; 32])
    }

    #[test]
    fn legacy_index_uri_is_rejected() {
        let binding = PlasmExecBinding {
            prompt_hash: "a".repeat(64),
            session_id: "sess".into(),
        };
        let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let ls = parse_logical_session_wire_ref(wire)
            .expect("wire")
            .as_uuid()
            .to_string();
        let uri = format!("plasm://session/{wire}/r/3");
        let err = lookup_from_artifact_uri(&uri, &binding, ls.as_str()).expect_err("legacy");
        assert!(matches!(
            err,
            RunArtifactResolveError::LegacyResourceIndexUri(3)
        ));
    }

    #[test]
    fn multi_step_legacy_index_uris_reject_even_when_session_matches() {
        let binding = PlasmExecBinding {
            prompt_hash: "a".repeat(64),
            session_id: "sess".into(),
        };
        let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let ls = parse_logical_session_wire_ref(wire)
            .expect("wire")
            .as_uuid()
            .to_string();
        for idx in [1u64, 3] {
            let uri = format!("plasm://session/{wire}/r/{idx}");
            let err =
                lookup_from_artifact_uri(&uri, &binding, ls.as_str()).expect_err("legacy index");
            assert!(matches!(
                err,
                RunArtifactResolveError::LegacyResourceIndexUri(i) if i == idx
            ));
        }
    }

    #[test]
    fn short_run_uri_lookup_returns_canonical_run_id() {
        let run_id = sample_run_id(7);
        let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let ls = parse_logical_session_wire_ref(wire)
            .expect("wire")
            .as_uuid()
            .to_string();
        let uri = plasm_session_short_run_uri(wire, &run_id);
        let lookup = lookup_from_artifact_uri(
            &uri,
            &PlasmExecBinding {
                prompt_hash: "a".repeat(64),
                session_id: "sess".into(),
            },
            ls.as_str(),
        )
        .expect("lookup");
        assert!(matches!(
            lookup,
            RunArtifactLookup::CanonicalRun { run_id: rid } if rid == run_id
        ));
    }
}
