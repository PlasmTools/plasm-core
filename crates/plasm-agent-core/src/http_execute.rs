//! Execute-session protocol (shared by **Axum** and **MCP**): after [`crate::http_discovery`],
//! clients open a session with `entry_id` + entity seeds, then run one or more Plasm lines.
//!
//! HTTP: `POST /execute` → `GET /execute/:prompt_hash/:session` → `POST` that path (default `Accept`:
//! **text/toon**, entity rows only); optional `GET .../artifacts/:run_id` for run snapshots. MCP uses the same
//! [`execute_session_create_response`] / [`execute_session_run_markdown`] helpers (Markdown + `_meta` / resource links).

use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Extension, FromRequestParts, Path};
use axum::http::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::future::join_all;
use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
use http_problem::Problem;
use indexmap::IndexMap;
use plasm_core::discovery::{CgsCatalog, DiscoveryError};
use plasm_core::error_render::{render_parse_error_with_feedback, FeedbackStyle};
use plasm_core::{
    domain_tsv_table_from_wrapped_prompt,
    expr_parser::{self, ParsedExpr},
    normalize_expr_query_capabilities, normalize_expr_query_capabilities_federated, AuthScheme,
    CgsContext, PagingHandle, PromptRenderMode, SymbolMap, CGS,
};
use plasm_runtime::{
    auth_resolution_mode_from_env, validate_principal_for_mode, AuthResolutionMode, AuthResolver,
    CompileOperationFn, CompileQueryFn, ExecuteOptions, ExecutionResult, ExecutionSource,
    ExecutionStats, GraphCache, QueryPaginationResumeData, RuntimeError, StreamConsumeOpts,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::Instrument;
use uuid::Uuid;

use crate::run_artifacts::{
    artifact_http_path, document_from_run, plasm_run_resource_uri,
    plasm_session_short_resource_uri, plasm_short_resource_uri, plasm_short_resource_uri_logical,
    ArtifactPayload, ArtifactPayloadMetadata, DocumentFromRun, RunArtifactHandle,
};
use crate::trace_hub::{
    trace_id_for_http_execute_session, McpPlasmTraceSink, PlasmLineTraceMeta, TraceEvent,
    TraceSegment,
};
use crate::trace_sink_emit::{McpTraceAuditFields, PlasmTraceContext};

/// Validated `/execute/:prompt_hash/:session_id` segments; rejects with RFC 7807 `problem+json`.
struct ExecutePath {
    prompt_hash: PromptHashHex,
    session_id: ExecuteSessionId,
}

fn problem_response_invalid_execute_path(
    axum_status: StatusCode,
    detail: impl Into<String>,
) -> Response {
    let pstatus = if axum_status == StatusCode::BAD_REQUEST {
        ProblemStatus::BAD_REQUEST
    } else if axum_status == StatusCode::INTERNAL_SERVER_ERROR {
        ProblemStatus::INTERNAL_SERVER_ERROR
    } else {
        ProblemStatus::BAD_REQUEST
    };
    let title = if pstatus == ProblemStatus::INTERNAL_SERVER_ERROR {
        "Internal Server Error"
    } else {
        "Bad Request"
    };
    problem_response(
        Problem::custom(
            pstatus,
            Uri::from_static(problem_types::EXECUTE_INVALID_PATH_PARAM),
        )
        .with_title(title)
        .with_detail(detail.into()),
    )
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

use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
use crate::execute_session::{ExecuteSession, GraphEpoch, SessionReuseKey};
use crate::execute_staging::{
    build_execute_stages, line_may_share_parallel_query_stage, ExecuteStage,
};
use crate::http_problem_util::problem_response;
use crate::http_problem_util::problem_types;
use crate::incoming_auth::{
    incoming_auth_problem, session_allows_principal, tenant_scope, IncomingPrincipal,
};
use crate::mcp_plasm_meta::{plasm_paging_json_value, PlasmMetaIndex, PlasmPagingStepMeta};
use crate::mcp_run_markdown::{
    execute_expression_preview, mcp_compact_markdown_multi_line, mcp_compact_markdown_single,
    mcp_format_execute_result_table_or_tsv, mcp_inline_run_snapshot_line,
    mcp_prepend_artifact_followup_markdown, mcp_preview_markdown_needed,
    merge_snapshot_column_hints, OmittedReferenceOnlyFields,
};
use crate::output::{
    apply_projection, format_result_with_cgs, http_execute_results_value,
    reference_only_omitted_field_names, InBandSummaryReport, LossySummaryFieldNames, OutputFormat,
};
use crate::server_state::PlasmHostState;
use std::collections::BTreeSet;

/// Re-export: MCP adaptive preview threshold (Unicode scalars).
pub use crate::mcp_run_markdown::MCP_PLASM_MARKDOWN_PREVIEW_THRESHOLD_CHARS;

/// Result of [`execute_session_run_markdown`] for MCP tool shaping (`_meta` only; snapshot URIs are inline in Markdown).
#[derive(Debug)]
pub struct ExecuteRunToolOutput {
    pub markdown: String,
    /// `CallToolResult.meta` map (includes `plasm` with `steps` for truncated steps only).
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
    pub result: ExecutionResult,
    pub artifact: Option<RunArtifactHandle>,
}

fn plasm_meta_object(
    handles: &[RunArtifactHandle],
    omitted_from_summary: &[String],
    lossy_per_step: Option<&[LossySummaryFieldNames]>,
    run_step_numbers: Option<&[usize]>,
    paging: Option<&[PlasmPagingStepMeta]>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    if !handles.is_empty() {
        let steps: Vec<serde_json::Value> = handles
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let mut step = serde_json::json!({
                    "run_id": h.run_id.to_string(),
                    "artifact_uri": h.plasm_uri,
                    "canonical_artifact_uri": h.canonical_plasm_uri,
                    "artifact_path": h.http_path,
                    "request_fingerprints": h.request_fingerprints,
                });
                if let Some(rs) = run_step_numbers {
                    if let Some(&run_step) = rs.get(i) {
                        if let Some(obj) = step.as_object_mut() {
                            obj.insert("run_step".into(), serde_json::json!(run_step));
                        }
                    }
                }
                if let Some(ls) = lossy_per_step {
                    if let Some(lossy) = ls.get(i) {
                        if !lossy.is_empty() {
                            if let Some(obj) = step.as_object_mut() {
                                obj.insert(
                                    "lossy_summary_fields".into(),
                                    serde_json::json!(lossy.as_slice()),
                                );
                            }
                        }
                    }
                }
                step
            })
            .collect();
        m.insert("steps".into(), serde_json::Value::Array(steps));
    }
    if !omitted_from_summary.is_empty() {
        m.insert(
            "omitted_from_summary".into(),
            serde_json::json!(omitted_from_summary),
        );
    }
    if let Some(ps) = paging {
        if let Some(v) = plasm_paging_json_value(ps) {
            m.insert("paging".into(), v);
        }
    }
    m
}

/// Maps the parsed `page(...)` handle to the key stored in [`ExecuteSession::paging_resume_by_handle`].
/// MCP (`logical_session_ref` set): namespaced `s0_pgN` only. HTTP: plain `pgN` only.
fn resolve_paging_storage_handle(
    trace: Option<&PlasmTraceContext>,
    handle: &PagingHandle,
) -> Result<PagingHandle, RunLineError> {
    let mcp_slot = trace.and_then(|t| t.logical_session_ref.as_deref());
    let s = handle.as_str();
    let is_ns = handle.is_logical_namespaced();
    match (mcp_slot, is_ns) {
        (Some(r), true) => {
            let slot = handle.logical_session_slot().ok_or_else(|| {
                RunLineError::Parse(format!("invalid namespaced paging handle `{s}`"))
            })?;
            if slot != r {
                return Err(RunLineError::Parse(format!(
                    "paging handle slot `{slot}` does not match current logical_session_ref `{r}`"
                )));
            }
            Ok(handle.clone())
        }
        (Some(r), false) => Err(RunLineError::Parse(format!(
            "MCP requires namespaced paging: use `page({r}_pgN)` from the tool result (plain `{s}` is not valid for MCP `plasm`)"
        ))),
        (None, true) => Err(RunLineError::Parse(
            "namespaced paging handles are only for MCP `plasm` with `plasm_context`; use plain `page(pgN)` for HTTP execute"
                .into(),
        )),
        (None, false) => Ok(handle.clone()),
    }
}

fn paging_followup_handle(parsed: &ParsedExpr, result: &ExecutionResult) -> Option<PagingHandle> {
    if !result.has_more {
        return None;
    }
    match &parsed.expr {
        plasm_core::Expr::Page(p) => Some(p.handle.clone()),
        _ => result.paging_handle.clone(),
    }
}

fn paging_step_meta(
    run_step: usize,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
) -> Option<PlasmPagingStepMeta> {
    let next_page_handle = paging_followup_handle(parsed, result)?;
    Some(PlasmPagingStepMeta::Next {
        run_step,
        returned_count: result.count,
        next_page_handle,
    })
}

fn append_paging_hint_markdown(
    markdown: String,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
) -> String {
    let Some(handle) = paging_followup_handle(parsed, result) else {
        return markdown;
    };
    format!(
        "{markdown}\n\n**More pages available:** use `page({})` for the next page.",
        handle.as_str()
    )
}

fn tool_meta_from_handles(
    handles: &[RunArtifactHandle],
    omitted_from_summary: &[String],
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let plasm = plasm_meta_object(handles, omitted_from_summary, None, None, None);
    if plasm.is_empty() {
        return None;
    }
    let mut meta = serde_json::Map::new();
    meta.insert("plasm".into(), serde_json::Value::Object(plasm));
    Some(meta)
}

