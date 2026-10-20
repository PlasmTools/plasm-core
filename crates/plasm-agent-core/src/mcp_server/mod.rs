//! MCP Streamable HTTP server (rmcp) over Plasm discovery + execute ([`crate::server_state::PlasmHostState`]).
//! Tool results use Markdown [`TextContent`] with canonical agent token TSV in **`content`**.
//! **`plasm`** dry-runs and **`plasm_run`** put compact review / row TSV in `content` and continuity
//! tokens in `_meta.plasm`. Negotiated MCP Apps receive full DAG / steps in `structuredContent.ui`
//! only — there is **no** agent `structuredContent.plasm` lane. Tool-only hosts omit
//! `structuredContent` entirely so connectors cannot suppress rows behind slim metadata.
//! Full plan DAG / run snapshot rows also live under `_meta.plasm.steps` (Run Explorer UI) or MCP
//! `resources/read` on `plan_uri` / run artifact URIs.
//! Run snapshot URIs in Markdown prefer canonical `plasm://execute/.../run/{run_id}`.
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
//! Tool text returns **dynamic declaration-only** context on fresh `plasm_context` opens (`reused: false`); repeated
//! opens with the same entry + capability picks omit the teaching body to avoid token churn.
//! **Symbols:** for a fixed binding (`prompt_hash` + `session`), `e#` / `m#` / `p#` grow **append-only**
//! when you add new picks; they do not reshuffle. A new primary catalog open or logical session starts a new
//! symbol space — always read tokens from the current session `prompt` / Plasm language text.
//! A soft cap evicts one arbitrary older binding when the map grows past [`MAX_MCP_EXEC_BINDINGS`].
//!
//! Plasm language / instructions body (first update on `plasm_context` open plus append-only deltas when you add
//! capability picks via `plasm_context`'s `seeds`) is counted in Unicode scalar values per MCP transport session.
//! `plasm` calls also accumulate invocation source text (`program`); both
//! `plasm` and `plasm_run` accumulate returned Markdown on success. Server logs use a rough **token estimate** ≈ `ceil(chars / 4)` per
//! bucket (`prompt` / `invocation` / `tool_response`). When the session leaves the SDK session store,
//! an `INFO` line logs cumulative character totals and token estimates (`plasm_agent::mcp`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::mcp_server::request::McpRequest;
use crate::mcp_server::request::ToolFailure;
use base64::Engine as _;
use plasm_core::CgsCatalog;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, InitializeRequestParams, InitializeResult,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ResourceTemplate,
};
use rmcp::{ErrorData, ServerHandler};
use tokio::sync::{Mutex, RwLock};

use crate::execute_session::ExecuteSession;
use crate::incoming_auth::{IncomingAuthMode, TenantPrincipal};
use crate::mcp_logical_ref::{format_logical_session_wire_ref, parse_logical_session_wire_ref};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::run_artifacts::{
    code_plan_handle, code_plan_http_path, plasm_code_plan_resource_uri, ArtifactPayload,
    CodePlanArchiveDocument,
};
use crate::server_state::PlasmHostState;
use chrono::Utc;
use uuid::Uuid;

mod artifact_access;
mod artifact_resolve;
mod call_tool_dispatch;
mod committed_plasm_run;
/// Best-effort bound on concurrent MCP transport sessions holding an execute binding (see module doc).
pub(crate) mod request;
pub use committed_plasm_run::compile_page_continuation;
#[cfg(test)]
mod future_size_probe;
mod host_fault;
mod host_policy;
mod initialize;
mod mcp_http_dns_rebinding;
mod mcp_http_user_agent;
mod mcp_plasm_invoke;
mod plasm_context;
mod plasm_tool_dry_meta;
mod plasm_tool_dry_run;
mod plasm_tool_handler;
mod read_run_artifact;
mod resource_helpers;
mod resource_read;
mod resource_read_trace;
pub(crate) mod schema;
#[cfg(test)]
mod stack_budget_test;
#[cfg(test)]
mod stateless;
mod teaching_prompt_reporter;
mod tool_parse;
mod tools;
mod trace;
mod transport;
mod ui_read;

