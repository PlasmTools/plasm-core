//! MCP Streamable HTTP server (rust-mcp-sdk) over Plasm discovery + execute ([`crate::server_state::PlasmHostState`]).
//! Tool results use Markdown [`TextContent`]; **`plasm`** sets `CallToolResult._meta.plasm` for **plan-only**
//! dry-runs (`plan` + `guidance`); **`plasm_run`** performs live execution and may attach request fingerprints,
//! artifact URIs, and optional `lossy_summary_fields` per truncated step in `_meta.plasm`.
//! Run snapshot URIs in Markdown use logical-session short form `plasm://session/{logical_session_ref}/r/{n}`
//! (`s0`, `s1`, … per MCP transport; see [`crate::run_artifacts::plasm_session_short_resource_uri`]);
//! canonical `plasm://execute/.../run/{uuid}` remains accepted on read.
//! Tool results may include run snapshot URIs and inline hints when full data requires MCP `resources/read`;
//! the server repeats that obligation in the reply when it applies.
//!
//! Execute bindings (`plasm_context` → `plasm` / `plasm_run`) are stored **per agent logical session**
//! ([`PlasmExecBinding`]), keyed by canonical logical session UUID from `plasm_context` (client uses per-transport **`logical_session_ref`** slots: `s0`, `s1`, …).
//! One MCP transport may host **many** logical sessions; `MCP-Session-Id` is transport correlation only.
//! If the server-side execute session expires while the MCP transport stays open, the next
//! `plasm_context` opens a **new** `(prompt_hash, session_id)` and refreshes the binding.
//! For an active binding, additional `plasm_context` calls may **append** new `{api, entity}`
//! capability picks (`seeds`). Re-applying the **same** seeds after those entities are already exposed returns a
//! compact notice **without** replaying the full Plasm teaching-table / TSV text (token-saving); steady state is
//! `plasm` / `plasm_run` with `logical_session_ref`.
//! Do not shrink or rotate picks to “narrow” the session; that only makes sense when opening a new binding.
//! Tenant MCP policy
//! is enforced from `Authorization: Bearer <api_key>` (opaque key from control-plane provision) when tenant configs exist.
//! Tool text includes
//! the full Plasm instructions body only when the session is newly created server-side (`reused: false`); repeated
//! opens with the same entry + capability picks omit the instruction body to avoid token churn.
//! **Symbols:** for a fixed binding (`prompt_hash` + `session`), `e#` / `m#` / `p#` grow **append-only**
//! when you add new picks; they do not reshuffle. A new primary catalog open or logical session starts a new
//! symbol space — always read tokens from the current session `prompt` / Plasm language text.
//! A soft cap evicts one arbitrary older binding when the map grows past [`MAX_MCP_EXEC_BINDINGS`].
//!
//! Plasm language / instructions body (first update on `plasm_context` open plus append-only deltas when you add
//! capability picks via `plasm_context`'s `seeds`) is counted in Unicode scalar values per MCP transport session.
//! Each `plasm` / `plasm_run` call also accumulates invocation text (`program` plus optional `reasoning`) and,
//! on success, returned Markdown. Server logs use a rough **token estimate** ≈ `ceil(chars / 4)` per
//! bucket (`prompt` / `invocation` / `tool_response`). When the session leaves the SDK session store,
//! an `INFO` line logs cumulative character totals and token estimates (`plasm_agent::mcp`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::discovery_human_format::{format_discovery_markdown_for_mcp, DiscoveryTablePolicy};
use crate::trace_hub::{CodePlanTrace, McpPlasmTraceSink, PlasmContextTrace};
use std::time::Duration;
use tracing::Instrument;

use async_trait::async_trait;
use base64::Engine as _;
use plasm_core::discovery::{CapabilityQuery, CgsCatalog, DiscoveryError};
use plasm_core::CgsDiscovery;
use plasm_core::PlanCommitRef;
use plasm_discovery::DiscoveryQuery;
use plasm_runtime::CancelSignal;
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::event_store::InMemoryEventStore;
use rust_mcp_sdk::mcp_server::hyper_server;
use rust_mcp_sdk::mcp_server::{
    HyperServer, HyperServerOptions, ServerHandler, ToMcpServerHandler,
};
use rust_mcp_sdk::schema::schema_utils::{CallToolError, CustomNotification};
use rust_mcp_sdk::schema::{
    BlobResourceContents, CallToolRequestParams, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceContent, ReadResourceRequestParams,
    ReadResourceResult, ResourceTemplate, RpcError, ServerCapabilities,
    ServerCapabilitiesResources, ServerCapabilitiesTools, TextContent, TextResourceContents, Tool,
    ToolAnnotations, ToolExecution, ToolExecutionTaskSupport, ToolInputSchema,
};
use rust_mcp_sdk::McpServer;
use tokio::sync::{Mutex, RwLock};

use crate::execute_pipeline::{ExecutePipeline, ExecutionIntent};
use crate::execute_session::ExecuteSession;
use crate::http_execute::{
    apply_capability_seeds, build_plasm_context_agent_markdown, build_plasm_context_tool_meta,
    normalize_capability_seeds, try_dispatch_operation_program, ApplyCapabilitySeedsOutcome,
    CapabilitySeed, RankedCapabilitiesArg,
};
use crate::incoming_auth::{tenant_scope, IncomingAuthMethod, IncomingAuthMode, TenantPrincipal};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_policy;
use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::mcp_stream_auth::{config_id_from_auth_info, is_anonymous_mcp_auth};
use crate::operation::{
    async_live_run_accept_parts, compute_plan_commit_id_from_dry, live_run_should_auto_async,
    op_accept_context_from_executable, plan_commit_meta, plan_requires_review_gate,
    should_spawn_async_live_run, spawn_async_plan_run, verify_plan_commit_for_dry,
    PlanCommitRecord, PLAN_COMMIT_TTL,
};
use crate::plan_dry_display::build_plan_dry_compact_view;
use crate::plasm_comp_wire::plasm_comp_json_from_dry;
use crate::plasm_compile::compile_plasm_expression;
use crate::plasm_plan_run::{
    evaluate_plasm_comp_dry, render_plasm_plan_dry_text_for_session, PlasmPlanRunHooks,
    PlasmPlanRunResult,
};
use crate::run_artifacts::{
    code_plan_handle, code_plan_http_path, parse_plasm_execute_run_uri,
    parse_plasm_session_short_resource_uri, plasm_code_plan_resource_uri,
    plasm_session_short_plan_uri, ArtifactPayload, CodePlanArchiveDocument,
    LogicalSessionUriSegment,
};
use crate::server_state::PlasmHostState;
use crate::session_identity::{ClientSessionKey, LogicalSessionId};
use crate::trace_sink_emit::PlasmTraceContext;
use crate::typed_discovery_host::run_typed_catalog_discovery;
use chrono::Utc;
use plasm_trace::RunArtifactArchiveRef;
use serde_json::json;
use uuid::Uuid;

/// Best-effort bound on concurrent MCP transport sessions holding an execute binding (see module doc).
const MAX_MCP_EXEC_BINDINGS: usize = 512;

/// Model-facing **`plasm`** tool description: **plan-only** program construction (session setup is in initialize instructions).
pub(crate) const MCP_PLASM_TOOL_DESCRIPTION: &str = include_str!("mcp_prompt/plasm_tool_head.txt");

/// Model-facing **`plasm_run`** tool description: live execution after plan review.
pub(crate) const MCP_PLASM_RUN_TOOL_DESCRIPTION: &str =
    include_str!("mcp_prompt/plasm_run_tool.txt");

/// Model-facing **`plasm_context`** tool description (teaching TSV + continuity; federation in initialize workflow).
pub(crate) const MCP_PLASM_CONTEXT_TOOL_DESCRIPTION: &str =
    include_str!("mcp_prompt/plasm_context_tool.txt");

/// One-line JSON-schema description for the shared **`program`** parameter on **`plasm`** / **`plasm_run`**.
pub(crate) const MCP_PROGRAM_PARAM_DESCRIPTION: &str = "Plasm program JSON string (`e#`/`m#`/`p#`/`r#`). Newlines preferred; coerced single-liners supported — see initialize **program contract** + `plasm_context` TSV.";

/// MCP initialize workflow text (orchestration + program contract).
pub(crate) const MCP_SERVER_INITIALIZE_WORKFLOW: &str = concat!(
    include_str!("mcp_prompt/workflow_head.txt"),
    include_str!("mcp_prompt/program_contract.txt"),
    include_str!("mcp_prompt/workflow_tail.txt"),
);

fn mcp_server_initialize_instructions() -> String {
    MCP_SERVER_INITIALIZE_WORKFLOW.to_string()
}

fn parse_tool_seeds(
    tool: &str,
    v: &serde_json::Value,
) -> Result<Vec<CapabilitySeed>, CallToolError> {
    if v.get("seeds").is_none() && (v.get("entry_id").is_some() || v.get("entities").is_some()) {
        return Err(CallToolError::invalid_arguments(
            tool,
            Some(
                "missing capability picks: `plasm_context` requires a non-empty `seeds` array of `{api, entity}` objects (`entry_id` per object is accepted instead of `api`); old top-level `{entry_id, entities}` is not supported"
                    .into(),
            ),
        ));
    }
    let seeds: Vec<CapabilitySeed> = serde_json::from_value(
        v.get("seeds")
            .cloned()
            .ok_or_else(|| {
                CallToolError::invalid_arguments(
                    tool,
                    Some(
                        "missing capability picks: expected non-empty `seeds` array of `{api, entity}` objects (`entry_id` key accepted per object)".into(),
                    ),
                )
            })?,
    )
    .map_err(|e| CallToolError::invalid_arguments(tool, Some(e.to_string())))?;
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err(CallToolError::invalid_arguments(
            tool,
            Some("`seeds` must be a non-empty array of capability picks: `{api, entity}` per object (`entry_id` key accepted per object)".into()),
        ));
    }
    Ok(seeds)
}

fn parse_plasm_context_ranked_capabilities(
    tool: &str,
    v: &serde_json::Value,
) -> Result<RankedCapabilitiesArg, CallToolError> {
    match v.get("ranked_capabilities") {
        None => Ok(RankedCapabilitiesArg::Unspecified),
        Some(serde_json::Value::Null) => Ok(RankedCapabilitiesArg::Set(None)),
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let s = item.as_str().ok_or_else(|| {
                    CallToolError::invalid_arguments(
                        tool,
                        Some(format!("ranked_capabilities[{i}] must be a string")),
                    )
                })?;
                out.push(s.to_string());
            }
            Ok(RankedCapabilitiesArg::Set(Some(out)))
        }
        Some(_) => Err(CallToolError::invalid_arguments(
            tool,
            Some("`ranked_capabilities` must be null or an array of strings".into()),
        )),
    }
}

pub(crate) fn parse_optional_principal(v: &serde_json::Value) -> Option<String> {
    v.get("principal")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn parse_logical_session_ref_arg(
    tool: &str,
    v: &serde_json::Value,
) -> Result<String, CallToolError> {
    let s = v
        .get("logical_session_ref")
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            CallToolError::invalid_arguments(
                tool,
                Some("missing `logical_session_ref`: call `plasm_context` first".into()),
            )
        })?;
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('s') && t[1..].chars().all(|c| c.is_ascii_digit()) {
        Ok(t.to_string())
    } else {
        Err(CallToolError::invalid_arguments(
            tool,
            Some(
                "invalid `logical_session_ref`: expected a slot id like `s0` or `s1` from `plasm_context`"
                    .into(),
            ),
        ))
    }
}

fn comp_content_sha256_hex(comp: &serde_json::Value) -> String {
    crate::evidence_chain::semantic_comp_commit_hex_from_json(comp)
}

fn plan_display_name_from_comp(comp: &serde_json::Value) -> String {
    comp.get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "unnamed plan".to_string())
}