fn build_mcp_tool_meta(
    meta_index: Option<&mut PlasmMetaIndex>,
    handles: &[RunArtifactHandle],
    omitted_from_summary: &OmittedReferenceOnlyFields,
    lossy_per_handle: &[LossySummaryFieldNames],
    expr_previews: &[String],
    run_step_numbers: Option<&[usize]>,
    paging: Option<&[PlasmPagingStepMeta]>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    debug_assert!(
        handles.is_empty() || lossy_per_handle.len() == handles.len(),
        "lossy_per_handle must align with handles"
    );
    let lossy_arg = if handles.is_empty() {
        None
    } else {
        Some(lossy_per_handle)
    };
    match meta_index {
        Some(idx) => {
            let (plasm, _desc_ids) = idx.build_plasm_meta(
                handles,
                omitted_from_summary.as_ref(),
                lossy_arg,
                expr_previews,
                run_step_numbers,
                paging,
            );
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".into(), serde_json::Value::Object(plasm));
            Some(meta)
        }
        None => {
            let plasm = plasm_meta_object(
                handles,
                omitted_from_summary.as_ref(),
                lossy_arg,
                run_step_numbers,
                paging,
            );
            if plasm.is_empty() {
                return None;
            }
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".into(), serde_json::Value::Object(plasm));
            Some(meta)
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateExecuteSessionBody {
    pub entry_id: String,
    pub entities: Vec<String>,
    /// When `PLASM_AUTH_RESOLUTION=delegated`, required non-empty string (end-user / tenant id).
    #[serde(default)]
    pub principal: Option<String>,
    /// MCP logical session from `plasm_context` (scopes execute-session reuse + short artifact URIs).
    #[serde(default)]
    pub logical_session_id: Option<Uuid>,
    /// When set (non-empty after trim), DOMAIN symbols use intent-scoped capability exposure (MCP `plasm_context`).
    #[serde(default)]
    pub context_intent: Option<String>,
    /// Optional capability wire names for ranked mutation gating when `context_intent` is set.
    #[serde(default)]
    pub ranked_capabilities: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CapabilitySeed {
    /// Registry catalog id (JSON key `api`; `entry_id` accepted as legacy alias for MCP seeds).
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
    /// True when this response came from [`ExecuteSessionStore::try_reuse_session`] (same `entry_id` +
    /// entity set as an existing non-expired session). MCP clients should omit redundant Plasm instructions
    /// churn when this is set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CapabilityWaveOutcome {
    pub mode: String,
    pub entry_id: String,
    pub entities: Vec<String>,
    pub markdown_delta: String,
    pub reused_session: bool,
    pub domain_prompt_chars_added: u64,
}

#[derive(Clone, Debug)]
pub struct ApplyCapabilitySeedsOutcome {
    pub prompt_hash: String,
    pub session_id: String,
    pub primary_entry_id: String,
    pub principal: Option<String>,
    pub waves: Vec<CapabilityWaveOutcome>,
    pub binding_updated: bool,
    /// When true, this call opened a **new** execute row (new `(prompt_hash, session)` and symbol space).
    /// Any cached `e#` / `m#` / `p#` from a prior `plasm_context` in this **logical** session is void.
    pub new_symbol_space: bool,
    /// MCP still had a `(prompt_hash, session)` pair but the execute store dropped it (TTL / idle).
    /// Caller should finalize the in-memory MCP trace and treat the binding as replaced.
    pub stale_execute_binding_recovered: bool,
    /// If [`Self::stale_execute_binding_recovered`], the `(prompt_hash, session_id)` that was stale.
    pub stale_binding_previous: Option<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecResponseKind {
    Json,
    Ndjson,
    Table,
    Toon,
}

#[derive(Debug)]
enum AcceptNegotiationError {
    NoSupportedMediaType,
}

fn negotiate_accept(raw: Option<&str>) -> Result<ExecResponseKind, AcceptNegotiationError> {
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

/// Sorted, deduplicated entity names for session reuse and incremental expand waves (must match across open/expand).
fn normalize_execute_entity_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

pub fn normalize_capability_seeds(mut seeds: Vec<CapabilitySeed>) -> Vec<CapabilitySeed> {
    for s in &mut seeds {
        s.entry_id = s.entry_id.trim().to_string();
        s.entity = s.entity.trim().to_string();
    }
    seeds.retain(|s| !s.entry_id.is_empty() && !s.entity.is_empty());
    let mut seen = std::collections::HashSet::<(String, String)>::new();
    let mut out = Vec::new();
    for s in seeds {
        let key = (s.entry_id.clone(), s.entity.clone());
        if seen.insert(key) {
            out.push(s);
        }
    }
    out
}

fn group_seed_entities_by_entry(seeds: &[CapabilitySeed]) -> IndexMap<String, Vec<String>> {
    let mut groups: IndexMap<String, Vec<String>> = IndexMap::new();
    for seed in seeds {
        groups
            .entry(seed.entry_id.clone())
            .or_default()
            .push(seed.entity.clone());
    }
    for entities in groups.values_mut() {
        *entities = normalize_execute_entity_names(std::mem::take(entities));
    }
    groups
}

/// Canonical multi-catalog plan: primary catalog (lexicographically first among distinct `entry_id`s)
/// and a **deterministic** processing order (primary first, then every other catalog in sorted order).
/// This removes dependence on the order seeds appear in the `plasm_context` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityExposurePlan {
    pub primary_entry_id: String,
    pub seeds_by_entry: IndexMap<String, Vec<String>>,
    /// Catalog `entry_id`s in order: primary, then non-primary keys sorted lexicographically.
    pub process_order: Vec<String>,
}

pub(crate) fn build_capability_exposure_plan(
    seeds: &[CapabilitySeed],
) -> Option<CapabilityExposurePlan> {
    let seeds_by_entry = group_seed_entities_by_entry(seeds);
    if seeds_by_entry.is_empty() {
        return None;
    }
    let primary_entry_id = primary_entry_id_for_grouped(&seeds_by_entry);
    let process_order = process_order_for_capability_plan(&primary_entry_id, &seeds_by_entry);
    Some(CapabilityExposurePlan {
        primary_entry_id,
        seeds_by_entry,
        process_order,
    })
}

/// Primary first, then all other `entry_id`s in lexicographic order (independent of seed order).
fn process_order_for_capability_plan(
    primary_entry_id: &str,
    grouped: &IndexMap<String, Vec<String>>,
) -> Vec<String> {
    let mut rest: Vec<&str> = grouped
        .keys()
        .map(|k| k.as_str())
        .filter(|k| *k != primary_entry_id)
        .collect();
    rest.sort();
    let mut out = vec![primary_entry_id.to_string()];
    out.extend(rest.iter().map(|s| (*s).to_string()));
    out
}

/// For expand-only waves: every catalog in the request is already loaded; use sorted `entry_id` order.
fn process_order_for_expand_group(grouped: &IndexMap<String, Vec<String>>) -> Vec<String> {
    let mut keys: Vec<String> = grouped.keys().cloned().collect();
    keys.sort();
    keys
}

/// Lexicographically first catalog `entry_id` in the group map.
/// [`SessionReuseKey::entry_id`] and the first-open path must not depend on seed input order
/// (hosts may reorder an equivalent seed set between calls).
fn primary_entry_id_for_grouped(grouped: &IndexMap<String, Vec<String>>) -> String {
    let mut keys: Vec<&str> = grouped.keys().map(|k| k.as_str()).collect();
    keys.sort();
    keys.into_iter()
        .next()
        .expect("grouped non-empty when seeds non-empty")
        .to_string()
}

/// One-line summary for LLM-facing session waves (MCP + stored `prompt_text`); not a Plasm expression.
/// Normalize optional MCP intent for DOMAIN filtering and [`SessionReuseKey::context_intent`].
#[inline]
pub(crate) fn normalize_context_intent_for_domain_filter(raw: Option<&str>) -> Option<String> {
    raw.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

/// MCP `plasm_context` `ranked_capabilities` argument: omitted vs explicit replace/clear.
#[derive(Clone, Debug)]
pub(crate) enum RankedCapabilitiesArg {
    /// Key absent — keep the session's ranked list on expand waves.
    Unspecified,
    /// Key present (`null`, `[]`, or string array) — replace the session list when intent-scoped.
    Set(Option<Vec<String>>),
}

pub(crate) fn normalize_ranked_capabilities_for_gate(
    raw: Option<Vec<String>>,
) -> Option<Vec<String>> {
    let mut v: Vec<String> = raw?
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort();
    v.dedup();
    Some(v)
}

async fn apply_ranked_capabilities_session_update(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    ranked_arg: &RankedCapabilitiesArg,
) -> Result<(), String> {
    let RankedCapabilitiesArg::Set(opt) = ranked_arg else {
        return Ok(());
    };
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let Some(sess_arc) = st.sessions.get(&prompt_hash_p, &session_id_p).await else {
        return Err("unknown or expired execute session".into());
    };
    let mut sess = (*sess_arc).clone();
    if sess.context_intent.is_none() {
        return Ok(());
    }
    sess.ranked_capabilities = normalize_ranked_capabilities_for_gate(opt.clone());
    st.sessions
        .replace_session(&prompt_hash_p, &session_id_p, sess)
        .await;
    Ok(())
}

pub(crate) fn format_plasm_context_wave_line(entry_id: &str, entities: &[String]) -> String {
    let mut v: Vec<String> = entities.to_vec();
    v.sort_unstable();
    format!("Added capabilities from {entry_id}: {}", v.join(", "))
}

pub(crate) const ADD_CAPABILITIES_SESSION_REUSE_HINT: &str =
    "_New types added for this logical_session_ref; previously loaded types remain valid._";

/// Wrap DOMAIN / incremental delta in a Markdown fenced block so MCP and other Markdown UIs
/// preserve newlines (CommonMark collapses single newlines in ordinary paragraphs).
fn wrap_domain_markdown_literal_block(body: &str, render_mode: PromptRenderMode) -> String {
    let t = body.trim_end();
    let fence = render_mode.markdown_fence_info_string();
    format!("```{fence}\n{t}\n```\n")
}

fn cgs_entity_names_sample(names: &[String], max_list: usize) -> String {
    if names.is_empty() {
        return "()".to_string();
    }
    let mut sorted = names.to_vec();
    sorted.sort();
    let show = sorted.len().min(max_list);
    let head = sorted[..show].join(", ");
    if sorted.len() > max_list {
        format!("{head}, … (+{} more)", sorted.len() - max_list)
    } else {
        head
    }
}

const MAX_PLASM_LINES_PER_REQUEST: usize = 64;

/// For tenant MCP: resolve `plasm:outbound:*` keys bound to each catalog `entry_id` via Phoenix tables.
async fn tenant_outbound_hosted_kv_for_entries(
    st: &PlasmHostState,
    cfg: &crate::mcp_runtime_config::McpRuntimeConfig,
    principal_incoming: Option<&crate::incoming_auth::TenantPrincipal>,
    entry_ids: &[String],
) -> HashMap<String, String> {
    let Some(repo) = st.mcp_config_repository() else {
        return HashMap::new();
    };
    let subject_lookup = crate::mcp_config_repository::effective_owner_subject_for_hosted_kv(
        cfg.id,
        cfg.owner_subject.as_deref(),
        principal_incoming.map(|p| p.subject.as_str()),
    );
    let mut out = HashMap::new();
    for eid in entry_ids {
        if !cfg.auth_config_by_entry.contains_key(eid) {
            continue;
        }
        match repo
            .fetch_hosted_kv_for_graph_binding(cfg.id, eid, subject_lookup)
            .await
        {
            Ok(Some(kv)) if !kv.trim().is_empty() => {
                crate::metrics::record_tenant_outbound_hosted_kv_lookup("hit");
                out.insert(eid.clone(), kv);
            }
            Ok(_) => {
                crate::metrics::record_tenant_outbound_hosted_kv_lookup("miss");
                tracing::warn!(
                    target: "plasm_agent::tenant_outbound",
                    config_id = %cfg.id,
                    entry_id = %eid,
                    "MCP auth binding exists but no active connected account with hosted_kv_key (link OAuth/API in the web app)"
                );
            }
            Err(e) => {
                crate::metrics::record_tenant_outbound_hosted_kv_lookup("error");
                tracing::warn!(
                    target: "plasm_agent::tenant_outbound",
                    config_id = %cfg.id,
                    entry_id = %eid,
                    error = %e,
                    "hosted_kv lookup for MCP auth binding failed"
                );
            }
        }
    }
    out
}

fn patch_auth_scheme_for_tenant_hosted(
    auth: Option<&AuthScheme>,
    hosted_kv_key: &str,
) -> AuthScheme {
    let kv = Some(hosted_kv_key.to_string());
    match auth {
        Some(AuthScheme::ApiKeyHeader { header, .. }) => AuthScheme::ApiKeyHeader {
            header: header.clone(),
            env: None,
            hosted_kv: kv,
        },
        Some(AuthScheme::ApiKeyQuery { param, .. }) => AuthScheme::ApiKeyQuery {
            param: param.clone(),
            env: None,
            hosted_kv: kv,
        },
        Some(AuthScheme::BearerToken { .. }) | None => AuthScheme::BearerToken {
            env: None,
            hosted_kv: kv,
        },
        Some(AuthScheme::Oauth2ClientCredentials { .. }) => AuthScheme::BearerToken {
            env: None,
            hosted_kv: kv,
        },
        Some(AuthScheme::None) => AuthScheme::BearerToken {
            env: None,
            hosted_kv: kv,
        },
    }
}

fn patch_cgs_outbound_auth(cgs: &Arc<CGS>, hosted_kv_key: &str) -> Arc<CGS> {
    let mut c = (**cgs).clone();
    c.auth = Some(patch_auth_scheme_for_tenant_hosted(
        c.auth.as_ref(),
        hosted_kv_key,
    ));
    Arc::new(c)
}

fn patch_cgs_context_outbound_hosted(ctx: CgsContext, hosted_kv_key: &str) -> CgsContext {
    let CgsContext { prefix, cgs } = ctx;
    CgsContext::new(prefix, patch_cgs_outbound_auth(&cgs, hosted_kv_key))
}

/// Create an execute session; same validation as `POST /execute`, returns session JSON (MCP / internal).
///
/// Reuses an existing non-expired session when `entry_id` and the sorted entity set match a prior open
/// (same `prompt_hash` / `session` pair), avoiding redundant `render_domain_prompt_bundle` work.
async fn execute_session_create_response_inner(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: CreateExecuteSessionBody,
    allow_reuse: bool,
    outbound_hosted_kv_by_entry: Option<&HashMap<String, String>>,
) -> Result<CreateExecuteSessionResponse, String> {
    if body.entities.is_empty() {
        crate::metrics::record_execute_session_outcome("error", "empty_entities");
        return Err("`entities` must be non-empty".into());
    }

    let mode = auth_resolution_mode_from_env();
    validate_principal_for_mode(mode, body.principal.as_deref()).inspect_err(|_| {
        crate::metrics::record_execute_session_outcome("error", "principal_validation");
    })?;
    let principal_stored: Option<String> = match mode {
        AuthResolutionMode::Env => None,
        AuthResolutionMode::Delegated => body.principal.as_ref().map(|s| s.trim().to_string()),
    };

    let names = normalize_execute_entity_names(body.entities);

    let reg = st.catalog.snapshot();
    let mut ctx = match reg.load_context(&body.entry_id) {
        Ok(c) => c,
        Err(DiscoveryError::UnknownEntry(id)) => {
            crate::metrics::record_execute_session_outcome("error", "unknown_entry");
            return Err(format!("unknown catalog entry: {id}"));
        }
        Err(e) => {
            crate::metrics::record_execute_session_outcome("error", "discovery");
            return Err(e.to_string());
        }
    };
    if let Some(map) = outbound_hosted_kv_by_entry {
        if let Some(kv) = map.get(&body.entry_id) {
            ctx = patch_cgs_context_outbound_hosted(ctx, kv);
        }
    }
    let ctx_arc = Arc::new(ctx);
    let catalog_cgs_hash = ctx_arc.cgs.catalog_cgs_hash_hex();

    let plugin_generation = st
        .plugin_manager
        .as_ref()
        .and_then(|m| m.current_generation());
    let plugin_generation_id = plugin_generation.as_ref().map(|g| g.id);

    let scope = tenant_scope(principal);
    let subj = principal.map(|p| p.subject.clone()).unwrap_or_default();

    let domain_filter_intent =
        normalize_context_intent_for_domain_filter(body.context_intent.as_deref());

    let ranked_for_domain = domain_filter_intent
        .as_ref()
        .and_then(|_| normalize_ranked_capabilities_for_gate(body.ranked_capabilities.clone()));

    let reuse_key = SessionReuseKey {
        tenant_scope: scope.clone(),
        entry_id: body.entry_id.clone(),
        catalog_cgs_hash: catalog_cgs_hash.clone(),
        entities: names.clone(),
        context_intent: domain_filter_intent.clone(),
        ranked_capabilities: ranked_for_domain.clone(),
        principal: principal_stored.clone(),
        plugin_generation_id,
        logical_session_id: body.logical_session_id.map(|u| u.hyphenated().to_string()),
    };

    if allow_reuse {
        if let Some((session_id_str, sess)) = st.sessions.try_reuse_session(&reuse_key).await {
            let _reuse = crate::spans::execute_session_reuse(
                reuse_key.entry_id.as_str(),
                reuse_key.catalog_cgs_hash.as_str(),
                sess.prompt_hash.as_str(),
                session_id_str.as_str(),
            )
            .entered();
            tracing::info!(
                entry_id = %reuse_key.entry_id,
                entities = ?reuse_key.entities,
                catalog_cgs_hash = %reuse_key.catalog_cgs_hash,
                prompt_hash = %sess.prompt_hash,
                session = %session_id_str,
                "reusing execute session (same entry_id + entities + catalog hash)"
            );
            crate::metrics::record_execute_session_outcome("reuse", "");
            return Ok(CreateExecuteSessionResponse {
                prompt_hash: sess.prompt_hash.clone(),
                session: session_id_str,
                prompt: sess.prompt_text.clone(),
                entry_id: sess.entry_id.clone(),
                entities: sess.entities.clone(),
                reused: true,
                principal: sess.principal.clone(),
            });
        }
    }

    let mut contexts_by_entry = IndexMap::new();
    contexts_by_entry.insert(body.entry_id.clone(), ctx_arc.clone());

    let cgs: Arc<CGS> = ctx_arc.cgs.clone();
    for e in &names {
        if cgs.get_entity(e).is_none() {
            crate::metrics::record_execute_session_outcome("error", "unknown_entity");
            return Err(format!("unknown entity `{e}` in this schema"));
        }
    }

    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let domain_exposure = match &domain_filter_intent {
        Some(intent_s) => {
            let mut relation_endpoints = names.clone();
            relation_endpoints.sort_unstable();
            relation_endpoints.dedup();
            let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
                cgs.as_ref(),
                body.entry_id.as_str(),
                intent_s.as_str(),
                &relation_endpoints,
                &names,
                ranked_for_domain.as_deref(),
            );
            plasm_core::DomainExposureSession::new_with_intent_delta(
                cgs.as_ref(),
                body.entry_id.as_str(),
                &refs,
                delta,
            )
        }
        None => plasm_core::DomainExposureSession::new(cgs.as_ref(), body.entry_id.as_str(), &refs),
    };
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let domain_prompt = st
        .engine
        .prompt_pipeline()
        .render_domain_first_wave_for_session(cgs.as_ref(), &domain_exposure, Some(sym_cross));
    let prompt =
        wrap_domain_markdown_literal_block(&domain_prompt, st.engine.prompt_pipeline().render_mode);
    let prompt_hash = PromptHashHex::from_prompt_sha256(&prompt);
    let session_id = ExecuteSessionId::new_random();
    let prompt_hash_str = prompt_hash.to_string();
    let session_id_str = session_id.to_string();

    let create_span = crate::spans::execute_session_create();
    tracing::debug!(
        tenant_scope = %scope,
        principal = %subj,
        entry_id = %body.entry_id,
        "execute session created"
    );

    let session = ExecuteSession::new(
        prompt_hash_str.clone(),
        prompt.clone(),
        cgs,
        contexts_by_entry,
        body.entry_id.clone(),
        scope,
        subj,
        Some(ctx_arc.cgs.http_backend.clone()),
        names.clone(),
        Some(domain_exposure),
        principal_stored.clone(),
        plugin_generation,
        catalog_cgs_hash,
        domain_filter_intent,
        ranked_for_domain,
    );
    st.sessions
        .insert(
            reuse_key,
            prompt_hash_str.clone(),
            session_id_str.clone(),
            session,
        )
        .instrument(create_span)
        .await;

    crate::metrics::record_execute_session_outcome("create", "");
    Ok(CreateExecuteSessionResponse {
        prompt_hash: prompt_hash_str,
        session: session_id_str,
        prompt,
        entry_id: body.entry_id,
        entities: names,
        reused: false,
        principal: principal_stored,
    })
}

pub async fn execute_session_create_response(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: CreateExecuteSessionBody,
) -> Result<CreateExecuteSessionResponse, String> {
    execute_session_create_response_inner(st, principal, body, true, None).await
}

/// Append another registry row’s [`plasm_core::CgsContext`] to an existing execute session (same
/// `prompt_hash` / `session`); monotonic `e#` / `m#` / `p#` via [`plasm_core::DomainExposureSession`].
pub async fn federate_execute_session(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    new_entry_id: String,
    entities: Vec<String>,
    principal: Option<String>,
    outbound_hosted_kv_by_entry: Option<&HashMap<String, String>>,
) -> Result<CapabilityWaveOutcome, String> {
    let mode = auth_resolution_mode_from_env();
    validate_principal_for_mode(mode, principal.as_deref())?;

    let names = normalize_execute_entity_names(entities);
    if names.is_empty() {
        return Err("`entities` must be non-empty".into());
    }

    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| e.to_string())?;

    let Some(sess_arc) = st.sessions.get(&prompt_hash_p, &session_id_p).await else {
        return Err("unknown or expired execute session".into());
    };
    let mut sess = (*sess_arc).clone();
    let scope_intent = sess.context_intent.clone();
    let ranked_slice = sess.ranked_capabilities.as_deref();

    if sess.contexts_by_entry.contains_key(&new_entry_id) {
        return Err(format!(
            "session already includes catalog entry `{new_entry_id}`"
        ));
    }

    let reg = st.catalog.snapshot();
    let mut ctx = match reg.load_context(&new_entry_id) {
        Ok(c) => c,
        Err(DiscoveryError::UnknownEntry(id)) => {
            return Err(format!("unknown catalog entry: {id}"));
        }
        Err(e) => return Err(e.to_string()),
    };
    if let Some(map) = outbound_hosted_kv_by_entry {
        if let Some(kv) = map.get(&new_entry_id) {
            ctx = patch_cgs_context_outbound_hosted(ctx, kv);
        }
    }
    let ctx_arc = Arc::new(ctx);

    for e in &names {
        if ctx_arc.get_entity(e).is_none() {
            return Err(format!("unknown entity `{e}` in this schema"));
        }
    }

    sess.contexts_by_entry
        .insert(new_entry_id.clone(), ctx_arc.clone());

    let Some(mut exp) = sess.domain_exposure.take() else {
        return Err("session has no incremental exposure state".into());
    };

    let layers: Vec<&CGS> = sess
        .contexts_by_entry
        .values()
        .map(|c| c.cgs.as_ref())
        .collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let n0 = exp.entities.len();
    if let Some(ref intent_s) = scope_intent {
        let mut relation_endpoints = sess.entities.clone();
        relation_endpoints.extend(names.iter().cloned());
        relation_endpoints.sort_unstable();
        relation_endpoints.dedup();
        let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
            ctx_arc.cgs.as_ref(),
            new_entry_id.as_str(),
            intent_s.as_str(),
            &relation_endpoints,
            &names,
            ranked_slice,
        );
        exp.expose_surface(
            &layers,
            ctx_arc.cgs.clone(),
            new_entry_id.as_str(),
            &refs,
            delta,
        );
    } else {
        exp.expose_entities(&layers, ctx_arc.cgs.clone(), new_entry_id.as_str(), &refs);
    }
    let added: Vec<&str> = exp.entities[n0..].iter().map(|s| s.as_str()).collect();

    if added.is_empty() {
        sess.domain_exposure = Some(exp);
        st.sessions
            .replace_session(&prompt_hash_p, &session_id_p, sess)
            .await;
        return Ok(CapabilityWaveOutcome {
            mode: "federate".to_string(),
            entry_id: new_entry_id,
            entities: names,
            markdown_delta: "_No new entities in this federated wave (already exposed)._"
                .to_string(),
            reused_session: true,
            domain_prompt_chars_added: 0,
        });
    }

    let by_entry: IndexMap<String, &CGS> = sess
        .contexts_by_entry
        .iter()
        .map(|(k, v)| (k.clone(), v.cgs.as_ref()))
        .collect();
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let delta = st
        .engine
        .prompt_pipeline()
        .render_domain_exposure_delta_federated(&by_entry, &exp, &added, Some(sym_cross));
    let mut names_sorted = names.clone();
    names_sorted.sort_unstable();
    let mut wave = String::new();
    wave.push_str("\n\n");
    wave.push_str(&format_plasm_context_wave_line(
        new_entry_id.as_str(),
        &names_sorted,
    ));
    wave.push_str("\n\n");
    wave.push_str(&wrap_domain_markdown_literal_block(
        &delta,
        st.engine.prompt_pipeline().render_mode,
    ));
    sess.prompt_text.push_str(&wave);
    sess.entities = exp.entities.clone();
    sess.domain_exposure = Some(exp);
    sess.domain_revision = sess.domain_revision.saturating_add(1);
    st.sessions
        .replace_session(&prompt_hash_p, &session_id_p, sess)
        .await;

    Ok(CapabilityWaveOutcome {
        mode: "federate".to_string(),
        entry_id: new_entry_id,
        entities: names,
        markdown_delta: wave.clone(),
        reused_session: false,
        domain_prompt_chars_added: wave.chars().count() as u64,
    })
}

/// Markdown reminder after an expand wave.
///
/// - **Delta wave** (`noop_expand == false`): new teaching-table rows are in **this** message—point the model at them.
/// - **No-op expand** (`noop_expand == true`): we intentionally **do not** replay the full TSV teaching table to save
///   tokens; remind the model that symbols still apply and steady state is `plasm` with `logical_session_ref`.
fn expand_session_symbol_reminder(n: usize, noop_expand: bool) -> String {
    if noop_expand {
        if n == 0 {
            return "_No exposed entities in this session yet._\n".to_string();
        }
        return format!(
            "_Symbols `e1`…`e{n}` are unchanged. Full Plasm teaching-table / TSV text is **not** repeated in this response (token-saving). Keep using your **`logical_session_ref`** with **`plasm`** / **`plasm_run`**; rely on the teaching table from the prior `plasm_context` open or append wave in chat history._\n",
            n = n
        );
    }
    if n == 0 {
        "_Follow the TSV teaching table in **this** `plasm_context` response for valid `e#` / `m#` / `p#` shapes._\n"
            .to_string()
    } else {
        format!(
            "_Symbols `e1`…`e{n}` are append-only for this logical session. Use them with the new teaching-table rows in **this** response._\n",
            n = n
        )
    }
}

/// Append expand-wave Plasm instruction blocks for more entity names; [`DomainExposureSession`] keeps `e#`/`m#`/`p#` stable.
pub async fn expand_execute_domain_session(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    prompt_hash: &str,
    session_id: &str,
    seeds: Vec<CapabilitySeed>,
) -> Result<String, String> {
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err("`seeds` must be non-empty".into());
    }
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| e.to_string())?;

    let Some(sess_arc) = st.sessions.get(&prompt_hash_p, &session_id_p).await else {
        return Err("unknown or expired execute session".into());
    };
    let mut sess = (*sess_arc).clone();
    if !session_allows_principal(&sess, principal) {
        return Err("forbidden: execute session tenant does not match caller".into());
    }
    let scope_intent = sess.context_intent.clone();
    let ranked_slice = sess.ranked_capabilities.as_deref();
    let Some(mut exp) = sess.domain_exposure.take() else {
        return Err("session has no incremental exposure state".into());
    };

    let layers: Vec<&CGS> = sess
        .contexts_by_entry
        .values()
        .map(|c| c.cgs.as_ref())
        .collect();
    let n0 = exp.entities.len();
    let mut groups: IndexMap<String, Vec<String>> = IndexMap::new();
    for seed in &seeds {
        let Some(ctx) = sess.contexts_by_entry.get(&seed.entry_id) else {
            return Err(format!(
                "unknown catalog entry `{}` in loaded session schemas",
                seed.entry_id
            ));
        };
        if ctx.get_entity(&seed.entity).is_none() {
            return Err(format!(
                "unknown entity `{}` in catalog `{}`",
                seed.entity, seed.entry_id
            ));
        }
        groups
            .entry(seed.entry_id.clone())
            .or_default()
            .push(seed.entity.clone());
    }
    let mut relation_base: Vec<String> = sess.entities.clone();
    for ents in groups.values() {
        for e in ents {
            if !relation_base.contains(e) {
                relation_base.push(e.clone());
            }
        }
    }
    relation_base.sort_unstable();
    relation_base.dedup();

    let eid_order = process_order_for_expand_group(&groups);
    let add_lines: Vec<String> = eid_order
        .iter()
        .map(|eid| {
            let ents = groups.get(eid).expect("eid in order is from groups");
            format_plasm_context_wave_line(eid, &normalize_execute_entity_names(ents.clone()))
        })
        .collect();
    for eid in eid_order {
        let Some(ctx) = sess.contexts_by_entry.get(&eid) else {
            return Err(format!(
                "unknown catalog entry `{eid}` in loaded session schemas"
            ));
        };
        let group = groups
            .get(&eid)
            .ok_or_else(|| format!("internal error: missing seed group for `{eid}`"))?
            .clone();
        let normalized = normalize_execute_entity_names(group);
        let refs: Vec<&str> = normalized.iter().map(|s| s.as_str()).collect();
        if let Some(ref intent_s) = scope_intent {
            let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
                ctx.cgs.as_ref(),
                eid.as_str(),
                intent_s.as_str(),
                &relation_base,
                &normalized,
                ranked_slice,
            );
            exp.expose_surface(&layers, ctx.cgs.clone(), eid.as_str(), &refs, delta);
        } else {
            exp.expose_entities(&layers, ctx.cgs.clone(), eid.as_str(), &refs);
        }
    }
    let added: Vec<&str> = exp.entities[n0..].iter().map(|s| s.as_str()).collect();

    let n_total = exp.entities.len();

    if added.is_empty() {
        sess.entities = exp.entities.clone();
        sess.domain_exposure = Some(exp);
        st.sessions
            .replace_session(&prompt_hash_p, &session_id_p, sess)
            .await;
        let mut out = String::from("_No new entities in this wave (already exposed)._\n\n");
        out.push_str(&expand_session_symbol_reminder(n_total, true));
        return Ok(out);
    }

    let cgs_primary = sess.cgs.as_ref();
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let delta = if sess.contexts_by_entry.len() > 1 {
        let by_entry: IndexMap<String, &CGS> = sess
            .contexts_by_entry
            .iter()
            .map(|(k, v)| (k.clone(), v.cgs.as_ref()))
            .collect();
        st.engine
            .prompt_pipeline()
            .render_domain_exposure_delta_federated(&by_entry, &exp, &added, Some(sym_cross))
    } else {
        st.engine.prompt_pipeline().render_domain_exposure_delta(
            cgs_primary,
            &exp,
            &added,
            Some(sym_cross),
        )
    };
    let mut wave = String::new();
    wave.push_str("\n\n");
    if !add_lines.is_empty() {
        wave.push_str(&add_lines.join("\n"));
        wave.push_str("\n\n");
    }
    wave.push_str(&expand_session_symbol_reminder(n_total, false));
    wave.push_str("\n\n");
    wave.push_str(&wrap_domain_markdown_literal_block(
        &delta,
        st.engine.prompt_pipeline().render_mode,
    ));
    sess.prompt_text.push_str(&wave);
    sess.entities = exp.entities.clone();
    sess.domain_exposure = Some(exp);
    sess.domain_revision = sess.domain_revision.saturating_add(1);
    st.sessions
        .replace_session(&prompt_hash_p, &session_id_p, sess)
        .await;
    Ok(wave)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_capability_seeds(
    st: &PlasmHostState,
    principal_incoming: Option<&crate::incoming_auth::TenantPrincipal>,
    binding: Option<(&str, &str)>,
    seeds: Vec<CapabilitySeed>,
    principal: Option<String>,
    tenant_mcp_cfg: Option<Arc<crate::mcp_runtime_config::McpRuntimeConfig>>,
    logical_session_id: Option<Uuid>,
    plasm_context_intent: &str,
    ranked_capabilities: RankedCapabilitiesArg,
) -> Result<ApplyCapabilitySeedsOutcome, String> {
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err("`seeds` must be non-empty".into());
    }

    // MCP `PlasmExecBinding` can outlive the in-memory [`ExecuteSessionStore`] row (idle expiry).
    // Treat a binding as absent so we open a fresh execute session instead of failing federate/expand.
    let mut stale_execute_binding_recovered = false;
    let mut stale_binding_previous: Option<(String, String)> = None;
    let had_binding = binding.is_some();
    let binding = match binding {
        None => None,
        Some((ph, sid)) => {
            if st.sessions.get_by_strs(ph, sid).await.is_some() {
                Some((ph, sid))
            } else {
                stale_execute_binding_recovered = true;
                stale_binding_previous = Some((ph.to_string(), sid.to_string()));
                tracing::info!(
                    target: "plasm_agent::http_execute",
                    prompt_hash = %ph,
                    session_id = %sid,
                    "apply_capability_seeds: MCP execute binding stale (session missing or expired); opening fresh execute session"
                );
                None
            }
        }
    };

    let plan = build_capability_exposure_plan(&seeds)
        .ok_or_else(|| "internal error: empty capability exposure plan".to_string())?;
    let primary_entry_id = plan.primary_entry_id.clone();

    let mut all_eids: Vec<String> = plan.seeds_by_entry.keys().cloned().collect();
    all_eids.sort();
    let outbound_map_storage = if let Some(ref cfg) = tenant_mcp_cfg {
        Some(
            tenant_outbound_hosted_kv_for_entries(st, cfg.as_ref(), principal_incoming, &all_eids)
                .await,
        )
    } else {
        None
    };
    let outbound_ref = outbound_map_storage.as_ref();

    let mut waves = Vec::new();
    let mut new_symbol_space = false;
    let (prompt_hash, session_id, binding_updated) = match binding {
        None => {
            let primary_entities = plan
                .seeds_by_entry
                .get(&primary_entry_id)
                .cloned()
                .ok_or_else(|| "missing primary entities".to_string())?;
            let created = execute_session_create_response_inner(
                st,
                principal_incoming,
                CreateExecuteSessionBody {
                    entry_id: primary_entry_id.clone(),
                    entities: primary_entities.clone(),
                    principal: principal.clone(),
                    logical_session_id,
                    context_intent: normalize_context_intent_for_domain_filter(Some(
                        plasm_context_intent,
                    )),
                    ranked_capabilities: match &ranked_capabilities {
                        RankedCapabilitiesArg::Unspecified => None,
                        RankedCapabilitiesArg::Set(opt) => opt.clone(),
                    },
                },
                plan.seeds_by_entry.len() <= 1,
                outbound_ref,
            )
            .await?;
            new_symbol_space = !created.reused;
            let mut open_md = String::new();
            if stale_execute_binding_recovered {
                open_md.push_str(
                    "**Prior Plasm symbol table is void.** The in-memory execute session for this logical handle was missing or expired. A new `(prompt_hash, session)` was opened — **discard** any cached `e#` / `m#` / `p#` or prior teaching-table text from earlier `plasm_context` output in this chat. Re-read the teaching table from this response only. Monotonic `e#` / `m#` / `p#` apply to the **new** session.\n\n",
                );
            }
            open_md.push_str(&format_plasm_context_wave_line(
                &created.entry_id,
                &primary_entities,
            ));
            open_md.push_str("\n\n");
            open_md.push_str(ADD_CAPABILITIES_SESSION_REUSE_HINT);
            if created.reused {
                open_md.push_str("\n\nSession unchanged.");
            }
            // First attach after binding resolution (`binding == None`): restate the current teaching-table snapshot
            // from the execute session prompt so the client receives authoritative `e#` / `m#` / `p#` for this open.
            // (Later expand waves with no new entities omit full teaching-table replay—see `expand_execute_domain_session`.)
            let mode = st.engine.prompt_pipeline().render_mode;
            if mode.is_tsv() {
                if let Some(body_tsv) = domain_tsv_table_from_wrapped_prompt(
                    &created.prompt,
                    mode.markdown_fence_info_string(),
                ) {
                    let wrapped = wrap_domain_markdown_literal_block(&body_tsv, mode);
                    open_md.push_str("\n\n");
                    open_md.push_str(&wrapped);
                } else {
                    open_md.push_str("\n\n");
                    open_md.push_str(&created.prompt);
                }
            } else {
                open_md.push_str("\n\n");
                open_md.push_str(&created.prompt);
            }
            let domain_prompt_chars_added = open_md.chars().count() as u64;
            waves.push(CapabilityWaveOutcome {
                mode: "open".to_string(),
                entry_id: created.entry_id.clone(),
                entities: primary_entities,
                markdown_delta: open_md.clone(),
                reused_session: created.reused,
                domain_prompt_chars_added,
            });
            (created.prompt_hash, created.session, true)
        }
        Some((ph, sid)) => (ph.to_string(), sid.to_string(), false),
    };

    if had_binding {
        apply_ranked_capabilities_session_update(
            st,
            prompt_hash.as_str(),
            session_id.as_str(),
            &ranked_capabilities,
        )
        .await?;
    }

    for eid in &plan.process_order {
        if *eid == primary_entry_id && binding.is_none() {
            continue;
        }
        let Some(entities) = plan.seeds_by_entry.get(eid) else {
            continue;
        };
        let has_session_entry = st
            .sessions
            .get_by_strs(&prompt_hash, &session_id)
            .await
            .map(|s| s.contexts_by_entry.contains_key(eid))
            .unwrap_or(false);
        if !has_session_entry {
            let wave = federate_execute_session(
                st,
                prompt_hash.as_str(),
                session_id.as_str(),
                eid.clone(),
                entities.clone(),
                principal.clone(),
                outbound_ref,
            )
            .await?;
            waves.push(wave);
        } else {
            let md = expand_execute_domain_session(
                st,
                principal_incoming,
                prompt_hash.as_str(),
                session_id.as_str(),
                entities
                    .iter()
                    .map(|e| CapabilitySeed {
                        entry_id: eid.clone(),
                        entity: e.clone(),
                    })
                    .collect(),
            )
            .await?;
            waves.push(CapabilityWaveOutcome {
                mode: "expand".to_string(),
                entry_id: eid.clone(),
                entities: entities.clone(),
                domain_prompt_chars_added: md.chars().count() as u64,
                markdown_delta: md,
                reused_session: false,
            });
        }
    }

    Ok(ApplyCapabilitySeedsOutcome {
        prompt_hash,
        session_id,
        primary_entry_id,
        principal,
        waves,
        binding_updated,
        new_symbol_space,
        stale_execute_binding_recovered,
        stale_binding_previous,
    })
}

