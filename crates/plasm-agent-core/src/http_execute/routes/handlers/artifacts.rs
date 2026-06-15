//! Run artifacts and evidence.

use super::super::super::*;

pub(crate) async fn get_execute_run_evidence(
    Extension(st): Extension<PlasmHostState>,
    Path((ph, sid, rid)): Path<(String, String, String)>,
) -> Response {
    let prompt_hash = match ph.parse::<PromptHashHex>() {
        Ok(v) => v,
        Err(msg) => {
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `prompt_hash` path segment: {msg}"),
            );
        }
    };
    let session_id = match sid.parse::<ExecuteSessionId>() {
        Ok(v) => v,
        Err(msg) => {
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `session_id` path segment: {msg}"),
            );
        }
    };
    let run_id = match rid.trim().parse::<RunArtifactWire>() {
        Ok(w) => w.0,
        Err(e) => {
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `run_id` path segment: {e}"),
            );
        }
    };
    match st
        .run_artifacts
        .get_evidence_bundle(prompt_hash.as_str(), session_id.as_str(), run_id)
        .await
    {
        Ok(Some(bundle)) => {
            let opts = plasm_evidence::VerifyOptions {
                trusted_public_keys: crate::evidence_chain::trusted_public_keys_from_env(),
            };
            let run_id_wire = run_id.to_wire();
            let artifact_bytes = st
                .run_artifacts
                .get(prompt_hash.as_str(), session_id.as_str(), run_id)
                .await;
            let (artifact_doc, parsed_for_seal) = if let Some(bytes) = artifact_bytes {
                match serde_json::from_slice::<plasm_evidence::RunArtifactForSeal>(&bytes) {
                    Ok(artifact_doc) => {
                        let parsed = artifact_doc.parsed_preimage.clone();
                        (Some(artifact_doc), Some(parsed))
                    }
                    Err(_) => (None, None),
                }
            } else {
                (None, None)
            };
            let artifact_ref = artifact_doc.as_ref();
            let parsed_ref = parsed_for_seal.as_ref();
            if let Err(e) = crate::evidence_chain::verify_evidence_for_http_serve(
                &bundle,
                &opts,
                run_id_wire.as_str(),
                artifact_ref,
                parsed_ref,
            ) {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::UNPROCESSABLE_ENTITY,
                        Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
                    )
                    .with_title("Evidence verification failed")
                    .with_detail(e.to_string()),
                );
            }
            Json(bundle).into_response()
        }
        Ok(None) => problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
            )
            .with_title("Not Found")
            .with_detail("evidence bundle not found for this run_id"),
        ),
        Err(e) => problem_response(
            Problem::custom(
                ProblemStatus::INTERNAL_SERVER_ERROR,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
            )
            .with_title("Evidence decode failed")
            .with_detail(e.to_string()),
        ),
    }
}

pub(crate) async fn get_execute_run_artifact(
    Extension(st): Extension<PlasmHostState>,
    Path((ph, sid, rid)): Path<(String, String, String)>,
) -> Response {
    let started = Instant::now();
    let prompt_hash = match ph.parse::<PromptHashHex>() {
        Ok(v) => v,
        Err(msg) => {
            crate::metrics::record_execute_artifact_serve("error", "bad_path", started.elapsed());
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `prompt_hash` path segment: {msg}"),
            );
        }
    };
    let session_id = match sid.parse::<ExecuteSessionId>() {
        Ok(v) => v,
        Err(msg) => {
            crate::metrics::record_execute_artifact_serve("error", "bad_path", started.elapsed());
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `session_id` path segment: {msg}"),
            );
        }
    };
    let run_id = match rid.trim().parse::<RunArtifactWire>() {
        Ok(w) => w.0,
        Err(e) => {
            crate::metrics::record_execute_artifact_serve("error", "bad_path", started.elapsed());
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `run_id` path segment: {e}"),
            );
        }
    };

    let live_sess = st
        .get_execute_session(prompt_hash.as_str(), session_id.as_str())
        .await;
    let live_payload = if let Some(sess) = &live_sess {
        sess.core
            .get_run_artifact(run_id)
            .await
            .map(|a| a.payload.clone())
    } else {
        None
    };
    if live_payload.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("hot");
    }
    let persisted_payload = if live_payload.is_none() {
        match st
            .run_artifacts
            .get_payload_result(prompt_hash.as_str(), session_id.as_str(), run_id)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                crate::metrics::record_execute_artifact_serve(
                    "error",
                    "decode_failed",
                    started.elapsed(),
                );
                return problem_response(
                    Problem::custom(
                        ProblemStatus::INTERNAL_SERVER_ERROR,
                        Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                    )
                    .with_title("Internal Server Error")
                    .with_detail(format!("run artifact decode failed: {e}")),
                );
            }
        }
    } else {
        None
    };
    if live_payload.is_none() && persisted_payload.is_some() {
        crate::metrics::record_execute_artifact_resolve_layer("archive");
    }
    let Some(payload) = live_payload.or(persisted_payload) else {
        crate::metrics::record_execute_artifact_serve("error", "not_found", started.elapsed());
        return problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_ARTIFACT),
            )
            .with_title("Not Found")
            .with_detail(
                "unknown run artifact for this session (wrong id, expired, or never stored)",
            ),
        );
    };

    let artifact_span = crate::spans::execute_artifact_serve();
    artifact_span.in_scope(|| {
        tracing::info!(
            target: "plasm_agent::http_execute",
            prompt_hash = %prompt_hash.as_str(),
            session_id = %session_id.as_str(),
            run_id = %run_id.to_wire(),
            bytes = payload.bytes.len(),
            "GET execute run artifact"
        );
    });

    let content_type = payload.metadata.content_type;
    let header = axum::http::HeaderValue::from_str(content_type.as_str())
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static("application/octet-stream"));
    crate::metrics::record_execute_artifact_serve("success", "none", started.elapsed());
    (StatusCode::OK, [(CONTENT_TYPE, header)], payload.bytes).into_response()
}
