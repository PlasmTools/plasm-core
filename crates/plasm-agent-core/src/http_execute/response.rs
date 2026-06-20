//! HTTP execute response shaping and Accept negotiation.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecResponseKind {
    Json,
    Ndjson,
    Table,
    Toon,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ExecuteRunQuery {
    /// When `plan` (case-insensitive), compile/type-check only — no live HTTP side effects (see also `X-Plasm-Run-Mode`).
    #[serde(default)]
    pub mode: Option<String>,
    /// When `false`, start live execute in the background and return `wait(sN_oM)` immediately.
    #[serde(default)]
    pub wait: Option<bool>,
    /// Bypass dry-run **review** soft gate (prefer `plan_commit_ref` from plan dry-run).
    #[serde(default)]
    pub force: Option<bool>,
    /// Plan acceptance token (`pcN`) from a matching plan dry-run.
    #[serde(default)]
    pub plan_commit_ref: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExecuteSessionContextBody {
    /// Optional; when opening intent-scoped teaching table via MCP this is required — HTTP expand may omit when session already has intent.
    #[serde(default)]
    pub intent: Option<String>,
    pub seeds: Vec<CapabilitySeed>,
}

#[derive(Debug, Serialize)]
pub struct ExecuteSessionSymbolsResponse {
    pub prompt_hash: String,
    pub session_id: String,
    pub domain_revision: u32,
    pub entry_id: String,
    pub entities: Vec<String>,
    pub loaded_catalogs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_intent: Option<String>,
    pub entity_symbols: Vec<plasm_core::ExposedEntitySymbolRow>,
}

#[derive(Debug, Serialize)]
pub struct ExecuteSessionStatusResponse {
    pub alive: bool,
    pub prompt_hash: String,
    pub session_id: String,
    pub domain_revision: u32,
    pub entry_id: String,
    pub entities: Vec<String>,
    pub loaded_catalogs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecuteSessionRunsResponse {
    pub prompt_hash: String,
    pub session_id: String,
    /// Runs currently retained in the session hot cache (FIFO); older runs may still be on disk/`RunArtifactStore`.
    pub runs: Vec<SessionRunSummary>,
}

#[derive(Debug)]
pub(crate) enum AcceptNegotiationError {
    NoSupportedMediaType,
}

pub(crate) fn run_mode_is_plan(headers: &HeaderMap, query: &ExecuteRunQuery) -> bool {
    if let Some(raw) = headers
        .get("x-plasm-run-mode")
        .and_then(|v| v.to_str().ok())
    {
        if raw.trim().eq_ignore_ascii_case("plan") {
            return true;
        }
    }
    query
        .mode
        .as_deref()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("plan"))
}

pub(crate) fn attach_plasm_run_headers(
    res: Response,
    artifact: Option<&RunArtifactHandle>,
) -> Response {
    let Some(h) = artifact else {
        return res;
    };
    let (mut parts, body) = res.into_parts();
    let hdr = &mut parts.headers;
    if let Ok(v) = HeaderValue::from_str(&h.run_id.to_wire()) {
        hdr.insert(HeaderName::from_static("x-plasm-run-id"), v);
    }
    if let Ok(v) = HeaderValue::from_str(h.http_path.as_str()) {
        hdr.insert(HeaderName::from_static("x-plasm-artifact-path"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&h.resource_index.to_string()) {
        hdr.insert(HeaderName::from_static("x-plasm-resource-index"), v);
    }
    Response::from_parts(parts, body)
}

pub(crate) fn respond_plan_payload(kind: ExecResponseKind, preview: serde_json::Value) -> Response {
    match kind {
        ExecResponseKind::Json => (
            StatusCode::OK,
            [(CONTENT_TYPE, "application/json; charset=utf-8")],
            Json(preview),
        )
            .into_response(),
        ExecResponseKind::Ndjson => {
            let line = match serde_json::to_string(&preview) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "plan preview serialization failed");
                    return problem_response(
                        Problem::custom(
                            ProblemStatus::INTERNAL_SERVER_ERROR,
                            Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                        )
                        .with_title("Internal Server Error")
                        .with_detail(e.to_string()),
                    );
                }
            };
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/x-ndjson; charset=utf-8")],
                line + "\n",
            )
                .into_response()
        }
        ExecResponseKind::Toon => {
            let s = toon::encode(&preview, None);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "text/toon; charset=utf-8")],
                s,
            )
                .into_response()
        }
        ExecResponseKind::Table => {
            let text = format!("{:#}", preview);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                text,
            )
                .into_response()
        }
    }
}