pub use initialize::{
    build_mcp_router_for_merge, build_mcp_router_for_transport, plasm_mcp_stateless_enabled,
    run_mcp_server, McpHttpTransport, McpServerError,
};

#[cfg(test)]
mod integration;
mod symbol_stability;
#[cfg(test)]
mod symbol_stability_rehydrate;
#[cfg(test)]
mod tests;

// Re-exports for sibling modules (`use super::*`) and crate-internal callers.
#[allow(unused_imports)]
pub(crate) use mcp_plasm_invoke::{parse_mcp_plasm_invocation, McpPlasmInvocation};
#[allow(unused_imports)]
pub(crate) use resource_helpers::{mcp_call_tool_error_class, mcp_key};
#[allow(unused_imports)]
pub(crate) use schema::{args_value, json_schema_non_empty_string_type, json_schema_string_type};
#[allow(unused_imports)]
pub(crate) use tool_parse::{
    comp_content_sha256_hex, parse_logical_session_ref_arg, parse_optional_principal,
    parse_plasm_context_session_mode, plan_display_name_from_comp, plan_node_count_from_comp,
};
pub(crate) use trace::CodePlanTraceInput;
#[allow(unused_imports)]
pub(crate) use transport::{
    mcp_chars_to_token_est, plasm_invocation_char_count, McpLogicalSessionState,
    McpSessionPlasmStats, McpTransportState, PlasmExecBinding,
};

const MAX_MCP_EXEC_BINDINGS: usize = 512;

const MCP_EXECUTE_SESSION_UNAVAILABLE: &str = "Execute session unavailable (expired or catalog reload): call `plasm_context` again with your capability picks (`seeds`).";

/// Per MCP transport session: Plasm execute `prompt_hash` + `session` ids (same as HTTP paths).
#[derive(Clone)]
pub(crate) struct PlasmMcpHandler {
    pub(crate) plasm: Arc<PlasmHostState>,
    /// MCP transport session key -> per-session mutable state.
    pub(crate) session_states: Arc<RwLock<HashMap<String, Arc<Mutex<McpTransportState>>>>>,
    transport_redis: Option<Arc<crate::mcp_transport_store::PlasmTransportRedisStore>>,
    peers: Arc<RwLock<HashMap<String, rmcp::Peer<rmcp::RoleServer>>>>,
}