fn plan_node_count_from_comp(comp: &serde_json::Value) -> usize {
    comp.get("steps")
        .and_then(|s| s.as_object())
        .map(|m| m.len())
        .or_else(|| {
            comp.get("bind")
                .and_then(|b| b.get("topo"))
                .and_then(|t| t.as_array())
                .map(|a| a.len())
        })
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)] // trace archive carries full execute-session + plan context
async fn trace_archive_and_emit_code_plan_evaluate(
    hub: &crate::trace_hub::TraceHub,
    store: &crate::run_artifacts::RunArtifactStore,
    mcp_key: &str,
    es: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    session_ref: &str,
    comp: &serde_json::Value,
    program: &str,
    comp_summary: serde_json::Value,
    plan_call_index: u64,
) {
    let plan_hash_str = comp_content_sha256_hex(comp);
    let plan_id = Uuid::new_v4();
    let plan_index = plan_call_index;
    let handle_str = code_plan_handle(plan_index);
    let doc = CodePlanArchiveDocument {
        kind: "code_plan".into(),
        plan_id: plan_id.to_string(),
        prompt_hash: prompt_hash.to_string(),
        session_id: session_id.to_string(),
        entry_id: es.entry_id.clone(),
        plan_index,
        plan_handle: handle_str.clone(),
        name: plan_display_name_from_comp(comp),
        code: program.to_string(),
        plan_hash: plan_hash_str.clone(),
        comp: comp.clone(),
        catalog_cgs_hash: es.catalog_cgs_hash.clone(),
        domain_revision: es.domain_revision,
        entities: es.entities.clone(),
        principal: es.principal.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let (plan_uri, canonical_plan_uri, plan_http_path) = match store
        .insert_code_plan(prompt_hash, session_id, plan_id, plan_index, &doc)
        .await
    {
        Ok(h) => (h.plasm_uri, h.canonical_plasm_uri, h.http_path),
        Err(e) => {
            tracing::warn!(
                target: "plasm_agent::mcp",
                error = %e,
                "failed to archive Plasm program plan for trace (non-fatal)"
            );
            (
                plasm_session_short_plan_uri(session_ref, plan_index),
                plasm_code_plan_resource_uri(prompt_hash, session_id, &plan_id),
                code_plan_http_path(prompt_hash, session_id, &plan_id),
            )
        }
    };
    let node_count = plan_node_count_from_comp(comp);
    let code_chars = program.chars().count() as u64;
    hub.trace_record_code_plan_evaluate(
        mcp_key,
        CodePlanTrace {
            plan_handle: handle_str,
            plan_id: plan_id.to_string(),
            plan_name: plan_display_name_from_comp(comp),
            plan_hash: plan_hash_str,
            plan_uri,
            canonical_plan_uri,
            plan_http_path,
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
            node_count,
            code_chars,
            comp: comp_summary,
            plasm_call_index: None,
            run_ids: Vec::new(),
            run_artifacts: Vec::new(),
        },
    )
    .await;
}

#[allow(clippy::too_many_arguments)] // trace archive carries full execute-session + plan context
async fn trace_archive_and_emit_code_plan_execute(
    hub: &crate::trace_hub::TraceHub,
    store: &crate::run_artifacts::RunArtifactStore,
    mcp_key: &str,
    es: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    session_ref: &str,
    comp: &serde_json::Value,
    program: &str,
    comp_summary: serde_json::Value,
    plan_call_index: u64,
    out: &PlasmPlanRunResult,
) {
    let plan_hash_str = comp_content_sha256_hex(comp);
    let plan_id = Uuid::new_v4();
    let plan_index = plan_call_index;
    let handle_str = code_plan_handle(plan_index);
    let doc = CodePlanArchiveDocument {
        kind: "code_plan".into(),
        plan_id: plan_id.to_string(),
        prompt_hash: prompt_hash.to_string(),
        session_id: session_id.to_string(),
        entry_id: es.entry_id.clone(),
        plan_index,
        plan_handle: handle_str.clone(),
        name: plan_display_name_from_comp(comp),
        code: program.to_string(),
        plan_hash: plan_hash_str.clone(),
        comp: comp.clone(),
        catalog_cgs_hash: es.catalog_cgs_hash.clone(),
        domain_revision: es.domain_revision,
        entities: es.entities.clone(),
        principal: es.principal.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let (plan_uri, canonical_plan_uri, plan_http_path) = match store
        .insert_code_plan(prompt_hash, session_id, plan_id, plan_index, &doc)
        .await
    {
        Ok(h) => (h.plasm_uri, h.canonical_plasm_uri, h.http_path),
        Err(e) => {
            tracing::warn!(
                target: "plasm_agent::mcp",
                error = %e,
                "failed to archive Plasm program plan for trace (non-fatal)"
            );
            (
                plasm_session_short_plan_uri(session_ref, plan_index),
                plasm_code_plan_resource_uri(prompt_hash, session_id, &plan_id),
                code_plan_http_path(prompt_hash, session_id, &plan_id),
            )
        }
    };
    let node_count = plan_node_count_from_comp(comp);
    let code_chars = program.chars().count() as u64;
    let run_ids: Vec<String> = out
        .code_plan_run_artifacts
        .iter()
        .map(|a| a.run_id.clone())
        .collect();
    hub.trace_record_code_plan_execute(
        mcp_key,
        CodePlanTrace {
            plan_handle: handle_str,
            plan_id: plan_id.to_string(),
            plan_name: plan_display_name_from_comp(comp),
            plan_hash: plan_hash_str,
            plan_uri,
            canonical_plan_uri,
            plan_http_path,
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
            node_count,
            code_chars,
            comp: comp_summary,
            plasm_call_index: Some(plan_call_index),
            run_ids,
            run_artifacts: out.code_plan_run_artifacts.clone(),
        },
    )
    .await;
}

/// Per MCP transport session: Plasm execute `prompt_hash` + `session` ids (same as HTTP paths).
#[derive(Clone, Default)]
pub(crate) struct PlasmExecBinding {
    pub(crate) prompt_hash: String,
    pub(crate) session_id: String,
}

/// Cumulative MCP-side text volume for token-ish telemetry (Unicode scalar counts).
#[derive(Clone, Default, Debug)]
pub(crate) struct McpSessionPlasmStats {
    /// Plasm instructions body from `plasm_context` tool results.
    teaching_prompt_chars: u64,
    /// `plasm` / `plasm_run` tool payloads: program string plus optional `reasoning`.
    plasm_invocation_chars: u64,
    /// Successful `plasm` / `plasm_run` tool Markdown bodies.
    plasm_response_chars: u64,
    plasm_call_count: u64,
}

#[derive(Default)]
pub(crate) struct McpLogicalSessionState {
    pub(crate) binding: Option<PlasmExecBinding>,
    stats: McpSessionPlasmStats,
    meta_index: PlasmMetaIndex,
}

#[derive(Default)]
pub(crate) struct McpTransportState {
    /// Logical session UUID string → per-agent state (execute binding, stats, `_meta.plasm` index).
    logical_by_id: HashMap<String, Arc<Mutex<McpLogicalSessionState>>>,
    /// Client-facing slot ids on this MCP transport (`s0`, …) → canonical logical session UUID.
    ref_to_uuid: HashMap<String, Uuid>,
    uuid_to_ref: HashMap<Uuid, String>,
    next_session_slot: u64,
}

impl McpTransportState {
    /// Assign a stable per-transport slot (`s{n}`) for this canonical logical id (idempotent).
    pub(crate) fn ensure_session_ref(&mut self, uuid: Uuid) -> String {
        if let Some(r) = self.uuid_to_ref.get(&uuid) {
            return r.clone();
        }
        let r = format!("s{}", self.next_session_slot);
        self.next_session_slot = self.next_session_slot.saturating_add(1);
        self.ref_to_uuid.insert(r.clone(), uuid);
        self.uuid_to_ref.insert(uuid, r.clone());
        r
    }
}

/// Rough token estimate for logging (Latin-heavy text; not a billing tokenizer).
#[inline]
fn mcp_chars_to_token_est(chars: u64) -> u64 {
    chars.saturating_add(3) / 4
}

/// Per `plasm` / `plasm_run` call: count program + optional reasoning for invocation telemetry.
fn plasm_invocation_char_count(program: &str, reasoning: Option<&str>) -> u64 {
    let mut n = program.chars().count() as u64;
    if let Some(r) = reasoning {
        n = n.saturating_add(r.chars().count() as u64);
    }
    n
}

pub(crate) struct PlasmMcpHandler {
    pub(crate) plasm: Arc<PlasmHostState>,
    /// MCP transport session key -> per-session mutable state.
    session_states: Arc<RwLock<HashMap<String, Arc<Mutex<McpTransportState>>>>>,
}

impl PlasmMcpHandler {
    pub(crate) fn new(plasm: Arc<PlasmHostState>) -> Self {
        Self {
            plasm,
            session_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) async fn session_state(&self, key: &str) -> Arc<Mutex<McpTransportState>> {
        {
            let g = self.session_states.read().await;
            if let Some(state) = g.get(key) {
                return Arc::clone(state);
            }
        }
        let mut g = self.session_states.write().await;
        Arc::clone(
            g.entry(key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(McpTransportState::default()))),
        )
    }

    pub(crate) async fn logical_mutex(
        &self,
        transport_key: &str,
        logical_id: &str,
    ) -> Arc<Mutex<McpLogicalSessionState>> {
        let transport = self.session_state(transport_key).await;
        {
            let g = transport.lock().await;
            if let Some(entry) = g.logical_by_id.get(logical_id) {
                return Arc::clone(entry);
            }
        }
        let mut g = transport.lock().await;
        Arc::clone(
            g.logical_by_id
                .entry(logical_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(McpLogicalSessionState::default()))),
        )
    }

    pub(crate) async fn resolve_logical_session_ref_to_uuid(
        &self,
        tool: &str,
        transport_key: &str,
        ref_str: &str,
    ) -> Result<Uuid, CallToolError> {
        let transport = self.session_state(transport_key).await;
        let g = transport.lock().await;
        g.ref_to_uuid.get(ref_str).copied().ok_or_else(|| {
            CallToolError::invalid_arguments(
                tool,
                Some(
                    "unknown `logical_session_ref`: call `plasm_context` on this MCP connection first"
                        .into(),
                ),
            )
        })
    }

    /// Resolve execute binding: in-memory per-logical row first, then shared `logical_execute_bindings`.
    ///
    /// **Locking:** drop the per-logical mutex before reading `logical_execute_bindings` so we never
    /// nest that mutex with the host `RwLock` (consistent lock order vs writers elsewhere).
    pub(crate) async fn resolve_binding_for_logical(
        &self,
        transport_key: &str,
        logical_uuid: Uuid,
    ) -> Option<PlasmExecBinding> {
        let lid = logical_uuid.to_string();
        let ls = self.logical_mutex(transport_key, &lid).await;
        let g = ls.lock().await;
        if let Some(b) = &g.binding {
            return Some(b.clone());
        }
        drop(g);
        let map = self.plasm.logical_execute_bindings.read().await;
        map.get(&logical_uuid).map(|(ph, sid)| PlasmExecBinding {
            prompt_hash: ph.clone(),
            session_id: sid.clone(),
        })
    }

    async fn mcp_plasm_token_snapshot_logical(
        &self,
        transport_key: &str,
        logical_id: &str,
    ) -> (u64, u64, u64, u64) {
        let ls = self.logical_mutex(transport_key, logical_id).await;
        let g = ls.lock().await;
        let tp = mcp_chars_to_token_est(g.stats.teaching_prompt_chars);
        let ti = mcp_chars_to_token_est(g.stats.plasm_invocation_chars);
        let tr = mcp_chars_to_token_est(g.stats.plasm_response_chars);
        (tp, ti, tr, tp.saturating_add(ti).saturating_add(tr))
    }

    /// Latest tenant MCP policy for this transport session (from HTTP `Authorization` + control-plane store).
    pub(crate) async fn tenant_mcp_cfg(
        &self,
        runtime: &Arc<dyn McpServer>,
    ) -> Result<Option<Arc<McpRuntimeConfig>>, CallToolError> {
        let has_tenant_configs = match self.plasm.mcp_config_repository() {
            Some(r) => r.has_tenant_configs().await.unwrap_or(false),
            None => false,
        };
        let auth = runtime.auth_info_cloned().await;
        let Some(info) = auth else {
            if has_tenant_configs {
                return Err(CallToolError::from_message(
                    "MCP Authorization required: send `Authorization: Bearer <api_key>` (tenant MCP API key from control plane).",
                ));
            }
            return Ok(None);
        };

        if is_anonymous_mcp_auth(&info) {
            return Ok(None);
        }

        let Some(id) = config_id_from_auth_info(&info) else {
            if has_tenant_configs {
                return Err(CallToolError::from_message(
                    "MCP Authorization missing tenant binding (expected Bearer API key).",
                ));
            }
            return Ok(None);
        };

        let Some(repo) = self.plasm.mcp_config_repository() else {
            return Ok(None);
        };

        let Some(cfg) = repo.get_runtime_config(&id).await.map_err(|_| {
            CallToolError::from_message(
                "Tenant MCP configuration store failed while loading policy.",
            )
        })?
        else {
            return Err(CallToolError::from_message(
                "Tenant MCP configuration is no longer available (disabled or revoked on the agent).",
            ));
        };
        if cfg.space_type == "personal" && cfg.owner_subject.is_none() {
            return Err(CallToolError::from_message(
                "Personal MCP configuration is missing owner binding metadata. Re-provision from control plane.",
            ));
        }

        Ok(Some(Arc::new(cfg)))
    }

    async fn mcp_principal_from_transport_auth(
        &self,
        runtime: &Arc<dyn McpServer>,
    ) -> Option<TenantPrincipal> {
        let info = runtime.auth_info_cloned().await?;
        let tenant_id = info.client_id?;
        let subject = info.user_id?;
        if tenant_id.trim().is_empty() || subject.trim().is_empty() {
            return None;
        }
        let method = if info
            .extra
            .as_ref()
            .and_then(|m| m.get("plasm_mcp_oauth"))
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            IncomingAuthMethod::Jwt
        } else {
            IncomingAuthMethod::ApiKey
        };
        Some(TenantPrincipal {
            tenant_id,
            subject,
            method,
        })
    }

    fn incoming_mode(&self) -> IncomingAuthMode {
        self.plasm
            .incoming_auth
            .as_ref()
            .map(|v| v.mode())
            .unwrap_or(IncomingAuthMode::Off)
    }

    /// Ensures MCP tool calls satisfy `PLASM_INCOMING_AUTH_MODE` (principal from MCP transport auth: API key / OAuth).
    pub(crate) async fn ensure_mcp_principal(
        &self,
        _mcp_key: &str,
        runtime: &Arc<dyn McpServer>,
    ) -> Result<Option<TenantPrincipal>, CallToolError> {
        let mode = self.incoming_mode();
        let p = self.mcp_principal_from_transport_auth(runtime).await;
        if mode == IncomingAuthMode::Required && p.is_none() {
            return Err(CallToolError::from_message(
                "incoming auth required: authenticate the MCP transport with a valid bearer credential",
            ));
        }
        Ok(p)
    }

    async fn trace_session_meta(
        &self,
        _mcp_key: &str,
        runtime: &Arc<dyn McpServer>,
    ) -> crate::trace_hub::TraceSessionMeta {
        use crate::trace_hub::{McpConfigRef, TraceSessionMeta};
        let tenant_incoming = self
            .mcp_principal_from_transport_auth(runtime)
            .await
            .map(|p| p.tenant_id);
        let (tenant_mcp, mcp_config) = match self.tenant_mcp_cfg(runtime).await {
            Ok(Some(cfg)) => (
                Some(cfg.tenant_id.clone()),
                Some(McpConfigRef {
                    config_id: cfg.id.to_string(),
                    tenant_id: cfg.tenant_id.clone(),
                }),
            ),
            _ => (None, None),
        };
        let tenant_id = tenant_incoming
            .or(tenant_mcp)
            .unwrap_or_else(|| "anonymous".to_string());
        TraceSessionMeta {
            tenant_id,
            project_slug: "main".into(),
            mcp_config,
        }
    }

    fn plasm_tools() -> Vec<Tool> {
        let mut init_props = BTreeMap::new();
        init_props.insert(
            "intent".into(),
            json_schema_string_type(
                "Stable string for one user goal or agent context (e.g. one id for the whole chat). Same intent + tenant reuses the same logical session—do not rotate a new value every user message.",
            ),
        );
        let mut discover_props = BTreeMap::new();
        discover_props.insert(
            "intent".into(),
            json_schema_non_empty_string_type(
                "One plain-language task description for the whole user goal. See MCP initialize workflow for discover orchestration.",
            ),
        );
        discover_props.insert(
            "typed".into(),
            json_schema_bool_type(
                "If **true**, response is fenced **`json`** (`DiscoveryDecision`) for structured disambiguation **instead of the default TSV table**. Leave unset/false for normal discovery.",
            ),
        );
        discover_props.insert(
            "max_options".into(),
            serde_json::from_value(serde_json::json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 32,
                "description": "Typed mode: max clarification options (default 8)."
            }))
            .expect("max_options schema"),
        );
        discover_props.insert(
            "enable_embeddings".into(),
            json_schema_bool_type(
                "Typed mode: when **true**, request local embedding rerank (requires binary built with `local-embeddings`; OSS release builds are lexical-only). Default **false**.",
            ),
        );
        discover_props.insert(
            "allowed_entry_ids".into(),
            serde_json::from_value(serde_json::json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Typed mode: optional restrict list of registry `entry_id`s."
            }))
            .expect("allowed_entry_ids schema"),
        );
        let mut context_props = init_props;
        context_props.insert(
            "seeds".into(),
            json_schema_non_empty_object_array(
                "Non-empty array of `{api, entity}` capability picks (or `{entry_id, entity}`). See MCP initialize workflow and `plasm_context` tool description.",
                vec!["api", "entity"],
            ),
        );
        context_props.insert(
            "ranked_capabilities".into(),
            serde_json::from_value(serde_json::json!({
                "type": ["array", "null"],
                "items": { "type": "string" },
                "description": "Optional capability **wire names** (e.g. from `discover_capabilities`). When non-empty, **non-seeded** mutators must appear in this list and score against **`intent`**. Seeded entities always teach **query/search/get** (and `primary_read`); **create/update/delete/action** need intent overlap (read-first open defers weak matches). Omit on expand to keep the session list; send **`null`** or **`[]`** to clear."
            }))
            .expect("ranked_capabilities schema"),
        );
        let mut plasm_program_props = BTreeMap::new();
        plasm_program_props.insert(
            "logical_session_ref".into(),
            json_schema_string_type(
                "Same `logical_session_ref` returned by `plasm_context`. Reuse for follow-up `plasm` (plan) and `plasm_run` (execute) calls.",
            ),
        );
        plasm_program_props.insert(
            "program".into(),
            json_schema_string_type(MCP_PROGRAM_PARAM_DESCRIPTION),
        );
        plasm_program_props.insert(
            "reasoning".into(),
            json_schema_string_type("Optional short note explaining the intent of this call."),
        );
        let mut plasm_run_props = plasm_program_props.clone();
        plasm_run_props.insert(
            "wait".into(),
            json_schema_bool_type(
                "When **false**, start live execute in the background and return an operation handle immediately (`wait(sN_oM)`). Default **true** for bounded ok plans; **review** plans auto-async even when omitted.",
            ),
        );
        plasm_run_props.insert(
            "force".into(),
            json_schema_bool_type(
                "Bypass the dry-run **review** soft gate for unbounded or risky plans. Prefer `plan_commit_ref` from a prior `plasm` dry-run when possible.",
            ),
        );
        plasm_run_props.insert(
            "plan_commit_ref".into(),
            json_schema_string_type(
                "Plan acceptance token (`pcN`) from a matching `plasm` dry-run. Required when dry verdict is **review** unless `force: true`.",
            ),
        );

        let mut tools = vec![
            Tool {
                name: "plasm_context".into(),
                title: Some("Open or extend Plasm context".into()),
                description: Some(MCP_PLASM_CONTEXT_TOOL_DESCRIPTION.into()),
                input_schema: ToolInputSchema::new(
                    vec!["intent".into(), "seeds".into()],
                    Some(context_props),
                    None,
                ),
                annotations: Some(ToolAnnotations {
                    read_only_hint: Some(false),
                    open_world_hint: Some(true),
                    ..Default::default()
                }),
                execution: Some(ToolExecution {
                    task_support: Some(ToolExecutionTaskSupport::Forbidden),
                }),
                icons: vec![],
                meta: None,
                output_schema: None,
            },
            Tool {
                name: "discover_capabilities".into(),
                title: Some("Resolve intent to capabilities".into()),
                description: Some(
                    "Resolve one user goal to catalog capabilities. **Default:** fenced **`tsv`** table (`api`, `entity`, `description`). **One `intent` string per goal** — see MCP initialize workflow. Skip when you already know every `api`/`entity`. Set **`typed: true`** only when the TSV ambiguity note requires structured disambiguation (returns fenced **`json`** instead).".into(),
                ),
                input_schema: ToolInputSchema::new(vec!["intent".into()], Some(discover_props), None),
                annotations: Some(ToolAnnotations {
                    read_only_hint: Some(true),
                    open_world_hint: Some(true),
                    ..Default::default()
                }),
                execution: Some(ToolExecution {
                    task_support: Some(ToolExecutionTaskSupport::Forbidden),
                }),
                icons: vec![],
                meta: None,
                output_schema: None,
            },
        ];
        tools.push(Tool {
            name: "plasm".into(),
            title: Some("Plan Plasm (dry-run)".into()),
            description: Some(MCP_PLASM_TOOL_DESCRIPTION.into()),
            input_schema: ToolInputSchema::new(
                vec!["logical_session_ref".into(), "program".into()],
                Some(plasm_program_props.clone()),
                None,
            ),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                open_world_hint: Some(true),
                ..Default::default()
            }),
            execution: Some(ToolExecution {
                task_support: Some(ToolExecutionTaskSupport::Forbidden),
            }),
            icons: vec![],
            meta: Some(crate::plan_ui_mcp::plan_review_ui_tool_meta()),
            output_schema: None,
        });
        tools.push(Tool {
            name: "plasm_run".into(),
            title: Some("Run Plasm (execute)".into()),
            description: Some(MCP_PLASM_RUN_TOOL_DESCRIPTION.into()),
            input_schema: ToolInputSchema::new(
                vec!["logical_session_ref".into(), "program".into()],
                Some(plasm_run_props),
                None,
            ),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(false),
                open_world_hint: Some(true),
                ..Default::default()
            }),
            execution: Some(ToolExecution {
                task_support: Some(ToolExecutionTaskSupport::Forbidden),
            }),
            icons: vec![],
            meta: Some(crate::run_explorer_ui_mcp::run_explorer_ui_tool_meta()),
            output_schema: None,
        });
        tools.push(Tool {
            name: "plasm_ui_list_catalogs".into(),
            title: Some("List tenant-enabled catalogs (MCP App)".into()),
            description: Some(
                "Returns registry `entry_id`s allowed by tenant MCP policy for MCP App bootstrap UIs."
                    .into(),
            ),
            input_schema: ToolInputSchema::new(vec![], Some(BTreeMap::new()), None),
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                open_world_hint: Some(false),
                ..Default::default()
            }),
            execution: Some(ToolExecution {
                task_support: Some(ToolExecutionTaskSupport::Forbidden),
            }),
            icons: vec![],
            meta: Some(
                serde_json::json!({
                    "ui": { "visibility": ["app"] }
                })
                .as_object()
                .cloned()
                .expect("plasm_ui_list_catalogs meta"),
            ),
            output_schema: None,
        });
        tools.extend(crate::workflow_mcp::workflow_mcp_tools());
        tools
    }
}

