//! MCP Streamable HTTP server (rust-mcp-sdk) over Plasm discovery + execute ([`crate::server_state::PlasmHostState`]).
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

use std::collections::HashMap;
use std::sync::Arc;

use crate::discovery_human_format::{format_discovery_markdown_for_mcp, DiscoveryTablePolicy};

use async_trait::async_trait;
use base64::Engine as _;
use plasm_core::discovery::{CapabilityQuery, DiscoveryError};
use plasm_core::{CgsCatalog, CgsDiscovery};
use rust_mcp_sdk::mcp_server::ServerHandler;
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{
    BlobResourceContents, CallToolRequestParams, CallToolResult, InitializeRequestParams,
    InitializeResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceContent, ReadResourceRequestParams, ReadResourceResult,
    ResourceTemplate, RpcError, TextContent, TextResourceContents,
};
use rust_mcp_sdk::McpServer;
use tokio::sync::{Mutex, RwLock};

use crate::execute_session::ExecuteSession;
use crate::http_execute::{normalize_capability_seeds, CapabilitySeed, RankedCapabilitiesArg};
use crate::incoming_auth::{IncomingAuthMode, TenantPrincipal};
use crate::mcp_logical_ref::{format_logical_session_wire_ref, parse_logical_session_wire_ref};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::mcp_policy;
use crate::mcp_runtime_config::McpRuntimeConfig;
use crate::mcp_stream_identity::McpTransportIdentity;
use crate::run_artifacts::{
    code_plan_handle, code_plan_http_path, plasm_code_plan_resource_uri, ArtifactPayload,
    CodePlanArchiveDocument,
};
use crate::server_state::PlasmHostState;
use chrono::Utc;
use uuid::Uuid;

/// Best-effort bound on concurrent MCP transport sessions holding an execute binding (see module doc).
mod artifact_access;
mod artifact_resolve;
mod call_tool_dispatch;
mod committed_plasm_run;
mod context_new_seeds;
mod discover;
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
mod resource_read;
mod resource_read_trace;
mod schema;
mod stateless;
mod teaching_prompt_reporter;
mod tool_parse;
mod tools;
mod trace;
mod transport;
mod ui_read;

