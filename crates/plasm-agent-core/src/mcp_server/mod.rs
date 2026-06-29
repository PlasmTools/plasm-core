//! MCP Streamable HTTP server (rust-mcp-sdk) over Plasm discovery + execute ([`crate::server_state::PlasmHostState`]).
//! Tool results use Markdown [`TextContent`]; **`plasm`** sets `CallToolResult._meta.plasm` for **plan-only**
//! dry-runs (`plan` + `guidance`); **`plasm_run`** performs live execution and may attach request fingerprints,
//! artifact URIs, and optional `lossy_summary_fields` per truncated step in `_meta.plasm`.
//! Run snapshot URIs in Markdown use logical-session short form `plasm://session/{logical_session_ref}/r/{n}`
//! (canonical `l_<token>` wire ref; see [`crate::run_artifacts::plasm_session_short_resource_uri`]);
//! canonical `plasm://execute/.../run/{uuid}` remains accepted on read.
//! Tool results may include run snapshot URIs and inline hints when full data requires MCP `resources/read`;
//! the server repeats that obligation in the reply when it applies.
//!
//! Execute bindings (`plasm_context` → `plasm` / `plasm_run`) are stored **per agent logical session**
//! ([`PlasmExecBinding`]), keyed by canonical logical session UUID from `plasm_context` (client uses stateless **`logical_session_ref`**: `l_<token>`).
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
//! Tool text returns **table-only** teaching TSV on fresh `plasm_context` opens (`reused: false`); repeated
//! opens with the same entry + capability picks omit the teaching body to avoid token churn.
//! **Symbols:** for a fixed binding (`prompt_hash` + `session`), `e#` / `m#` / `p#` grow **append-only**
//! when you add new picks; they do not reshuffle. A new primary catalog open or logical session starts a new
//! symbol space — always read tokens from the current session `prompt` / Plasm language text.
//! A soft cap evicts one arbitrary older binding when the map grows past [`MAX_MCP_EXEC_BINDINGS`].
//!
//! Plasm language / instructions body (first update on `plasm_context` open plus append-only deltas when you add
//! capability picks via `plasm_context`'s `seeds`) is counted in Unicode scalar values per MCP transport session.
//! `plasm` calls also accumulate invocation text (`program` plus optional `reasoning`); both
//! `plasm` and `plasm_run` accumulate returned Markdown on success. Server logs use a rough **token estimate** ≈ `ceil(chars / 4)` per
//! bucket (`prompt` / `invocation` / `tool_response`). When the session leaves the SDK session store,
//! an `INFO` line logs cumulative character totals and token estimates (`plasm_agent::mcp`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::discovery_human_format::{format_discovery_markdown_for_mcp, DiscoveryTablePolicy};
use crate::trace_hub::{McpPlasmTraceSink, PlanRunTraceHooks, PlasmContextTrace};
use std::time::Duration;
use tracing::Instrument;

use async_trait::async_trait;
use base64::Engine as _;
use plasm_core::discovery::{CapabilityQuery, CgsCatalog, DiscoveryError};
use plasm_core::CgsDiscovery;
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::event_store::InMemoryEventStore;
use rust_mcp_sdk::mcp_server::hyper_server;
use rust_mcp_sdk::mcp_server::{
    HyperServer, HyperServerOptions, ServerHandler, ToMcpServerHandler,
};
use rust_mcp_sdk::schema::schema_utils::{CallToolError, CustomNotification};
use rust_mcp_sdk::schema::SdkError;
use rust_mcp_sdk::schema::{
    BlobResourceContents, CallToolRequestParams, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceContent, ReadResourceRequestParams,
    ReadResourceResult, ResourceTemplate, RpcError, ServerCapabilities,
    ServerCapabilitiesResources, ServerCapabilitiesTools, TextContent, TextResourceContents,
};
use rust_mcp_sdk::session_store::SessionStore;
use rust_mcp_sdk::McpServer;
use tokio::sync::{Mutex, RwLock};