pub(crate) fn json_schema_bool_type(
    description: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::json!("boolean"));
    m.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    m
}

pub(crate) fn json_schema_string_type(
    description: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::json!("string"));
    m.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    m
}

pub(crate) fn json_schema_non_empty_string_type(
    description: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = json_schema_string_type(description);
    m.insert("minLength".into(), serde_json::json!(1));
    m
}

fn json_schema_non_empty_object_array(
    description: &str,
    required_fields: Vec<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut item_props = serde_json::Map::new();
    item_props.insert(
        "api".into(),
        serde_json::Value::Object(json_schema_string_type(
            "**`api`**: integration id from the discovery table (or any id you already know). **`entry_id`** is an alternate JSON key for the same value",
        )),
    );
    item_props.insert(
        "entity".into(),
        serde_json::Value::Object(json_schema_string_type(
            "**`entity`**: resource type name from the discovery table (or a name you already know)",
        )),
    );
    let mut item_obj = serde_json::Map::new();
    item_obj.insert("type".into(), serde_json::json!("object"));
    item_obj.insert("properties".into(), serde_json::Value::Object(item_props));
    item_obj.insert(
        "required".into(),
        serde_json::Value::Array(
            required_fields
                .into_iter()
                .map(|f| serde_json::Value::String(f.to_string()))
                .collect(),
        ),
    );
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::json!("array"));
    m.insert("items".into(), serde_json::Value::Object(item_obj));
    m.insert("minItems".into(), serde_json::json!(1));
    m.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    m
}

fn args_value(params: &CallToolRequestParams) -> serde_json::Value {
    serde_json::Value::Object(params.arguments.clone().unwrap_or_default())
}

/// MCP `discover_capabilities`: `intent` is exactly one non-empty task description string.
fn mcp_discover_query_from_arguments(v: &serde_json::Value) -> Result<CapabilityQuery, String> {
    let Some(obj) = v.as_object() else {
        return Err("discover_capabilities arguments must be a JSON object".to_string());
    };
    if obj.contains_key("query") || obj.contains_key("utterance") {
        return Err(
            "discover_capabilities now requires `intent` as a single string; `query`/`utterance` are not accepted"
                .to_string(),
        );
    }
    let intent = match obj.get("intent") {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Some(serde_json::Value::String(_)) | None | Some(serde_json::Value::Null) => {
            return Err("discover_capabilities `intent` must be a non-empty string".to_string());
        }
        Some(_) => {
            return Err("discover_capabilities `intent` must be a single string".to_string());
        }
    };
    Ok(CapabilityQuery {
        tokens: vec![intent],
        phrases: vec![],
        ..CapabilityQuery::default()
    })
}

fn discovery_mcp_error(e: DiscoveryError) -> CallToolError {
    match e {
        DiscoveryError::EmptyQuery => {
            CallToolError::invalid_arguments("discover_capabilities", Some(e.to_string()))
        }
        DiscoveryError::UnknownEntry(_) => CallToolError::from_message(format!("catalog: {e}")),
    }
}

fn typed_discovery_mcp_error(e: plasm_discovery::DiscoveryError) -> CallToolError {
    match e {
        plasm_discovery::DiscoveryError::EmptyUtterance
        | plasm_discovery::DiscoveryError::InvalidClarificationAnswer => {
            CallToolError::invalid_arguments("discover_capabilities", Some(e.to_string()))
        }
        plasm_discovery::DiscoveryError::UnknownEntry(_) => {
            CallToolError::from_message(format!("typed discovery: {e}"))
        }
        _ => CallToolError::from_message(format!("typed discovery: {e}")),
    }
}

fn mcp_typed_discovery_query_from_arguments(
    obj: &serde_json::Map<String, serde_json::Value>,
    intent: &str,
) -> Result<DiscoveryQuery, String> {
    let utterance = intent.trim().to_string();
    if utterance.is_empty() {
        return Err("typed discovery requires a non-empty `intent`".to_string());
    }
    let max_options = obj
        .get("max_options")
        .and_then(|v| v.as_u64())
        .unwrap_or(8)
        .clamp(1, 32) as usize;
    let enable_embeddings = obj
        .get("enable_embeddings")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let allowed_entry_ids = match obj.get("allowed_entry_ids") {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    Ok(DiscoveryQuery {
        utterance,
        allowed_entry_ids,
        prior_state: None,
        max_options,
        enable_embeddings,
        force_entry_id: None,
        force_entity: None,
    })
}

fn mcp_key(runtime: &Arc<dyn McpServer>) -> Result<String, CallToolError> {
    runtime.session_id().ok_or_else(|| {
        CallToolError::from_message(
            "MCP session not ready: complete the MCP initialize handshake before calling tools.",
        )
    })
}

fn mcp_call_tool_error_class(err: &CallToolError) -> &'static str {
    let msg = err.to_string();
    if msg.contains("entry_id not allowed by tenant MCP configuration") {
        return "entry_not_allowed";
    }
    if msg.contains("incoming auth required") {
        return "incoming_auth_required";
    }
    if msg.contains("MCP Authorization missing tenant binding") {
        return "missing_tenant_binding";
    }
    if msg.contains("Tenant MCP configuration is no longer available") {
        return "tenant_mcp_unavailable";
    }
    if msg.contains("Personal MCP configuration is missing owner binding") {
        return "owner_binding_missing";
    }
    if msg.contains("MCP Authorization required") {
        return "mcp_authorization_required";
    }
    "call_tool_error"
}

fn mcp_truncate_resource_uri_display(uri: &str) -> String {
    const MAX: usize = 160;
    if uri.chars().count() <= MAX {
        uri.to_string()
    } else {
        format!(
            "{}…",
            uri.chars().take(MAX.saturating_sub(1)).collect::<String>()
        )
    }
}