fn trace_expr_api_meta(expr: &plasm_core::Expr) -> (Option<String>, String) {
    use plasm_core::Expr;
    match expr {
        Expr::Query(q) => (
            q.capability_name.as_ref().map(|c| c.as_str().to_string()),
            "query".to_string(),
        ),
        Expr::Invoke(i) => (
            Some(i.capability.as_str().to_string()),
            "invoke".to_string(),
        ),
        Expr::Get(_) => (None, "get".to_string()),
        Expr::Create(c) => (
            Some(c.capability.as_str().to_string()),
            "create".to_string(),
        ),
        Expr::Delete(d) => (
            Some(d.capability.as_str().to_string()),
            "delete".to_string(),
        ),
        Expr::Chain(_) => (None, "chain".to_string()),
        Expr::Page(p) => (None, format!("page {}", p.handle)),
    }
}

fn trace_api_entry_id_for_execute_root(sess: &ExecuteSession, root_entity: &str) -> String {
    if let Some(fed) = sess.federation_dispatch() {
        fed.qualified_entity_for_exposed_entity(root_entity)
            .map(|qk| qk.catalog_entry_id)
            .unwrap_or_else(|| sess.entry_id.clone())
    } else {
        sess.entry_id.clone()
    }
}

fn trace_api_entry_id_for_parsed_line(sess: &ExecuteSession, parsed: &ParsedExpr) -> String {
    match &parsed.expr {
        plasm_core::Expr::Page(p) => sess
            .peek_paging_resume(&p.handle)
            .map(|r| trace_api_entry_id_for_execute_root(sess, r.query.entity.as_str()))
            .unwrap_or_else(|| sess.entry_id.clone()),
        _ => trace_api_entry_id_for_execute_root(sess, parsed.expr.primary_entity()),
    }
}