pub(crate) use initialize::mcp_stateless_server_details;
pub use initialize::{
    build_mcp_hyper_server_for_merge, build_mcp_router_for_merge, mcp_hyper_router,
    plasm_mcp_stateless_enabled, run_mcp_server,
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
pub(crate) use discover::{
    discovery_mcp_error, mcp_call_tool_error_class, mcp_discover_query_from_arguments, mcp_key,
};
#[allow(unused_imports)]
pub(crate) use mcp_plasm_invoke::{parse_mcp_plasm_invocation, McpPlasmInvocation};
#[allow(unused_imports)]
pub(crate) use schema::{args_value, json_schema_non_empty_string_type, json_schema_string_type};
#[cfg(test)]
pub(crate) use tool_parse::parse_tool_seeds;
#[allow(unused_imports)]
pub(crate) use tool_parse::{
    comp_content_sha256_hex, parse_logical_session_ref_arg, parse_optional_principal,
    parse_plasm_context_ranked_capabilities, parse_plasm_context_session_mode,
    parse_tool_seeds_optional, plan_display_name_from_comp, plan_node_count_from_comp,
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
        self.detect_and_cache_host_policy(transport_key, runtime)
            .await
            .artifact_access
    }

    pub(crate) async fn artifact_access_mode_for_runtime(
        &self,
        runtime: &Arc<dyn McpServer>,
    ) -> crate::mcp_run_markdown::ArtifactAccessMode {
        if let Some(key) = runtime.session_id() {
            return self.resolved_artifact_access_mode(&key, runtime).await;
        }
        let client = crate::mcp_client_info::McpClientInfo::observe(runtime.as_ref());
        host_policy::resolve_mcp_host_policy(
            &client,
            artifact_access::client_user_agent_hint(None).as_deref(),
        )
        .artifact_access
    }

    pub(crate) async fn resolved_mcp_ui_apps_supported(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
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

    pub(crate) async fn mcp_ui_apps_enabled_for_runtime(
        &self,
        runtime: &Arc<dyn McpServer>,
    ) -> bool {
        if let Some(key) = runtime.session_id() {
            return self.resolved_mcp_ui_apps_supported(&key, runtime).await;
        }
        let client = crate::mcp_client_info::McpClientInfo::observe(runtime.as_ref());
        host_policy::resolve_mcp_host_policy(&client, None).mcp_ui_apps
    }

    /// Observe client semantics once and cache eligible host-policy fields on the transport.
    async fn detect_and_cache_host_policy(
        &self,
        transport_key: &str,
        runtime: &Arc<dyn McpServer>,
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
                &r,
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
    async fn handle_initialize_request(
        &self,
        params: InitializeRequestParams,
        runtime: Arc<dyn McpServer>,
    ) -> Result<InitializeResult, RpcError> {
        let client = crate::mcp_client_info::McpClientInfo::from_initialize(params.clone());
        let ua_hint = runtime.session_id().and_then(|key| {
            artifact_access::client_user_agent_hint(self.plasm.mcp_http_user_agent(&key).as_deref())
        });
        let policy = host_policy::resolve_mcp_host_policy(&client, ua_hint.as_deref());
        if let Some(key) = runtime.session_id() {
            let transport = self.session_state(&key).await;
            let mut g = transport.lock().await;
            g.mcp_ui_apps_supported = Some(policy.mcp_ui_apps);
            g.artifact_access_mode = Some(policy.artifact_access);
        }

        let mut server_info = runtime.server_info().to_owned();
        if let Some(updated_protocol_version) =
            rust_mcp_sdk::mcp_server::enforce_compatible_protocol_version(
                &params.protocol_version,
                &server_info.protocol_version,
            )
            .map_err(|err| {
                tracing::error!(
                    "Incompatible protocol version: client: {} server: {}",
                    &params.protocol_version,
                    &server_info.protocol_version
                );
                RpcError::internal_error().with_message(err.to_string())
            })?
        {
            server_info.protocol_version = updated_protocol_version;
        }

        runtime
            .set_client_details(params.clone())
            .await
            .map_err(|err| RpcError::internal_error().with_message(format!("{err}")))?;

        crate::mcp_ui_capability::apply_server_ui_extensions(
            &mut server_info.capabilities,
            policy.mcp_ui_apps,
        );

        tracing::info!(
            client_info.name = %params.client_info.name,
            client_info.version = %params.client_info.version,
            mcp_ui_apps_supported = policy.mcp_ui_apps,
            artifact_access_mode = ?policy.artifact_access,
            "MCP initialize: host policy negotiated"
        );

        Ok(server_info)
    }

    async fn handle_list_tools_request(
        &self,
        _request: Option<PaginatedRequestParams>,
        runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        let mode = self.artifact_access_mode_for_runtime(&runtime).await;
        let ui_enabled = self.mcp_ui_apps_enabled_for_runtime(&runtime).await;
        Ok(ListToolsResult {
            tools: tools::plasm_tools(mode, ui_enabled),
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
        runtime: Arc<dyn McpServer>,
    ) -> Result<ListResourceTemplatesResult, RpcError> {
        let ui_enabled = self.mcp_ui_apps_enabled_for_runtime(&runtime).await;
        let mut resource_templates = Vec::new();
        if ui_enabled {
            resource_templates.extend(crate::mcp_app::mcp_app_resource_templates());
        }
        resource_templates.extend([
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
        ]);
        Ok(ListResourceTemplatesResult {
            resource_templates,
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
        call_tool_dispatch::dispatch_plasm_mcp_call_tool_request(self, params, runtime).await
    }
}