fn mcp_artifact_payload_chars(payload: &ArtifactPayload) -> (u64, bool) {
    match std::str::from_utf8(&payload.bytes) {
        Ok(s) => (s.chars().count() as u64, false),
        Err(_) => (payload.bytes.len() as u64, true),
    }
}

fn read_resource_result_for_payload(
    uri: &str,
    payload: ArtifactPayload,
) -> Result<ReadResourceResult, RpcError> {
    let maybe_utf8 = std::str::from_utf8(&payload.bytes)
        .ok()
        .map(|s| s.to_string());
    Ok(ReadResourceResult {
        contents: vec![if let Some(text) = maybe_utf8 {
            ReadResourceContent::TextResourceContents(TextResourceContents {
                meta: None,
                mime_type: Some(payload.metadata.content_type),
                text,
                uri: uri.to_string(),
            })
        } else {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&payload.bytes);
            ReadResourceContent::BlobResourceContents(
                BlobResourceContents::new(b64, uri.to_string())
                    .with_mime_type(payload.metadata.content_type),
            )
        }],
        meta: None,
    })
}

impl PlasmMcpHandler {
    #[allow(clippy::too_many_arguments)]
    async fn emit_mcp_resource_read_trace(
        &self,
        logical_session_trace_key: Option<&str>,
        archive: Option<RunArtifactArchiveRef>,
        uri: &str,
        maybe_payload: Option<&ArtifactPayload>,
        started: Instant,
        result: &str,
        error_class: Option<&str>,
    ) {
        let Some(mcp_key) = logical_session_trace_key.filter(|s| !s.is_empty()) else {
            return;
        };
        let (chars_added, is_binary) = maybe_payload
            .map(mcp_artifact_payload_chars)
            .unwrap_or((0, false));
        let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.plasm
            .trace_hub
            .trace_record_mcp_resource_read(
                mcp_key,
                archive,
                mcp_truncate_resource_uri_display(uri),
                chars_added,
                is_binary,
                duration_ms,
                result,
                error_class,
            )
            .await;
    }