pub(crate) fn negotiate_accept(
    raw: Option<&str>,
) -> Result<ExecResponseKind, AcceptNegotiationError> {
    let raw = match raw {
        None => return Ok(ExecResponseKind::Toon),
        Some(s) if s.trim().is_empty() => return Ok(ExecResponseKind::Toon),
        Some(s) => s,
    };
    let mut items: Vec<(f32, &str)> = Vec::new();
    for part in raw.split(',') {
        let mut mime = part.trim();
        let mut q = 1.0f32;
        if let Some(idx) = mime.find(';') {
            let (m, rest) = mime.split_at(idx);
            mime = m.trim();
            for p in rest[1..].split(';') {
                let p = p.trim();
                if let Some(qs) = p.strip_prefix("q=") {
                    if let Ok(qv) = qs.parse::<f32>() {
                        q = qv;
                    }
                }
            }
        }
        if q > 0.0 {
            items.push((q, mime));
        }
    }
    items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut saw_specific = false;
    for (_, mime) in &items {
        match *mime {
            "*/*" => return Ok(ExecResponseKind::Toon),
            _ => saw_specific = true,
        }
        match *mime {
            "application/json" => return Ok(ExecResponseKind::Json),
            "application/x-ndjson" | "application/ndjson" | "application/jsonlines" => {
                return Ok(ExecResponseKind::Ndjson);
            }
            "text/plain" => return Ok(ExecResponseKind::Table),
            "text/toon" | "application/x-toon" => return Ok(ExecResponseKind::Toon),
            _ => {}
        }
    }

    if saw_specific {
        Err(AcceptNegotiationError::NoSupportedMediaType)
    } else {
        Ok(ExecResponseKind::Toon)
    }
}

pub(crate) fn respond_execute_result(
    kind: ExecResponseKind,
    json_value: serde_json::Value,
    result: &ExecutionResult,
    response_meta: Option<serde_json::Map<String, serde_json::Value>>,
    cgs: Option<&CGS>,
    artifact: Option<&RunArtifactHandle>,
) -> Response {
    let res = match kind {
        ExecResponseKind::Json => {
            let body = if let Some(meta) = response_meta {
                serde_json::json!({
                    "results": json_value,
                    "_meta": meta,
                })
            } else {
                json_value
            };
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/json; charset=utf-8")],
                Json(body),
            )
                .into_response()
        }
        ExecResponseKind::Ndjson => {
            let line = match serde_json::to_string(&json_value) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "NDJSON response serialization failed");
                    return problem_response(
                        Problem::custom(
                            ProblemStatus::INTERNAL_SERVER_ERROR,
                            Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                        )
                        .with_title("Internal Server Error")
                        .with_detail(e.to_string()),
                    );
                }
            };
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/x-ndjson; charset=utf-8")],
                line + "\n",
            )
                .into_response()
        }
        ExecResponseKind::Table => {
            let (text, _, _) = format_result_with_cgs(result, OutputFormat::Table, cgs);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                text,
            )
                .into_response()
        }
        ExecResponseKind::Toon => {
            let s = toon::encode(&json_value, None);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "text/toon; charset=utf-8")],
                s,
            )
                .into_response()
        }
    };
    attach_plasm_run_headers(res, artifact)
}