fn plasm_line_trace_meta(
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    api_entry_id: Option<String>,
) -> PlasmLineTraceMeta {
    let (capability, operation) = trace_expr_api_meta(&parsed.expr);
    let mut repl_pre = String::from("→ ");
    repl_pre.push_str(&crate::expr_display::expr_display(&parsed.expr));
    if let Some(ref proj) = parsed.projection {
        if !proj.is_empty() {
            repl_pre.push_str(&format!("\n  projection: [{}]", proj.join(", ")));
        }
    }
    let repl_post = format!(
        "{} results · {:?} · {}ms · net {} · cache {}/{}",
        result.count,
        result.source,
        result.stats.duration_ms,
        result.stats.network_requests,
        result.stats.cache_hits,
        result.stats.cache_misses
    );
    PlasmLineTraceMeta {
        source_expression: line.to_string(),
        repl_pre,
        repl_post,
        capability,
        operation,
        api_entry_id,
    }
}

async fn trace_emit_plasm_line(
    sink: &McpPlasmTraceSink,
    line_index: usize,
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    sess: &ExecuteSession,
) {
    let api = Some(trace_api_entry_id_for_parsed_line(sess, parsed));
    let meta = plasm_line_trace_meta(line, parsed, result, api);
    sink.hub
        .trace_add_plasm_line(
            &sink.mcp_key,
            sink.call_index,
            line_index,
            meta,
            result,
            vec![],
        )
        .await;
}

/// Run one or more Plasm lines; Markdown for tools plus MCP `_meta` / resource link metadata.
///
/// When `meta_index` is [`Some`], `_meta.plasm` uses compact `dict_ref` + `index_delta`, and large
/// markdown may be replaced by a preview (see [`MCP_PLASM_MARKDOWN_PREVIEW_THRESHOLD_CHARS`]).
#[allow(clippy::too_many_arguments)]
pub async fn execute_session_run_markdown(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    prompt_hash: &str,
    session_id: &str,
    expressions: Vec<String>,
    meta_index: Option<&mut PlasmMetaIndex>,
    trace: Option<PlasmTraceContext>,
    hub_sink: Option<McpPlasmTraceSink>,
) -> Result<ExecuteRunToolOutput, String> {
    let prompt_hash: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let session_id: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| e.to_string())?;

    let Some(sess) = st.sessions.get(&prompt_hash, &session_id).await else {
        return Err("unknown or expired execute session".into());
    };

    if !session_allows_principal(&sess, principal) {
        return Err("forbidden: execute session tenant does not match caller".into());
    }

    if expressions.is_empty() {
        return Err(
            "no lines to run: provide a non-empty line, a JSON array of line strings, or JSON {\"lines\":[\"...\"]}"
                .into(),
        );
    }
    if expressions.len() > MAX_PLASM_LINES_PER_REQUEST {
        return Err(format!(
            "too many lines in one request (max {MAX_PLASM_LINES_PER_REQUEST}, got {})",
            expressions.len()
        ));
    }

    let multiple_lines = expressions.len() > 1;

    if !multiple_lines {
        let line = expressions[0].as_str();
        let mut cache = sess.graph_cache.lock().await;
        match run_single_plasm_line(
            line,
            &sess,
            st,
            &mut cache,
            session_id.as_str(),
            trace.as_ref(),
            0,
        )
        .await
        {
            Ok((parsed, result, artifact)) => {
                if let Some(ref sink) = hub_sink {
                    let api = Some(trace_api_entry_id_for_parsed_line(&sess, &parsed));
                    let meta = plasm_line_trace_meta(line, &parsed, &result, api);
                    sink.hub
                        .trace_add_plasm_line(
                            &sink.mcp_key,
                            sink.call_index,
                            0,
                            meta,
                            &result,
                            vec![],
                        )
                        .await;
                }
                let mut out = String::new();
                out.push_str("→ ");
                let parsed_disp = crate::expr_display::expr_display(&parsed.expr);
                out.push_str(&parsed_disp);
                out.push('\n');
                let proj = parsed.projection.as_deref();
                if let Some(ref p) = parsed.projection {
                    out.push_str("  projection: [");
                    out.push_str(&p.join(", "));
                    out.push_str("]\n");
                }
                out.push('\n');
                out.push_str("## Result\n\n");
                let cgs = Some(sess.cgs.as_ref());
                let formatted = mcp_format_execute_result_table_or_tsv(&result, cgs);
                out.push_str(&formatted.block.into_mcp_result_markdown());
                let handles: Vec<RunArtifactHandle> = artifact.into_iter().collect();
                let preview_needed = mcp_preview_markdown_needed(meta_index.is_some(), &out);
                let truncated = !handles.is_empty()
                    && (preview_needed
                        || !formatted.reference_only_omitted.is_empty()
                        || !formatted.lossy_summary_fields.is_empty()
                        || formatted.in_band_report.any_loss());
                let expr_previews: Vec<String> = if handles.is_empty() {
                    vec![]
                } else {
                    vec![execute_expression_preview(line)]
                };
                let mut markdown = out;
                if preview_needed {
                    let column_hints = merge_snapshot_column_hints(
                        &formatted.lossy_summary_fields,
                        &formatted.in_band_report,
                    );
                    markdown = mcp_compact_markdown_single(
                        line,
                        &parsed_disp,
                        proj,
                        result.count,
                        &formatted.reference_only_omitted,
                        &column_hints,
                    );
                }
                if truncated {
                    markdown.push_str(&mcp_inline_run_snapshot_line(&handles[0]));
                }
                let handles_meta: &[RunArtifactHandle] =
                    if truncated { handles.as_slice() } else { &[] };
                let markdown = mcp_prepend_artifact_followup_markdown(
                    markdown,
                    meta_index.is_some(),
                    handles_meta,
                    &formatted.reference_only_omitted,
                );
                let markdown = append_paging_hint_markdown(markdown, &parsed, &result);
                let run_step_one = [1_usize];
                let lossy_for_meta = if truncated {
                    vec![merge_snapshot_column_hints(
                        &formatted.lossy_summary_fields,
                        &formatted.in_band_report,
                    )]
                } else {
                    vec![]
                };
                let paging_slice: Vec<PlasmPagingStepMeta> =
                    paging_step_meta(1, &parsed, &result).into_iter().collect();
                let paging_for_meta = (!paging_slice.is_empty()).then_some(paging_slice.as_slice());
                let tool_meta = build_mcp_tool_meta(
                    meta_index,
                    handles_meta,
                    &formatted.reference_only_omitted,
                    lossy_for_meta.as_slice(),
                    &expr_previews,
                    if truncated {
                        Some(run_step_one.as_slice())
                    } else {
                        None
                    },
                    paging_for_meta,
                );
                Ok(ExecuteRunToolOutput {
                    markdown,
                    tool_meta,
                })
            }
            Err(RunLineError::Parse(d)) | Err(RunLineError::Normalize(d)) => Err(d),
            Err(RunLineError::Projection(d)) => Err(d),
            Err(RunLineError::Runtime(e, src)) => Err(format!("{e}\nsource expression: {src}")),
            Err(RunLineError::ArtifactSerialization(e)) => {
                Err(format!("artifact serialization failed: {e}"))
            }
            Err(RunLineError::ArtifactPersist(d)) => {
                Err(format!("run artifact persist failed: {d}"))
            }
        }
    } else {
        let steps = match execute_staged_plasm_lines(
            &expressions,
            &sess,
            st,
            session_id.as_str(),
            trace.as_ref(),
            hub_sink.as_ref(),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return Err(mcp_staged_execute_error(e)),
        };
        let total = steps.len();
        let header = "# Plasm run\n\n";
        let mut per_step_body: Vec<String> = Vec::with_capacity(total);
        let mut per_step_omitted: Vec<OmittedReferenceOnlyFields> = Vec::with_capacity(total);
        let mut per_step_lossy: Vec<LossySummaryFieldNames> = Vec::with_capacity(total);
        let mut per_step_in_band: Vec<InBandSummaryReport> = Vec::with_capacity(total);
        let mut per_step_artifact: Vec<Option<RunArtifactHandle>> = Vec::with_capacity(total);
        let mut per_step_compact: Vec<(String, String, usize)> = Vec::with_capacity(total);
        let mut paging_step_metas: Vec<PlasmPagingStepMeta> = Vec::new();
        let mut paging_hints: Vec<String> = Vec::new();
        let mut total_entity_rows: usize = 0;
        let mut omitted_union: BTreeSet<String> = BTreeSet::new();
        let cgs = Some(sess.cgs.as_ref());
        for (index, line) in expressions.iter().enumerate() {
            let (parsed, result, artifact) = &steps[index];
            if let Some(pm) = paging_step_meta(index + 1, parsed, result) {
                let h = match &pm {
                    PlasmPagingStepMeta::Next {
                        next_page_handle, ..
                    } => next_page_handle.as_str(),
                };
                paging_hints.push(format!(
                    "- Step {}: more pages available — use `page({h})` for the next page.",
                    index + 1
                ));
                paging_step_metas.push(pm);
            }
            total_entity_rows = total_entity_rows.saturating_add(result.count);
            per_step_compact.push((
                line.clone(),
                crate::expr_display::expr_display(&parsed.expr),
                result.count,
            ));
            let formatted = mcp_format_execute_result_table_or_tsv(result, cgs);
            omitted_union.extend(formatted.reference_only_omitted.as_ref().iter().cloned());
            per_step_omitted.push(formatted.reference_only_omitted);
            per_step_lossy.push(formatted.lossy_summary_fields);
            per_step_in_band.push(formatted.in_band_report.clone());
            per_step_artifact.push(artifact.clone());
            let mut sec = String::new();
            sec.push_str(&format!(
                "## Step {} of {}\n\n`{}`\n\n→ {}\n",
                index + 1,
                total,
                line,
                crate::expr_display::expr_display(&parsed.expr)
            ));
            if let Some(ref proj) = parsed.projection {
                sec.push_str("  projection: [");
                sec.push_str(&proj.join(", "));
                sec.push_str("]\n");
            }
            sec.push('\n');
            sec.push_str(&formatted.block.into_mcp_result_markdown());
            per_step_body.push(sec);
        }
        let omitted_for_steps: OmittedReferenceOnlyFields = omitted_union.into();
        let mut full_sections = String::from(header);
        for (i, b) in per_step_body.iter().enumerate() {
            if i > 0 {
                full_sections.push_str("\n\n");
            }
            full_sections.push_str(b);
        }
        let preview = mcp_preview_markdown_needed(meta_index.is_some(), &full_sections);
        let mut truncated_steps: Vec<(usize, RunArtifactHandle)> = Vec::new();
        for i in 0..total {
            let step_no = i + 1;
            let Some(h) = per_step_artifact[i].as_ref() else {
                continue;
            };
            let truncated = preview
                || !per_step_omitted[i].is_empty()
                || !per_step_lossy[i].is_empty()
                || per_step_in_band[i].any_loss();
            if truncated {
                truncated_steps.push((step_no, h.clone()));
            }
        }
        let handles_meta: Vec<RunArtifactHandle> =
            truncated_steps.iter().map(|(_, h)| h.clone()).collect();
        let run_step_vec: Vec<usize> = truncated_steps.iter().map(|(s, _)| *s).collect();
        let expr_previews_filtered: Vec<String> = truncated_steps
            .iter()
            .map(|(step_no, _)| execute_expression_preview(&expressions[step_no - 1]))
            .collect();
        let lossy_meta_truncated: Vec<LossySummaryFieldNames> = truncated_steps
            .iter()
            .map(|(step_no, _)| {
                let i = *step_no - 1;
                merge_snapshot_column_hints(&per_step_lossy[i], &per_step_in_band[i])
            })
            .collect();
        let truncated_refs: Vec<(usize, &RunArtifactHandle)> =
            truncated_steps.iter().map(|(s, h)| (*s, h)).collect();

        let mut lossy_union_set: BTreeSet<String> = BTreeSet::new();
        if preview {
            for i in 0..total {
                if per_step_artifact[i].is_some() {
                    for name in per_step_lossy[i].as_slice() {
                        lossy_union_set.insert(name.clone());
                    }
                    for name in per_step_in_band[i].field_names() {
                        lossy_union_set.insert(name.clone());
                    }
                }
            }
        }
        let lossy_preview_union =
            LossySummaryFieldNames::from_vec_sorted_dedup(lossy_union_set.into_iter().collect());

        let markdown = if preview {
            mcp_compact_markdown_multi_line(
                total,
                total_entity_rows,
                &per_step_compact,
                &omitted_for_steps,
                &lossy_preview_union,
                &truncated_refs,
            )
        } else {
            let mut s = String::from(header);
            for i in 0..total {
                if i > 0 {
                    s.push_str("\n\n");
                }
                s.push_str(&per_step_body[i]);
                if let Some(h) = per_step_artifact[i].as_ref() {
                    let truncated = !per_step_omitted[i].is_empty()
                        || !per_step_lossy[i].is_empty()
                        || per_step_in_band[i].any_loss();
                    if truncated {
                        s.push_str(&mcp_inline_run_snapshot_line(h));
                    }
                }
            }
            s
        };
        let mut markdown = mcp_prepend_artifact_followup_markdown(
            markdown,
            meta_index.is_some(),
            &handles_meta,
            &omitted_for_steps,
        );
        if !paging_hints.is_empty() {
            markdown.push_str("\n\n### Paging\n\n");
            markdown.push_str(&paging_hints.join("\n"));
        }
        let paging_for_meta =
            (!paging_step_metas.is_empty()).then_some(paging_step_metas.as_slice());
        let tool_meta = build_mcp_tool_meta(
            meta_index,
            &handles_meta,
            &omitted_for_steps,
            lossy_meta_truncated.as_slice(),
            &expr_previews_filtered,
            if run_step_vec.is_empty() {
                None
            } else {
                Some(run_step_vec.as_slice())
            },
            paging_for_meta,
        );
        Ok(ExecuteRunToolOutput {
            markdown,
            tool_meta,
        })
    }
}