    /// Shared MCP implementation for [`Self::handle_call_tool_request`] (`plasm` = plan-only, `plasm_run` = execute).
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn handle_plasm_mcp_tool(
        &self,
        key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &serde_json::Value,
        tool_name: &'static str,
        dry_run_only: bool,
        started: Instant,
    ) -> Result<CallToolResult, CallToolError> {
        let principal_incoming = self.ensure_mcp_principal(key, runtime).await?;
        let session_ref = parse_logical_session_ref_arg(tool_name, v)?;
        let logical_uuid = self
            .resolve_logical_session_ref_to_uuid(tool_name, key, &session_ref)
            .await?;
        let scope = tenant_scope(principal_incoming.as_ref());
        if !self
            .plasm
            .logical_sessions
            .verify_tenant(LogicalSessionId(logical_uuid), &scope)
            .await
        {
            return Ok(CallToolResult::with_error(CallToolError::from_message(
                "logical_session_ref is unknown or does not belong to this tenant scope",
            )));
        }
        let ls_key = logical_uuid.to_string();
        let state = self.logical_mutex(key, &ls_key).await;
        let needs_binding_hydrate = {
            let g = state.lock().await;
            g.binding.is_none()
        };
        if needs_binding_hydrate {
            if let Some(b) = self.resolve_binding_for_logical(key, logical_uuid).await {
                let mut g = state.lock().await;
                g.binding = Some(b);
            }
        }
        if dry_run_only && v.get("execute").is_some() {
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(false),
                "error",
                "invalid_arguments",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(CallToolError::invalid_arguments(
                tool_name,
                Some(
                    "remove `execute`: `plasm` is plan-only. Call `plasm_run` with the same `logical_session_ref` and `program` for live execution after reviewing the dry-run plan."
                        .into(),
                ),
            )));
        }
        let Some(program) = v
            .get("program")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(false),
                "error",
                "invalid_arguments",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(
                CallToolError::invalid_arguments(
                    tool_name,
                    Some("missing or invalid `program`: non-empty string".into()),
                ),
            ));
        };
        let reasoning = v
            .get("reasoning")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty());
        let wait_live = v.get("wait").and_then(|x| x.as_bool()).unwrap_or(true);
        let force_run = v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
        let plan_commit_ref = v
            .get("plan_commit_ref")
            .and_then(|x| x.as_str())
            .and_then(PlanCommitRef::parse);
        let plasm_tool_span = if dry_run_only {
            crate::spans::mcp_tool_plasm(false, 1, session_ref.as_str())
        } else {
            crate::spans::mcp_tool_plasm_run(false, 1, session_ref.as_str())
        };
        let run_live = !dry_run_only;
        let (binding, this_invocation_chars, mut idx, call_count) = {
            let mut g = state.lock().await;
            let binding = g.binding.clone();
            let this_invocation_chars = plasm_invocation_char_count(&program, reasoning);
            g.stats.plasm_invocation_chars = g
                .stats
                .plasm_invocation_chars
                .saturating_add(this_invocation_chars);
            g.stats.plasm_call_count = g.stats.plasm_call_count.saturating_add(1);
            let call_count = g.stats.plasm_call_count;
            let idx = std::mem::take(&mut g.meta_index);
            (binding, this_invocation_chars, idx, call_count)
        };
        let Some(b) = binding else {
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(false),
                "error",
                "no_session",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(CallToolError::from_message(
                "No session: call `plasm_context` with capability picks (`seeds`) first.",
            )));
        };

        if self
            .plasm
            .sessions
            .get_by_strs(&b.prompt_hash, &b.session_id)
            .await
            .is_none()
        {
            {
                let mut g = state.lock().await;
                g.binding = None;
            }
            {
                let mut map = self.plasm.logical_execute_bindings.write().await;
                map.remove(&logical_uuid);
            }
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(false),
                "error",
                "session_expired",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(CallToolError::from_message(
                "Execute session expired: call `plasm_context` again with your capability picks (`seeds`) to open a new session.",
            )));
        }

        let trace_meta = self.trace_session_meta(key, runtime).await;
        let trace_id = self
            .plasm
            .trace_hub
            .ensure_logical_session(&ls_key, Some(key), trace_meta)
            .await;
        let mcp_trace = PlasmTraceContext {
            trace_id,
            call_index: Some(call_count as i64),
            mcp_session_id: Some(key.to_string()),
            logical_session_id: Some(ls_key.clone()),
            logical_session_ref: Some(session_ref.clone()),
        };
        let reasoning_chars = reasoning.map(|r| r.chars().count() as u64);
        let call_index = self
            .plasm
            .trace_hub
            .trace_record_plasm_invocation(
                &ls_key,
                false,
                1,
                reasoning_chars,
                this_invocation_chars,
                reasoning.map(str::to_string),
            )
            .await;

        let sink = McpPlasmTraceSink {
            hub: Arc::clone(&self.plasm.trace_hub),
            mcp_key: ls_key.clone(),
            call_index,
        };

        let run_result = async {
            let Some(es) = self
                .plasm
                .sessions
                .get_by_strs(&b.prompt_hash, &b.session_id)
                .await
            else {
                return Err(
                    "Execute session expired: call `plasm_context` again with your capability picks (`seeds`) to open a new session."
                        .to_string(),
                );
            };
            if let Some(op_result) =
                try_dispatch_operation_program(&es, Some(&mcp_trace), &program).await
            {
                return op_result;
            }
            let plan_name = format!("plasm_dag_call_{call_count}");
            let pipeline = self.plasm.engine.prompt_pipeline();
            let cross = self.plasm.sessions.symbol_map_cross_cache();
            let compile = compile_plasm_expression(
                pipeline,
                Some(cross),
                &es,
                &plan_name,
                &program,
            );
            match compile {
                Ok(bundle) => {
                    let comp_archive = crate::plasm_comp_wire::plasm_comp_wire_json(
                        bundle.artifact(),
                        None,
                    );
                    if run_live {
                        crate::evidence_chain::begin_plan_evidence_with_anchors(
                            &es,
                            b.session_id.as_str(),
                            crate::evidence_chain::evidence_anchors(
                                plan_commit_ref.as_ref(),
                                Some(mcp_trace.trace_id),
                                Some(call_count),
                            ),
                        )
                        .map_err(|e| format!("evidence begin: {e}"))?;
                        let dry_gate = evaluate_plasm_comp_dry(&es, &bundle)?;
                        let compact = build_plan_dry_compact_view(
                            dry_gate.validated_plan(),
                            &dry_gate.topological_order,
                            &dry_gate.review,
                            &dry_gate.graph_summary,
                            Some(&es),
                        );
                        if plan_requires_review_gate(
                            compact.verdict,
                            force_run,
                            plan_commit_ref.as_ref(),
                        ) {
                            return Err(
                                "plan_requires_review: call `plasm` dry-run first, then pass `plan_commit_ref` or `force: true` on `plasm_run`"
                                    .to_string(),
                            );
                        }
                        if let Some(pc) = plan_commit_ref.as_ref() {
                            verify_plan_commit_for_dry(&es, pc, &dry_gate)?;
                        }
                        let auto_async =
                            live_run_should_auto_async(&dry_gate.review, wait_live);
                        if should_spawn_async_live_run(wait_live, &dry_gate.review) {
                            es.try_begin_live_program_run()?;
                            let handle = es.mint_operation_handle(session_ref.as_str());
                            let accept = op_accept_context_from_executable(
                                plan_commit_ref.clone(),
                                Some(compact.verdict),
                                auto_async,
                                Some(key.to_string()),
                                bundle.executable(),
                                &bundle.artifact().comp,
                            );
                            spawn_async_plan_run(
                                Arc::clone(&es),
                                Arc::clone(&self.plasm),
                                b.prompt_hash.clone(),
                                b.session_id.clone(),
                                bundle.clone(),
                                handle.clone(),
                                CancelSignal::new(),
                                accept,
                            )?;
                            let (markdown, meta) = async_live_run_accept_parts(
                                &handle,
                                plan_commit_ref.as_ref(),
                                compact.verdict,
                                auto_async,
                            );
                            let comp_json = plasm_comp_json_from_dry(&dry_gate);
                            return Ok(PlasmPlanRunResult {
                                version: serde_json::json!({}),
                                node_results: Vec::new(),
                                graph_summary: serde_json::json!({}),
                                comp: comp_json,
                                code_plan_run_artifacts: Vec::new(),
                                run_markdown: Some(markdown),
                                run_plasm_meta: Some(meta),
                                return_steps: Vec::new(),
                            });
                        }
                        es.begin_sync_live_run()?;
                        let sync_result = ExecutePipeline::run_program(
                            &es,
                            self.plasm.as_ref(),
                            &b.prompt_hash,
                            &b.session_id,
                            &bundle,
                            ExecutionIntent::Live,
                            Some(PlasmPlanRunHooks {
                                meta_index: &mut idx,
                                trace: mcp_trace.clone(),
                                sink: sink.clone(),
                            }),
                        )
                        .await;
                        es.end_sync_live_run();
                        match sync_result {
                            Ok(out) => {
                                trace_archive_and_emit_code_plan_execute(
                                    &self.plasm.trace_hub,
                                    &self.plasm.run_artifacts,
                                    &ls_key,
                                    &es,
                                    b.prompt_hash.as_str(),
                                    b.session_id.as_str(),
                                    session_ref.as_str(),
                                    &comp_archive,
                                    &program,
                                    out.comp.clone(),
                                    call_count,
                                    &out,
                                )
                                .await;
                                Ok(out)
                            }
                            Err(e) => Err(e),
                        }
                    } else {
                        crate::evidence_chain::begin_plan_evidence_with_anchors(
                            &es,
                            b.session_id.as_str(),
                            crate::evidence_chain::evidence_anchors(
                                None,
                                Some(mcp_trace.trace_id),
                                Some(call_count),
                            ),
                        )
                        .map_err(|e| format!("evidence begin: {e}"))?;
                        let dry = evaluate_plasm_comp_dry(&es, &bundle)?;
                        let dry_text = render_plasm_plan_dry_text_for_session(
                            &dry,
                            None,
                            Some(&es),
                        );
                        let markdown = format!("```text\n{dry_text}\n```");
                        let comp_json = plasm_comp_json_from_dry(&dry);
                        let compact = build_plan_dry_compact_view(
                            dry.validated_plan(),
                            &dry.topological_order,
                            &dry.review,
                            &dry.graph_summary,
                            Some(&es),
                        );
                        let commit_ref = es.mint_plan_commit_ref();
                        es.register_plan_commit(PlanCommitRecord {
                            commit_ref: commit_ref.clone(),
                            commit_id: compute_plan_commit_id_from_dry(&dry),
                            dry_review: dry.review.clone(),
                            verdict: compact.verdict,
                            expires_at: std::time::Instant::now() + PLAN_COMMIT_TTL,
                        });
                        trace_archive_and_emit_code_plan_evaluate(
                            &self.plasm.trace_hub,
                            &self.plasm.run_artifacts,
                            &ls_key,
                            &es,
                            b.prompt_hash.as_str(),
                            b.session_id.as_str(),
                            session_ref.as_str(),
                            &comp_archive,
                            &program,
                            comp_json.clone(),
                            call_count,
                        )
                        .await;
                        let mut plasm_obj = serde_json::Map::new();
                        plasm_obj.insert("dry_run".into(), serde_json::json!(true));
                        plasm_obj.insert("comp".into(), comp_json.clone());
                        plasm_obj.extend(plan_commit_meta(
                            &commit_ref,
                            &dry.review,
                            compact.verdict,
                        ));
                        let ux_ctx = crate::plan_ux_reflection::PlanUxBuildContext {
                            session: Some(&es),
                            param_bindings: &[],
                        };
                        plasm_obj.insert(
                            "plan_ux_reflection".into(),
                            crate::plan_ux_reflection::plan_ux_reflection_value(&dry, &ux_ctx),
                        );
                        if dry
                            .graph_summary
                            .get("dry_review")
                            .and_then(|v| v.get("has_unprojected_multi_row_read"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        {
                            plasm_obj.insert("projection_warning".into(), serde_json::json!(true));
                        }
                        if let Some(unused) = dry
                            .graph_summary
                            .get("unused_seeds")
                            .and_then(|v| v.as_array())
                        {
                            if !unused.is_empty() {
                                plasm_obj.insert(
                                    "unused_seeds".into(),
                                    serde_json::Value::Array(unused.clone()),
                                );
                            }
                        }
                        let mut meta = serde_json::Map::new();
                        meta.insert("plasm".into(), serde_json::Value::Object(plasm_obj));
                        crate::plan_ui_mcp::attach_plan_review_ui_meta(&mut meta);
                        Ok(PlasmPlanRunResult {
                            version: dry.version,
                            node_results: dry.node_results,
                            graph_summary: dry.graph_summary,
                            comp: comp_json,
                            code_plan_run_artifacts: Vec::new(),
                            run_markdown: Some(markdown),
                            run_plasm_meta: Some(meta),
                            return_steps: Vec::new(),
                        })
                    }
                }
                Err(e) => Err(e),
            }
        }
        .instrument(plasm_tool_span)
        .await;
        {
            let mut g = state.lock().await;
            g.meta_index = idx;
        }
        match run_result {
            Ok(out) => {
                let markdown = out
                    .run_markdown
                    .unwrap_or_else(|| "# Plasm program plan\n\nNo execution output.".to_string());
                let response_chars = markdown.chars().count() as u64;
                if response_chars > 0 {
                    let mut g = state.lock().await;
                    g.stats.plasm_response_chars =
                        g.stats.plasm_response_chars.saturating_add(response_chars);
                    self.plasm
                        .trace_hub
                        .trace_note_plasm_response_chars(
                            &ls_key,
                            response_chars,
                            tool_name,
                            call_index,
                            false,
                            1,
                        )
                        .await;
                }
                let (tok_prompt, tok_inv, tok_resp, tok_total) =
                    self.mcp_plasm_token_snapshot_logical(key, &ls_key).await;
                tracing::info!(
                    target: "plasm_agent::mcp",
                    tool = tool_name,
                    ok = true,
                    tokens_est_prompt = tok_prompt,
                    tokens_est_invocation = tok_inv,
                    tokens_est_tool_response = tok_resp,
                    tokens_est_session_total = tok_total,
                    "MCP tool: plasm / plasm_run"
                );
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(false),
                    "success",
                    "none",
                    started.elapsed(),
                );
                let blocks = vec![ContentBlock::TextContent(TextContent::new(
                    markdown, None, None,
                ))];
                let mut res = CallToolResult::from_content(blocks);
                if let Some(m) = out.run_plasm_meta {
                    res = res.with_meta(Some(m));
                    res = crate::mcp_ui_payload::mirror_plasm_structured_content(res);
                }
                Ok(res)
            }
            Err(msg) => {
                self.plasm
                    .trace_hub
                    .trace_add_plasm_error(&ls_key, call_index, None, msg.clone())
                    .await;
                let (tok_prompt, tok_inv, tok_resp, tok_total) =
                    self.mcp_plasm_token_snapshot_logical(key, &ls_key).await;
                tracing::error!(
                    target: "plasm_agent::mcp",
                    tool = tool_name,
                    tokens_est_prompt = tok_prompt,
                    tokens_est_invocation = tok_inv,
                    tokens_est_tool_response = tok_resp,
                    tokens_est_session_total = tok_total,
                    message = %msg,
                    "MCP tool: plasm / plasm_run failed"
                );
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(false),
                    "error",
                    "execute_failed",
                    started.elapsed(),
                );
                Ok(CallToolResult::with_error(CallToolError::from_message(msg)))
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_mcp_tool_plasm_context(
        &self,
        key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &serde_json::Value,
    ) -> Result<CallToolResult, CallToolError> {
        let tname = "plasm_context";
        let principal_incoming = self.ensure_mcp_principal(key, runtime).await?;
        let intent = v.get("intent").and_then(|x| x.as_str()).ok_or_else(|| {
            CallToolError::invalid_arguments(tname, Some("missing `intent`".into()))
        })?;
        let scope = tenant_scope(principal_incoming.as_ref());
        let rec = self
            .plasm
            .logical_sessions
            .init_session(&scope, &ClientSessionKey::new(intent))
            .await;
        let logical_session_ref = {
            let transport = self.session_state(key).await;
            let mut g = transport.lock().await;
            g.ensure_session_ref(rec.logical_session_id.as_uuid())
        };
        let logical_uuid = rec.logical_session_id.as_uuid();
        let ls_key = logical_uuid.to_string();
        let seeds = parse_tool_seeds(tname, v)?;
        let ranked_capabilities = parse_plasm_context_ranked_capabilities(tname, v)?;
        let principal = parse_optional_principal(v);
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        let allowed_ids: Option<Vec<String>> = tcfg.as_ref().map(|cfg| {
            let mut ids: Vec<String> = cfg.allowed_entry_ids.iter().cloned().collect();
            ids.sort();
            ids
        });
        let seeds = crate::http_execute::resolve_capability_seeds(
            seeds,
            self.plasm.catalog.snapshot().as_ref(),
            allowed_ids.as_deref(),
        )
        .map_err(CallToolError::from_message)?;
        let distinct_entries: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for s in &seeds {
                if seen.insert(s.entry_id.clone()) {
                    out.push(s.entry_id.clone());
                }
            }
            out
        };
        if let Some(ref cfg) = tcfg {
            for eid in &distinct_entries {
                if !cfg.entry_allowed(eid) {
                    return Err(CallToolError::from_message(format!(
                        "entry_id not allowed by tenant MCP configuration: {eid}"
                    )));
                }
            }
        }
        let binding = self.resolve_binding_for_logical(key, logical_uuid).await;
        tracing::debug!(
            target: "plasm_agent::mcp",
            tool = tname,
            logical_session_ref = %logical_session_ref,
            logical_session_id = %ls_key,
            mcp_execute_binding_present = binding.is_some(),
            "MCP plasm_context: Plasm execute binding before apply_capability_seeds (false means open path; true means expand/federate against existing prompt_hash/session)"
        );
        let context_span = crate::spans::mcp_tool_plasm_context(logical_session_ref.as_str());
        let out: ApplyCapabilitySeedsOutcome = apply_capability_seeds(
            self.plasm.as_ref(),
            principal_incoming.as_ref(),
            binding
                .as_ref()
                .map(|b| (b.prompt_hash.as_str(), b.session_id.as_str())),
            seeds,
            principal,
            tcfg.clone(),
            Some(logical_uuid),
            intent,
            ranked_capabilities,
        )
        .instrument(context_span)
        .await
        .map_err(|msg| CallToolError::new(std::io::Error::other(msg)))?;

        if out.stale_execute_binding_recovered {
            self.plasm.trace_hub.finalize_mcp_session(&ls_key).await;
        }

        if out.binding_updated {
            {
                let mut g = self.session_states.write().await;
                if g.len() >= MAX_MCP_EXEC_BINDINGS && !g.contains_key(key) {
                    if let Some(victim) = g.keys().next().cloned() {
                        tracing::warn!(
                            victim = %victim,
                            limit = MAX_MCP_EXEC_BINDINGS,
                            "evicting MCP transport slot to respect soft cap"
                        );
                        g.remove(&victim);
                    }
                }
            }
            let ls = self.logical_mutex(key, &ls_key).await;
            let mut g = ls.lock().await;
            g.binding = Some(PlasmExecBinding {
                prompt_hash: out.prompt_hash.clone(),
                session_id: out.session_id.clone(),
            });
            drop(g);
            let mut map = self.plasm.logical_execute_bindings.write().await;
            map.insert(
                logical_uuid,
                (out.prompt_hash.clone(), out.session_id.clone()),
            );
        }
        let trace_meta = self.trace_session_meta(key, runtime).await;
        self.plasm
            .trace_hub
            .ensure_logical_session(&ls_key, Some(key), trace_meta)
            .await;

        let total_teaching_chars: u64 = out
            .waves
            .iter()
            .map(|w| w.teaching_prompt_chars_added)
            .sum();
        let exposed_entities: usize = out
            .waves
            .iter()
            .flat_map(|w| {
                w.entities
                    .iter()
                    .map(|entity| format!("{}:{entity}", w.entry_id))
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let catalog_count = {
            let mut ids = std::collections::BTreeSet::new();
            for w in &out.waves {
                ids.insert(w.entry_id.as_str());
            }
            ids.len()
        };
        tracing::info!(
            target: "plasm_agent::mcp",
            tool = "plasm_context",
            logical_session_ref = %logical_session_ref,
            exposed_entities,
            catalog_count,
            response_teaching_chars = total_teaching_chars,
            wave_count = out.waves.len(),
            "MCP plasm_context response telemetry"
        );
        let text = build_plasm_context_agent_markdown(logical_session_ref.as_str(), &out.waves);
        for wave in &out.waves {
            if wave.teaching_prompt_chars_added > 0 {
                let ls = self.logical_mutex(key, &ls_key).await;
                let mut g = ls.lock().await;
                g.stats.teaching_prompt_chars = g
                    .stats
                    .teaching_prompt_chars
                    .saturating_add(wave.teaching_prompt_chars_added);
            }
            self.plasm
                .trace_hub
                .trace_record_plasm_context(
                    &ls_key,
                    PlasmContextTrace {
                        teaching_prompt_chars_added: wave.teaching_prompt_chars_added,
                        reused_session: wave.reused_session,
                        mode: wave.mode.clone(),
                        entry_id: Some(wave.entry_id.clone()),
                        entities: wave.entities.clone(),
                        seeds: wave
                            .entities
                            .iter()
                            .map(|e| format!("{}:{e}", wave.entry_id))
                            .collect(),
                    },
                )
                .await;
        }
        let (domain_revision, relations) = if let Some(sess_arc) = self
            .plasm
            .sessions
            .get_by_strs(&out.prompt_hash, &out.session_id)
            .await
        {
            let rel = sess_arc
                .teaching_exposure
                .as_ref()
                .map(|exposure| exposure.exposed_relation_symbol_rows())
                .filter(|rows| !rows.is_empty())
                .map(|rows| json!(rows));
            (Some(sess_arc.domain_revision), rel)
        } else {
            (None, None)
        };
        let plasm = build_plasm_context_tool_meta(
            logical_session_ref.as_str(),
            &out,
            domain_revision,
            relations,
        );
        let mut res = CallToolResult::text_content(vec![TextContent::new(text, None, None)]);
        if !plasm.is_empty() {
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".to_string(), serde_json::Value::Object(plasm));
            res = res.with_meta(Some(meta));
        }
        Ok(res)
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_mcp_tool_discover_capabilities(
        &self,
        key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &serde_json::Value,
    ) -> Result<CallToolResult, CallToolError> {
        self.ensure_mcp_principal(key, runtime).await?;
        let q = mcp_discover_query_from_arguments(v)
            .map_err(|msg| CallToolError::invalid_arguments("discover_capabilities", Some(msg)))?;
        let discover_span = crate::spans::mcp_tool_discover_capabilities();
        let _discover_guard = discover_span.enter();
        tracing::info!(
            target: "plasm_agent::mcp",
            tool = "discover_capabilities",
            intent = %q.tokens.first().map(String::as_str).unwrap_or_default(),
            "MCP tool: discover_capabilities (search)"
        );
        let reg = self.plasm.catalog.snapshot();
        let Some(obj) = v.as_object() else {
            return Err(CallToolError::invalid_arguments(
                "discover_capabilities",
                Some("arguments must be a JSON object".into()),
            ));
        };
        let typed = obj.get("typed").and_then(|x| x.as_bool()).unwrap_or(false);
        if typed {
            let intent = q.tokens.first().map(String::as_str).unwrap_or_default();
            let mut dq = mcp_typed_discovery_query_from_arguments(obj, intent).map_err(|msg| {
                CallToolError::invalid_arguments("discover_capabilities", Some(msg))
            })?;
            let tcfg = self.tenant_mcp_cfg(runtime).await?;
            if let Some(cfg) = tcfg {
                if dq.allowed_entry_ids.is_empty() {
                    dq.allowed_entry_ids =
                        mcp_policy::filter_registry_entries(reg.list_entries(), cfg.as_ref())
                            .into_iter()
                            .map(|m| m.entry_id)
                            .collect();
                } else {
                    dq.allowed_entry_ids.retain(|e| cfg.entry_allowed(e));
                }
            }
            let decision = run_typed_catalog_discovery(
                &reg,
                dq,
                self.plasm.discovery_embedding_store(),
                Some(self.plasm.discovery_index_cache()),
                #[cfg(feature = "local-embeddings")]
                Some(self.plasm.discovery_embedder()),
            )
            .await
            .map_err(typed_discovery_mcp_error)?;
            drop(_discover_guard);
            let json = serde_json::to_string_pretty(&decision).map_err(|e| {
                CallToolError::from_message(format!("serialize typed discovery: {e}"))
            })?;
            let text = format!("```json\n{json}\n```");
            return Ok(CallToolResult::text_content(vec![TextContent::new(
                text, None, None,
            )]));
        }

        let mut r = reg.discover(&q).map_err(discovery_mcp_error)?;
        drop(_discover_guard);
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        if let Some(cfg) = tcfg {
            r = mcp_policy::filter_discovery_result(r, cfg.as_ref());
        }
        let formatted = format_discovery_markdown_for_mcp(&r, &DiscoveryTablePolicy::default());
        let mut res =
            CallToolResult::text_content(vec![TextContent::new(formatted.markdown, None, None)]);
        if formatted.omission.truncated {
            let mut meta = serde_json::Map::new();
            let mut discovery = serde_json::Map::new();
            discovery.insert("truncated".into(), serde_json::json!(true));
            discovery.insert("shown".into(), serde_json::json!(formatted.omission.shown));
            discovery.insert(
                "omitted".into(),
                serde_json::json!(formatted.omission.omitted),
            );
            let top: Vec<serde_json::Value> = formatted
                .omission
                .top_omitted
                .iter()
                .map(|(api, entity)| serde_json::json!({ "api": api, "entity": entity }))
                .collect();
            discovery.insert("top_omitted".into(), serde_json::Value::Array(top));
            let mut plasm = serde_json::Map::new();
            plasm.insert("discovery".into(), serde_json::Value::Object(discovery));
            meta.insert("plasm".into(), serde_json::Value::Object(plasm));
            res = res.with_meta(Some(meta));
        }
        Ok(res)
    }

    async fn handle_mcp_tool_ui_list_catalogs(
        &self,
        key: &str,
        runtime: &Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        self.ensure_mcp_principal(key, runtime).await?;
        let reg = self.plasm.catalog.snapshot();
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        let entries = if let Some(cfg) = tcfg.as_ref() {
            crate::mcp_policy::filter_registry_entries(reg.list_entries(), cfg)
        } else {
            reg.list_entries()
        };
        let mut entry_ids: Vec<String> = entries.into_iter().map(|m| m.entry_id).collect();
        entry_ids.sort();
        let text = if entry_ids.is_empty() {
            "No MCP-enabled catalogs.".to_string()
        } else {
            format!("MCP-enabled catalogs: {}", entry_ids.join(", "))
        };
        let mut meta = serde_json::Map::new();
        meta.insert(
            "plasm".into(),
            serde_json::json!({ "entry_ids": entry_ids }),
        );
        Ok(
            CallToolResult::text_content(vec![TextContent::new(text, None, None)])
                .with_meta(Some(meta)),
        )
    }
}

#[async_trait]
impl ServerHandler for PlasmMcpHandler {
    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: Self::plasm_tools(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_list_resources_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListResourcesResult, RpcError> {
        Ok(ListResourcesResult {
            resources: vec![],
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_list_resource_templates_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListResourceTemplatesResult, RpcError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![
                ResourceTemplate {
                    annotations: None,
                    description: Some(
                        "Plasm cross-catalog workflow MCP App (parameter form + plan canvas).".into(),
                    ),
                    icons: vec![],
                    meta: None,
                    mime_type: Some(crate::workflow_mcp::WORKFLOW_UI_MIME.into()),
                    name: "plasm_workflow_app".into(),
                    title: Some("Plasm workflow MCP App".into()),
                    uri_template: crate::workflow_mcp::WORKFLOW_UI_URI.into(),
                },
                ResourceTemplate {
                    annotations: None,
                    description: Some(
                        "Plasm plan review MCP App (program editor + plan canvas for `plasm` dry-run).".into(),
                    ),
                    icons: vec![],
                    meta: None,
                    mime_type: Some(crate::plan_ui_mcp::PLAN_REVIEW_UI_MIME.into()),
                    name: "plasm_plan_review_app".into(),
                    title: Some("Plasm plan review MCP App".into()),
                    uri_template: crate::plan_ui_mcp::PLAN_REVIEW_UI_URI.into(),
                },
                ResourceTemplate {
                    annotations: None,
                    description: Some(
                        "Plasm run explorer MCP App (step list + entity table for live `plasm_run` / `run_workflow`).".into(),
                    ),
                    icons: vec![],
                    meta: None,
                    mime_type: Some(crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_MIME.into()),
                    name: "plasm_run_explorer_app".into(),
                    title: Some("Plasm run explorer MCP App".into()),
                    uri_template: crate::run_explorer_ui_mcp::RUN_EXPLORER_UI_URI.into(),
                },
                ResourceTemplate {
                    annotations: None,
                    description: Some(
                        "Typed bytes for one execute run artifact. `prompt_hash` and `session_id` match `plasm_context`; `run_id` is in `plasm_run` (or prior live) result metadata."
                            .into(),
                    ),
                    icons: vec![],
                    meta: None,
                    mime_type: Some("application/octet-stream".into()),
                    name: "plasm_execute_run".into(),
                    title: Some("Plasm execute run artifact (canonical)".into()),
                    uri_template: "plasm://execute/{prompt_hash}/{session_id}/run/{run_id}".into(),
                },
                ResourceTemplate {
                    annotations: None,
                    description: Some(
                        "Short alias for the same snapshot JSON as the canonical URI. `logical_session_ref` is the slot from `plasm_context` (`s0`, …); `n` is monotonic within that logical session’s execute binding."
                            .into(),
                    ),
                    icons: vec![],
                    meta: None,
                    mime_type: Some("application/octet-stream".into()),
                    name: "plasm_execute_run_short".into(),
                    title: Some("Plasm execute run artifact (short index)".into()),
                    uri_template: "plasm://session/{logical_session_ref}/r/{n}".into(),
                },
            ],
            meta: None,
            next_cursor: None,
        })
    }

    #[tracing::instrument(
        skip(self, runtime),
        name = "plasm_agent.mcp.resources.read_request",
        level = "trace"
    )]
    async fn handle_read_resource_request(
        &self,
        params: ReadResourceRequestParams,
        runtime: Arc<dyn McpServer>,
    ) -> Result<ReadResourceResult, RpcError> {
        let started = Instant::now();
        let uri = params.uri.trim();
        if let Some(bundle) = crate::mcp_app::bundle_for_uri(uri) {
            let Some((content, result_meta)) = crate::mcp_app::read_resource_text(uri) else {
                return Err(
                    RpcError::invalid_params().with_message(format!("unknown ui resource: {uri}"))
                );
            };
            crate::metrics::record_mcp_resource_read(
                bundle.resource_metric,
                "success",
                "none",
                started.elapsed(),
            );
            return Ok(ReadResourceResult {
                contents: vec![ReadResourceContent::TextResourceContents(content)],
                meta: Some(result_meta),
            });
        }
        if let Some((segment, resource_index)) = parse_plasm_session_short_resource_uri(uri) {
            let Some(transport_key) = runtime.session_id() else {
                crate::metrics::record_mcp_resource_read(
                    "logical_short",
                    "error",
                    "session_not_ready",
                    started.elapsed(),
                );
                return Err(RpcError::invalid_params().with_message(
                    "MCP session not ready: complete the initialize handshake before resources/read.",
                ));
            };
            let logical_uuid = match segment {
                LogicalSessionUriSegment::Uuid(u) => u,
                LogicalSessionUriSegment::Slot(s) => {
                    let transport = self.session_state(&transport_key).await;
                    let g = transport.lock().await;
                    let Some(u) = g.ref_to_uuid.get(&s).copied() else {
                        crate::metrics::record_mcp_resource_read(
                            "logical_short",
                            "error",
                            "unknown_session_ref",
                            started.elapsed(),
                        );
                        return Err(RpcError::invalid_params().with_message(
                            "unknown logical session slot in URI: use a `plasm://session/s{n}/r/...` URI from this connection after `plasm_context`, or the canonical `plasm://execute/.../run/...` URI.",
                        ));
                    };
                    u
                }
            };
            let ls_key = logical_uuid.to_string();
            let binding = {
                let map = self.plasm.logical_execute_bindings.read().await;
                map.get(&logical_uuid).map(|(ph, sid)| PlasmExecBinding {
                    prompt_hash: ph.clone(),
                    session_id: sid.clone(),
                })
            };
            let Some(b) = binding else {
                crate::metrics::record_mcp_resource_read(
                    "logical_short",
                    "error",
                    "no_binding",
                    started.elapsed(),
                );
                self.emit_mcp_resource_read_trace(
                    Some(&ls_key),
                    None,
                    uri,
                    None,
                    started,
                    "error",
                    Some("no_binding"),
                )
                .await;
                return Err(RpcError::invalid_params().with_message(
                    "no execute session for this logical session: call plasm_context with capability picks (`seeds`) first",
                ));
            };
            let live_sess = self
                .plasm
                .sessions
                .get_by_strs(b.prompt_hash.as_str(), b.session_id.as_str())
                .await;
            let live_art = if let Some(ref sess) = live_sess {
                sess.core
                    .get_run_artifact_by_resource_index(resource_index)
                    .await
            } else {
                None
            };
            let live_payload = live_art.as_ref().map(|a| a.payload.clone());
            if live_payload.is_some() {
                crate::metrics::record_execute_artifact_resolve_layer("hot");
            }
            let persisted_payload = if live_payload.is_none() {
                match self
                    .plasm
                    .run_artifacts
                    .get_payload_result_by_resource_index(
                        b.prompt_hash.as_str(),
                        b.session_id.as_str(),
                        resource_index,
                    )
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        crate::metrics::record_mcp_resource_read(
                            "logical_short",
                            "error",
                            "decode_failed",
                            started.elapsed(),
                        );
                        let rid = self
                            .plasm
                            .run_artifacts
                            .resolve_run_id_for_resource_index(
                                b.prompt_hash.as_str(),
                                b.session_id.as_str(),
                                resource_index,
                            )
                            .await;
                        let arch = rid.map(|run_id| RunArtifactArchiveRef {
                            prompt_hash: b.prompt_hash.clone(),
                            session_id: b.session_id.clone(),
                            run_id: run_id.to_wire(),
                            resource_index: Some(resource_index),
                        });
                        self.emit_mcp_resource_read_trace(
                            Some(&ls_key),
                            arch,
                            uri,
                            None,
                            started,
                            "error",
                            Some("decode_failed"),
                        )
                        .await;
                        return Err(RpcError::internal_error()
                            .with_message(format!("run artifact decode failed: {e}")));
                    }
                }
            } else {
                None
            };
            if live_payload.is_none() && persisted_payload.is_some() {
                crate::metrics::record_execute_artifact_resolve_layer("archive");
            }
            let Some(payload) = live_payload.or(persisted_payload) else {
                crate::metrics::record_mcp_resource_read(
                    "logical_short",
                    "error",
                    "unknown_artifact",
                    started.elapsed(),
                );
                self.emit_mcp_resource_read_trace(
                    Some(&ls_key),
                    None,
                    uri,
                    None,
                    started,
                    "error",
                    Some("unknown_artifact"),
                )
                .await;
                return Err(RpcError::invalid_params().with_message(format!(
                    "unknown run artifact index {resource_index} for this session"
                )));
            };
            let run_id = live_art.as_ref().map(|a| a.run_id).or(self
                .plasm
                .run_artifacts
                .resolve_run_id_for_resource_index(
                    b.prompt_hash.as_str(),
                    b.session_id.as_str(),
                    resource_index,
                )
                .await);
            let archive = run_id.map(|run_id| RunArtifactArchiveRef {
                prompt_hash: b.prompt_hash.clone(),
                session_id: b.session_id.clone(),
                run_id: run_id.to_wire(),
                resource_index: Some(resource_index),
            });
            crate::spans::mcp_resource_read().in_scope(|| {
                tracing::info!(
                    target: "plasm_agent::mcp",
                    uri = %uri,
                    logical_session_id = %logical_uuid,
                    prompt_hash = %b.prompt_hash,
                    session_id = %b.session_id,
                    resource_index,
                    bytes = payload.bytes.len(),
                    "MCP resources/read"
                );
            });
            crate::metrics::record_mcp_resource_read(
                "logical_short",
                "success",
                "none",
                started.elapsed(),
            );
            self.emit_mcp_resource_read_trace(
                Some(&ls_key),
                archive,
                uri,
                Some(&payload),
                started,
                "success",
                None,
            )
            .await;
            return read_resource_result_for_payload(uri, payload);
        }

        let Some((prompt_hash, session_id, run_id)) = parse_plasm_execute_run_uri(uri) else {
            crate::metrics::record_mcp_resource_read(
                "unsupported",
                "error",
                "unsupported_uri",
                started.elapsed(),
            );
            return Err(
                RpcError::invalid_params().with_message(format!("unsupported resource URI: {uri}"))
            );
        };
        let ls_key_opt = self
            .plasm
            .logical_session_id_for_execute_binding(prompt_hash.as_str(), session_id.as_str())
            .await
            .map(|u| u.to_string());
        let canonical_archive = RunArtifactArchiveRef {
            prompt_hash: prompt_hash.clone(),
            session_id: session_id.clone(),
            run_id: run_id.to_wire(),
            resource_index: None,
        };
        let live_sess = self
            .plasm
            .sessions
            .get_by_strs(prompt_hash.as_str(), session_id.as_str())
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
            match self
                .plasm
                .run_artifacts
                .get_payload_result(&prompt_hash, &session_id, run_id)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    crate::metrics::record_mcp_resource_read(
                        "canonical",
                        "error",
                        "decode_failed",
                        started.elapsed(),
                    );
                    self.emit_mcp_resource_read_trace(
                        ls_key_opt.as_deref(),
                        Some(canonical_archive.clone()),
                        uri,
                        None,
                        started,
                        "error",
                        Some("decode_failed"),
                    )
                    .await;
                    return Err(RpcError::internal_error()
                        .with_message(format!("run artifact decode failed: {e}")));
                }
            }
        } else {
            None
        };
        if live_payload.is_none() && persisted_payload.is_some() {
            crate::metrics::record_execute_artifact_resolve_layer("archive");
        }
        let Some(payload) = live_payload.or(persisted_payload) else {
            crate::metrics::record_mcp_resource_read(
                "canonical",
                "error",
                "unknown_artifact",
                started.elapsed(),
            );
            self.emit_mcp_resource_read_trace(
                ls_key_opt.as_deref(),
                Some(canonical_archive.clone()),
                uri,
                None,
                started,
                "error",
                Some("unknown_artifact"),
            )
            .await;
            return Err(RpcError::invalid_params().with_message(
                "unknown run artifact (wrong run_id or not yet stored for this session)",
            ));
        };
        crate::spans::mcp_resource_read().in_scope(|| {
            tracing::info!(
                target: "plasm_agent::mcp",
                uri = %uri,
                prompt_hash = %prompt_hash,
                session_id = %session_id,
                run_id = %run_id.to_wire(),
                bytes = payload.bytes.len(),
                "MCP resources/read"
            );
        });
        crate::metrics::record_mcp_resource_read("canonical", "success", "none", started.elapsed());
        self.emit_mcp_resource_read_trace(
            ls_key_opt.as_deref(),
            Some(canonical_archive),
            uri,
            Some(&payload),
            started,
            "success",
            None,
        )
        .await;
        read_resource_result_for_payload(uri, payload)
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        // Delegate to a free async fn so `#[async_trait]`'s wrapper future stays tiny: merging the
        // full `handle_plasm_mcp_tool` state machine here exceeded `dyn Future` bounds on rustc 1.87+.
        dispatch_plasm_mcp_call_tool_request(self, params, runtime).await
    }
}