use crate::mcp_transport_store::{
    PlasmTransportRedisStore, RedisSessionStore, SessionRuntimeFactory,
};

use crate::execute_session::ExecuteSession;
use crate::http_execute::{
    apply_capability_seeds, build_plasm_context_agent_markdown, build_plasm_context_tool_meta,
    normalize_capability_seeds, try_dispatch_operation_program, ApplyCapabilitySeedsOutcome,
    CapabilitySeed, RankedCapabilitiesArg,
};
use crate::incoming_auth::{tenant_scope, IncomingAuthMode, TenantPrincipal};
use crate::mcp_logical_ref::{format_logical_session_wire_ref, parse_logical_session_wire_ref};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_policy;
use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::mcp_stream_identity::McpTransportIdentity;
use crate::operation::{
    compute_plan_commit_id_from_dry, plan_commit_meta, PlanCommitDryCache, PlanCommitRecord,
    PLAN_COMMIT_TTL,
};
use crate::plan_dry_display::build_plan_dry_compact_view;
use crate::plasm_comp_wire::trace_comp_wire_from_dry;
use crate::plasm_compile::compile_plasm_expression;
use crate::plasm_plan_run::{
    evaluate_plasm_comp_dry, render_plasm_plan_dry_text_for_session, PlasmPlanRunResult,
};
use crate::run_artifacts::{
    code_plan_handle, code_plan_http_path, plasm_code_plan_resource_uri,
    plasm_session_short_plan_uri, ArtifactPayload, CodePlanArchiveDocument,
};
use crate::server_state::PlasmHostState;
use crate::session_identity::{ClientSessionKey, LogicalSessionId};
use crate::trace_sink_emit::PlasmTraceContext;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

/// Best-effort bound on concurrent MCP transport sessions holding an execute binding (see module doc).
mod artifact_access;
mod artifact_resolve;
mod committed_plasm_run;
mod discover;
mod mcp_http_user_agent;
mod mcp_plasm_invoke;
mod read_run_artifact;
mod resource_read;
mod resource_read_trace;
mod schema;
mod tool_parse;
mod tools;
mod trace;
mod transport;

#[cfg(test)]
mod integration;
#[cfg(test)]
mod tests;

pub(crate) use discover::{
    discovery_mcp_error, mcp_call_tool_error_class, mcp_discover_query_from_arguments, mcp_key,
};
pub(crate) use mcp_plasm_invoke::{parse_mcp_plasm_invocation, McpPlasmInvocation};
use plasm_core::prompt_render::MCP_INITIALIZE_WORKFLOW;
pub(crate) use schema::{args_value, json_schema_non_empty_string_type, json_schema_string_type};
pub(crate) use tool_parse::{
    comp_content_sha256_hex, parse_logical_session_ref_arg, parse_optional_principal,
    parse_plasm_context_ranked_capabilities, parse_tool_seeds, plan_display_name_from_comp,
    plan_node_count_from_comp,
};
pub(crate) use trace::CodePlanTraceInput;
pub(crate) use transport::{
    mcp_chars_to_token_est, plasm_invocation_char_count, McpLogicalSessionState,
    McpSessionPlasmStats, McpTransportState, PlasmExecBinding,
};

const MAX_MCP_EXEC_BINDINGS: usize = 512;

const MCP_EXECUTE_SESSION_UNAVAILABLE: &str = "Execute session unavailable (expired or catalog reload): call `plasm_context` again with your capability picks (`seeds`).";

/// Per MCP transport session: Plasm execute `prompt_hash` + `session` ids (same as HTTP paths).
pub(crate) struct PlasmMcpHandler {
    pub(crate) plasm: Arc<PlasmHostState>,
    /// MCP transport session key -> per-session mutable state.
    session_states: Arc<RwLock<HashMap<String, Arc<Mutex<McpTransportState>>>>>,
    transport_redis: Option<Arc<crate::mcp_transport_store::PlasmTransportRedisStore>>,
}