fn run_line_error_string(e: RunLineError) -> String {
    match e {
        RunLineError::Parse(d) | RunLineError::Normalize(d) | RunLineError::Projection(d) => d,
        RunLineError::Runtime(e, src) => format!("{e}\nsource expression: {src}"),
        RunLineError::ArtifactSerialization(e) => format!("artifact serialization failed: {e}"),
        RunLineError::ArtifactPersist(d) => format!("run artifact persist failed: {d}"),
    }
}

pub async fn execute_plasm_plasm_line(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
    line: &str,
    trace: Option<&PlasmTraceContext>,
    line_index: i64,
) -> Result<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>), String> {
    let parsed = parse_plasm_line(line, sess, st).map_err(run_line_error_string)?;
    let mut cache = sess.graph_cache.lock().await;
    run_parsed_plasm_line(
        line, sess, st, &mut cache, session_id, parsed, trace, line_index,
    )
    .await
    .map_err(run_line_error_string)
}

pub async fn execute_plasm_parsed_expr(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
    source_label: &str,
    parsed: ParsedExpr,
    trace: Option<&PlasmTraceContext>,
    line_index: i64,
) -> Result<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>), String> {
    let mut cache = sess.graph_cache.lock().await;
    run_parsed_plasm_line(
        source_label,
        sess,
        st,
        &mut cache,
        session_id,
        parsed,
        trace,
        line_index,
    )
    .await
    .map_err(run_line_error_string)
}

pub async fn trace_record_plasm_line(
    sink: &McpPlasmTraceSink,
    line_index: usize,
    line: &str,
    parsed: &ParsedExpr,
    result: &ExecutionResult,
    sess: &ExecuteSession,
) {
    trace_emit_plasm_line(sink, line_index, line, parsed, result, sess).await;
}

pub async fn archive_plasm_result_snapshot(
    st: &PlasmHostState,
    sess: &ExecuteSession,
    session_id: &str,
    entry_id_override: Option<&str>,
    expressions: Vec<String>,
    result: &ExecutionResult,
    trace: Option<&PlasmTraceContext>,
) -> Result<RunArtifactHandle, String> {
    let run_id = Uuid::new_v4();
    let resource_index = sess.mint_run_resource_index();
    let doc = document_from_run(DocumentFromRun {
        run_id,
        prompt_hash: sess.prompt_hash.as_str(),
        session_id,
        entry_id: entry_id_override.unwrap_or(sess.entry_id.as_str()),
        principal: sess.principal.clone(),
        expressions,
        result,
        resource_index: Some(resource_index),
    });
    let payload_bytes =
        serde_json::to_vec(&doc).map_err(|e| format!("artifact serialization failed: {e}"))?;
    let payload_len = payload_bytes.len();
    let payload = ArtifactPayload {
        metadata: ArtifactPayloadMetadata::json_default(),
        bytes: Bytes::from(payload_bytes),
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
        .map_err(|e| format!("run artifact persist failed: {e}"))?;
    crate::metrics::record_run_artifact_archive_put_ok();
    let epoch = {
        let cache = sess.graph_cache.lock().await;
        GraphEpoch(cache.stats().version)
    };
    let appended = sess
        .core
        .append_run_artifact(run_id, epoch, resource_index, payload)
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
    let canonical_plasm_uri =
        plasm_run_resource_uri(sess.prompt_hash.as_str(), session_id, &run_id);
    let plasm_uri = trace
        .and_then(|c| {
            c.logical_session_ref
                .as_deref()
                .map(|seg| plasm_session_short_resource_uri(seg, resource_index))
        })
        .unwrap_or_else(|| plasm_short_resource_uri(resource_index));
    Ok(RunArtifactHandle {
        run_id,
        plasm_uri,
        canonical_plasm_uri,
        http_path: artifact_http_path(sess.prompt_hash.as_str(), session_id, &run_id),
        payload_len,
        request_fingerprints: result.request_fingerprints.clone(),
    })
}

pub fn publish_plasm_result_steps(
    cgs: Option<&CGS>,
    meta_index: Option<&mut PlasmMetaIndex>,
    steps: &[PublishedResultStep],
) -> ExecuteRunToolOutput {
    let total = steps.len();
    let mut markdown = if total <= 1 {
        String::from("## Result\n\n")
    } else {
        String::from("# Plan run\n\n")
    };
    let mut handles = Vec::new();
    let mut omitted_union: BTreeSet<String> = BTreeSet::new();
    let mut lossy = Vec::new();
    let mut expr_previews = Vec::new();
    let mut run_step_markers = Vec::new();
    let mut paging = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        if total > 1 {
            markdown.push_str(&format!("## Step {} of {}\n\n", i + 1, total));
        }
        if let Some(name) = &step.name {
            markdown.push_str("output: ");
            markdown.push_str(name);
            if let Some(node_id) = &step.node_id {
                markdown.push_str(" -> ");
                markdown.push_str(node_id);
            }
            markdown.push('\n');
        } else if let Some(node_id) = &step.node_id {
            markdown.push_str("output: ");
            markdown.push_str(node_id);
            markdown.push('\n');
        }
        if let (Some(entry_id), Some(entity)) = (&step.entry_id, &step.entity) {
            markdown.push_str("  owner: ");
            markdown.push_str(entry_id);
            markdown.push('.');
            markdown.push_str(entity);
            markdown.push('\n');
        }
        markdown.push_str("→ ");
        markdown.push_str(&step.display);
        markdown.push('\n');
        if let Some(proj) = &step.projection {
            markdown.push_str("  projection: [");
            markdown.push_str(&proj.join(", "));
            markdown.push_str("]\n");
        }
        markdown.push('\n');
        let formatted =
            mcp_format_execute_result_table_or_tsv(&step.result, step.cgs.as_deref().or(cgs));
        omitted_union.extend(formatted.reference_only_omitted.as_ref().iter().cloned());
        markdown.push_str(&formatted.block.into_mcp_result_markdown());
        if let Some(handle) = &step.artifact {
            handles.push(handle.clone());
            expr_previews.push(step.display.clone());
            run_step_markers.push(i + 1);
            lossy.push(merge_snapshot_column_hints(
                &formatted.lossy_summary_fields,
                &formatted.in_band_report,
            ));
            if !formatted.reference_only_omitted.is_empty()
                || !formatted.lossy_summary_fields.is_empty()
                || formatted.in_band_report.any_loss()
            {
                markdown.push_str(&mcp_inline_run_snapshot_line(handle));
            }
        }
        if let Some(handle) = &step.result.paging_handle {
            paging.push(PlasmPagingStepMeta::Next {
                run_step: i + 1,
                returned_count: step.result.count,
                next_page_handle: handle.clone(),
            });
            markdown.push_str(&format!(
                "\n\nmore pages available - use `page({})` for the next page.",
                handle.as_str()
            ));
        }
        if i + 1 < total {
            markdown.push_str("\n\n");
        }
    }
    let omitted_for_steps: OmittedReferenceOnlyFields = omitted_union.into();
    let markdown = mcp_prepend_artifact_followup_markdown(
        markdown,
        meta_index.is_some(),
        &handles,
        &omitted_for_steps,
    );
    let paging_for_meta = (!paging.is_empty()).then_some(paging.as_slice());
    let run_step_numbers_for_meta =
        (!run_step_markers.is_empty()).then_some(run_step_markers.as_slice());
    let tool_meta = build_mcp_tool_meta(
        meta_index,
        &handles,
        &omitted_for_steps,
        lossy.as_slice(),
        &expr_previews,
        run_step_numbers_for_meta,
        paging_for_meta,
    );
    ExecuteRunToolOutput {
        markdown,
        tool_meta,
    }
}

fn split_expression_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn parse_execute_lines_body(content_type: Option<&str>, raw: &[u8]) -> Result<Vec<String>, String> {
    let mime = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if mime == "application/json" || mime.ends_with("+json") {
        let v: serde_json::Value =
            serde_json::from_slice(raw).map_err(|e| format!("invalid JSON body: {e}"))?;
        let strings: Vec<String> = if let Some(arr) = v.as_array() {
            arr.iter()
                .enumerate()
                .map(|(i, x)| {
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("lines[{i}] must be a JSON string"))
                })
                .collect::<Result<Vec<_>, _>>()?
        } else if let Some(obj) = v.as_object() {
            let Some(arr) = obj.get("lines").and_then(|x| x.as_array()) else {
                return Err(
                    "JSON body must be a JSON array of strings or {\"lines\": [\"...\"]}".into(),
                );
            };
            arr.iter()
                .enumerate()
                .map(|(i, x)| {
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("lines[{i}] must be a JSON string"))
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            return Err(
                "JSON body must be a JSON array of strings or {\"lines\": [\"...\"]}".into(),
            );
        };
        Ok(strings
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    } else {
        let s = std::str::from_utf8(raw).map_err(|e| format!("invalid UTF-8: {e}"))?;
        Ok(split_expression_lines(s))
    }
}

fn plugin_execute_options_from_session(
    sess: &ExecuteSession,
) -> (
    Option<Arc<CompileOperationFn>>,
    Option<Arc<CompileQueryFn>>,
    Option<u64>,
) {
    let Some(pg) = sess.plugin_generation.as_ref() else {
        return (None, None, None);
    };
    (
        Some(pg.compile_operation_fn.clone()),
        Some(pg.compile_query_fn.clone()),
        Some(pg.id),
    )
}

/// Upper bound for valid `e#` indices: number of entities exposed in this session (initial open + expand waves).
fn session_entity_symbol_upper_bound(sess: &ExecuteSession) -> Option<usize> {
    if let Some(exp) = sess.domain_exposure.as_ref() {
        let n = exp.entities.len();
        return (n > 0).then_some(n);
    }
    let n = sess.entities.len();
    (n > 0).then_some(n)
}

/// If the parser reports an unknown entity token, note how many `e#` symbols exist this session.
fn augment_unknown_entity_parse_error(msg: String, sess: &ExecuteSession) -> String {
    if !msg.contains("unknown entity") {
        return msg;
    }
    let Some(k) = session_entity_symbol_upper_bound(sess) else {
        return msg;
    };
    format!(
        "{msg} (this session defines entity symbols e1..e{k} only; use the current Plasm instructions in the `prompt` field)"
    )
}

/// Parser diagnostic plus imperative correction (same pipeline as REPL), for MCP/HTTP tool errors.
fn execute_session_parse_error_message(
    err: &expr_parser::ParseError,
    expanded: &str,
    source_line: &str,
    cgs: &CGS,
    sym_map: &SymbolMap,
) -> String {
    let step = render_parse_error_with_feedback(
        err,
        expanded,
        source_line,
        cgs,
        FeedbackStyle::SymbolicLlm { map: sym_map },
    );
    format!("{err}\n\n{}", step.correction)
}

enum RunLineError {
    Parse(String),
    Normalize(String),
    /// [`ExecutionEngine::auto_resolve_projection`] failed; surface to clients instead of silent degradation.
    Projection(String),
    /// Runtime failure after successful parse; second field is the **source** line (before symbol expansion / parse) for logging and MCP.
    Runtime(RuntimeError, String),
    ArtifactSerialization(serde_json::Error),
    /// Durable run snapshot write failed (object store / memory backend).
    ArtifactPersist(String),
}

fn run_line_error_metric_labels(err: &RunLineError) -> (&'static str, &'static str) {
    match err {
        RunLineError::Parse(_) => ("parse", "parse"),
        RunLineError::Normalize(_) => ("parse", "normalize"),
        RunLineError::Projection(_) => ("projection", "projection"),
        RunLineError::Runtime(_, _) => ("execute", "runtime"),
        RunLineError::ArtifactSerialization(_) => ("artifact", "serialization"),
        RunLineError::ArtifactPersist(_) => ("artifact", "persist"),
    }
}

/// Staged multi-line run failure: per-line errors or ordered merge after a parallel query stage.
enum StagedExecuteError {
    Step {
        index: usize,
        total: usize,
        line: String,
        err: RunLineError,
    },
    Merge {
        err: RuntimeError,
    },
}

fn mcp_staged_execute_error(e: StagedExecuteError) -> String {
    match e {
        StagedExecuteError::Step {
            index,
            total,
            line,
            err,
        } => match err {
            RunLineError::Parse(d) | RunLineError::Normalize(d) => format!(
                "line {} of {total}: {d}\nexpression: {}",
                index + 1,
                execute_expression_preview(&line)
            ),
            RunLineError::Projection(d) => format!(
                "line {} of {total}: projection enrichment failed: {d}\nexpression: {}",
                index + 1,
                execute_expression_preview(&line)
            ),
            RunLineError::Runtime(e, src) => format!(
                "line {} of {total}: {e}\nsource expression: {src}",
                index + 1
            ),
            RunLineError::ArtifactSerialization(e) => format!(
                "line {} of {total}: artifact serialization failed: {e}",
                index + 1
            ),
            RunLineError::ArtifactPersist(d) => format!(
                "line {} of {total}: run artifact persist failed: {d}",
                index + 1
            ),
        },
        StagedExecuteError::Merge { err } => {
            format!("graph merge after parallel stage failed: {err}")
        }
    }
}

fn http_staged_execute_error(
    e: StagedExecuteError,
    sess: &ExecuteSession,
    prompt_hash: &PromptHashHex,
    session_id: &ExecuteSessionId,
) -> Response {
    match e {
        StagedExecuteError::Step {
            index,
            total,
            line,
            err,
        } => match err {
            RunLineError::Parse(d) | RunLineError::Normalize(d) => {
                plasm_line_step_bad_request(index, total, &line, d)
            }
            RunLineError::Projection(d) => problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_PROJECTION_ENRICHMENT_FAILED),
                )
                .with_title("Internal Server Error")
                .with_detail(format!(
                    "line {} of {total}: projection enrichment failed: {d}\nexpression: {}",
                    index + 1,
                    execute_expression_preview(&line)
                )),
            ),
            RunLineError::Runtime(e, _src) => execution_failed_response(
                &e,
                &line,
                sess,
                prompt_hash,
                session_id,
                Some(index),
                total,
            ),
            RunLineError::ArtifactSerialization(e) => problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                )
                .with_title("Internal Server Error")
                .with_detail(format!(
                    "line {} of {total}: artifact serialization failed: {e}",
                    index + 1
                )),
            ),
            RunLineError::ArtifactPersist(d) => problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                )
                .with_title("Internal Server Error")
                .with_detail(format!(
                    "line {} of {total}: run artifact persist failed: {d}",
                    index + 1
                )),
            ),
        },
        StagedExecuteError::Merge { err } => problem_response(
            Problem::custom(
                ProblemStatus::INTERNAL_SERVER_ERROR,
                Uri::from_static(problem_types::EXECUTE_EXECUTION_FAILED),
            )
            .with_title("Internal Server Error")
            .with_detail(err.to_string()),
        ),
    }
}

