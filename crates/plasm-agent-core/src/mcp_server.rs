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
//! compact notice **without** replaying the full DOMAIN/TSV teaching text (token-saving); steady state is
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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::trace_hub::{CodePlanTrace, McpPlasmTraceSink, PlasmContextTrace};
use std::time::Duration;
use tracing::Instrument;

use async_trait::async_trait;
use base64::Engine as _;
use plasm_core::discovery::{CapabilityQuery, CgsCatalog, DiscoveryError};
use plasm_core::CgsDiscovery;
use plasm_discovery::DiscoveryQuery;
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::event_store::InMemoryEventStore;
use rust_mcp_sdk::mcp_server::hyper_server;
use rust_mcp_sdk::mcp_server::{
    HyperServer, HyperServerOptions, ServerHandler, ToMcpServerHandler,
};
use rust_mcp_sdk::schema::{schema_utils::CallToolError, ToolExecution, ToolExecutionTaskSupport};
use rust_mcp_sdk::schema::{
    BlobResourceContents, CallToolRequestParams, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceContent, ReadResourceRequestParams,
    ReadResourceResult, ResourceTemplate, RpcError, ServerCapabilities,
    ServerCapabilitiesResources, ServerCapabilitiesTools, TextContent, TextResourceContents, Tool,
    ToolAnnotations, ToolInputSchema,
};
use rust_mcp_sdk::McpServer;
use tokio::sync::{Mutex, RwLock};