impl PlasmMcpHandler {
    pub(crate) fn new(plasm: Arc<PlasmHostState>) -> Self {
        Self {
            plasm,
            session_states: Arc::new(RwLock::new(HashMap::new())),
            transport_redis: None,
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) fn with_transport_redis(
        mut self,
        store: Arc<crate::mcp_transport_store::PlasmTransportRedisStore>,
    ) -> Self {
        self.transport_redis = Some(store);
        self
    }

    async fn bind_request(
        &self,
        context: &rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Arc<McpRequest> {
        let runtime = Arc::new(McpRequest::from_context(context));
        self.peers
            .write()
            .await
            .insert(runtime.transport_key.clone(), context.peer.clone());
        runtime
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

    /// Redis transport snapshot — off the MCP tool response critical path.
    pub(crate) fn schedule_persist_transport_state(&self, transport_key: &str) {
        if self.transport_redis.is_none() {
            return;
        }
        crate::metrics::record_mcp_response_deferred_io("transport_state");
        let handler = self.clone();
        let key = transport_key.to_string();
        tokio::spawn(async move {
            handler.persist_transport_state(key.as_str()).await;
        });
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
    ) -> Result<Uuid, ToolFailure> {
        parse_logical_session_wire_ref(ref_str)
            .map(|id| id.as_uuid())
            .map_err(|e| ToolFailure::invalid_arguments(tool, Some(e.to_string())))
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
        runtime: &Arc<McpRequest>,
    ) -> Result<Option<Arc<McpRuntimeConfig>>, ToolFailure> {
        let has_tenant_configs = match self.plasm.mcp_config_repository() {
            Some(r) => r.has_tenant_configs().await.map_err(ToolFailure::cause)?,
            None => false,
        };
        let auth = runtime.identity.clone();
        let Some(info) = auth else {
            if has_tenant_configs {
                return Err(ToolFailure::rejected(
                    "MCP Authorization required: send `Authorization: Bearer <api_key>` (tenant MCP API key from control plane).",
                ));
            }
            return Ok(None);
        };

        if info.anonymous {
            return Ok(None);
        }

        let identity = Some(info);
        let Some(id) = identity.and_then(|i| i.mcp_config_id) else {
            if has_tenant_configs {
                return Err(ToolFailure::rejected(
                    "MCP Authorization missing tenant binding (expected Bearer API key).",
                ));
            }
            return Ok(None);
        };

        let Some(repo) = self.plasm.mcp_config_repository() else {
            return Ok(None);
        };

        let Some(cfg) = repo.get_runtime_config(&id).await.map_err(|_| {
            ToolFailure::rejected("Tenant MCP configuration store failed while loading policy.")
        })?
        else {
            return Err(ToolFailure::rejected(
                "Tenant MCP configuration is no longer available (disabled or revoked on the agent).",
            ));
        };
        if cfg.space_type == "personal" && cfg.owner_subject.is_none() {
            return Err(ToolFailure::rejected(
                "Personal MCP configuration is missing owner binding metadata. Re-provision from control plane.",
            ));
        }

        Ok(Some(Arc::new(cfg)))
    }

    async fn mcp_principal_from_transport_auth(
        &self,
        runtime: &Arc<McpRequest>,
    ) -> Option<TenantPrincipal> {
        let info = runtime.identity.clone()?;
        Some(info)?.to_tenant_principal()
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
        runtime: &Arc<McpRequest>,
    ) -> Result<Option<TenantPrincipal>, ToolFailure> {
        let mode = self.incoming_mode();
        let p = self.mcp_principal_from_transport_auth(runtime).await;
        if mode == IncomingAuthMode::Required && p.is_none() {
            return Err(ToolFailure::rejected(
                "incoming auth required: authenticate the MCP transport with a valid bearer credential",
            ));
        }
        Ok(p)
    }

    pub(crate) async fn resolved_artifact_access_mode(
        &self,
        transport_key: &str,
        runtime: &Arc<McpRequest>,
    ) -> crate::mcp_run_markdown::ArtifactAccessMode {
        let transport = self.session_state(transport_key).await;
        {
            let g = transport.lock().await;
            if let Some(mode) = g.artifact_access_mode {
                return mode;
            }
        }
        self.detect_and_cache_host_policy(transport_key, runtime)
            .await
            .artifact_access
    }

    pub(crate) async fn artifact_access_mode_for_runtime(
        &self,
        runtime: &Arc<McpRequest>,
    ) -> crate::mcp_run_markdown::ArtifactAccessMode {
        self.resolved_artifact_access_mode(&runtime.transport_key, runtime)
            .await
    }

    pub(crate) async fn resolved_mcp_ui_apps_supported(
        &self,
        transport_key: &str,
        runtime: &Arc<McpRequest>,
    ) -> bool {
        let transport = self.session_state(transport_key).await;
        {
            let g = transport.lock().await;
            if let Some(enabled) = g.mcp_ui_apps_supported {
                return enabled;
            }
        }
        self.detect_and_cache_host_policy(transport_key, runtime)
            .await
            .mcp_ui_apps
    }

    pub(crate) async fn mcp_ui_apps_enabled_for_runtime(&self, runtime: &Arc<McpRequest>) -> bool {
        self.resolved_mcp_ui_apps_supported(&runtime.transport_key, runtime)
            .await
    }

    /// Observe client semantics once and cache eligible host-policy fields on the transport.
    async fn detect_and_cache_host_policy(
        &self,
        transport_key: &str,
        runtime: &Arc<McpRequest>,
    ) -> host_policy::McpHostPolicy {
        let http_ua = self.plasm.mcp_http_user_agent(transport_key);
        let client = crate::mcp_client_info::McpClientInfo::observe(runtime.as_ref());
        let policy = host_policy::resolve_mcp_host_policy(
            &client,
            artifact_access::client_user_agent_hint(http_ua.as_deref()).as_deref(),
        );
        host_policy::log_resolved_host_policy(&client, &policy);
        let transport = self.session_state(transport_key).await;
        let mut g = transport.lock().await;
        if policy.cache_artifact_access {
            g.artifact_access_mode = Some(policy.artifact_access);
        }
        if policy.cache_mcp_ui_apps {
            g.mcp_ui_apps_supported = Some(policy.mcp_ui_apps);
        }
        policy
    }

    async fn trace_session_meta(
        &self,
        _mcp_key: &str,
        runtime: &Arc<McpRequest>,
    ) -> crate::trace_hub::TraceSessionMeta {
        use crate::trace_hub::{McpConfigRef, TraceSessionMeta};

        let identity = runtime.identity.clone();

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
    async fn handle_mcp_tool_ui_list_catalogs(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
    ) -> Result<CallToolResult, ToolFailure> {
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
            CallToolResult::success(vec![rmcp::model::ContentBlock::text(text)])
                .with_meta(Some(meta.into())),
        )
    }
}

impl ServerHandler for PlasmMcpHandler {
    fn get_info(&self) -> InitializeResult {
        initialize::mcp_initialize_result()
    }

    async fn initialize(
        &self,
        params: InitializeRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        context.peer.set_peer_info(params.clone());
        let client = crate::mcp_client_info::McpClientInfo::from_initialize(params.clone());
        let ua = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.headers.get(http::header::USER_AGENT))
            .and_then(|value| value.to_str().ok());
        let policy = host_policy::resolve_mcp_host_policy(&client, ua);
        let mut result = self.negotiate_initialize(&params)?;
        crate::mcp_ui_capability::apply_server_ui_extensions(
            &mut result.capabilities,
            policy.mcp_ui_apps,
        );
        Ok(result)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let runtime = self.bind_request(&context).await;
        let mode = self.artifact_access_mode_for_runtime(&runtime).await;
        let ui_enabled = self.mcp_ui_apps_enabled_for_runtime(&runtime).await;
        Ok(ListToolsResult {
            tools: tools::plasm_tools(mode, ui_enabled),
            ..Default::default()
        })
    }

    async fn list_resources(
        &self,
        _params: Option<PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let _runtime = self.bind_request(&context).await;
        Ok(ListResourcesResult::default())
    }

    async fn list_resource_templates(
        &self,
        _params: Option<PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let runtime = self.bind_request(&context).await;
        let ui_enabled = self.mcp_ui_apps_enabled_for_runtime(&runtime).await;
        let mut resource_templates = Vec::new();
        if ui_enabled {
            resource_templates.extend(crate::mcp_app::mcp_app_resource_templates());
        }
        let mut canonical = ResourceTemplate::new(
            "plasm://execute/{prompt_hash}/{session_id}/run/{run_id}",
            "plasm_execute_run",
        );
        canonical.description = Some(
            "Typed bytes for one execute run artifact. `prompt_hash` and `session_id` match `plasm_context`; `run_id` is in `plasm_run` (or prior live) result metadata.".into(),
        );
        canonical.mime_type = Some("application/octet-stream".into());
        canonical.title = Some("Plasm execute run artifact (canonical)".into());
        let mut short = ResourceTemplate::new(
            "plasm://session/{logical_session_ref}/r/{n}",
            "plasm_execute_run_short",
        );
        short.description = Some(
            "Short alias for the same snapshot JSON as the canonical URI. `logical_session_ref` is the canonical `l_<token>` from `plasm_context`; `n` is monotonic within that logical session’s execute binding.".into(),
        );
        short.mime_type = Some("application/octet-stream".into());
        short.title = Some("Plasm execute run artifact (short index)".into());
        resource_templates.extend([canonical, short]);
        Ok(ListResourceTemplatesResult {
            resource_templates,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        let runtime = self.bind_request(&context).await;
        resource_read::handle_read_resource_request(self, params, runtime)
            .await
            .map(Into::into)
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, ErrorData> {
        let runtime = self.bind_request(&context).await;
        // Dispatch exposes a typed boxed future, keeping the compiler/executor
        // state machine behind the service boundary rather than in the transport.
        Ok(
            call_tool_dispatch::dispatch_plasm_mcp_call_tool_request(self, params, runtime)
                .await
                .unwrap_or_else(ToolFailure::into_result)
                .into(),
        )
    }
}