async fn dispatch_plasm_mcp_call_tool_request(
    handler: &PlasmMcpHandler,
    params: CallToolRequestParams,
    runtime: Arc<dyn McpServer>,
) -> Result<CallToolResult, CallToolError> {
    fn record_workflow_tool(
        tname: &'static str,
        res: &Result<CallToolResult, CallToolError>,
        started: Instant,
    ) {
        let elapsed = started.elapsed();
        match res {
            Ok(_) => crate::metrics::record_mcp_tool(tname, None, "success", "none", elapsed),
            Err(e) => crate::metrics::record_mcp_tool(
                tname,
                None,
                "error",
                mcp_call_tool_error_class(e),
                elapsed,
            ),
        }
    }

    let key = mcp_key(&runtime)?;
    let v = args_value(&params);

    tracing::trace!(
        target: "plasm_agent.mcp.call_tool",
        tool = %params.name,
        "call_tool dispatch"
    );

    match params.name.as_str() {
        "plasm_context" => {
            let started = Instant::now();
            let tname = "plasm_context";
            let res = handler
                .handle_mcp_tool_plasm_context(key.as_str(), &runtime, &v)
                .await;
            let elapsed = started.elapsed();
            match &res {
                Ok(_) => crate::metrics::record_mcp_tool(tname, None, "success", "none", elapsed),
                Err(e) => crate::metrics::record_mcp_tool(
                    tname,
                    None,
                    "error",
                    mcp_call_tool_error_class(e),
                    elapsed,
                ),
            }
            res
        }
        "discover_capabilities" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_discover_capabilities(key.as_str(), &runtime, &v)
                .await;
            let elapsed = started.elapsed();
            match &res {
                Ok(_) => crate::metrics::record_mcp_tool(
                    "discover_capabilities",
                    None,
                    "success",
                    "none",
                    elapsed,
                ),
                Err(e) => crate::metrics::record_mcp_tool(
                    "discover_capabilities",
                    None,
                    "error",
                    mcp_call_tool_error_class(e),
                    elapsed,
                ),
            }
            res
        }
        "plasm_ui_list_catalogs" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_ui_list_catalogs(key.as_str(), &runtime)
                .await;
            let elapsed = started.elapsed();
            match &res {
                Ok(_) => crate::metrics::record_mcp_tool(
                    "plasm_ui_list_catalogs",
                    None,
                    "success",
                    "none",
                    elapsed,
                ),
                Err(e) => crate::metrics::record_mcp_tool(
                    "plasm_ui_list_catalogs",
                    None,
                    "error",
                    mcp_call_tool_error_class(e),
                    elapsed,
                ),
            }
            res
        }
        "open_workflow" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_open_workflow(key.as_str(), &runtime, &v)
                .await;
            record_workflow_tool("open_workflow", &res, started);
            res
        }
        "dry_workflow" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_dry_workflow(key.as_str(), &runtime, &v)
                .await;
            record_workflow_tool("dry_workflow", &res, started);
            res
        }
        "run_workflow" => {
            let started = Instant::now();
            let res = handler
                .handle_mcp_tool_run_workflow(key.as_str(), &runtime, &v)
                .await;
            record_workflow_tool("run_workflow", &res, started);
            res
        }
        "plasm" | "plasm_run" => {
            let started = Instant::now();
            let dry_run_only = matches!(params.name.as_str(), "plasm");
            let tool_name: &'static str = if dry_run_only { "plasm" } else { "plasm_run" };
            handler
                .handle_plasm_mcp_tool(&key, &runtime, &v, tool_name, dry_run_only, started)
                .await
        }
        _ => {
            crate::metrics::record_mcp_tool(
                "unknown_tool",
                None,
                "error",
                "unknown_tool",
                Duration::from_secs(0),
            );
            Err(CallToolError::unknown_tool(params.name))
        }
    }
}