fn parse_plasm_line(
    line: &str,
    sess: &ExecuteSession,
    st: &PlasmHostState,
) -> Result<ParsedExpr, RunLineError> {
    let pipeline = st.engine.prompt_pipeline();
    let mut parsed = crate::plasm_plan_run::parse_plasm_surface_line(
        sess,
        Some(st.sessions.symbol_map_cross_cache()),
        pipeline,
        line,
    )
    .map_err(|e| {
        let expanded = pipeline.expand_expr_for_session_with_optional_exposure(
            line,
            sess.cgs.as_ref(),
            &sess.entities,
            sess.domain_exposure.as_ref(),
        );
        let sym_map = crate::plasm_plan_run::symbol_map_for_plasm_surface_parse(
            sess,
            Some(st.sessions.symbol_map_cross_cache()),
        );
        RunLineError::Parse(augment_unknown_entity_parse_error(
            execute_session_parse_error_message(&e, &expanded, line, sess.cgs.as_ref(), &sym_map),
            sess,
        ))
    })?;
    if let Some(ref fed) = sess.federation_dispatch() {
        normalize_expr_query_capabilities_federated(
            &mut parsed.expr,
            fed.as_ref(),
            sess.cgs.as_ref(),
        )
    } else {
        normalize_expr_query_capabilities(&mut parsed.expr, sess.cgs.as_ref())
    }
    .map_err(|e| RunLineError::Normalize(e.to_string()))?;
    Ok(parsed)
}

fn synthetic_page_result(
    sess: &ExecuteSession,
    handle: &PagingHandle,
    mut cursor: crate::execute_session::SyntheticPageCursor,
    trace: Option<&PlasmTraceContext>,
) -> ExecutionResult {
    let start = cursor.offset.min(cursor.rows.len());
    let end = start
        .saturating_add(cursor.page_size)
        .min(cursor.rows.len());
    let entities = cursor.rows[start..end].to_vec();
    cursor.offset = end;
    let has_more = cursor.offset < cursor.rows.len();
    let request_fingerprints = cursor.request_fingerprints.clone();
    let paging_handle = if has_more {
        sess.upsert_synthetic_paging_resume(handle, cursor);
        Some(handle.clone())
    } else {
        sess.remove_paging_resume(handle);
        None
    };
    let _ = trace;
    ExecutionResult {
        count: entities.len(),
        entities,
        has_more,
        pagination_resume: None,
        paging_handle,
        source: ExecutionSource::Cache,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
        },
        request_fingerprints,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_parsed_plasm_line(
    line: &str,
    sess: &ExecuteSession,
    st: &PlasmHostState,
    cache: &mut GraphCache,
    session_id: &str,
    parsed: ParsedExpr,
    trace: Option<&PlasmTraceContext>,
    line_index: i64,
) -> Result<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>), RunLineError> {
    let wall = Instant::now();
    let mut log_expr = format!("→ {}", crate::expr_display::expr_display(&parsed.expr));
    if let Some(ref proj) = parsed.projection {
        if !proj.is_empty() {
            log_expr.push_str(&format!("\n  projection: [{}]", proj.join(", ")));
        }
    }
    let expr_span =
        crate::spans::execute_expression_line(sess.entry_id.as_str(), line.len(), log_expr.len());
    expr_span.in_scope(|| {
        tracing::trace!(
            target: "plasm_agent::http_execute",
            entry_id = %sess.entry_id,
            source_expression = %line,
            parsed_expression = %log_expr,
            "execute expression"
        );
    });
    let page_storage_key: Option<PagingHandle> = match &parsed.expr {
        plasm_core::Expr::Page(p) => Some(resolve_paging_storage_handle(trace, &p.handle)?),
        _ => None,
    };
    let _page_paging_serial = if page_storage_key.is_some() {
        Some(sess.paging_op_lock.lock().await)
    } else {
        None
    };
    if let Some(ref key) = page_storage_key {
        if let Some(cursor) = sess.peek_synthetic_paging_resume(key) {
            let result = synthetic_page_result(sess, key, cursor, trace);
            let run_id = Uuid::new_v4();
            let resource_index = sess.mint_run_resource_index();
            let doc = document_from_run(DocumentFromRun {
                run_id,
                prompt_hash: sess.prompt_hash.as_str(),
                session_id,
                entry_id: sess.entry_id.as_str(),
                principal: sess.principal.clone(),
                expressions: vec![line.to_string()],
                result: &result,
                resource_index: Some(resource_index),
            });
            let payload_bytes =
                serde_json::to_vec(&doc).map_err(RunLineError::ArtifactSerialization)?;
            let payload_len = payload_bytes.len();
            let payload = ArtifactPayload {
                metadata: ArtifactPayloadMetadata::json_default(),
                bytes: Bytes::from(payload_bytes),
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
                .map_err(|e| RunLineError::ArtifactPersist(e.to_string()))?;
            let appended = sess
                .core
                .append_run_artifact(
                    run_id,
                    GraphEpoch(cache.stats().version),
                    resource_index,
                    payload,
                )
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
            let canonical_plasm_uri =
                plasm_run_resource_uri(sess.prompt_hash.as_str(), session_id, &run_id);
            let plasm_uri = trace
                .and_then(|c| {
                    c.logical_session_ref
                        .as_deref()
                        .map(|seg| plasm_session_short_resource_uri(seg, resource_index))
                })
                .unwrap_or_else(|| plasm_short_resource_uri(resource_index));
            let artifact = Some(RunArtifactHandle {
                run_id,
                plasm_uri,
                canonical_plasm_uri,
                http_path: artifact_http_path(sess.prompt_hash.as_str(), session_id, &run_id),
                payload_len,
                request_fingerprints: result.request_fingerprints.clone(),
            });
            return Ok((parsed, result, artifact));
        }
    }
    let mut page_resume_owned: Option<QueryPaginationResumeData> = if let Some(ref key) =
        page_storage_key
    {
        Some(sess.peek_paging_resume(key).ok_or_else(|| {
                let detail = match trace.and_then(|t| t.logical_session_ref.as_deref()) {
                    Some(r) => format!(
                        "unknown paging handle `{}` — stale continuation or wrong logical session; use `page({r}_pgN)` from the latest tool result for this `logical_session_ref`",
                        key.as_str()
                    ),
                    None => format!(
                        "unknown paging handle `{}` (handles are minted when a paginated query returns additional pages)",
                        key.as_str()
                    ),
                };
                RunLineError::Parse(detail)
            })?)
    } else {
        None
    };

    let root_entity_owned: String = if let Some(r) = &page_resume_owned {
        r.query.entity.to_string()
    } else {
        parsed.expr.primary_entity().to_string()
    };
    let root_entity = root_entity_owned.as_str();
    let fed_holder = sess.federation_dispatch();
    let exec_cgs: &plasm_core::CGS = match fed_holder.as_ref() {
        Some(fed) => fed.resolve_cgs(root_entity, sess.cgs.as_ref()),
        None => sess.cgs.as_ref(),
    };
    let http_backend_for_root = fed_holder
        .as_ref()
        .and_then(|fed| fed.context_for_entity(root_entity))
        .map(|ctx| ctx.cgs.http_backend.clone())
        .or_else(|| sess.http_backend.clone());
    let auth_for_exec = exec_cgs.auth.clone();
    let (compile_operation_fn, compile_query_fn, plugin_generation_id) =
        plugin_execute_options_from_session(sess);
    let fp_sink = Arc::new(Mutex::new(Vec::<String>::new()));
    let secret_provider = st.effective_outbound_secret_provider();
    let exec_opts = ExecuteOptions {
        request_fingerprint_sink: Some(fp_sink.clone()),
        http_base_url_override: http_backend_for_root,
        auth_resolver_override: auth_for_exec
            .map(|scheme| Arc::new(AuthResolver::new(scheme, secret_provider.clone()))),
        compile_operation_fn,
        compile_query_fn,
        plugin_generation_id,
        federation: fed_holder.clone(),
    };
    let (_, operation) = trace_expr_api_meta(&parsed.expr);
    let mut result = match &parsed.expr {
        plasm_core::Expr::Page(page) => {
            let resume = page_resume_owned.take().ok_or_else(|| {
                RunLineError::Parse(
                    "internal: page expression without pagination snapshot".to_string(),
                )
            })?;
            let consume = StreamConsumeOpts {
                fetch_all: false,
                max_items: page.limit,
                one_page: true,
            };
            st.engine
                .execute_pagination_resume(
                    resume,
                    exec_cgs,
                    cache,
                    Some(st.mode),
                    consume,
                    exec_opts.clone(),
                )
                .instrument(expr_span.clone())
                .await
        }
        _ => {
            st.engine
                .execute(
                    &parsed.expr,
                    exec_cgs,
                    cache,
                    Some(st.mode),
                    StreamConsumeOpts::default(),
                    exec_opts.clone(),
                )
                .instrument(expr_span.clone())
                .await
        }
    }
    .map_err(|e| {
        let ms = wall.elapsed().as_secs_f64() * 1000.0;
        crate::metrics::record_execute_expression_line(
            sess.entry_id.as_str(),
            operation.as_str(),
            "error",
            "runtime",
            ms,
            0,
            0,
        );
        tracing::error!(
            target: "plasm_agent::http_execute",
            entry_id = %sess.entry_id,
            error = %e,
            "execute failed"
        );
        tracing::trace!(
            target: "plasm_agent::http_execute",
            source_expression = %line,
            parsed_expression = %log_expr,
            "execute failed (expression detail)"
        );
        RunLineError::Runtime(e, line.to_string())
    })?;

    if let Some(ref storage_key) = page_storage_key {
        if result.has_more {
            if let Some(next) = result.pagination_resume.take() {
                sess.upsert_paging_resume(storage_key, next);
            } else {
                sess.remove_paging_resume(storage_key);
            }
        } else {
            sess.remove_paging_resume(storage_key);
        }
    } else if result.has_more {
        if let Some(resume) = result.pagination_resume.take() {
            let h = sess.register_paging_continuation(
                resume,
                trace.and_then(|t| t.logical_session_ref.as_deref()),
            );
            result.paging_handle = Some(h);
        }
    }

    if let Some(ref fields) = parsed.projection {
        if !result.entities.is_empty() {
            let entity_type = result.entities[0].reference.entity_type.clone();
            let proj_cgs: &plasm_core::CGS = match fed_holder.as_ref() {
                Some(fed) => fed.resolve_cgs(entity_type.as_str(), sess.cgs.as_ref()),
                None => sess.cgs.as_ref(),
            };
            match st
                .engine
                .auto_resolve_projection(
                    result.entities.clone(),
                    &entity_type,
                    fields,
                    proj_cgs,
                    cache,
                    st.mode,
                    exec_opts,
                )
                .instrument(expr_span.clone())
                .await
            {
                Ok(enriched) => {
                    result.entities = enriched;
                    result.count = result.entities.len();
                }
                Err(e) => {
                    let ms = wall.elapsed().as_secs_f64() * 1000.0;
                    crate::metrics::record_execute_expression_line(
                        sess.entry_id.as_str(),
                        operation.as_str(),
                        "error",
                        "projection",
                        ms,
                        0,
                        0,
                    );
                    tracing::error!(
                        target: "plasm_agent::http_execute",
                        entry_id = %sess.entry_id,
                        error = %e,
                        "projection enrichment failed"
                    );
                    tracing::trace!(
                        target: "plasm_agent::http_execute",
                        source_expression = %line,
                        parsed_expression = %log_expr,
                        "projection enrichment failed (expression detail)"
                    );
                    return Err(RunLineError::Projection(e.to_string()));
                }
            }
            apply_projection(&mut result, fields);
        }
    }

    result.request_fingerprints = fp_sink.lock().unwrap_or_else(|e| e.into_inner()).clone();

    let run_id = Uuid::new_v4();
    let resource_index = sess.mint_run_resource_index();
    let doc = document_from_run(DocumentFromRun {
        run_id,
        prompt_hash: sess.prompt_hash.as_str(),
        session_id,
        entry_id: sess.entry_id.as_str(),
        principal: sess.principal.clone(),
        expressions: vec![line.to_string()],
        result: &result,
        resource_index: Some(resource_index),
    });
    let payload_bytes = serde_json::to_vec(&doc).map_err(|e| {
        let ms = wall.elapsed().as_secs_f64() * 1000.0;
        crate::metrics::record_execute_expression_line(
            sess.entry_id.as_str(),
            operation.as_str(),
            "error",
            "serialization",
            ms,
            0,
            0,
        );
        RunLineError::ArtifactSerialization(e)
    })?;
    let payload_len = payload_bytes.len();
    let payload = ArtifactPayload {
        metadata: ArtifactPayloadMetadata::json_default(),
        bytes: Bytes::from(payload_bytes),
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
        .map_err(|e| {
            let ms = wall.elapsed().as_secs_f64() * 1000.0;
            crate::metrics::record_execute_expression_line(
                sess.entry_id.as_str(),
                operation.as_str(),
                "error",
                "artifact_persist",
                ms,
                0,
                0,
            );
            RunLineError::ArtifactPersist(e.to_string())
        })?;
    crate::metrics::record_run_artifact_archive_put_ok();
    let appended = sess
        .core
        .append_run_artifact(
            run_id,
            GraphEpoch(cache.stats().version),
            resource_index,
            payload,
        )
        .instrument(expr_span)
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
    let artifact = {
        let canonical_plasm_uri =
            plasm_run_resource_uri(sess.prompt_hash.as_str(), session_id, &run_id);
        let plasm_uri = trace
            .and_then(|c| {
                if let Some(ref seg) = c.logical_session_ref {
                    Some(plasm_session_short_resource_uri(
                        seg.as_str(),
                        resource_index,
                    ))
                } else {
                    c.logical_session_id
                        .as_deref()
                        .and_then(|ls| Uuid::parse_str(ls).ok())
                        .map(|u| plasm_short_resource_uri_logical(&u, resource_index))
                }
            })
            .unwrap_or_else(|| plasm_short_resource_uri(resource_index));
        let http_path = artifact_http_path(sess.prompt_hash.as_str(), session_id, &run_id);
        Some(RunArtifactHandle {
            run_id,
            plasm_uri,
            canonical_plasm_uri,
            http_path,
            payload_len,
            request_fingerprints: result.request_fingerprints.clone(),
        })
    };

    if let Some(ctx) = trace {
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let api = Some(trace_api_entry_id_for_execute_root(sess, root_entity));
        let meta = plasm_line_trace_meta(line, &parsed, &result, api);
        let call_idx = ctx.call_index.unwrap_or(0).max(0) as u64;
        let ev = TraceEvent::at(
            wall_ms,
            TraceSegment::PlasmLine {
                call_index: call_idx,
                line_index: line_index.max(0) as usize,
                source_expression: meta.source_expression,
                repl_pre: meta.repl_pre,
                repl_post: meta.repl_post,
                capability: meta.capability,
                operation: meta.operation,
                api_entry_id: meta.api_entry_id,
                duration_ms: result.stats.duration_ms,
                stats: result.stats.clone(),
                source: result.source,
                request_fingerprints: result.request_fingerprints.clone(),
                http_calls: vec![],
            },
        );
        crate::trace_sink_emit::spawn_emit_mcp_trace_segment(
            st.trace_ingest.as_ref(),
            &McpTraceAuditFields {
                trace_id: ctx.trace_id,
                mcp_session_id: ctx.mcp_session_id.clone(),
                logical_session_id: ctx.logical_session_id.clone(),
                plasm_prompt_hash: Some(sess.prompt_hash.to_string()),
                plasm_execute_session: Some(session_id.to_string()),
                run_id: Some(run_id),
                tenant_id: (!sess.tenant_scope.is_empty()).then(|| sess.tenant_scope.clone()),
                principal_sub: (!sess.principal_subject.is_empty())
                    .then(|| sess.principal_subject.clone()),
            },
            &ev,
            None,
        );
    }

    let ms = wall.elapsed().as_secs_f64() * 1000.0;
    crate::metrics::record_execute_expression_line(
        sess.entry_id.as_str(),
        operation.as_str(),
        "success",
        "none",
        ms,
        result.stats.cache_hits as u64,
        result.stats.cache_misses as u64,
    );

    Ok((parsed, result, artifact))
}

async fn run_single_plasm_line(
    line: &str,
    sess: &ExecuteSession,
    st: &PlasmHostState,
    cache: &mut GraphCache,
    session_id: &str,
    trace: Option<&PlasmTraceContext>,
    line_index: i64,
) -> Result<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>), RunLineError> {
    let wall = Instant::now();
    let parsed = match parse_plasm_line(line, sess, st) {
        Ok(p) => p,
        Err(e) => {
            let (op, ec) = run_line_error_metric_labels(&e);
            crate::metrics::record_execute_expression_line(
                sess.entry_id.as_str(),
                op,
                "error",
                ec,
                wall.elapsed().as_secs_f64() * 1000.0,
                0,
                0,
            );
            return Err(e);
        }
    };
    run_parsed_plasm_line(line, sess, st, cache, session_id, parsed, trace, line_index).await
}

/// Run multiple program lines with staged scheduling: consecutive parallel-safe root queries may
/// run in a fork-merge stage; all other lines run sequentially with a fully merged session cache between stages.
async fn execute_staged_plasm_lines(
    expressions: &[String],
    sess: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    trace: Option<&PlasmTraceContext>,
    hub_sink: Option<&McpPlasmTraceSink>,
) -> Result<Vec<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>)>, StagedExecuteError> {
    let total = expressions.len();
    let mut parsed_exprs = Vec::with_capacity(total);
    for (index, line) in expressions.iter().enumerate() {
        match parse_plasm_line(line, sess, st) {
            Ok(p) => parsed_exprs.push(p),
            Err(err) => {
                return Err(StagedExecuteError::Step {
                    index,
                    total,
                    line: line.clone(),
                    err,
                });
            }
        }
    }
    let flags: Vec<bool> = parsed_exprs
        .iter()
        .map(line_may_share_parallel_query_stage)
        .collect();
    let stages = build_execute_stages(&flags);
    let mut combined: Vec<Option<(ParsedExpr, ExecutionResult, Option<RunArtifactHandle>)>> =
        (0..total).map(|_| None).collect();

    for stage in stages {
        match stage {
            ExecuteStage::Sequential(idx) => {
                let mut cache = sess.graph_cache.lock().await;
                let r = run_parsed_plasm_line(
                    &expressions[idx],
                    sess,
                    st,
                    &mut cache,
                    session_id,
                    parsed_exprs[idx].clone(),
                    trace,
                    idx as i64,
                )
                .await
                .map_err(|err| StagedExecuteError::Step {
                    index: idx,
                    total,
                    line: expressions[idx].clone(),
                    err,
                })?;
                if let Some(sink) = hub_sink {
                    let (ref parsed, ref result, _) = r;
                    trace_emit_plasm_line(sink, idx, &expressions[idx], parsed, result, sess).await;
                }
                combined[idx] = Some(r);
            }
            ExecuteStage::Parallel(idxs) => {
                // `join_all` interleaves concurrent futures on one task; `Span::current()` inside
                // `run_parsed_plasm_line` can be wrong on first poll without an explicit parent chain.
                // Attach each branch under the current span (Tower HTTP / MCP handler) so OTLP traces
                // are not orphaned from the transport request.
                let parallel_parent = tracing::Span::current();
                let base = sess.graph_cache.snapshot().await;
                let sess_c = sess.clone();
                let st_c = st.clone();
                let sid = session_id.to_string();
                let futures = idxs.iter().map(|&idx| {
                    let line = expressions[idx].clone();
                    let parsed = parsed_exprs[idx].clone();
                    let sess = sess_c.clone();
                    let st = st_c.clone();
                    let sid = sid.clone();
                    let mut fork = base.clone();
                    let line_fork_span = tracing::trace_span!(
                        parent: parallel_parent.clone(),
                        "plasm_agent.execute.parallel_plasm_line",
                        line_index = idx,
                    );
                    async move {
                        run_parsed_plasm_line(
                            &line, &sess, &st, &mut fork, &sid, parsed, trace, idx as i64,
                        )
                        .await
                        .map(|triple| (triple, fork))
                    }
                    .instrument(line_fork_span)
                });
                let step_results = join_all(futures).await;
                let mut forks_ordered: Vec<GraphCache> = Vec::with_capacity(idxs.len());
                for (k, res) in step_results.into_iter().enumerate() {
                    let idx = idxs[k];
                    match res {
                        Ok((triple, fork)) => {
                            if let Some(sink) = hub_sink {
                                let (ref parsed, ref result, _) = triple;
                                trace_emit_plasm_line(
                                    sink,
                                    idx,
                                    &expressions[idx],
                                    parsed,
                                    result,
                                    &sess_c,
                                )
                                .await;
                            }
                            combined[idx] = Some(triple);
                            forks_ordered.push(fork);
                        }
                        Err(err) => {
                            return Err(StagedExecuteError::Step {
                                index: idx,
                                total,
                                line: expressions[idx].clone(),
                                err,
                            });
                        }
                    }
                }
                let mut g = sess.graph_cache.lock().await;
                for fork in forks_ordered {
                    g.merge_from_graph(&fork)
                        .map_err(|e| StagedExecuteError::Merge { err: e })?;
                }
            }
        }
    }

    Ok(combined
        .into_iter()
        .map(|o| o.expect("staged line planner: each line index runs exactly once"))
        .collect())
}