pub(crate) fn respond_staged_lines_execute_result(
    kind: ExecResponseKind,
    step_values: Vec<serde_json::Value>,
    step_tables: Option<Vec<String>>,
    response_meta: Option<serde_json::Map<String, serde_json::Value>>,
    artifact: Option<&RunArtifactHandle>,
) -> Response {
    let res = match kind {
        ExecResponseKind::Json => {
            let body = if let Some(meta) = response_meta {
                serde_json::json!({
                    "results": step_values,
                    "_meta": meta,
                })
            } else {
                serde_json::Value::Array(step_values)
            };
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/json; charset=utf-8")],
                Json(body),
            )
                .into_response()
        }
        ExecResponseKind::Ndjson => {
            let mut lines = Vec::with_capacity(step_values.len());
            for step in &step_values {
                let line = match serde_json::to_string(step) {
                    Ok(s) => s,
                    Err(e) => {
                        return problem_response(
                            Problem::custom(
                                ProblemStatus::INTERNAL_SERVER_ERROR,
                                Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                            )
                            .with_title("Internal Server Error")
                            .with_detail(e.to_string()),
                        );
                    }
                };
                lines.push(line);
            }
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "application/x-ndjson; charset=utf-8")],
                lines.join("\n") + "\n",
            )
                .into_response()
        }
        ExecResponseKind::Toon => {
            let body = serde_json::Value::Array(step_values);
            let s = toon::encode(&body, None);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "text/toon; charset=utf-8")],
                s,
            )
                .into_response()
        }
        ExecResponseKind::Table => {
            let Some(tables) = step_tables else {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::INTERNAL_SERVER_ERROR,
                        Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                    )
                    .with_title("Internal Server Error")
                    .with_detail("multi-line table response missing formatted steps"),
                );
            };
            let text = tables.join("\n\n---\n\n");
            (
                StatusCode::OK,
                [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                text,
            )
                .into_response()
        }
    };
    attach_plasm_run_headers(res, artifact)
}

#[allow(dead_code)]
pub(crate) fn execution_failed_response(
    e: &RuntimeError,
    line: &str,
    sess: &ExecuteSession,
    prompt_hash: &PromptHashHex,
    session_id: &ExecuteSessionId,
    step_index0: Option<usize>,
    step_total: usize,
) -> Response {
    let expr_preview = execute_expression_preview(line);
    let entity_names: Vec<String> = sess.cgs.entities.keys().map(|k| k.to_string()).collect();
    let cgs_ctx = format!(
        "CGS context: catalog_entry_id={}; session_entities={:?}; source_expression={expr_preview}; cgs_entity_count={}; cgs_entity_names_sample=[{}]",
        sess.entry_id,
        sess.entities,
        entity_names.len(),
        cgs_entity_names_sample(&entity_names, 24),
    );
    let line_prefix = match step_index0 {
        Some(i) => {
            let line_no = i + 1;
            format!("line {line_no} of {step_total}: ")
        }
        None => String::new(),
    };
    tracing::error!(
        target: "plasm_agent::http_execute",
        error = %e,
        prompt_hash = %prompt_hash,
        session_id = %session_id,
        "expression execution failed"
    );
    tracing::trace!(
        target: "plasm_agent::http_execute",
        source_expression = %expr_preview,
        cgs_ctx = %cgs_ctx,
        "expression execution failed (detail)"
    );
    let detail = format!("{line_prefix}{e}\n\n{cgs_ctx}");
    problem_response(
        Problem::custom(
            ProblemStatus::INTERNAL_SERVER_ERROR,
            Uri::from_static(problem_types::EXECUTE_EXECUTION_FAILED),
        )
        .with_title("Internal Server Error")
        .with_detail(detail),
    )
}

#[allow(dead_code)]
pub(crate) fn plasm_line_step_bad_request(
    step_index: usize,
    total: usize,
    line: &str,
    message: impl Into<String>,
) -> Response {
    let message = message.into();
    let line_no = step_index + 1;
    let detail = format!(
        "line {line_no} of {total}: {message}\nexpression: {}",
        execute_expression_preview(line)
    );
    problem_response(
        Problem::custom(
            ProblemStatus::BAD_REQUEST,
            Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
        )
        .with_title("Bad Request")
        .with_detail(detail),
    )
}