/// Detect MCP transport sessions that disappeared from the SDK session store (disconnect / DELETE),
/// finalize logical-session traces that are no longer live, and drop orphaned per-transport state.
#[allow(private_interfaces)]
pub(crate) fn spawn_mcp_teaching_prompt_session_reporter(
    server: &HyperServer,
    plasm: Arc<PlasmHostState>,
    session_states: Arc<RwLock<HashMap<String, Arc<Mutex<McpTransportState>>>>>,
) {
    let store = server.state().session_store.clone();
    tokio::spawn(async move {
        type SessionStates = Arc<RwLock<HashMap<String, Arc<Mutex<McpTransportState>>>>>;
        async fn stats_for_logical_session(
            session_states: &SessionStates,
            logical_id: &str,
        ) -> McpSessionPlasmStats {
            let g = session_states.read().await;
            for (_tk, st) in g.iter() {
                let s = st.lock().await;
                if let Some(ls) = s.logical_by_id.get(logical_id) {
                    let lg = ls.lock().await;
                    return lg.stats.clone();
                }
            }
            McpSessionPlasmStats::default()
        }

        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            for pending in plasm.op_progress_hub.drain_mcp_pending() {
                let mut op_params = serde_json::Map::new();
                op_params.insert("line".into(), json!(pending.line));
                op_params.insert("n".into(), json!(pending.n));
                if let Some(c) = pending.plan_commit {
                    op_params.insert("c".into(), json!(c));
                }
                if let Some(transport) = store.get(&pending.transport_key).await {
                    let _ = transport
                        .notify_custom(CustomNotification {
                            method: "notifications/plasm/op".into(),
                            params: Some(op_params),
                        })
                        .await;
                }
            }
            let current: HashSet<String> = store.keys().await.into_iter().collect();
            let mut live_trace_keys: HashSet<String> = HashSet::new();
            {
                let g = session_states.read().await;
                for tk in &current {
                    if let Some(st_arc) = g.get(tk) {
                        let s = st_arc.lock().await;
                        for lid in s.logical_by_id.keys() {
                            live_trace_keys.insert(lid.clone());
                        }
                    }
                }
            }
            let trace_hub_active = plasm.trace_hub.active_mcp_session_count().await;
            tracing::trace!(
                target: "plasm_agent::mcp",
                session_store_keys = current.len(),
                live_logical_sessions = live_trace_keys.len(),
                trace_hub_active,
                "trace hub vs MCP session store"
            );
            let finalized = plasm
                .trace_hub
                .finalize_disconnected_sessions(&live_trace_keys)
                .await;
            for ended in &finalized {
                let stats = stats_for_logical_session(&session_states, ended).await;
                let tp = mcp_chars_to_token_est(stats.teaching_prompt_chars);
                let ti = mcp_chars_to_token_est(stats.plasm_invocation_chars);
                let tr = mcp_chars_to_token_est(stats.plasm_response_chars);
                let tt = tp.saturating_add(ti).saturating_add(tr);
                tracing::info!(
                    target: "plasm_agent::mcp",
                    logical_session_id = %ended,
                    teaching_prompt_chars_total = stats.teaching_prompt_chars,
                    plasm_invocation_chars_total = stats.plasm_invocation_chars,
                    plasm_response_chars_total = stats.plasm_response_chars,
                    plasm_call_count_total = stats.plasm_call_count,
                    tokens_est_prompt = tp,
                    tokens_est_invocation = ti,
                    tokens_est_tool_response = tr,
                    tokens_est_session_total = tt,
                    "MCP logical session trace finalized (no live transport binding)"
                );
            }
            {
                let mut g = session_states.write().await;
                g.retain(|tk, _| current.contains(tk));
            }
            let idle_ms = mcp_trace_idle_finish_ms();
            if idle_ms > 0 {
                let finalized_idle = plasm
                    .trace_hub
                    .finalize_idle_traces(&live_trace_keys, idle_ms)
                    .await;
                for ended in finalized_idle {
                    tracing::info!(
                        target: "plasm_agent::mcp",
                        logical_session_id = %ended,
                        idle_ms,
                        "MCP logical session trace finalized (idle timeout); transport still connected"
                    );
                }
            }
        }
    });
}