fn respond_execute_result(
    kind: ExecResponseKind,
    json_value: serde_json::Value,
    result: &ExecutionResult,
    response_meta: Option<serde_json::Map<String, serde_json::Value>>,
    cgs: Option<&CGS>,
) -> Response {
    match kind {
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
    }
}

fn respond_staged_lines_execute_result(
    kind: ExecResponseKind,
    step_values: Vec<serde_json::Value>,
    step_tables: Option<Vec<String>>,
    response_meta: Option<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    match kind {
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
    }
}

fn execution_failed_response(
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

fn plasm_line_step_bad_request(
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

pub fn execute_routes() -> Router {
    Router::new()
        .route("/execute", post(post_create_execute_session))
        .route(
            "/execute/{prompt_hash}/{session_id}",
            get(get_execute_session).post(post_run_execute_session),
        )
        .route(
            "/execute/{prompt_hash}/{session_id}/artifacts/{run_id}",
            get(get_execute_run_artifact),
        )
}

async fn post_create_execute_session(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    Json(body): Json<CreateExecuteSessionBody>,
) -> Response {
    match execute_session_create_response(&st, principal.as_ref(), body).await {
        Ok(created) => {
            let location = format!("/execute/{}/{}", created.prompt_hash, created.session);
            // `prompt_hash` and `session` are in the URL; full session JSON (including Plasm instructions in `prompt`) is
            // served by GET on that same path — safe for clients that follow 303 with GET.
            (StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response()
        }
        Err(e) => {
            if e == "`entities` must be non-empty" {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::BAD_REQUEST,
                        Uri::from_static(problem_types::EXECUTE_EMPTY_ENTITIES),
                    )
                    .with_title("Bad Request")
                    .with_detail(e),
                );
            }
            if e.contains("PLASM_AUTH_RESOLUTION=delegated") && e.contains("principal") {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::BAD_REQUEST,
                        Uri::from_static(problem_types::EXECUTE_PRINCIPAL_REQUIRED),
                    )
                    .with_title("Bad Request")
                    .with_detail(e),
                );
            }
            if e.starts_with("unknown catalog entry:") {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::NOT_FOUND,
                        Uri::from_static(problem_types::EXECUTE_UNKNOWN_CATALOG_ENTRY),
                    )
                    .with_title("Not Found")
                    .with_detail(e),
                );
            }
            if e.contains("unknown entity `") && e.contains("` in this schema") {
                return problem_response(
                    Problem::custom(
                        ProblemStatus::BAD_REQUEST,
                        Uri::from_static(problem_types::EXECUTE_UNKNOWN_ENTITY),
                    )
                    .with_title("Bad Request")
                    .with_detail(e),
                );
            }
            problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_REGISTRY_ERROR),
                )
                .with_title("Bad Request")
                .with_detail(e),
            )
        }
    }
}

async fn get_execute_session(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    ExecutePath {
        prompt_hash,
        session_id,
    }: ExecutePath,
) -> Response {
    let Some(sess) = st.sessions.get(&prompt_hash, &session_id).await else {
        let _miss = crate::spans::execute_session_lookup_miss().entered();
        tracing::debug!(
            prompt_hash = %prompt_hash,
            session_id = %session_id,
            "execute session GET lookup miss"
        );
        return problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_SESSION),
            )
            .with_title("Not Found")
            .with_detail("unknown or expired execute session"),
        );
    };

    if !session_allows_principal(&sess, principal.as_ref()) {
        return incoming_auth_problem(
            crate::incoming_auth::IncomingAuthFailure::Invalid(
                "execute session tenant does not match caller".into(),
            ),
            true,
        );
    }

    Json(CreateExecuteSessionResponse {
        prompt_hash: sess.prompt_hash.clone(),
        session: session_id.to_string(),
        prompt: sess.prompt_text.clone(),
        entry_id: sess.entry_id.clone(),
        entities: sess.entities.clone(),
        reused: false,
        principal: sess.principal.clone(),
    })
    .into_response()
}