impl PlasmMcpHandler {
    pub(crate) fn new(plasm: Arc<PlasmHostState>) -> Self {
        Self {
            plasm,
            session_states: Arc::new(RwLock::new(HashMap::new())),
            transport_redis: None,
        }
    }

    pub(crate) fn with_transport_redis(
        mut self,
        store: Arc<crate::mcp_transport_store::PlasmTransportRedisStore>,
    ) -> Self {
        self.transport_redis = Some(store);
        self
    }

    pub(crate) async fn persist_transport_state(&self, transport_key: &str) {
        let Some(redis) = self.transport_redis.as_ref() else {
            return;
        };
        let state = self.session_state(transport_key).await;
        let snapshot = {
            let g = state.lock().await;
            g.to_persisted()
        };
        let mut merged: crate::mcp_transport_store::PersistedPlasmTransportState = snapshot;
        {
            let g = state.lock().await;
            for (id, ls) in &g.logical_by_id {
                if let Some(binding) = ls.lock().await.binding.clone() {
                    merged.logical_bindings.insert(id.clone(), binding);
                }
            }
        }
        redis.save_snapshot(transport_key, &merged).await;
    }
    pub(crate) async fn session_state(&self, key: &str) -> Arc<Mutex<McpTransportState>> {
        {
            let g = self.session_states.read().await;
            if let Some(state) = g.get(key) {
                if let Some(redis) = self.transport_redis.as_ref() {
                    redis.touch(key).await;
                }
                return Arc::clone(state);
            }
        }
        let hydrated = if let Some(redis) = self.transport_redis.as_ref() {
            redis.load(key).await.map(McpTransportState::from_persisted)
        } else {
            None
        };
        let mut g = self.session_states.write().await;
        Arc::clone(
            g.entry(key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(hydrated.unwrap_or_default()))),
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

    pub(crate) fn resolve_logical_session_ref_to_uuid(
        &self,
        tool: &str,
        ref_str: &str,
    ) -> Result<Uuid, CallToolError> {
        parse_logical_session_wire_ref(ref_str)
            .map(|id| id.as_uuid())
            .map_err(|e| CallToolError::invalid_arguments(tool, Some(e.to_string())))
    }

    pub(crate) async fn resolve_binding_stateless(
        &self,
        logical_uuid: Uuid,
    ) -> Option<PlasmExecBinding> {
        self.plasm
            .logical_execute_bindings
            .get(&logical_uuid)
            .await
            .map(|(ph, sid)| PlasmExecBinding {
                prompt_hash: ph,
                session_id: sid,
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
        self.plasm
            .logical_execute_bindings
            .get(&logical_uuid)
            .await
            .map(|(ph, sid)| PlasmExecBinding {
                prompt_hash: ph,
                session_id: sid,
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

        if McpTransportIdentity::from_auth_info(&info).is_some_and(|i| i.anonymous) {
            return Ok(None);
        }

        let identity = McpTransportIdentity::from_auth_info(&info);
        let Some(id) = identity.and_then(|i| i.mcp_config_id) else {
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
        McpTransportIdentity::from_auth_info(&info)?.to_tenant_principal()
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

    pub(crate) async fn resolved_artifact_access_mode(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
    ) -> crate::mcp_run_markdown::ArtifactAccessMode {
        let transport = self.session_state(transport_key).await;
        {
            let g = transport.lock().await;
            if let Some(mode) = g.artifact_access_mode {
                return mode;
            }
        }
        self.detect_and_cache_artifact_access_mode(transport_key, runtime)
            .await
    }

    pub(crate) async fn artifact_access_mode_for_runtime(
        &self,
        runtime: &Arc<dyn McpServer>,
    ) -> crate::mcp_run_markdown::ArtifactAccessMode {
        if let Some(key) = runtime.session_id() {
            return self.resolved_artifact_access_mode(&key, runtime).await;
        }
        runtime.wait_for_initialization().await;
        artifact_access::detect_artifact_access_mode(
            runtime.client_info().as_ref(),
            artifact_access::client_user_agent_hint(None).as_deref(),
        )
    }

    async fn detect_and_cache_artifact_access_mode(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
    ) -> crate::mcp_run_markdown::ArtifactAccessMode {
        let transport = self.session_state(transport_key).await;
        if let Some(mode) = artifact_access::artifact_access_mode_from_env() {
            tracing::info!(
                artifact_access_mode = ?mode,
                source = "PLASM_MCP_ARTIFACT_ACCESS",
                "resolved MCP artifact access mode"
            );
            transport.lock().await.artifact_access_mode = Some(mode);
            return mode;
        }
        if runtime.client_info().is_none() {
            runtime.wait_for_initialization().await;
        }
        let http_ua = self.plasm.mcp_http_user_agent(transport_key);
        let client_info = runtime.client_info();
        let mode = artifact_access::detect_artifact_access_mode(
            client_info.as_ref(),
            artifact_access::client_user_agent_hint(http_ua.as_deref()).as_deref(),
        );
        if let Some(params) = client_info.as_ref() {
            tracing::info!(
                client_info.name = %params.client_info.name,
                client_info.version = %params.client_info.version,
                artifact_access_mode = ?mode,
                "resolved MCP artifact access mode"
            );
        } else {
            tracing::info!(
                artifact_access_mode = ?mode,
                "resolved MCP artifact access mode (no client_info)"
            );
        }
        transport.lock().await.artifact_access_mode = Some(mode);
        mode
    }

    async fn trace_session_meta(
        &self,
        _mcp_key: &str,
        runtime: &Arc<dyn McpServer>,
    ) -> crate::trace_hub::TraceSessionMeta {
        use crate::trace_hub::{McpConfigRef, TraceSessionMeta};

        let identity = runtime
            .auth_info_cloned()
            .await
            .and_then(|info| McpTransportIdentity::from_auth_info(&info));

        let Some(identity) = identity else {
            return TraceSessionMeta {
                tenant_id: "anonymous".into(),
                project_slug: "main".into(),
                mcp_config: None,
            };
        };

        if identity.anonymous {
            return TraceSessionMeta {
                tenant_id: "anonymous".into(),
                project_slug: "main".into(),
                mcp_config: None,
            };
        }

        let mcp_config = identity.mcp_config_id.map(|id| McpConfigRef {
            config_id: id.to_string(),
            tenant_id: identity.incoming_tenant_id.clone(),
        });

        TraceSessionMeta {
            tenant_id: identity.incoming_tenant_id,
            project_slug: "main".into(),
            mcp_config,
        }
    }
}

impl PlasmMcpHandler {
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
        let logical_uuid = self.resolve_logical_session_ref_to_uuid(tool_name, &session_ref)?;
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
                drop(g);
                self.persist_transport_state(key).await;
            }
        }
        let invocation = match parse_mcp_plasm_invocation(tool_name, v, dry_run_only) {
            Ok(invocation) => invocation,
            Err(result) => {
                crate::metrics::record_mcp_tool(
                    tool_name,
                    Some(false),
                    "error",
                    "invalid_arguments",
                    started.elapsed(),
                );
                return Ok(result);
            }
        };
        let reasoning = v
            .get("reasoning")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty());
        let wait_live = true;
        let force_run = false;
        let plasm_tool_span = if dry_run_only {
            crate::spans::mcp_tool_plasm(false, 1, session_ref.as_str())
        } else {
            crate::spans::mcp_tool_plasm_run(false, 1, session_ref.as_str())
        };
        let run_live = matches!(invocation, McpPlasmInvocation::Run(_));
        let (binding, this_invocation_chars, meta_index) = {
            let g = state.lock().await;
            let binding = g.binding.clone();
            let this_invocation_chars =
                plasm_invocation_char_count(invocation.invocation_text(), reasoning);
            let meta_index = Arc::clone(&g.meta_index);
            drop(g);
            let mut g = state.lock().await;
            g.stats.plasm_invocation_chars = g
                .stats
                .plasm_invocation_chars
                .saturating_add(this_invocation_chars);
            g.stats.plasm_call_count = g.stats.plasm_call_count.saturating_add(1);
            (binding, this_invocation_chars, meta_index)
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
            .get_execute_session(&b.prompt_hash, &b.session_id)
            .await
            .is_none()
        {
            {
                let mut g = state.lock().await;
                g.binding = None;
            }
            {
                self.plasm
                    .logical_execute_bindings
                    .remove(&logical_uuid)
                    .await;
            }
            crate::metrics::record_mcp_tool(
                tool_name,
                Some(false),
                "error",
                "session_expired",
                started.elapsed(),
            );
            return Ok(CallToolResult::with_error(CallToolError::from_message(
                MCP_EXECUTE_SESSION_UNAVAILABLE,
            )));
        }

        let trace_meta = self.trace_session_meta(key, runtime).await;
        let trace_id = self
            .plasm
            .trace_hub
            .ensure_logical_session(&ls_key, Some(key), trace_meta)
            .await;
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
        let mcp_trace = PlasmTraceContext {
            trace_id,
            call_index: Some(call_index as i64),
            mcp_session_id: Some(key.to_string()),
            logical_session_id: Some(ls_key.clone()),
            logical_session_ref: Some(session_ref.clone()),
        };

        let sink = McpPlasmTraceSink {
            hub: Arc::clone(&self.plasm.trace_hub),
            mcp_key: ls_key.clone(),
            call_index,
        };
        let artifact_mode = self.resolved_artifact_access_mode(key, runtime).await;
        let mcp_result_policy = crate::mcp_run_markdown::McpResultTransportPolicy {
            artifact_access: artifact_mode,
            ..Default::default()
        };
        let plan_trace = PlanRunTraceHooks {
            trace: mcp_trace.clone(),
            sink,
            meta_index: Some(meta_index),
        };

        let run_result = async {
            let Some(es) = self
                .plasm
                .get_execute_session(&b.prompt_hash, &b.session_id)
                .await
            else {
                return Err(MCP_EXECUTE_SESSION_UNAVAILABLE.to_string());
            };
            if let Some(program) = invocation.program() {
                if let Some(op_result) = try_dispatch_operation_program(
                    &es,
                    Some(self.plasm.as_ref()),
                    Some(&mcp_trace),
                    program,
                    Some(self.plasm.sessions.symbol_map_cross_cache()),
                )
                .await
                {
                    return op_result;
                }
            }
            if run_live {
                let run_target = invocation
                    .run_target()
                    .ok_or_else(|| "missing `run_ref` on plasm_run invocation".to_string())?;
                let ingress = committed_plasm_run::resolve_mcp_live_run_ingress(
                    &es,
                    &mcp_trace,
                    run_target,
                    self.plasm.engine.prompt_pipeline(),
                    self.plasm.sessions.symbol_map_cross_cache(),
                    call_index,
                )
                .await?;
                let wire = committed_plasm_run::McpExecuteWire {
                    prompt_hash: b.prompt_hash.clone(),
                    session_id: b.session_id.clone(),
                    session_ref: session_ref.clone(),
                    ls_key: ls_key.clone(),
                    mcp_session_key: key.to_string(),
                };
                let artifacts = committed_plasm_run::CommittedRunArtifacts {
                    trace_hub: Arc::clone(&self.plasm.trace_hub),
                    run_artifacts: Arc::clone(&self.plasm.run_artifacts),
                    program_for_trace: ingress.program_for_trace.clone(),
                    plan_call_index: call_index,
                };
                committed_plasm_run::execute_mcp_live_run(committed_plasm_run::ExecuteMcpLiveRun {
                    es: Arc::clone(&es),
                    host: Arc::clone(&self.plasm),
                    wire,
                    bundle: ingress.bundle,
                    kind: ingress.kind,
                    mcp_trace: mcp_trace.clone(),
                    artifacts,
                    plan_trace: Some(plan_trace),
                    mcp_result_policy: Some(mcp_result_policy),
                    force_run,
                    wait_live,
                })
                .await
            } else {
                let program = invocation
                    .program()
                    .ok_or_else(|| "missing `program`: call `plasm` with a program".to_string())?;
                let plan_name = format!("plasm_dag_call_{call_index}");
                let pipeline = self.plasm.engine.prompt_pipeline();
                let cross = self.plasm.sessions.symbol_map_cross_cache();
                let bundle =
                    compile_plasm_expression(pipeline, Some(cross), &es, &plan_name, program)?;
                let program_for_trace = program.to_string();
                let dry = evaluate_plasm_comp_dry(&es, &bundle)?;
                        let dry_text = render_plasm_plan_dry_text_for_session(
                            &dry,
                            None,
                            Some(&es),
                        );
                        let comp_wire = Arc::new(trace_comp_wire_from_dry(&dry));
                        let compact = build_plan_dry_compact_view(
                            dry.validated_plan(),
                            &dry.topological_order,
                            &dry.review,
                            &dry.graph_summary,
                            Some(&es),
                        );
                        let commit_ref = es.mint_plan_commit_ref();
                        let mut markdown = format!("```text\n{dry_text}\n```");
                        markdown.push_str(&format!(
                            "\n\n**Run:** pass `run_ref`: `{}` to **`plasm_run`**. Do not echo the program.",
                            commit_ref.as_str()
                        ));
                        let commit_record = PlanCommitRecord {
                            commit_ref: commit_ref.clone(),
                            commit_id: compute_plan_commit_id_from_dry(&dry),
                            domain_revision: es.domain_revision,
                            artifact: dry.artifact().clone(),
                            program: program_for_trace.clone(),
                            dry_review: dry.review.clone(),
                            verdict: compact.verdict,
                            expires_at: std::time::Instant::now() + PLAN_COMMIT_TTL,
                            dry_cache: PlanCommitDryCache::from_dry(&dry),
                        };
                        crate::plan_commit_store::register_plan_commit_and_persist(
                            self.plasm.as_ref(),
                            Arc::clone(&es),
                            b.prompt_hash.as_str(),
                            b.session_id.as_str(),
                            commit_record,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                        let ux_ctx = crate::plan_ux_reflection::PlanUxBuildContext {
                            session: Some(&es),
                            param_bindings: &[],
                        };
                        let plan_ux_reflection =
                            crate::plan_ux_reflection::plan_ux_reflection_value(&dry, &ux_ctx);
                        CodePlanTraceInput {
                            hub: &self.plasm.trace_hub,
                            store: &self.plasm.run_artifacts,
                            mcp_key: &ls_key,
                            es: &es,
                            prompt_hash: b.prompt_hash.as_str(),
                            session_id: b.session_id.as_str(),
                            session_ref: session_ref.as_str(),
                            comp: Arc::clone(&comp_wire),
                            program: &program_for_trace,
                            plan_call_index: call_index,
                            code_chars: program_for_trace.chars().count() as u64,
                        }
                        .emit_evaluate(Some(plan_ux_reflection.clone()))
                        .await;
                        let mut plasm_obj = serde_json::Map::new();
                        plasm_obj.insert("dry_run".into(), serde_json::json!(true));
                        plasm_obj.insert(
                            "logical_session_ref".into(),
                            serde_json::json!(session_ref.as_str()),
                        );
                        plasm_obj.insert("program".into(), serde_json::json!(program_for_trace));
                        plasm_obj.insert("comp".into(), comp_wire.to_json_value());
                        plasm_obj.extend(plan_commit_meta(
                            &commit_ref,
                            &dry.review,
                            compact.verdict,
                        ));
                        plasm_obj.insert(
                            "domain_revision".into(),
                            serde_json::json!(es.domain_revision),
                        );
                        plasm_obj.insert("plan_ux_reflection".into(), plan_ux_reflection);
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
                                    "session_notes".into(),
                                    serde_json::json!({ "unused_seeds": unused }),
                                );
                            }
                        }
                        let mut meta = serde_json::Map::new();
                        meta.insert("plasm".into(), serde_json::Value::Object(plasm_obj));
                        Ok(PlasmPlanRunResult {
                            version: dry.version,
                            node_results: dry.node_results,
                            graph_summary: dry.graph_summary,
                            comp: Some(comp_wire.as_ref().clone()),
                            code_plan_run_artifacts: Vec::new(),
                            run_markdown: Some(markdown),
                            run_plasm_meta: Some(meta),
                            return_steps: Vec::new(),
                        })
            }
        }
        .instrument(plasm_tool_span)
        .await;
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
                    if let Some(plasm_obj) = crate::mcp_ui_payload::plasm_obj_from_tool_meta(m) {
                        res = crate::mcp_ui_payload::finalize_mcp_tool_result(res, plasm_obj);
                    }
                }
                self.persist_transport_state(key).await;
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
        let logical_session_ref = format_logical_session_wire_ref(rec.logical_session_id);
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
            self.plasm
                .logical_execute_bindings
                .insert(
                    logical_uuid,
                    out.prompt_hash.clone(),
                    out.session_id.clone(),
                )
                .await;
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
        let relations_delta = {
            let deltas: Vec<_> = out
                .waves
                .iter()
                .flat_map(|w| w.relations_delta.iter().cloned())
                .collect();
            if deltas.is_empty() {
                None
            } else {
                Some(json!(deltas))
            }
        };
        let plasm = build_plasm_context_tool_meta(
            logical_session_ref.as_str(),
            &out,
            domain_revision,
            relations,
            relations_delta,
        );
        let mut res = CallToolResult::text_content(vec![TextContent::new(text, None, None)]);
        if !plasm.is_empty() {
            let mut meta = serde_json::Map::new();
            meta.insert("plasm".to_string(), serde_json::Value::Object(plasm));
            res = res.with_meta(Some(meta));
        }
        self.persist_transport_state(key).await;
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
        let mut r = reg.discover(&q).map_err(discovery_mcp_error)?;
        drop(_discover_guard);
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        if let Some(cfg) = tcfg {
            r = mcp_policy::filter_discovery_result(r, cfg.as_ref());
        }
        let formatted = format_discovery_markdown_for_mcp(&r, &DiscoveryTablePolicy::default());
        let mut res =
            CallToolResult::text_content(vec![TextContent::new(formatted.markdown, None, None)]);
        let mut meta = serde_json::Map::new();
        meta.insert(
            "plasm".into(),
            serde_json::Value::Object(crate::discovery_human_format::discovery_plasm_tool_meta(
                &formatted.omission,
            )),
        );
        res = res.with_meta(Some(meta));
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
        runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        let mode = self.artifact_access_mode_for_runtime(&runtime).await;
        Ok(ListToolsResult {
            tools: tools::plasm_tools(mode),
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
                        "Short alias for the same snapshot JSON as the canonical URI. `logical_session_ref` is the canonical `l_<token>` from `plasm_context`; `n` is monotonic within that logical session’s execute binding."
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

    async fn handle_read_resource_request(
        &self,
        params: ReadResourceRequestParams,
        runtime: Arc<dyn McpServer>,
    ) -> Result<ReadResourceResult, RpcError> {
        resource_read::handle_read_resource_request(self, params, runtime).await
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
        "plasm_read_run_artifact" => {
            let started = Instant::now();
            let res = handler
                .handle_read_run_artifact(key.as_str(), &runtime, &v)
                .await;
            let elapsed = started.elapsed();
            match &res {
                Ok(_) => crate::metrics::record_mcp_tool(
                    "plasm_read_run_artifact",
                    None,
                    "success",
                    "none",
                    elapsed,
                ),
                Err(e) => crate::metrics::record_mcp_tool(
                    "plasm_read_run_artifact",
                    None,
                    "error",
                    mcp_call_tool_error_class(e),
                    elapsed,
                ),
            }
            res
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
                if let Some(calls) = pending.stats.calls {
                    op_params.insert("calls".into(), json!(calls));
                }
                if let Some(last_ms) = pending.stats.last_ms {
                    op_params.insert("last_ms".into(), json!(last_ms));
                }
                if let Some(elapsed_ms) = pending.stats.elapsed_ms {
                    op_params.insert("elapsed_ms".into(), json!(elapsed_ms));
                }
                if let Some(rows) = pending.stats.rows {
                    op_params.insert("rows".into(), json!(rows));
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
        instructions: Some(MCP_INITIALIZE_WORKFLOW.to_string()),
        meta: None,
    }
}

/// Build MCP Streamable HTTP server (not started) for merging with discovery routes on one port.
pub async fn build_mcp_hyper_server_for_merge(
    plasm: Arc<PlasmHostState>,
) -> SdkResult<HyperServer> {
    build_mcp_hyper_server(plasm, "0.0.0.0", 0).await
}

/// MCP HTTP routes with User-Agent capture for artifact-access detection.
pub fn mcp_hyper_router(server: HyperServer, plasm: Arc<PlasmHostState>) -> axum::Router<()> {
    use axum::middleware;
    server.into_router().layer(middleware::from_fn_with_state(
        plasm,
        mcp_http_user_agent::capture_mcp_http_user_agent,
    ))
}

async fn build_mcp_hyper_server(
    plasm: Arc<PlasmHostState>,
    host: &str,
    port: u16,
) -> SdkResult<HyperServer> {
    let mut handler_struct = PlasmMcpHandler::new(Arc::clone(&plasm));
    let session_states = Arc::clone(&handler_struct.session_states);
    let server_details = Arc::new(mcp_initialize_result());

    if let Some(backend) = plasm.redis_backend.as_ref() {
        let plasm_redis = Arc::new(PlasmTransportRedisStore::new(Arc::clone(backend)));
        handler_struct = handler_struct.with_transport_redis(plasm_redis);
    }

    let mcp_handler = handler_struct.to_mcp_server_handler();

    let session_store: Option<Arc<dyn SessionStore>> =
        if let Some(backend) = plasm.redis_backend.as_ref() {
            let store: Arc<RedisSessionStore> = Arc::new(RedisSessionStore::new(
                Arc::clone(backend),
                Arc::new(SessionRuntimeFactory {
                    server_details: Arc::clone(&server_details),
                    handler: mcp_handler.clone(),
                    task_store: None,
                    client_task_store: None,
                    message_observer: None,
                }),
            ));
            store.ping().await.map_err(|e| {
                SdkError::internal_error().with_message(&format!(
                    "PLASM_MCP_TRANSPORT_REDIS_URL configured but Redis ping failed: {e}"
                ))
            })?;
            tracing::info!("MCP transport session store: Redis (multi-replica safe)");
            Some(store)
        } else {
            None
        };

    let auth_provider: Option<Arc<dyn rust_mcp_sdk::auth::AuthProvider>> =
        if plasm.mcp_config_repository().is_some() || plasm.incoming_auth.is_some() {
            Some(Arc::new(
                crate::mcp_stream_auth::PlasmMcpApiKeyAuthProvider::new(Arc::clone(&plasm)),
            ))
        } else {
            None
        };
    let server = hyper_server::create_server(
        (*server_details).clone(),
        mcp_handler,
        HyperServerOptions {
            host: host.to_string(),
            port,
            event_store: Some(Arc::new(InMemoryEventStore::default())),
            health_endpoint: Some("/health".into()),
            sse_support: false,
            auth: auth_provider,
            session_store,
            ..Default::default()
        },
    );
    spawn_mcp_teaching_prompt_session_reporter(&server, Arc::clone(&plasm), session_states);
    Ok(server)
}

/// Run Streamable HTTP MCP on `host`:`port` (default MCP path `/mcp` from the SDK).
pub async fn run_mcp_server(host: &str, port: u16, plasm: Arc<PlasmHostState>) -> SdkResult<()> {
    let server = build_mcp_hyper_server(plasm, host, port).await?;
    server.start().await
}