/// When set and > 0, active traces with no hub activity for this many milliseconds are moved to
/// `completed` even if the MCP transport session is still in the SDK store (list UIs stop showing `live`).
fn mcp_trace_idle_finish_ms() -> u64 {
    std::env::var("PLASM_MCP_TRACE_IDLE_FINISH_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

fn mcp_initialize_result() -> InitializeResult {
    InitializeResult {
        server_info: Implementation {
            name: "plasm".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("Plasm agent".into()),
            description: Some(
                "Stable **`intent`**; **`plasm_context`** with **`seeds`**, then **`plasm`** (dry-run) and **`plasm_run`** (execute) with the same **`logical_session_ref`**. Call **`plasm_context`** again to **append** new picks or when continuity requires it—not every turn."
                    .into(),
            ),
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools { list_changed: None }),
            resources: Some(ServerCapabilitiesResources {
                list_changed: None,
                subscribe: Some(false),
            }),
            ..Default::default()
        },
        protocol_version: ProtocolVersion::V2025_11_25.into(),
        instructions: Some(mcp_server_initialize_instructions()),
        meta: None,
    }
}

/// Build MCP Streamable HTTP server (not started) for merging with discovery routes on one port.
pub fn build_mcp_hyper_server_for_merge(plasm: Arc<PlasmHostState>) -> HyperServer {
    let handler_struct = PlasmMcpHandler::new(Arc::clone(&plasm));
    let session_states = Arc::clone(&handler_struct.session_states);
    let handler = handler_struct.to_mcp_server_handler();
    let auth_provider: Option<Arc<dyn rust_mcp_sdk::auth::AuthProvider>> =
        if plasm.mcp_config_repository().is_some() || plasm.incoming_auth.is_some() {
            Some(Arc::new(
                crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider::new(Arc::clone(&plasm)),
            ))
        } else {
            None
        };
    let server = hyper_server::create_server(
        mcp_initialize_result(),
        handler,
        HyperServerOptions {
            host: "0.0.0.0".into(),
            port: 0,
            event_store: Some(Arc::new(InMemoryEventStore::default())),
            health_endpoint: Some("/health".into()),
            sse_support: false,
            auth: auth_provider,
            ..Default::default()
        },
    );
    spawn_mcp_teaching_prompt_session_reporter(&server, Arc::clone(&plasm), session_states);
    server
}

/// Run Streamable HTTP MCP on `host`:`port` (default MCP path `/mcp` from the SDK).
pub async fn run_mcp_server(host: &str, port: u16, plasm: Arc<PlasmHostState>) -> SdkResult<()> {
    let handler_struct = PlasmMcpHandler::new(Arc::clone(&plasm));
    let session_states = Arc::clone(&handler_struct.session_states);
    let handler = handler_struct.to_mcp_server_handler();
    let auth_provider: Option<Arc<dyn rust_mcp_sdk::auth::AuthProvider>> =
        if plasm.mcp_config_repository().is_some() || plasm.incoming_auth.is_some() {
            Some(Arc::new(
                crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider::new(Arc::clone(&plasm)),
            ))
        } else {
            None
        };
    let server = hyper_server::create_server(
        mcp_initialize_result(),
        handler,
        HyperServerOptions {
            host: host.to_string(),
            port,
            event_store: Some(Arc::new(InMemoryEventStore::default())),
            health_endpoint: Some("/health".into()),
            sse_support: false,
            auth: auth_provider,
            ..Default::default()
        },
    );
    spawn_mcp_teaching_prompt_session_reporter(&server, Arc::clone(&plasm), session_states);
    server.start().await
}

#[cfg(test)]
mod tests {
    use super::{mcp_discover_query_from_arguments, parse_tool_seeds};
    use insta::assert_snapshot;

    #[test]
    fn mcp_discover_maps_intent_to_capability_query() {
        let v = serde_json::json!({
            "intent": "Find electric type chart data for Pokemon",
        });
        let q = mcp_discover_query_from_arguments(&v).expect("deserialize");
        assert_eq!(q.tokens, vec!["Find electric type chart data for Pokemon"]);
        assert!(q.phrases.is_empty());
        assert!(q.entity_hints.is_empty());
        assert!(q.pick_entry.is_none());
        assert!(q.kinds.is_empty());
    }

    #[test]
    fn mcp_discover_rejects_legacy_query_array() {
        let v = serde_json::json!({
            "query": ["github", "repository commits"],
        });
        let err = mcp_discover_query_from_arguments(&v).expect_err("legacy query rejected");
        assert!(
            err.contains("requires `intent`") && err.contains("not accepted"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn mcp_discover_rejects_non_string_intent() {
        let v = serde_json::json!({
            "intent": ["github", "commits"],
        });
        let err = mcp_discover_query_from_arguments(&v).expect_err("array intent rejected");
        assert!(err.contains("single string"), "unexpected: {err}");
    }

    #[test]
    fn plasm_invocation_char_count_sums_program_and_reasoning() {
        assert_eq!(super::plasm_invocation_char_count("a", None), 1);
        assert_eq!(super::plasm_invocation_char_count("a", Some("#c")), 1 + 2);
    }

    /// Serialize Insta reads/writes under `src/snapshots` (parallel `cargo nextest` threads otherwise flake).
    fn with_insta_snapshots<R>(f: impl FnOnce() -> R) -> R {
        static INSTA_SNAPSHOT_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = INSTA_SNAPSHOT_MUTEX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_path(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/snapshots"),
        );
        settings.bind(f)
    }

    /// Model-facing copy; update with `just update-insta-snapshots` (not `INSTA_UPDATE=always` in your shell).
    #[test]
    fn mcp_plasm_tool_description_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!(
                "mcp_plasm_tool_description",
                super::MCP_PLASM_TOOL_DESCRIPTION
            );
        });
    }

    #[test]
    fn mcp_plasm_run_tool_description_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!(
                "mcp_plasm_run_tool_description",
                super::MCP_PLASM_RUN_TOOL_DESCRIPTION
            );
        });
    }

    #[test]
    fn mcp_server_initialize_instructions_snapshot() {
        with_insta_snapshots(|| {
            insta::assert_snapshot!(
                "mcp_server_initialize_instructions",
                super::mcp_server_initialize_instructions()
            );
        });
    }

    #[test]
    fn mcp_server_initialize_workflow_uses_intent_not_query() {
        let text = super::MCP_SERVER_INITIALIZE_WORKFLOW;
        assert!(text.contains("`intent`"));
        assert!(text.contains("One goal"));
        assert!(text.contains("one **`intent`** per goal"));
        assert!(!text.contains("several discovery calls"));
        assert!(!text.contains("pass **`query`**"));
        assert!(!text.contains("syntax guide in MCP initialize"));
        assert!(text.contains("plasm_context` teaching TSV"));
        assert!(text.contains("Multi-API"));
        let discover = super::PlasmMcpHandler::plasm_tools()
            .into_iter()
            .find(|t| t.name == "discover_capabilities")
            .expect("discover_capabilities");
        let discover_desc = discover.description.as_deref().unwrap_or("");
        assert!(
            discover_desc.len() < 400,
            "discover tool description too long"
        );
        assert!(!discover_desc.contains("query"));
    }

    /// Static MCP prompt byte budgets (UTF-8 byte length). Targets from MCP prompt dedup plan.
    #[test]
    fn mcp_prompt_char_budget() {
        let init = super::mcp_server_initialize_instructions();
        assert!(
            init.len() < 3200,
            "initialize instructions too long: {} chars",
            init.len()
        );
        let head = include_str!("mcp_prompt/workflow_head.txt");
        let contract = include_str!("mcp_prompt/program_contract.txt");
        let tail = include_str!("mcp_prompt/workflow_tail.txt");
        assert!(
            head.len() < 950,
            "workflow_head too long: {} chars",
            head.len()
        );
        assert!(
            contract.len() < 900,
            "program_contract too long: {} chars",
            contract.len()
        );
        assert!(
            tail.len() < 750,
            "workflow_tail too long: {} chars",
            tail.len()
        );
        assert_eq!(init.len(), head.len() + contract.len() + tail.len());
        assert!(
            super::MCP_PLASM_TOOL_DESCRIPTION.len() < 1200,
            "plasm tool description too long: {} chars",
            super::MCP_PLASM_TOOL_DESCRIPTION.len()
        );
        assert!(
            super::MCP_PLASM_CONTEXT_TOOL_DESCRIPTION.len() < 900,
            "plasm_context tool description too long: {} chars",
            super::MCP_PLASM_CONTEXT_TOOL_DESCRIPTION.len()
        );
        assert!(
            super::MCP_PROGRAM_PARAM_DESCRIPTION.len() < 200,
            "program param description too long: {} chars",
            super::MCP_PROGRAM_PARAM_DESCRIPTION.len()
        );
        let tools = super::PlasmMcpHandler::plasm_tools();
        let v = serde_json::to_value(
            tools
                .iter()
                .find(|t| t.name == "plasm")
                .expect("plasm tool")
                .input_schema
                .clone(),
        )
        .expect("input_schema json");
        let program_desc = v
            .get("properties")
            .and_then(|p| p.get("program"))
            .and_then(|s| s.get("description"))
            .and_then(|d| d.as_str())
            .expect("plasm program param description");
        assert_eq!(program_desc, super::MCP_PROGRAM_PARAM_DESCRIPTION);
    }

    #[test]
    fn mcp_tool_list_hides_internal_auth_and_registry_tools() {
        let names: Vec<String> = super::PlasmMcpHandler::plasm_tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(!names.iter().any(|n| n == "plasm_incoming_auth"));
        assert!(!names.iter().any(|n| n == "list_registry"));
        assert!(names.iter().any(|n| n == "plasm_context"));
        assert!(names.iter().any(|n| n == "discover_capabilities"));
        let removed_init_tool = format!("plasm_{}", "session_init");
        let removed_add_tool = format!("add_{}", "capabilities");
        assert!(!names.iter().any(|n| n == &removed_init_tool));
        assert!(!names.iter().any(|n| n == &removed_add_tool));
        assert!(!names.iter().any(|n| n == "add_code_capabilities"));
        assert!(!names.iter().any(|n| n == "evaluate_code_plan"));
        assert!(!names.iter().any(|n| n == "execute_code_plan"));
        assert!(!names.iter().any(|n| n == "execute"));
        assert!(names.iter().any(|n| n == "plasm"));
        assert!(names.iter().any(|n| n == "plasm_run"));
    }

    #[test]
    fn plasm_context_tool_description_snapshot() {
        with_insta_snapshots(|| {
            let tools = super::PlasmMcpHandler::plasm_tools();
            let context = tools
                .iter()
                .find(|t| t.name == "plasm_context")
                .and_then(|t| t.description.as_ref())
                .expect("plasm_context description")
                .clone();
            insta::assert_snapshot!("plasm_context_tool_description", context);
        });
    }

    #[test]
    fn plasm_context_tool_description_contract_append_vs_refresh() {
        let tools = super::PlasmMcpHandler::plasm_tools();
        let desc = tools
            .iter()
            .find(|t| t.name == "plasm_context")
            .and_then(|t| t.description.as_deref())
            .expect("plasm_context description");
        let workflow = super::MCP_SERVER_INITIALIZE_WORKFLOW;
        assert!(
            desc.contains("**Adding picks:**"),
            "expected append guidance in plasm_context description"
        );
        assert!(
            workflow.contains("Steady state:"),
            "expected steady-state guidance in initialize workflow"
        );
    }

    #[test]
    fn plasm_context_input_schema_requires_intent_and_seeds() {
        let tools = super::PlasmMcpHandler::plasm_tools();
        let ctx = tools
            .iter()
            .find(|t| t.name == "plasm_context")
            .expect("plasm_context tool");
        let v = serde_json::to_value(&ctx.input_schema).expect("input_schema json");
        let required = v
            .get("required")
            .and_then(|x| x.as_array())
            .expect("required array");
        assert!(required.iter().any(|x| x.as_str() == Some("intent")));
        assert!(required.iter().any(|x| x.as_str() == Some("seeds")));
        assert!(!required
            .iter()
            .any(|x| x.as_str() == Some("client_session_key")));
        let props = v
            .get("properties")
            .and_then(|x| x.as_object())
            .expect("properties object");
        assert!(
            props.contains_key("intent"),
            "expected `intent` property, got keys: {:?}",
            props.keys().collect::<Vec<_>>()
        );
        assert!(!props.contains_key("client_session_key"));
        assert_eq!(
            props
                .get("intent")
                .and_then(|x| x.get("type"))
                .and_then(|x| x.as_str()),
            Some("string")
        );
        assert!(
            props.contains_key("ranked_capabilities"),
            "expected optional ranked_capabilities on plasm_context"
        );
    }

    /// MCP hosts (e.g. Cursor) may validate `tools/call` args against the advertised JSON Schema
    /// from `tools/list`. Discovery accepts one `intent` string only; array-shaped `query` is a
    /// removed interface, not a compatibility path.
    #[test]
    fn discover_capabilities_input_schema_requires_single_intent_string() {
        let tools = super::PlasmMcpHandler::plasm_tools();
        let discover = tools
            .iter()
            .find(|t| t.name == "discover_capabilities")
            .expect("discover_capabilities tool");
        let v = serde_json::to_value(&discover.input_schema).expect("input_schema json");
        let required = v
            .get("required")
            .and_then(|x| x.as_array())
            .expect("required array");
        assert!(required.iter().any(|x| x.as_str() == Some("intent")));
        let props = v.get("properties").and_then(|p| p.as_object()).unwrap();
        assert!(!props.contains_key("query"));
        assert!(!props.contains_key("utterance"));
        let intent = v
            .get("properties")
            .and_then(|p| p.get("intent"))
            .expect("intent property in input_schema");
        assert_eq!(intent.get("type").and_then(|x| x.as_str()), Some("string"));
        assert_eq!(intent.get("minLength").and_then(|x| x.as_u64()), Some(1));
    }

    #[test]
    fn plasm_input_schema_advertises_single_program_string() {
        let tools = super::PlasmMcpHandler::plasm_tools();
        let plasm = tools
            .iter()
            .find(|t| t.name == "plasm")
            .expect("plasm tool");
        let v = serde_json::to_value(&plasm.input_schema).expect("input_schema json");
        let required = v
            .get("required")
            .and_then(|x| x.as_array())
            .expect("required array");
        assert!(required.iter().any(|x| x.as_str() == Some("program")));
        assert!(!required.iter().any(|x| x.as_str() == Some("expressions")));
        let props = v
            .get("properties")
            .and_then(|x| x.as_object())
            .expect("properties object");
        assert_eq!(
            props
                .get("program")
                .and_then(|x| x.get("type"))
                .and_then(|x| x.as_str()),
            Some("string")
        );
        assert!(!props.contains_key("expressions"));
        assert!(
            !props.contains_key("execute"),
            "`plasm` input_schema must not advertise `execute` (use `plasm_run` for live execution)"
        );
    }

    #[test]
    fn plasm_run_input_schema_matches_plasm_program_shape() {
        let tools = super::PlasmMcpHandler::plasm_tools();
        let plasm_run = tools
            .iter()
            .find(|t| t.name == "plasm_run")
            .expect("plasm_run tool");
        let v = serde_json::to_value(&plasm_run.input_schema).expect("input_schema json");
        let required = v
            .get("required")
            .and_then(|x| x.as_array())
            .expect("required array");
        assert!(required.iter().any(|x| x.as_str() == Some("program")));
        let props = v
            .get("properties")
            .and_then(|x| x.as_object())
            .expect("properties object");
        assert_eq!(
            props
                .get("program")
                .and_then(|x| x.get("type"))
                .and_then(|x| x.as_str()),
            Some("string")
        );
        assert!(!props.contains_key("execute"));
    }

    #[test]
    fn mcp_discover_ignores_unknown_json_keys() {
        let v = serde_json::json!({
            "intent": "find x resources",
            "kinds": ["query"],
        });
        let q = mcp_discover_query_from_arguments(&v).expect("deserialize");
        assert_eq!(q.tokens, vec!["find x resources"]);
        assert!(q.kinds.is_empty());
    }

    /// Reference output for `discover_capabilities` Markdown (fenced tabular block; same columns as discovery).
    #[test]
    fn discover_markdown_emits_tsv_snapshot() {
        use plasm_core::discovery::{
            CapabilityQuery, DiscoveryResult, EntitySummary, RankedCandidate,
        };
        let r = DiscoveryResult {
            contexts: vec![],
            candidates: vec![RankedCandidate {
                entry_id: "demo".into(),
                entity: "Widget".into(),
                capability_name: "list".into(),
                score: 2,
                reason_codes: vec![],
                capability_description: "List widgets".into(),
            }],
            ambiguities: vec![],
            applied_query_echo: CapabilityQuery::default(),
            closure_stats: None,
            schema_neighborhoods: vec![],
            entity_summaries: vec![EntitySummary {
                entry_id: "demo".into(),
                name: "Widget".into(),
                description: " A contrived \t widget \n line ".into(),
            }],
        };
        assert_snapshot!(
            crate::discovery_human_format::format_discovery_markdown(&r),
            @"
```tsv
api\tentity\tdescription
demo\tWidget\tA contrived widget line
```

"
        );
    }

    #[test]
    fn plasm_context_requires_non_empty_seeds() {
        let err = parse_tool_seeds("plasm_context", &serde_json::json!({ "seeds": [] }))
            .expect_err("expected invalid seeds");
        assert!(
            err.to_string().contains("non-empty array"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn plasm_context_legacy_shape_returns_actionable_error() {
        let err = parse_tool_seeds(
            "plasm_context",
            &serde_json::json!({ "entry_id": "pokeapi", "entities": ["Pokemon"] }),
        )
        .expect_err("expected invalid legacy shape");
        assert!(
            err.to_string()
                .contains("old top-level `{entry_id, entities}`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn plasm_context_seeds_accept_api_or_entry_id_alias() {
        let api = parse_tool_seeds(
            "plasm_context",
            &serde_json::json!({ "seeds": [{ "api": "pokeapi", "entity": "Pokemon" }] }),
        )
        .expect("api key");
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].entry_id, "pokeapi");
        assert_eq!(api[0].entity, "Pokemon");

        let legacy = parse_tool_seeds(
            "plasm_context",
            &serde_json::json!({ "seeds": [{ "entry_id": "pokeapi", "entity": "Pokemon" }] }),
        )
        .expect("entry_id alias");
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].entry_id, "pokeapi");
    }
}
