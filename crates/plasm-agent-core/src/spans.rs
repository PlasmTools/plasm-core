//! Semantic tracing spans for the Plasm agent host (HTTP execute, MCP, CLI).
//!
//! Names are a **stable observability contract** (`plasm_agent.<surface>.<operation>`): they
//! describe product semantics (execute line, session reuse, MCP tool), not Rust module paths,
//! so refactors do not churn trace dashboards.
//!
//! ## Async propagation (Tokio + OTLP)
//!
//! - New spans default their **parent** to `tracing::Span::current()` at
//!   **creation time**. If that snapshot is empty (e.g. concurrent `join_all` arms before any
//!   `.instrument`), OpenTelemetry exports **orphan** spans under the service root.
//! - Prefer **`.instrument(span)`** on `async` blocks/futures that cross `await`, and attach
//!   **`parent: explicit_parent`** when spawning parallel work (see `http_execute` batch parallel
//!   stage).
//! - Background `tokio::spawn` (trace hub ingest, session finalizers) intentionally often has no
//!   request parent; do not expect those spans to link to HTTP/MCP requests.
//! - Subscriber wiring lives in [`plasm_otel`]; `tower_http::TraceLayer` request spans only
//!   correlate with app spans when handlers and nested futures retain the active context as above.

use tracing::Span;
use uuid::Uuid;

// --- HTTP / shared execute ---------------------------------------------------

/// One Plasm path line executed in an HTTP or MCP session (detail logs belong at **trace**).
#[inline]
pub fn execute_expression_line(
    entry_id: &str,
    source_len: usize,
    parsed_display_len: usize,
) -> Span {
    tracing::trace_span!(
        "plasm_agent.execute.expression",
        entry_id = entry_id,
        source_len = source_len,
        parsed_display_len = parsed_display_len,
    )
}

#[inline]
pub(crate) fn execute_session_reuse(
    entry_id: &str,
    catalog_cgs_hash: &str,
    prompt_hash: &str,
    session_id: &str,
) -> Span {
    tracing::info_span!(
        "plasm_agent.execute.session_reuse",
        entry_id = entry_id,
        catalog_cgs_hash = catalog_cgs_hash,
        prompt_hash = prompt_hash,
        session_id = session_id,
    )
}

#[inline]
pub(crate) fn execute_session_create() -> Span {
    tracing::debug_span!("plasm_agent.execute.session_create")
}

#[inline]
pub(crate) fn execute_session_lookup_miss() -> Span {
    tracing::debug_span!("plasm_agent.execute.session_lookup_miss")
}

#[inline]
pub(crate) fn execute_artifact_serve() -> Span {
    tracing::info_span!("plasm_agent.execute.artifact_serve")
}

// --- CLI dispatch ------------------------------------------------------------

#[inline]
pub fn execute_cli_expression(entity_cli_name: &str) -> Span {
    tracing::trace_span!("plasm_agent.cli.expression", entity_cli = entity_cli_name)
}

// --- MCP ---------------------------------------------------------------------

#[inline]
pub(crate) fn mcp_tool_discover_capabilities() -> Span {
    tracing::info_span!("plasm_agent.mcp.tool.discover_capabilities")
}

#[inline]
pub(crate) fn mcp_tool_plasm_context(logical_session_ref: &str) -> Span {
    tracing::info_span!(
        "plasm_agent.mcp.tool.plasm_context",
        logical_session_ref = %logical_session_ref,
    )
}

#[inline]
pub(crate) fn mcp_tool_plasm(multi_line: bool, line_count: u64, logical_session_ref: &str) -> Span {
    tracing::info_span!(
        "plasm_agent.mcp.tool.plasm",
        multi_line = multi_line,
        line_count = line_count,
        logical_session_ref = %logical_session_ref,
    )
}

#[inline]
pub(crate) fn mcp_tool_plasm_run(
    multi_line: bool,
    line_count: u64,
    logical_session_ref: &str,
) -> Span {
    tracing::info_span!(
        "plasm_agent.mcp.tool.plasm_run",
        multi_line = multi_line,
        line_count = line_count,
        logical_session_ref = %logical_session_ref,
    )
}

#[inline]
pub(crate) fn mcp_resource_read() -> Span {
    tracing::info_span!("plasm_agent.mcp.resource.read")
}

#[inline]
pub(crate) fn execute_graph_snapshot_finalize(through_seq: u64) -> Span {
    tracing::info_span!(
        "plasm_agent.graph.snapshot_finalize",
        through_seq = through_seq,
    )
}