async fn get_execute_run_artifact(
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
    let run_id = match Uuid::parse_str(rid.trim()) {
        Ok(u) => u,
        Err(_) => {
            crate::metrics::record_execute_artifact_serve("error", "bad_path", started.elapsed());
            return problem_response_invalid_execute_path(
                StatusCode::BAD_REQUEST,
                "invalid `run_id` path segment: expected UUID",
            );
        }
    };

    let live_sess = st.sessions.get(&prompt_hash, &session_id).await;
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
            run_id = %run_id,
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

async fn post_run_execute_session(
    Extension(st): Extension<PlasmHostState>,
    Extension(IncomingPrincipal(principal)): Extension<IncomingPrincipal>,
    ExecutePath {
        prompt_hash,
        session_id,
    }: ExecutePath,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(sess) = st.sessions.get(&prompt_hash, &session_id).await else {
        let _miss = crate::spans::execute_session_lookup_miss().entered();
        tracing::debug!(
            prompt_hash = %prompt_hash,
            session_id = %session_id,
            "execute session lookup miss"
        );
        return problem_response(
            Problem::custom(
                ProblemStatus::NOT_FOUND,
                Uri::from_static(problem_types::EXECUTE_UNKNOWN_SESSION),
            )
            .with_title("Not Found")
            .with_detail("unknown or expired execute session"),
        );
    };

    if !session_allows_principal(&sess, principal.as_ref()) {
        return incoming_auth_problem(
            crate::incoming_auth::IncomingAuthFailure::Invalid(
                "execute session tenant does not match caller".into(),
            ),
            true,
        );
    }

    let accept = headers.get(ACCEPT).and_then(|v| v.to_str().ok());
    let kind = match negotiate_accept(accept) {
        Ok(k) => k,
        Err(AcceptNegotiationError::NoSupportedMediaType) => {
            return problem_response(
                Problem::custom(
                    ProblemStatus::NOT_ACCEPTABLE,
                    Uri::from_static(problem_types::EXECUTE_UNSUPPORTED_ACCEPT),
                )
                .with_title("Not Acceptable")
                .with_detail(
                    "supported Accept values include application/json, application/x-ndjson, text/plain, text/toon (default when Accept is omitted: text/toon)",
                ),
            );
        }
    };

    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok());

    let expressions = match parse_execute_lines_body(content_type, &body) {
        Ok(v) => v,
        Err(msg) => {
            let type_uri = if msg.starts_with("invalid UTF-8:") {
                problem_types::EXECUTE_INVALID_BODY_ENCODING
            } else {
                problem_types::EXECUTE_INVALID_REQUEST_BODY
            };
            return problem_response(
                Problem::custom(ProblemStatus::BAD_REQUEST, Uri::from_static(type_uri))
                    .with_title("Bad Request")
                    .with_detail(msg),
            );
        }
    };

    if expressions.is_empty() {
        return problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_EMPTY_EXPRESSION),
            )
            .with_title("Bad Request")
            .with_detail(
                "no lines to run: send a non-empty text/plain body, newline-separated Plasm lines, or JSON {\"lines\":[\"...\"]}",
            ),
        );
    }

    if expressions.len() > MAX_PLASM_LINES_PER_REQUEST {
        return problem_response(
            Problem::custom(
                ProblemStatus::BAD_REQUEST,
                Uri::from_static(problem_types::EXECUTE_INVALID_REQUEST_BODY),
            )
            .with_title("Bad Request")
            .with_detail(format!(
                "too many lines in one request (max {MAX_PLASM_LINES_PER_REQUEST}, got {})",
                expressions.len()
            )),
        );
    }

    let multiple_lines = expressions.len() > 1;

    let http_trace = PlasmTraceContext {
        trace_id: trace_id_for_http_execute_session(
            sess.tenant_scope.as_str(),
            prompt_hash.as_str(),
            session_id.as_str(),
        ),
        call_index: None,
        mcp_session_id: None,
        logical_session_id: None,
        logical_session_ref: None,
    };

    if !multiple_lines {
        let line = expressions[0].as_str();
        let mut cache = sess.graph_cache.lock().await;
        return match run_single_plasm_line(
            line,
            &sess,
            &st,
            &mut cache,
            session_id.as_str(),
            Some(&http_trace),
            0,
        )
        .await
        {
            Ok((_parsed, result, artifact)) => {
                let json_value = http_execute_results_value(&result);
                let omitted = reference_only_omitted_field_names(&result, Some(sess.cgs.as_ref()));
                let handles: &[RunArtifactHandle] = match &artifact {
                    Some(h) => std::slice::from_ref(h),
                    None => &[],
                };
                let response_meta = tool_meta_from_handles(handles, &omitted);
                respond_execute_result(
                    kind,
                    json_value,
                    &result,
                    response_meta,
                    Some(sess.cgs.as_ref()),
                )
            }
            Err(RunLineError::Parse(d)) => problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(d),
            ),
            Err(RunLineError::Normalize(d)) => problem_response(
                Problem::custom(
                    ProblemStatus::BAD_REQUEST,
                    Uri::from_static(problem_types::EXECUTE_INVALID_EXPRESSION),
                )
                .with_title("Bad Request")
                .with_detail(d),
            ),
            Err(RunLineError::Projection(d)) => problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_PROJECTION_ENRICHMENT_FAILED),
                )
                .with_title("Internal Server Error")
                .with_detail(d),
            ),
            Err(RunLineError::Runtime(e, _src)) => {
                execution_failed_response(&e, line, &sess, &prompt_hash, &session_id, None, 1)
            }
            Err(RunLineError::ArtifactSerialization(e)) => problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                )
                .with_title("Internal Server Error")
                .with_detail(format!("artifact serialization failed: {e}")),
            ),
            Err(RunLineError::ArtifactPersist(d)) => problem_response(
                Problem::custom(
                    ProblemStatus::INTERNAL_SERVER_ERROR,
                    Uri::from_static(problem_types::EXECUTE_SERIALIZATION_FAILED),
                )
                .with_title("Internal Server Error")
                .with_detail(format!("run artifact persist failed: {d}")),
            ),
        };
    }

    let steps = match execute_staged_plasm_lines(
        &expressions,
        &sess,
        &st,
        session_id.as_str(),
        Some(&http_trace),
        None,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return http_staged_execute_error(e, &sess, &prompt_hash, &session_id),
    };
    let total = steps.len();
    let mut step_values = Vec::with_capacity(total);
    let mut step_tables = if kind == ExecResponseKind::Table {
        Some(Vec::with_capacity(total))
    } else {
        None
    };
    let mut step_artifacts: Vec<RunArtifactHandle> = Vec::new();
    let mut omitted_union: BTreeSet<String> = BTreeSet::new();
    let cgs = Some(sess.cgs.as_ref());
    for (_parsed, result, artifact) in &steps {
        if let Some(h) = artifact {
            step_artifacts.push(h.clone());
        }
        step_values.push(http_execute_results_value(result));
        if let Some(ref mut tabs) = step_tables {
            let (table, omitted, _) = format_result_with_cgs(result, OutputFormat::Table, cgs);
            omitted_union.extend(omitted);
            tabs.push(table);
        } else {
            omitted_union.extend(reference_only_omitted_field_names(result, cgs));
        }
    }
    let omitted_vec: Vec<String> = omitted_union.into_iter().collect();
    let steps_response_meta = tool_meta_from_handles(&step_artifacts, &omitted_vec);
    respond_staged_lines_execute_result(kind, step_values, step_tables, steps_response_meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http;
    use crate::incoming_auth::IncomingPrincipal;
    use axum::body::Body;
    use axum::extract::Extension;
    use axum::http::Request;
    use axum::Router;
    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
    use std::path::Path;
    use tower::util::ServiceExt;

    #[test]
    fn primary_entry_id_is_lexicographic_not_seed_insertion_order() {
        let seeds = vec![
            CapabilitySeed {
                entry_id: "zeta".into(),
                entity: "A".into(),
            },
            CapabilitySeed {
                entry_id: "alpha".into(),
                entity: "B".into(),
            },
        ];
        let grouped = group_seed_entities_by_entry(&seeds);
        assert_eq!(primary_entry_id_for_grouped(&grouped), "alpha");
    }

    #[test]
    fn capability_exposure_plan_is_invariant_to_seed_order() {
        let seeds_a = vec![
            CapabilitySeed {
                entry_id: "zeta".into(),
                entity: "A".into(),
            },
            CapabilitySeed {
                entry_id: "alpha".into(),
                entity: "B".into(),
            },
        ];
        let seeds_b = vec![
            CapabilitySeed {
                entry_id: "alpha".into(),
                entity: "B".into(),
            },
            CapabilitySeed {
                entry_id: "zeta".into(),
                entity: "A".into(),
            },
        ];
        let a =
            build_capability_exposure_plan(&normalize_capability_seeds(seeds_a)).expect("plan a");
        let b =
            build_capability_exposure_plan(&normalize_capability_seeds(seeds_b)).expect("plan b");
        assert_eq!(a, b);
        assert_eq!(a.primary_entry_id, "alpha");
        assert_eq!(
            a.process_order,
            vec!["alpha".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn plasm_plan_publication_renders_named_output_owner() {
        let out = publish_plasm_result_steps(
            None,
            None,
            &[PublishedResultStep {
                name: Some("sorted".to_string()),
                node_id: Some("p1".to_string()),
                entry_id: Some("pokemon".to_string()),
                entity: Some("Pokemon".to_string()),
                cgs: None,
                display: "Pokemon[id,name]".to_string(),
                projection: Some(vec!["id".to_string(), "name".to_string()]),
                result: ExecutionResult {
                    count: 0,
                    entities: vec![],
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 0,
                        cache_hits: 0,
                        cache_misses: 0,
                    },
                    request_fingerprints: vec![],
                },
                artifact: None,
            }],
        );
        assert!(out.markdown.contains("output: sorted -> p1"));
        assert!(out.markdown.contains("owner: pokemon.Pokemon"));
    }

    fn test_state_with_registry() -> PlasmHostState {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs.clone(),
        )]);
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        http::build_plasm_host_state(http::PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            plugin_manager: None,
            incoming_auth: None,
            run_artifacts: std::sync::Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
    }

    fn test_app_execute(st: PlasmHostState) -> Router<()> {
        execute_routes()
            .layer(Extension(st.clone()))
            .layer(Extension(IncomingPrincipal(None)))
    }

    #[tokio::test]
    async fn invalid_prompt_hash_path_segment_is_400() {
        let st = test_state_with_registry();
        let app = test_app_execute(st);
        // 63 hex digits — invalid length for SHA-256 hex (expect 64).
        let bad_hash = "a".repeat(63);
        let good_session = "0123456789abcdef0123456789abcdef";
        let uri = format!("/execute/{bad_hash}/{good_session}");
        let run = Request::builder()
            .method("POST")
            .uri(&uri)
            .header("accept", "application/json")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(run).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let ct = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.starts_with("application/problem+json"),
            "expected problem+json, got {ct:?}"
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            doc.get("type").and_then(|t| t.as_str()),
            Some(problem_types::EXECUTE_INVALID_PATH_PARAM)
        );
        assert!(
            doc.get("detail")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .contains("prompt_hash"),
            "detail should name prompt_hash: {doc:?}"
        );
    }

    async fn get_execute_session_json(
        app: &Router<()>,
        location_path: &str,
    ) -> CreateExecuteSessionResponse {
        let get = Request::builder()
            .method("GET")
            .uri(location_path)
            .header("accept", "application/json")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(get).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).expect("session JSON")
    }

    #[tokio::test]
    async fn create_session_then_bad_expression_is_400() {
        let st = test_state_with_registry();
        let app = test_app_execute(st.clone());

        let create = Request::builder()
            .method("POST")
            .uri("/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
            ))
            .unwrap();
        let res = app.clone().oneshot(create).await.unwrap();
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let loc = res
            .headers()
            .get(LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(loc.starts_with("/execute/"));

        let post_body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            post_body.is_empty(),
            "303 create must not include a body; session JSON is from GET {loc}"
        );

        let created = get_execute_session_json(&app, loc.as_str()).await;
        assert_eq!(created.entities, vec!["Profile"]);
        let expected_hash = PromptHashHex::from_prompt_sha256(&created.prompt);
        assert_eq!(created.prompt_hash, expected_hash.to_string());

        let run_uri = format!("/execute/{}/{}", created.prompt_hash, created.session);
        // Parse/type errors return problem+json without hitting the backend (Profile{} would need HTTP).
        let run = Request::builder()
            .method("POST")
            .uri(&run_uri)
            .header("accept", "application/json")
            .body(Body::from("@@@not-plasm"))
            .unwrap();
        let res2 = app.oneshot(run).await.unwrap();
        assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn staged_table_response_joins_sections() {
        let res = respond_staged_lines_execute_result(
            ExecResponseKind::Table,
            vec![serde_json::json!([]), serde_json::json!([1, 2])],
            Some(vec!["first_table".to_string(), "second_table".to_string()]),
            None,
        );
        assert_eq!(res.status(), StatusCode::OK);
        let ct = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.starts_with("text/plain"),
            "expected text/plain, got {ct:?}"
        );
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("---"),
            "expected table sections separated by ---: {text:?}"
        );
    }

    #[tokio::test]
    async fn staged_toon_response_is_outer_array() {
        let res = respond_staged_lines_execute_result(
            ExecResponseKind::Toon,
            vec![serde_json::json!(["a"]), serde_json::json!([])],
            None,
            None,
        );
        assert_eq!(res.status(), StatusCode::OK);
        let ct = res
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.starts_with("text/toon"),
            "expected text/toon, got {ct:?}"
        );
    }

    #[tokio::test]
    async fn staged_parse_error_names_step_index() {
        let st = test_state_with_registry();
        let app = test_app_execute(st.clone());
        let create = Request::builder()
            .method("POST")
            .uri("/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] }).to_string(),
            ))
            .unwrap();
        let res = app.clone().oneshot(create).await.unwrap();
        let loc = res
            .headers()
            .get(LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let created = get_execute_session_json(&app, loc.as_str()).await;
        let run_uri = format!("/execute/{}/{}", created.prompt_hash, created.session);
        let run = Request::builder()
            .method("POST")
            .uri(&run_uri)
            .header("accept", "application/json")
            .body(Body::from("@@@\nProfile{}"))
            .unwrap();
        let res2 = app.oneshot(run).await.unwrap();
        assert_eq!(res2.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(res2.into_body(), usize::MAX)
            .await
            .unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let detail = doc.get("detail").and_then(|d| d.as_str()).unwrap_or("");
        assert!(
            detail.contains("line 1 of 2:"),
            "expected line index in detail: {detail:?}"
        );
    }

    #[tokio::test]
    async fn same_inputs_same_prompt_hash() {
        let st = test_state_with_registry();
        let app = test_app_execute(st);

        async fn create_session(app: &Router<()>) -> CreateExecuteSessionResponse {
            let create = Request::builder()
                .method("POST")
                .uri("/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "entry_id": "overshow", "entities": ["Profile"] })
                        .to_string(),
                ))
                .unwrap();
            let res = app.clone().oneshot(create).await.unwrap();
            assert_eq!(res.status(), StatusCode::SEE_OTHER);
            let loc = res.headers().get(LOCATION).unwrap().to_str().unwrap();
            get_execute_session_json(app, loc).await
        }

        let a = create_session(&app).await;
        let b = create_session(&app).await;
        assert_eq!(a.prompt_hash, b.prompt_hash);
        assert_eq!(
            a.session, b.session,
            "server should reuse session id for same entry + entities"
        );
        assert!(!a.reused, "first GET should not set reused");
        assert!(
            !b.reused,
            "GET session JSON does not surface create-time reuse"
        );
    }

    #[tokio::test]
    async fn execute_session_create_marks_reused_on_second_open() {
        let st = test_state_with_registry();
        let body = CreateExecuteSessionBody {
            entry_id: "overshow".into(),
            entities: vec!["Profile".into()],
            principal: None,
            logical_session_id: None,
            context_intent: None,
            ranked_capabilities: None,
        };
        let first = execute_session_create_response(&st, None, body.clone())
            .await
            .expect("first open");
        assert!(!first.reused);
        let second = execute_session_create_response(&st, None, body)
            .await
            .expect("second open");
        assert!(second.reused);
        assert_eq!(first.prompt_hash, second.prompt_hash);
        assert_eq!(first.session, second.session);
    }

    #[tokio::test]
    async fn expand_domain_session_updates_session_entities() {
        let st = test_state_with_registry();
        let created = execute_session_create_response(
            &st,
            None,
            CreateExecuteSessionBody {
                entry_id: "overshow".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
            },
        )
        .await
        .expect("open");
        assert_eq!(created.entities, vec!["Profile"]);

        let first_wave = expand_execute_domain_session(
            &st,
            None,
            &created.prompt_hash,
            &created.session,
            vec![CapabilitySeed {
                entry_id: "overshow".into(),
                entity: "RecordedContent".into(),
            }],
        )
        .await
        .expect("expand");
        assert!(
            first_wave.contains("Added capabilities from overshow: RecordedContent"),
            "expected add line: {first_wave}"
        );
        assert!(
            first_wave.contains("```tsv"),
            "expected fenced DOMAIN (default TSV render): {first_wave}"
        );
        assert!(
            first_wave.contains("`e1`…`e2`"),
            "expected e1..eN reminder: {first_wave}"
        );

        let sess = st
            .sessions
            .get_by_strs(created.prompt_hash.as_str(), created.session.as_str())
            .await
            .expect("session");
        assert_eq!(
            sess.entities,
            vec!["Profile".to_string(), "RecordedContent".to_string()],
            "GET /execute and logs use cumulative exposed entities after expand"
        );

        let dup = expand_execute_domain_session(
            &st,
            None,
            &created.prompt_hash,
            &created.session,
            vec![CapabilitySeed {
                entry_id: "overshow".into(),
                entity: "RecordedContent".into(),
            }],
        )
        .await
        .expect("expand duplicate");
        assert!(dup.contains("already exposed"));
        assert!(
            !dup.contains("```tsv"),
            "no-op expand must not replay full DOMAIN fence: {dup}"
        );
        assert!(
            dup.contains("**not** repeated") || dup.contains("token-saving"),
            "expected no-op token-saving hint: {dup}"
        );
        let sess2 = st
            .sessions
            .get_by_strs(created.prompt_hash.as_str(), created.session.as_str())
            .await
            .expect("session");
        assert_eq!(
            sess2.entities,
            vec!["Profile".to_string(), "RecordedContent".to_string()]
        );
    }

    #[tokio::test]
    async fn unknown_entity_parse_error_includes_session_bounds() {
        let st = test_state_with_registry();
        let created = execute_session_create_response(
            &st,
            None,
            CreateExecuteSessionBody {
                entry_id: "overshow".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
            },
        )
        .await
        .expect("open");
        let err = execute_session_run_markdown(
            &st,
            None,
            &created.prompt_hash,
            &created.session,
            vec!["e9()".into()],
            None,
            None,
            None,
        )
        .await
        .expect_err("out-of-range e#");
        assert!(
            err.contains("unknown entity"),
            "expected unknown entity in {err:?}"
        );
        assert!(
            err.contains("e1..e1"),
            "expected session symbol bound hint in {err:?}"
        );
    }

    #[test]
    fn negotiate_accept_variants() {
        assert_eq!(negotiate_accept(None).unwrap(), ExecResponseKind::Toon);
        assert_eq!(negotiate_accept(Some("")).unwrap(), ExecResponseKind::Toon);
        assert_eq!(
            negotiate_accept(Some("*/*")).unwrap(),
            ExecResponseKind::Toon
        );
        assert_eq!(
            negotiate_accept(Some("application/json")).unwrap(),
            ExecResponseKind::Json
        );
        assert_eq!(
            negotiate_accept(Some("text/plain")).unwrap(),
            ExecResponseKind::Table
        );
        assert_eq!(
            negotiate_accept(Some("text/toon")).unwrap(),
            ExecResponseKind::Toon
        );
        assert_eq!(
            negotiate_accept(Some("application/x-ndjson")).unwrap(),
            ExecResponseKind::Ndjson
        );
        assert!(negotiate_accept(Some("application/soap+xml")).is_err());
    }
}
