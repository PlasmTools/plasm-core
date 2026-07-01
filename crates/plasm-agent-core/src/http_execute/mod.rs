//! Execute-session protocol (shared by **Axum** and **MCP**): after [`crate::http_discovery`],
//! clients open a session with `entry_id` + entity seeds, then run one Plasm program.
//!
//! HTTP: `POST /execute` → `GET /execute/:prompt_hash/:session` → `POST` that path (default `Accept`:
//! **text/toon**, entity rows only); optional `GET .../artifacts/:run_id` for run snapshots. MCP uses
//! [`publish_plasm_result_steps`] for live run Markdown + `_meta` / resource links.

mod operations;
pub(crate) use operations::http_operation_trace;
pub use operations::{
    handle_cancel_operation, handle_wait_operation, operation_error_to_string,
    try_dispatch_operation_program,
};

mod deps;
mod wire;

pub(crate) use deps::*;

use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequestParts, Path};
use axum::http::StatusCode;
use axum::response::Response;
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use plasm_core::{PagingHandle, PromptRenderMode, CGS};
use plasm_runtime::ExecutionResult;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
use crate::http_problem_util::{problem_response, problem_types};
use crate::run_artifacts::RunArtifactHandle;
use crate::trace_sink_emit::PlasmTraceContext;

pub(crate) use wire::{
    create_execute_session_response, problem_response_invalid_execute_path,
    wire_execute_session_prompt,
};

/// Validated `/execute/:prompt_hash/:session_id` segments; rejects with RFC 7807 `problem+json`.
pub(crate) struct ExecutePath {
    prompt_hash: PromptHashHex,
    session_id: ExecuteSessionId,
}

fn problem_response_from_path_rejection(rej: PathRejection) -> Response {
    match rej {
        PathRejection::FailedToDeserializePathParams(e) => {
            problem_response_invalid_execute_path(e.status(), e.body_text())
        }
        PathRejection::MissingPathParams(_) => problem_response_invalid_execute_path(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no path parameters found for matched route",
        ),
        _ => problem_response_invalid_execute_path(
            StatusCode::BAD_REQUEST,
            "path parameters could not be extracted",
        ),
    }
}

impl<S> FromRequestParts<S> for ExecutePath
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Path((h, sid)) = Path::<(String, String)>::from_request_parts(parts, state)
            .await
            .map_err(problem_response_from_path_rejection)?;

        let prompt_hash = h.parse::<PromptHashHex>().map_err(|msg| {
            problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `prompt_hash` path segment: {msg}"),
            )
        })?;

        let session_id = sid.parse::<ExecuteSessionId>().map_err(|msg| {
            problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                format!("invalid `session_id` path segment: {msg}"),
            )
        })?;

        Ok(Self {
            prompt_hash,
            session_id,
        })
    }
}

/// Re-export: MCP adaptive preview threshold (Unicode scalars).
pub use crate::mcp_run_markdown::{
    McpResultTransportPolicy, MCP_IN_BAND_ENTITY_ROW_CAP,
    MCP_PLASM_MARKDOWN_PREVIEW_THRESHOLD_CHARS,
};

/// Result of [`publish_plasm_result_steps`] for MCP tool shaping (`_meta` only; snapshot URIs are inline in Markdown).
#[derive(Debug)]
pub struct ExecuteRunToolOutput {
    pub markdown: String,
    pub tool_meta: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone)]