use crate::execute_session::ExecuteSession;
use crate::http_execute::{
    apply_capability_seeds, execute_session_run_markdown, normalize_capability_seeds,
    ApplyCapabilitySeedsOutcome, CapabilitySeed, RankedCapabilitiesArg,
};
use crate::incoming_auth::{tenant_scope, IncomingAuthMethod, IncomingAuthMode, TenantPrincipal};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_policy;
use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::mcp_stream_auth::{config_id_from_auth_info, is_anonymous_mcp_auth};
use crate::plasm_dag::{
    compile_plasm_dag_to_plan, compile_plasm_surface_line_to_plan, is_plasm_dag_candidate,
    split_bare_plasm_roots,
};
use crate::plasm_plan::{parse_plan_value, validate_plan_artifact};
use crate::plasm_plan_run::{
    evaluate_validated_plasm_plan_dry, plasm_plan_dag_json, plasm_plan_review_guidance_lines,
    render_plasm_plan_dry_text, run_plasm_plan, PlasmPlanRunHooks, PlasmPlanRunResult,
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
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Best-effort bound on concurrent MCP transport sessions holding an execute binding (see module doc).
const MAX_MCP_EXEC_BINDINGS: usize = 512;

/// Model-facing **`plasm`** tool description: **plan-only** program construction (session setup is in initialize instructions).
pub(crate) const MCP_PLASM_TOOL_DESCRIPTION: &str = concat!(
    include_str!("mcp_prompt/plasm_tool_head.txt"),
    include_str!("mcp_prompt/shared_program_newlines.txt"),
    include_str!("mcp_prompt/plasm_tool_tail.txt"),
);

/// Model-facing **`plasm_run`** tool description: live execution after plan review.
pub(crate) const MCP_PLASM_RUN_TOOL_DESCRIPTION: &str =
    include_str!("mcp_prompt/plasm_run_tool.txt");

/// MCP initialize workflow text. The Plasm syntax guide is appended by [`mcp_server_initialize_instructions`].
pub(crate) const MCP_SERVER_INITIALIZE_WORKFLOW: &str = concat!(
    include_str!("mcp_prompt/workflow_head.txt"),
    include_str!("mcp_prompt/shared_program_newlines.txt"),
    include_str!("mcp_prompt/workflow_tail.txt"),
);

fn mcp_server_initialize_instructions() -> String {
    format!(
        "{workflow}\n\nPlasm syntax guide:\n\n{frontmatter}",
        workflow = MCP_SERVER_INITIALIZE_WORKFLOW,
        frontmatter = plasm_core::prompt_render::render_plasm_mcp_language_frontmatter(),
    )
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

fn parse_optional_principal(v: &serde_json::Value) -> Option<String> {
    v.get("principal")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

fn parse_logical_session_ref_arg(
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

fn plan_content_sha256_hex(plan: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(plan).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn plan_display_name_from_dag(plan_dag: &serde_json::Value) -> String {
    plan_dag
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "unnamed plan".to_string())
}

fn plan_node_count_from_dag(plan_dag: &serde_json::Value) -> usize {
    plan_dag
        .get("nodes")
        .and_then(|n| n.as_array())
        .map(Vec::len)
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
    plan: &serde_json::Value,
    program: &str,
    plan_dag: serde_json::Value,
    plan_call_index: u64,
) {
    let plan_hash_str = plan_content_sha256_hex(plan);
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
        name: plan_display_name_from_dag(&plan_dag),
        code: program.to_string(),
        plan_hash: plan_hash_str.clone(),
        plan: plan.clone(),
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
    let node_count = plan_node_count_from_dag(&plan_dag);
    let code_chars = program.chars().count() as u64;
    hub.trace_record_code_plan_evaluate(
        mcp_key,
        CodePlanTrace {
            plan_handle: handle_str,
            plan_id: plan_id.to_string(),
            plan_name: plan_display_name_from_dag(&plan_dag),
            plan_hash: plan_hash_str,
            plan_uri,
            canonical_plan_uri,
            plan_http_path,
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
            node_count,
            code_chars,
            dag: plan_dag,
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
    plan: &serde_json::Value,
    program: &str,
    plan_dag: serde_json::Value,
    plan_call_index: u64,
    out: &PlasmPlanRunResult,
) {
    let plan_hash_str = plan_content_sha256_hex(plan);
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
        name: plan_display_name_from_dag(&plan_dag),
        code: program.to_string(),
        plan_hash: plan_hash_str.clone(),
        plan: plan.clone(),
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
    let node_count = plan_node_count_from_dag(&plan_dag);
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
            plan_name: plan_display_name_from_dag(&plan_dag),
            plan_hash: plan_hash_str,
            plan_uri,
            canonical_plan_uri,
            plan_http_path,
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
            node_count,
            code_chars,
            dag: plan_dag,
            plasm_call_index: Some(plan_call_index),
            run_ids,
            run_artifacts: out.code_plan_run_artifacts.clone(),
        },
    )
    .await;
}

/// Per MCP transport session: Plasm execute `prompt_hash` + `session` ids (same as HTTP paths).
#[derive(Clone, Default)]
struct PlasmExecBinding {
    prompt_hash: String,
    session_id: String,
}

/// Cumulative MCP-side text volume for token-ish telemetry (Unicode scalar counts).
#[derive(Clone, Default, Debug)]
pub(crate) struct McpSessionPlasmStats {
    /// Plasm instructions body from `plasm_context` tool results.
    domain_prompt_chars: u64,
    /// `plasm` / `plasm_run` tool payloads: program string plus optional `reasoning`.
    plasm_invocation_chars: u64,
    /// Successful `plasm` / `plasm_run` tool Markdown bodies.
    plasm_response_chars: u64,
    plasm_call_count: u64,
}

#[derive(Default)]
struct McpLogicalSessionState {
    binding: Option<PlasmExecBinding>,
    stats: McpSessionPlasmStats,
    meta_index: PlasmMetaIndex,
}

#[derive(Default)]
struct McpTransportState {
    /// Logical session UUID string → per-agent state (execute binding, stats, `_meta.plasm` index).
    logical_by_id: HashMap<String, Arc<Mutex<McpLogicalSessionState>>>,
    /// Client-facing slot ids on this MCP transport (`s0`, …) → canonical logical session UUID.
    ref_to_uuid: HashMap<String, Uuid>,
    uuid_to_ref: HashMap<Uuid, String>,
    next_session_slot: u64,
}

impl McpTransportState {
    /// Assign a stable per-transport slot (`s{n}`) for this canonical logical id (idempotent).
    fn ensure_session_ref(&mut self, uuid: Uuid) -> String {
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
    plasm: Arc<PlasmHostState>,
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

    async fn session_state(&self, key: &str) -> Arc<Mutex<McpTransportState>> {
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

    async fn logical_mutex(
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

    async fn resolve_logical_session_ref_to_uuid(
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
    async fn resolve_binding_for_logical(
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
        let tp = mcp_chars_to_token_est(g.stats.domain_prompt_chars);
        let ti = mcp_chars_to_token_est(g.stats.plasm_invocation_chars);
        let tr = mcp_chars_to_token_est(g.stats.plasm_response_chars);
        (tp, ti, tr, tp.saturating_add(ti).saturating_add(tr))
    }

    /// Latest tenant MCP policy for this transport session (from HTTP `Authorization` + control-plane store).
    async fn tenant_mcp_cfg(
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
    async fn ensure_mcp_principal(
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
            "query".into(),
            json_schema_string_or_string_array(
                "Describe the **task or goal** you need catalog coverage for (plain language). Response is a table of matching **capabilities**: **`api`**, **`entity`**, **`description`** per row — turn the rows you need into **`plasm_context`** **`seeds`** (each row is one `{api, entity}` or `{entry_id, entity}`).",
            ),
        );
        discover_props.insert(
            "typed".into(),
            json_schema_bool_type(
                "If **true**, response is fenced **`json`** (`DiscoveryDecision`) for structured disambiguation instead of the capability table.",
            ),
        );
        discover_props.insert(
            "utterance".into(),
            json_schema_string_type(
                "Typed mode: optional single string for the same intent as **`query`**; if omitted, **`query`** is used.",
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
                "Typed mode: when **true** (default), use local fastembed recall (CPU, downloads model on first use). Set **false** for lexical-only.",
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
                "Non-empty **`seeds`** — each object is **one capability pick**: **`api`** (integration id) and **`entity`** (resource type). Include **every** pick this program needs on first open; several **`api`** values belong in the **same** array. Example: [{\"api\":\"github\",\"entity\":\"Issue\"},{\"api\":\"slack\",\"entity\":\"Channel\"}]. Per object you may use **`entry_id`** instead of **`api`**.",
                vec!["api", "entity"],
            ),
        );
        context_props.insert(
            "ranked_capabilities".into(),
            serde_json::from_value(serde_json::json!({
                "type": ["array", "null"],
                "items": { "type": "string" },
                "description": "Optional capability **wire names** (e.g. from `discover_capabilities`). When non-empty and intent-scoped DOMAIN is active, **mutating** capabilities must appear in this list **and** score against **`intent`**. Omit on expand to keep the session list; send **`null`** or **`[]`** to clear."
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
            json_schema_string_type(
                "JSON string: one Plasm line, a full multi-line `plasm_program`, or (on **`plasm_run` only`) comma-separated final roots. **Compositional programs:** literal newlines (U+000A) between physical lines — one `ident = …` binding per line, then roots — never multiple bindings separated only by spaces on one line. Good: `repo = e2(...)\\ncommits = …\\ncommits`. Bad: `repo = … commits = …` on one line. Heredocs span multiple physical lines inside this string; use teaching rows from `plasm_context`. **Tagged `<<TAG` heredocs close on the first line whose trimmed text equals `TAG`** — choose an opaque `TAG` that cannot appear as its own line inside the blob (long labels like `PLASM_MAIL_9c2e` for RFC822/MIME bodies; avoid short tags like `RFC` / `END`). **Bracket-render synthesis** (`[fields] <<TAG …` postfix after projection): Minijinja `rows`; rendered `content` may be any textual format (plain text, markdown, HTML, JSON text, …); **prefer** for outputs that summarize queried rows; **static** heredocs remain fine for fixed payloads not derived from those rows; string slots need `binding.content`, not bare `binding`. In brace predicates, entity-reference slots: DOMAIN constructors, scalar keys, or `binding.<relation>` — full rows only when identity fields match the target entity. `binding.<relation>` works from surface/relation rows and row-preserving projections such as `trimmed = row[id,name]`; aggregate/render/derive/data/for_each labels are not row anchors.",
            ),
        );
        plasm_program_props.insert(
            "reasoning".into(),
            json_schema_string_type("Optional short note explaining the intent of this call."),
        );

        let mut tools = vec![
            Tool {
                name: "plasm_context".into(),
                title: Some("Open or extend Plasm context".into()),
                description: Some(
                    "**Open or extend** one logical session. Send a stable **`intent`** and a non-empty **`seeds`** array: each object **`{ \"api\", \"entity\" }`** is **one capability pick**. **List every pick on the first open** — several **`api`** values in one array is normal and still **one** session for **`plasm`** / **`plasm_run`**. You may use **`entry_id`** instead of **`api`** per object. Returns **`logical_session_ref`** (`s0`, …) as the first line of the Markdown body. **Adding picks:** call again with **additional** `{api, entity}` rows when you discover more integrations. **Steady state:** reuse **`logical_session_ref`** with **`plasm`** / **`plasm_run`**; do **not** repeat **`plasm_context`** with identical **`seeds`** every turn—no-op expands omit full DOMAIN replay (token-saving).\n\n\
                    **Federation / symbols:** `e#` / `m#` / `p#` are **session-local and append-only**. Additional `plasm_context` picks append new teaching rows; prior `e#` / `m#` / `p#` meanings remain unchanged, so compare the latest response for newly added rows instead of reinterpreting old symbols. **`p#` tokens are scoped by registry `entry_id`, owning CGS entity, and slot identity** — same wire name on **different** entities or catalogs gets **distinct** opaque `p#` values (no structural sharing across unrelated rows). **`e#` order follows exposed entity rows** in DOMAIN. Prefer **canonical constructor keys** from the CGS (e.g. `owner=`, `repo=`) when multiple integrations load. **Postfix projections** use the **row entity** of that expression; bracket chains like `author[login]` normalize to dotted paths (`author.login`). **`m#` methods are per `(catalog entry_id, domain entity, kebab)`** — resolve `eN` from the current DOMAIN heading before calling `eN.mK()`. Response **`_meta.plasm`** may include **`domain_revision`**, **`domain_wave_count`**, and **`catalog_entry_ids`** so you can verify new teaching text shipped.".into(),
                ),
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
                    "Map what you are trying to do (**intent** / task wording) to concrete catalog **capabilities**. Send **`query`** as a short description of that intent. You get a table of candidate capabilities (`**api**`, **`entity**`, **`description`**); use the rows you need to build **`plasm_context`** **`seeds`**. Skip when you already know **`api`**/**`entity`** for every capability you need. With **`typed: true`**, the reply is fenced **`json`** (`DiscoveryDecision`) for stepwise disambiguation instead of the table.".into(),
                ),
                input_schema: ToolInputSchema::new(vec![], Some(discover_props), None),
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
            meta: None,
            output_schema: None,
        });
        tools.push(Tool {
            name: "plasm_run".into(),
            title: Some("Run Plasm (execute)".into()),
            description: Some(MCP_PLASM_RUN_TOOL_DESCRIPTION.into()),
            input_schema: ToolInputSchema::new(
                vec!["logical_session_ref".into(), "program".into()],
                Some(plasm_program_props),
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
        });
        tools
    }
}

fn json_schema_string_type(description: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::json!("string"));
    m.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    m
}

fn json_schema_bool_type(description: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("type".into(), serde_json::json!("boolean"));
    m.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    m
}

fn json_schema_string_or_string_array(
    description: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let v = serde_json::json!({
        "description": description,
        "oneOf": [
            {
                "type": "string",
                "minLength": 1,
                "description": "One intent phrase or keywords for the task."
            },
            {
                "type": "array",
                "minItems": 1,
                "items": { "type": "string", "minLength": 1 },
                "description": "Several strings for the same search (combined for coverage)."
            }
        ]
    });
    match v {
        serde_json::Value::Object(m) => m,
        _ => unreachable!(),
    }
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

/// MCP `discover_capabilities`: `query` is one string or several strings (same as HTTP discovery;
/// values feed [`CapabilityQuery::tokens`]).
fn mcp_discover_query_from_arguments(v: &serde_json::Value) -> Result<CapabilityQuery, String> {
    let Some(obj) = v.as_object() else {
        return Err("discover_capabilities arguments must be a JSON object".to_string());
    };
    let q = obj.get("query");
    let tokens: Vec<String> = match q {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                Vec::new()
            } else {
                vec![s.clone()]
            }
        }
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    serde_json::Value::String(s) if !s.is_empty() => out.push(s.clone()),
                    serde_json::Value::String(_) => {}
                    _ => {
                        return Err(
                            "discover_capabilities `query` array must contain only strings"
                                .to_string(),
                        );
                    }
                }
            }
            out
        }
        Some(_) => {
            return Err(
                "discover_capabilities `query` must be a string or an array of strings".to_string(),
            );
        }
    };
    Ok(CapabilityQuery {
        tokens,
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
    legacy_tokens: &[String],
) -> Result<DiscoveryQuery, String> {
    let utterance = obj
        .get("utterance")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| legacy_tokens.join(" ").trim().to_string());
    if utterance.is_empty() {
        return Err("typed discovery requires a non-empty `utterance` or `query`".to_string());
    }
    let max_options = obj
        .get("max_options")
        .and_then(|v| v.as_u64())
        .unwrap_or(8)
        .clamp(1, 32) as usize;
    let enable_embeddings = obj
        .get("enable_embeddings")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
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

/// MCP entity `description` column: max chars (Unicode scalars).
const MCP_DISCOVERY_ENTITY_SUMMARY_MAX: usize = 200;

/// Single-line TSV field: collapse whitespace, strip tabs, truncate (Unicode scalars).
fn mcp_discovery_tsv_field(s: &str, max_chars: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let no_tabs = collapsed.replace('\t', " ");
    let n = no_tabs.chars().count();
    if n <= max_chars {
        no_tabs
    } else {
        let head: String = no_tabs.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn entity_summary_description<'a>(
    entity_summaries: &'a [plasm_core::discovery::EntitySummary],
    entity: &str,
) -> Option<&'a str> {
    entity_summaries
        .iter()
        .find(|e| e.name == entity)
        .map(|e| e.description.as_str())
}

fn format_discovery_tsv_body(
    candidates: &[plasm_core::discovery::RankedCandidate],
    entity_summaries: &[plasm_core::discovery::EntitySummary],
) -> String {
    let mut by_entry: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for c in candidates {
        by_entry
            .entry(c.entry_id.clone())
            .or_default()
            .insert(c.entity.clone());
    }

    let mut lines = vec!["api\tentity\tdescription".to_string()];
    for (eid, entities) in &by_entry {
        for entity in entities {
            let description = entity_summary_description(entity_summaries, entity)
                .map(|raw| mcp_discovery_tsv_field(raw, MCP_DISCOVERY_ENTITY_SUMMARY_MAX))
                .unwrap_or_default();
            lines.push(format!(
                "{}\t{}\t{}",
                mcp_discovery_tsv_field(eid, 200),
                mcp_discovery_tsv_field(entity, 200),
                description,
            ));
        }
    }
    lines.join("\n")
}

fn format_discovery_markdown(r: &plasm_core::discovery::DiscoveryResult) -> String {
    use plasm_core::discovery::Ambiguity;

    let mut s = String::new();
    if r.candidates.is_empty() {
        s.push_str("_No matching entities._\n\n");
    } else {
        let body = format_discovery_tsv_body(&r.candidates, &r.entity_summaries);
        s.push_str("```tsv\n");
        s.push_str(&body);
        s.push_str("\n```\n\n");
    }

    if !r.ambiguities.is_empty() {
        s.push_str("**Same name in more than one `api`**\n\n");
        for Ambiguity {
            dimension: _,
            entry_ids,
            capability_name,
            score,
        } in &r.ambiguities
        {
            s.push_str(&format!(
                "- `{capability_name}` (score {score}) — pick one `api`: {}\n",
                entry_ids.join(", ")
            ));
        }
        s.push('\n');
    }

    s
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
    async fn handle_plasm_mcp_tool(
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
        let mut expressions = vec![program.clone()];
        if let Some(expanded) = split_bare_plasm_roots(&program) {
            expressions = expanded;
        }
        let reasoning = v
            .get("reasoning")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty());
        let line_count = expressions.len();
        if line_count > 1 && dry_run_only {
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(true),
                "error",
                "invalid_arguments",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(
                CallToolError::invalid_arguments(
                    tool_name,
                    Some(
                        "comma-separated multi-root expressions are not supported on plan-only `plasm`: compose one multi-line `plasm_program`, or call `plasm_run` with the same `logical_session_ref` and `program` after reviewing a dry-run plan."
                            .into(),
                    ),
                ),
            ));
        }
        let plasm_tool_span = if dry_run_only {
            crate::spans::mcp_tool_plasm(line_count > 1, line_count as u64, session_ref.as_str())
        } else {
            crate::spans::mcp_tool_plasm_run(
                line_count > 1,
                line_count as u64,
                session_ref.as_str(),
            )
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
                Some(line_count > 1),
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
                Some(line_count > 1),
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
                line_count > 1,
                line_count,
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

        if expressions.len() == 1 {
            let Some(es) = self
                .plasm
                .sessions
                .get_by_strs(&b.prompt_hash, &b.session_id)
                .await
            else {
                return Ok(CallToolResult::with_error(CallToolError::from_message(
                    "Execute session expired: call `plasm_context` again with your capability picks (`seeds`) to open a new session.",
                )));
            };
            let plan_name = format!("plasm_dag_call_{call_count}");
            let pipeline = self.plasm.engine.prompt_pipeline();
            let cross = self.plasm.sessions.symbol_map_cross_cache();
            let compile = if is_plasm_dag_candidate(&expressions) {
                compile_plasm_dag_to_plan(pipeline, Some(cross), &es, &plan_name, &expressions[0])
            } else {
                compile_plasm_surface_line_to_plan(
                    pipeline,
                    Some(cross),
                    &es,
                    &plan_name,
                    &expressions[0],
                )
            };
            let run_result = match compile {
                Ok(plan) => {
                    if run_live {
                        match run_plasm_plan(
                            &es,
                            self.plasm.as_ref(),
                            principal_incoming.as_ref(),
                            &b.prompt_hash,
                            &b.session_id,
                            &plan,
                            true,
                            Some(PlasmPlanRunHooks {
                                meta_index: &mut idx,
                                trace: mcp_trace.clone(),
                                sink: sink.clone(),
                            }),
                        )
                        .await
                        {
                            Ok(out) => {
                                trace_archive_and_emit_code_plan_execute(
                                    &self.plasm.trace_hub,
                                    &self.plasm.run_artifacts,
                                    &ls_key,
                                    &es,
                                    b.prompt_hash.as_str(),
                                    b.session_id.as_str(),
                                    session_ref.as_str(),
                                    &plan,
                                    &program,
                                    out.plan_dag.clone(),
                                    call_count,
                                    &out,
                                )
                                .await;
                                Ok(out)
                            }
                            Err(e) => Err(e),
                        }
                    } else {
                        match parse_plan_value(&plan) {
                            Err(e) => Err(e),
                            Ok(plan_typed) => match validate_plan_artifact(&plan_typed) {
                                Err(e) => Err(e),
                                Ok(validated) => {
                                    match evaluate_validated_plasm_plan_dry(&es, &validated) {
                                        Err(e) => Err(e),
                                        Ok(dry) => {
                                            let dry_text = render_plasm_plan_dry_text(&dry, None);
                                            let guidance = plasm_plan_review_guidance_lines(&dry);
                                            let markdown = format!("```text\n{dry_text}\n```");
                                            let plan_json = plasm_plan_dag_json(&dry);
                                            trace_archive_and_emit_code_plan_evaluate(
                                                &self.plasm.trace_hub,
                                                &self.plasm.run_artifacts,
                                                &ls_key,
                                                &es,
                                                b.prompt_hash.as_str(),
                                                b.session_id.as_str(),
                                                session_ref.as_str(),
                                                &plan,
                                                &program,
                                                plan_json.clone(),
                                                call_count,
                                            )
                                            .await;
                                            let mut plasm_obj = serde_json::Map::new();
                                            plasm_obj
                                                .insert("dry_run".into(), serde_json::json!(true));
                                            plasm_obj.insert("plan".into(), plan_json.clone());
                                            plasm_obj.insert(
                                                "guidance".into(),
                                                serde_json::Value::Array(
                                                    guidance
                                                        .into_iter()
                                                        .map(serde_json::Value::String)
                                                        .collect(),
                                                ),
                                            );
                                            let mut meta = serde_json::Map::new();
                                            meta.insert(
                                                "plasm".into(),
                                                serde_json::Value::Object(plasm_obj),
                                            );
                                            Ok(PlasmPlanRunResult {
                                                version: dry.version,
                                                node_results: dry.node_results,
                                                graph_summary: dry.graph_summary,
                                                plan_dag: plan_json,
                                                code_plan_run_artifacts: Vec::new(),
                                                run_markdown: Some(markdown),
                                                run_plasm_meta: Some(meta),
                                            })
                                        }
                                    }
                                }
                            },
                        }
                    }
                }
                Err(e) => Err(e),
            };
            {
                let mut g = state.lock().await;
                g.meta_index = idx;
            }
            match run_result {
                Ok(out) => {
                    let markdown = out.run_markdown.unwrap_or_else(|| {
                        "# Plasm program plan\n\nNo execution output.".to_string()
                    });
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
                    }
                    return Ok(res);
                }
                Err(msg) => {
                    self.plasm
                        .trace_hub
                        .trace_add_plasm_error(&ls_key, call_index, None, msg.clone())
                        .await;
                    crate::metrics::record_mcp_tool(
                        tool_name,
                        Some(false),
                        "error",
                        "execute_failed",
                        started.elapsed(),
                    );
                    return Ok(CallToolResult::with_error(CallToolError::from_message(msg)));
                }
            }
        }

        let run_result = execute_session_run_markdown(
            self.plasm.as_ref(),
            principal_incoming.as_ref(),
            &b.prompt_hash,
            &b.session_id,
            expressions,
            Some(&mut idx),
            Some(mcp_trace),
            Some(sink),
        )
        .instrument(plasm_tool_span)
        .await;
        {
            let mut g = state.lock().await;
            g.meta_index = idx;
        }
        match run_result {
            Ok(out) => {
                let response_chars = out.markdown.chars().count() as u64;
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
                            line_count > 1,
                            line_count,
                        )
                        .await;
                }
                let (tok_prompt, tok_inv, tok_resp, tok_total) =
                    self.mcp_plasm_token_snapshot_logical(key, &ls_key).await;
                tracing::info!(
                    target: "plasm_agent::mcp",
                    tool = tool_name,
                    line_count,
                    multi_line = line_count > 1,
                    ok = true,
                    tokens_est_prompt = tok_prompt,
                    tokens_est_invocation = tok_inv,
                    tokens_est_tool_response = tok_resp,
                    tokens_est_session_total = tok_total,
                    "MCP tool: plasm / plasm_run (expression detail: plasm_agent::http_execute)"
                );
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(line_count > 1),
                    "success",
                    "none",
                    started.elapsed(),
                );
                let blocks = vec![ContentBlock::TextContent(TextContent::new(
                    out.markdown,
                    None,
                    None,
                ))];
                let mut res = CallToolResult::from_content(blocks);
                if let Some(m) = out.tool_meta {
                    res = res.with_meta(Some(m));
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
                    line_count,
                    multi_line = line_count > 1,
                    tokens_est_prompt = tok_prompt,
                    tokens_est_invocation = tok_inv,
                    tokens_est_tool_response = tok_resp,
                    tokens_est_session_total = tok_total,
                    message = %msg,
                    "MCP tool: plasm / plasm_run failed"
                );
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(line_count > 1),
                    "error",
                    "execute_failed",
                    started.elapsed(),
                );
                Ok(CallToolResult::with_error(CallToolError::from_message(msg)))
            }
        }
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
                            run_id,
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
                run_id,
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
            run_id,
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
                run_id = %run_id,
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

    #[tracing::instrument(
        skip(self, runtime),
        name = "plasm_agent.mcp.call_tool",
        fields(mcp.tool = %params.name),
        level = "trace"
    )]
    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let key = mcp_key(&runtime)?;
        let v = args_value(&params);

        match params.name.as_str() {
            "plasm_context" => {
                let started = Instant::now();
                let tname = "plasm_context";
                let res: Result<CallToolResult, CallToolError> = async {
                    let principal_incoming = self.ensure_mcp_principal(&key, &runtime).await?;
                    let intent = v
                        .get("intent")
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| {
                        CallToolError::invalid_arguments(
                            tname,
                            Some("missing `intent`".into()),
                        )
                    })?;
                    let scope = tenant_scope(principal_incoming.as_ref());
                    let rec = self
                        .plasm
                        .logical_sessions
                        .init_session(&scope, &ClientSessionKey::new(intent))
                        .await;
                    let logical_session_ref = {
                        let transport = self.session_state(&key).await;
                        let mut g = transport.lock().await;
                        g.ensure_session_ref(rec.logical_session_id.as_uuid())
                    };
                    let logical_uuid = rec.logical_session_id.as_uuid();
                    let ls_key = logical_uuid.to_string();
                    let seeds = parse_tool_seeds(tname, &v)?;
                    let ranked_capabilities = parse_plasm_context_ranked_capabilities(tname, &v)?;
                    let principal = parse_optional_principal(&v);
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
                    let tcfg = self.tenant_mcp_cfg(&runtime).await?;
                    if let Some(ref cfg) = tcfg {
                        for eid in &distinct_entries {
                            if !cfg.entry_allowed(eid) {
                                return Err(CallToolError::from_message(format!(
                                    "entry_id not allowed by tenant MCP configuration: {eid}"
                                )));
                            }
                        }
                    }
                    let binding = self
                        .resolve_binding_for_logical(&key, logical_uuid)
                        .await;
                    tracing::debug!(
                        target: "plasm_agent::mcp",
                        tool = tname,
                        logical_session_ref = %logical_session_ref,
                        logical_session_id = %ls_key,
                        mcp_execute_binding_present = binding.is_some(),
                        "MCP plasm_context: Plasm execute binding before apply_capability_seeds (false means open path; true means expand/federate against existing prompt_hash/session)"
                    );
                    let context_span =
                        crate::spans::mcp_tool_plasm_context(logical_session_ref.as_str());
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
                            if g.len() >= MAX_MCP_EXEC_BINDINGS && !g.contains_key(&key) {
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
                        let ls = self.logical_mutex(&key, &ls_key).await;
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
                    let trace_meta = self.trace_session_meta(&key, &runtime).await;
                    self.plasm
                        .trace_hub
                        .ensure_logical_session(&ls_key, Some(&key), trace_meta)
                        .await;

                    let mut text = String::new();
                    for wave in &out.waves {
                        if !wave.markdown_delta.is_empty() {
                            text.push_str(&wave.markdown_delta);
                            if !text.ends_with('\n') {
                                text.push('\n');
                            }
                        }
                    }
                    for wave in &out.waves {
                        if wave.domain_prompt_chars_added > 0 {
                            let ls = self.logical_mutex(&key, &ls_key).await;
                            let mut g = ls.lock().await;
                            g.stats.domain_prompt_chars = g
                                .stats
                                .domain_prompt_chars
                                .saturating_add(wave.domain_prompt_chars_added);
                        }
                        self.plasm
                            .trace_hub
                            .trace_record_plasm_context(
                                &ls_key,
                                PlasmContextTrace {
                                    domain_prompt_chars_added: wave.domain_prompt_chars_added,
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
                    let execute_binding =
                        json!({ "prompt_hash": out.prompt_hash, "session_id": out.session_id });
                    let mut plasm = serde_json::Map::new();
                    plasm.insert(
                        "logical_session_id".to_string(),
                        json!(rec.logical_session_id.to_string()),
                    );
                    plasm.insert("execute_binding".to_string(), execute_binding.clone());
                    let mut continuity = serde_json::Map::new();
                    continuity.insert(
                        "stale_binding_recovered".to_string(),
                        json!(out.stale_execute_binding_recovered),
                    );
                    if out.stale_execute_binding_recovered {
                        if let Some((ref ph, ref sid)) = out.stale_binding_previous {
                            continuity.insert(
                                "previous_execute".to_string(),
                                json!({ "prompt_hash": ph, "session_id": sid }),
                            );
                        }
                    }
                    continuity.insert(
                        "new_symbol_space".to_string(),
                        json!(out.new_symbol_space),
                    );
                    if out.new_symbol_space {
                        continuity.insert(
                            "discard_cached_plasm_symbols".to_string(),
                            json!(true),
                        );
                    }
                    plasm.insert("continuity".to_string(), serde_json::Value::Object(continuity));
                    plasm.insert(
                        "logical_session_ref".to_string(),
                        json!(logical_session_ref),
                    );
                    plasm.insert(
                        "intent".to_string(),
                        json!(rec.intent.as_str()),
                    );
                    plasm.insert("tenant_scope".to_string(), json!(rec.tenant_scope));
                    plasm.insert(
                        "domain_wave_count".to_string(),
                        json!(out.waves.len()),
                    );
                    if let Some(sess_arc) = self
                        .plasm
                        .sessions
                        .get_by_strs(&out.prompt_hash, &out.session_id)
                        .await
                    {
                        plasm.insert(
                            "domain_revision".to_string(),
                            json!(sess_arc.domain_revision),
                        );
                        let mut loaded: Vec<String> =
                            sess_arc.contexts_by_entry.keys().cloned().collect();
                        loaded.sort();
                        plasm.insert("catalog_entry_ids".to_string(), json!(loaded));
                    }
                    if text.is_empty() {
                        text = format!("`{logical_session_ref}`\n");
                    } else {
                        text = format!("`{logical_session_ref}`\n\n{text}");
                    }
                    let mut res = CallToolResult::text_content(vec![TextContent::new(
                        text, None, None,
                    )]);
                    if !plasm.is_empty() {
                        let mut meta = serde_json::Map::new();
                        meta.insert("plasm".to_string(), serde_json::Value::Object(plasm));
                        res = res.with_meta(Some(meta));
                    }
                    Ok(res)
                }
                .await;
                let elapsed = started.elapsed();
                match &res {
                    Ok(_) => {
                        crate::metrics::record_mcp_tool(tname, None, "success", "none", elapsed)
                    }
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
                let res: Result<CallToolResult, CallToolError> = async {
                    self.ensure_mcp_principal(&key, &runtime).await?;
                    let q = mcp_discover_query_from_arguments(&v).map_err(|msg| {
                        CallToolError::invalid_arguments("discover_capabilities", Some(msg))
                    })?;
                    let discover_span = crate::spans::mcp_tool_discover_capabilities();
                    let _discover_guard = discover_span.enter();
                    tracing::info!(
                        target: "plasm_agent::mcp",
                        tool = "discover_capabilities",
                        query = ?q.tokens,
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
                        let mut dq = mcp_typed_discovery_query_from_arguments(obj, &q.tokens)
                            .map_err(|msg| {
                                CallToolError::invalid_arguments("discover_capabilities", Some(msg))
                            })?;
                        let tcfg = self.tenant_mcp_cfg(&runtime).await?;
                        if let Some(cfg) = tcfg {
                            if dq.allowed_entry_ids.is_empty() {
                                dq.allowed_entry_ids = mcp_policy::filter_registry_entries(
                                    reg.list_entries(),
                                    cfg.as_ref(),
                                )
                                .into_iter()
                                .map(|m| m.entry_id)
                                .collect();
                            } else {
                                dq.allowed_entry_ids.retain(|e| cfg.entry_allowed(e));
                            }
                        }
                        let decision = run_typed_catalog_discovery(&reg, dq)
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
                    let tcfg = self.tenant_mcp_cfg(&runtime).await?;
                    if let Some(cfg) = tcfg {
                        r = mcp_policy::filter_discovery_result(r, cfg.as_ref());
                    }
                    let text = format_discovery_markdown(&r);
                    Ok(CallToolResult::text_content(vec![TextContent::new(
                        text, None, None,
                    )]))
                }
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
            "plasm" | "plasm_run" => {
                let started = Instant::now();
                let dry_run_only = matches!(params.name.as_str(), "plasm");
                let tool_name: &'static str = if dry_run_only { "plasm" } else { "plasm_run" };
                self.handle_plasm_mcp_tool(&key, &runtime, &v, tool_name, dry_run_only, started)
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
}

/// Detect MCP transport sessions that disappeared from the SDK session store (disconnect / DELETE),
/// finalize logical-session traces that are no longer live, and drop orphaned per-transport state.
fn spawn_mcp_domain_prompt_session_reporter(
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
                let tp = mcp_chars_to_token_est(stats.domain_prompt_chars);
                let ti = mcp_chars_to_token_est(stats.plasm_invocation_chars);
                let tr = mcp_chars_to_token_est(stats.plasm_response_chars);
                let tt = tp.saturating_add(ti).saturating_add(tr);
                tracing::info!(
                    target: "plasm_agent::mcp",
                    logical_session_id = %ended,
                    domain_prompt_chars_total = stats.domain_prompt_chars,
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
            name: "plasm-agent".into(),
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
    spawn_mcp_domain_prompt_session_reporter(&server, Arc::clone(&plasm), session_states);
    server.start().await
}

#[cfg(test)]
mod tests {
    use super::{mcp_discover_query_from_arguments, parse_tool_seeds};
    use insta::assert_snapshot;

    #[test]
    fn mcp_discover_maps_subset_to_capability_query() {
        let v = serde_json::json!({
            "query": ["electric", "type chart"],
        });
        let q = mcp_discover_query_from_arguments(&v).expect("deserialize");
        assert_eq!(q.tokens, vec!["electric", "type chart"]);
        assert!(q.phrases.is_empty());
        assert!(q.entity_hints.is_empty());
        assert!(q.pick_entry.is_none());
        assert!(q.kinds.is_empty());
    }

    #[test]
    fn mcp_discover_query_accepts_single_intent_string() {
        let v = serde_json::json!({
            "query": "github repository commits git linear issue",
        });
        let q = mcp_discover_query_from_arguments(&v).expect("deserialize");
        assert_eq!(q.tokens, vec!["github repository commits git linear issue"]);
    }

    #[test]
    fn plasm_invocation_char_count_sums_program_and_reasoning() {
        assert_eq!(super::plasm_invocation_char_count("a", None), 1);
        assert_eq!(super::plasm_invocation_char_count("a", Some("#c")), 1 + 2);
    }

    /// Model-facing copy; update with `INSTA_UPDATE=1 cargo test -p plasm-agent-core mcp_`.
    #[test]
    fn mcp_plasm_tool_description_snapshot() {
        insta::assert_snapshot!(
            "mcp_plasm_tool_description",
            super::MCP_PLASM_TOOL_DESCRIPTION
        );
    }

    #[test]
    fn mcp_plasm_run_tool_description_snapshot() {
        insta::assert_snapshot!(
            "mcp_plasm_run_tool_description",
            super::MCP_PLASM_RUN_TOOL_DESCRIPTION
        );
    }

    #[test]
    fn mcp_server_initialize_instructions_snapshot() {
        insta::assert_snapshot!(
            "mcp_server_initialize_instructions",
            super::mcp_server_initialize_instructions()
        );
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
        let tools = super::PlasmMcpHandler::plasm_tools();
        let context = tools
            .iter()
            .find(|t| t.name == "plasm_context")
            .and_then(|t| t.description.as_ref())
            .expect("plasm_context description")
            .clone();
        insta::assert_snapshot!("plasm_context_tool_description", context);
    }

    #[test]
    fn plasm_context_tool_description_contract_append_vs_refresh() {
        let tools = super::PlasmMcpHandler::plasm_tools();
        let desc = tools
            .iter()
            .find(|t| t.name == "plasm_context")
            .and_then(|t| t.description.as_deref())
            .expect("plasm_context description");
        assert!(
            desc.contains("**Adding picks:**") && desc.contains("**Steady state:**"),
            "expected append vs steady-state distinction in plasm_context description"
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
    /// from `tools/list`. `query` must allow a single string so intent-style searches are not
    /// rejected as "expected a sequence" before the request reaches the server.
    #[test]
    fn discover_capabilities_input_schema_advertises_string_or_array_query() {
        use serde_json::json;
        let tools = super::PlasmMcpHandler::plasm_tools();
        let discover = tools
            .iter()
            .find(|t| t.name == "discover_capabilities")
            .expect("discover_capabilities tool");
        let v = serde_json::to_value(&discover.input_schema).expect("input_schema json");
        let q = v
            .get("properties")
            .and_then(|p| p.get("query"))
            .expect("query property in input_schema");
        let one_of = q
            .get("oneOf")
            .and_then(|x| x.as_array())
            .expect("query.oneOf array");
        assert!(
            one_of.len() >= 2,
            "query schema should oneOf string and array, got: {}",
            json!(q)
        );
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
            "query": ["x"],
            "kinds": ["query"],
        });
        let q = mcp_discover_query_from_arguments(&v).expect("deserialize");
        assert_eq!(q.tokens, vec!["x"]);
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
                name: "Widget".into(),
                description: " A contrived \t widget \n line ".into(),
            }],
        };
        assert_snapshot!(
            super::format_discovery_markdown(&r),
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