#[inline]
pub(crate) fn execute_graph_rehydrate(mode: &'static str, logical_count: usize) -> Span {
    tracing::debug_span!(
        "plasm_agent.graph.rehydrate",
        mode = mode,
        logical_count = logical_count,
    )
}

// --- Security (control plane, transport identity, tenant binding) ------------
//
// Never attach secrets (API key material, JWTs, `Authorization`) to spans or logs here.

/// Control-plane MCP runtime config upsert (tenant + config identity).
#[inline]
pub fn security_mcp_config_upsert(config_id: &Uuid, tenant_id: &str, version: u64) -> Span {
    tracing::debug_span!(
        "plasm_agent.security.mcp_config_upsert",
        config_id = %config_id,
        tenant_id = tenant_id,
        version = version,
    )
}

/// Control-plane MCP config revoke (stops tenant MCP surface for that config).
#[inline]
pub fn security_mcp_config_revoke(config_id: &Uuid) -> Span {
    tracing::info_span!("plasm_agent.security.mcp_config_revoke", config_id = %config_id)
}

/// Provision a new MCP transport API key for a config (key material is never logged).
#[inline]
pub fn security_mcp_api_key_provision(config_id: &Uuid) -> Span {
    tracing::info_span!(
        "plasm_agent.security.mcp_api_key_provision",
        config_id = %config_id,
    )
}

#[inline]
pub fn security_mcp_api_key_rotate(config_id: &Uuid) -> Span {
    tracing::info_span!(
        "plasm_agent.security.mcp_api_key_rotate",
        config_id = %config_id,
    )
}

/// Replace a single MCP API key (revoke one + provision one; key material is never logged).
#[inline]
pub fn security_mcp_api_key_rotate_one(config_id: &Uuid) -> Span {
    tracing::info_span!(
        "plasm_agent.security.mcp_api_key_rotate_one",
        config_id = %config_id,
    )
}

/// Status lookup for whether a public key is bound (no key bytes).
#[inline]
pub fn security_mcp_api_key_status(config_id: &Uuid) -> Span {
    tracing::debug_span!(
        "plasm_agent.security.mcp_api_key_status",
        config_id = %config_id,
    )
}

/// Resolve incoming-auth subject → tenant binding (subject string is never recorded).
#[inline]
pub fn security_incoming_tenant_resolve(subject_len: usize, has_github_login: bool) -> Span {
    tracing::info_span!(
        "plasm_agent.security.incoming_tenant_resolve",
        subject_len = subject_len,
        has_github_login = has_github_login,
    )
}

#[inline]
pub fn security_incoming_tenant_create_org(
    tenant_id: &str,
    workspace_slug: &str,
    subject_len: usize,
) -> Span {
    tracing::info_span!(
        "plasm_agent.security.incoming_tenant_create_org",
        tenant_id = tenant_id,
        workspace_slug = workspace_slug,
        subject_len = subject_len,
    )
}

/// Per-request incoming JWT / API-key evaluation (tenant id only when authenticated).
#[inline]
pub fn security_incoming_http(principal: bool, tenant_id: &str) -> Span {
    tracing::trace_span!(
        "plasm_agent.security.incoming_http",
        principal = principal,
        tenant_id = tenant_id,
    )
}

// --- Billing / audit (trace sink envelope, durable rows) ----------------------

/// Building an audit batch for the trace sink (`mcp_trace_segment` rows; payload is `plasm_trace::TraceEvent` JSON).
#[inline]
pub(crate) fn billing_audit_batch_emit(event_count: usize, event_kind: &str) -> Span {
    tracing::debug_span!(
        "plasm_agent.billing.audit_batch_emit",
        event_count = event_count,
        event_kind = event_kind,
    )
}

/// HTTP POST to `PLASM_TRACE_SINK_URL/v1/events` (billing-relevant path).
#[inline]
pub(crate) fn billing_trace_sink_http_post() -> Span {
    tracing::debug_span!("plasm_agent.billing.trace_sink_http_post")
}

/// Tenant-scoped trace list (UI / API; enforces viewer tenant in handler).
#[inline]
pub(crate) fn billing_trace_list(viewer_tenant: &str, limit: usize, offset: usize) -> Span {
    tracing::debug_span!(
        "plasm_agent.billing.trace_list",
        viewer_tenant = viewer_tenant,
        limit = limit,
        offset = offset,
    )
}

/// Tenant-scoped trace detail read (may hit trace sink read replica).
#[inline]
pub(crate) fn billing_trace_detail(viewer_tenant: &str, trace_id: &Uuid) -> Span {
    tracing::debug_span!(
        "plasm_agent.billing.trace_detail",
        viewer_tenant = viewer_tenant,
        trace_id = %trace_id,
    )
}