pub struct PublishedResultStep {
    pub name: Option<String>,
    pub node_id: Option<String>,
    pub entry_id: Option<String>,
    pub entity: Option<String>,
    pub cgs: Option<Arc<CGS>>,
    pub display: String,
    pub projection: Option<Vec<String>>,
    pub result: Arc<ExecutionResult>,
    pub artifact: Option<RunArtifactHandle>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateExecuteSessionBody {
    pub entry_id: String,
    pub entities: Vec<String>,
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub logical_session_id: Option<Uuid>,
    #[serde(default)]
    pub context_intent: Option<String>,
    #[serde(default)]
    pub ranked_capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub read_first_seeded_exposure: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilitySeed {
    #[serde(rename = "api", alias = "entry_id")]
    pub entry_id: String,
    pub entity: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateExecuteSessionResponse {
    pub prompt_hash: String,
    pub session: String,
    pub prompt: String,
    pub entry_id: String,
    pub entities: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityWaveOutcome {
    pub mode: String,
    pub entry_id: String,
    pub entities: Vec<String>,
    pub markdown_delta: String,
    pub reused_session: bool,
    pub teaching_prompt_chars_added: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations_delta: Vec<plasm_core::ExposedRelationSymbolRow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyCapabilitySeedsOutcome {
    pub prompt_hash: String,
    pub session_id: String,
    pub primary_entry_id: String,
    pub principal: Option<String>,
    pub waves: Vec<CapabilityWaveOutcome>,
    pub binding_updated: bool,
    pub new_symbol_space: bool,
    pub stale_execute_binding_recovered: bool,
    pub stale_binding_previous: Option<(String, String)>,
    /// Pinned catalog digest changed; prior `e#`/`p#` are void.
    pub symbol_space_reset: bool,
}

/// Maps the parsed `page(...)` handle to the key stored in [`ExecuteSession::paging_resume_by_handle`].
/// MCP (`logical_session_ref` set): namespaced `l_<token>_pgN` only. HTTP: plain `pgN` only.
pub(crate) fn resolve_paging_storage_handle(
    trace: Option<&PlasmTraceContext>,
    handle: &PagingHandle,
) -> Result<PagingHandle, crate::execute_pipeline::RunLineError> {
    let mcp_ref = trace.and_then(|t| t.logical_session_ref.as_deref());
    let s = handle.as_str();
    let is_ns = handle.is_logical_namespaced();
    match (mcp_ref, is_ns) {
        (Some(r), true) => {
            let slot = handle.logical_session_ref().ok_or_else(|| {
                crate::execute_pipeline::RunLineError::Parse(format!(
                    "invalid namespaced paging handle `{s}`"
                ))
            })?;
            if slot != r {
                return Err(crate::execute_pipeline::RunLineError::Parse(format!(
                    "paging handle ref `{slot}` does not match current logical_session_ref `{r}`"
                )));
            }
            Ok(handle.clone())
        }
        (Some(r), false) => Err(crate::execute_pipeline::RunLineError::Parse(format!(
            "MCP requires namespaced paging: use `page({r}_pgN)` from the tool result (plain `{s}` is not valid for MCP `plasm`)"
        ))),
        (None, true) => Err(crate::execute_pipeline::RunLineError::Parse(
            "namespaced paging handles are only for MCP `plasm` with `plasm_context`; use plain `page(pgN)` for HTTP execute"
                .into(),
        )),
        (None, false) => Ok(handle.clone()),
    }
}

mod context;
#[cfg(test)]
pub(crate) use context::execute_session_create_response_inner;
pub(crate) use context::{
    patch_cgs_context_outbound_hosted, patch_cgs_context_resolved_http_backend,
    resolve_http_backend_for_entry,
};
mod ingress;
mod mcp_publish;
mod proof_bind;
mod response;
mod routes;
mod run_line;
mod trace;

pub use crate::execute_pipeline::RunLineError;
pub(crate) use run_line::run_parsed_plasm_line;

pub(crate) use context::replay_teaching_exposure_waves;
pub use context::{
    apply_capability_seeds, execute_session_create_response, expand_execute_teaching_session,
    federate_execute_session, normalize_capability_seeds, resolve_capability_seeds,
    ExpandTeachingWaveResult, RankedCapabilitiesArg,
};
pub(crate) use context::{
    apply_federate_exposure_wave, build_initial_exposure_wave, ExposureCatalogWave,
};
pub(crate) use context::{
    build_capability_exposure_plan, build_plasm_context_agent_markdown,
    build_plasm_context_tool_meta, cgs_entity_names_sample,
    normalize_context_intent_for_domain_filter,
};
pub(crate) use ingress::parse_execute_program_body;
pub(crate) use mcp_publish::{
    publish_plasm_result_steps, publish_with_shared_meta_index, tool_meta_from_handles,
};
pub(crate) use response::ExecuteRunQuery;
pub(crate) use response::{
    negotiate_accept, respond_execute_result, respond_plan_payload,
    respond_staged_lines_execute_result, run_mode_is_plan, AcceptNegotiationError,
    ExecResponseKind,
};
pub use response::{
    ExecuteSessionContextBody, ExecuteSessionRunsResponse, ExecuteSessionStatusResponse,
    ExecuteSessionSymbolsResponse,
};
pub use routes::execute_routes;
pub(crate) use trace::trace_api_entry_id_for_execute_root;
pub use trace::{
    archive_plasm_result_snapshot, execute_plasm_parsed_expr, execute_plasm_plasm_line,
    run_seal_record_for_handle, trace_record_plasm_line,
};
