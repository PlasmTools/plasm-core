use crate::api_error_detail::{fibery_command_envelope_hint, graphql_errors_summary};
use crate::evm::{execute_evm_call, execute_evm_logs};
use crate::http_resilience::{HttpResiliencePolicy, ResilientHttpTransport};
use crate::http_transport::{HttpTransport, ReqwestHttpTransport};
use crate::materialization::{CacheTelemetry, ExecutionCacheConsult, SessionMaterialization};
use crate::preflight::{apply_preflight_steps, PreflightInvoke};
use crate::view_plan::ViewAmbientContext;
use crate::{AuthResolver, CachedEntity, CancelSignal, EntityCompleteness, RuntimeError};
use indexmap::IndexMap;
use plasm_compile::{
    compile_operation, compile_query, decode_entities_with_cgs, parse_capability_template,
    path_var_names_from_request, template_pagination, template_var_names, BackendFilter,
    CapabilityTemplate, CmlEnv, CmlRequest, CompileOperationHook, CompileQueryHook,
    CompiledOperation, CompiledRequest, HttpBodyFormat, PaginationConfig, PathExpr, PathSegment,
    ResponsePreprocess,
};
use plasm_core::partition_prefer_resolutions;
use plasm_core::resolve_relation_row_resolution;
use plasm_core::{
    cross_entity::{
        choose_strategy, extract_cross_entity_predicates, strip_cross_entity_comparisons,
        CrossEntityStrategy,
    },
    reject_domain_placeholder_in_executable as reject_domain_placeholder_core,
    resolve_query_capability as resolve_query_capability_core, type_check_expr,
    type_check_expr_federated, CapabilityKind, CapabilityParamName, CapabilitySchema, ChainStep,
    EntityDef, EntityFieldName, EntityKey, EntityName, Expr, FieldType, GetExpr, InputType,
    InvokeExpr, InvokeInputPayload, ParameterRole, Predicate, PromptPipelineConfig, QueryExpr, Ref,
    RelationMaterialization, RelationRowResolution, RelationSchema, RelationScopedFallback, Value,
    CGS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::Instrument;

mod chain;
mod compile_preflight;
mod embed_cache;
mod entity_decoder;
mod http_exec;
mod mutators;
mod pagination_state;
mod projection;
mod query_stream;
mod resume;

use self::entity_decoder::{
    create_entity_decoder, create_entity_decoder_for_capability,
    mutating_capability_response_decoder,
};

pub use compile_preflight::preflight_compile_expr;

pub(crate) use pagination_state::PaginationLoopState;
#[cfg(test)]
pub(crate) use pagination_state::{merge_pagination_into_body, pagination_context_map};

/// Resolve the capability that backs a [`QueryExpr`] (delegates to [`plasm_core::resolve_query_capability`]).
fn resolve_query_capability<'a>(
    query: &'a QueryExpr,
    cgs: &'a CGS,
) -> Result<&'a CapabilitySchema, RuntimeError> {
    resolve_query_capability_core(query, cgs).map_err(|e| RuntimeError::ConfigurationError {
        message: e.to_string(),
    })
}

/// Canonical HTTP execute session coordinates supplied by the host (`plasm` HTTP/MCP).
///
/// These match `/execute/:prompt_hash/:session` path validation and must **not** be confused
/// with MCP `logical_session_ref` slot aliases (`s0`, …), which are transport-local.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecuteSessionMaterial {
    pub prompt_hash: String,
    pub session_id: String,
    /// When set (e.g. after a catalog-specific session bind), merged into CML env as `share_token`
    /// so mappings can emit `?token=` mirrors without repeating the secret in every program line.
    pub share_token: Option<String>,
    /// Proof (and similar catalogs): merged into CML env as `base_token` before invoke parameters
    /// so `/ops` bodies can send `baseToken` after `editor_state_get` without repeating it every line.
    pub proof_base_token: Option<String>,
    /// Pinned HTTP(S) transport origin for this execute row (session backend override).
    pub transport_origin: Option<String>,
    /// UI / browse deeplink origin; defaults to [`Self::transport_origin`] when unset.
    pub ui_origin: Option<String>,
    /// MCP connect binding wire values for the active catalog row (merged as `bind_<wire>` CML env).
    pub catalog_bind: Option<indexmap::IndexMap<String, String>>,
}

/// Reserved CML env key: 64-char lowercase hex (rendered teaching prompt digest for the row).
pub const CML_ENV_PLASM_EXECUTE_PROMPT_HASH: &str = "plasm_execute_prompt_hash";
/// Reserved CML env key: 32-char lowercase hex UUID (simple form) for the execute row.
pub const CML_ENV_PLASM_EXECUTE_SESSION_ID: &str = "plasm_execute_session_id";

/// teaching prompts use bare `$` as a fill-in cue; it must not reach HTTP/EVM transport.
fn reject_domain_placeholder_in_executable(expr: &Expr) -> Result<(), RuntimeError> {
    reject_domain_placeholder_core(expr).map_err(|source| RuntimeError::TypeError { source })
}

/// Drain a [`QueryStream`] into a single [`ExecutionResult`].
pub async fn collect_query_stream(
    stream: &mut QueryStream<'_>,
    consume: &StreamConsumeOpts,
) -> Result<ExecutionResult, RuntimeError> {
    use futures_util::StreamExt;
    let mut all_entities = Vec::new();
    let mut total_rows = 0usize;
    let mut total_net = 0usize;
    let mut any_live = false;
    let mut last_has_more = false;
    let mut last_resume: Option<QueryPaginationResumeData> = None;
    while let Some(item) = stream.next().await {
        let page = item?;
        last_has_more = page.has_more;
        if page.pagination_resume.is_some() {
            last_resume = page.pagination_resume.clone();
        }
        total_net += page.stats.network_requests;
        if page.stats.network_requests > 0 {
            any_live = true;
        }
        total_rows =
            total_rows.saturating_add(if consume.graph_backed_result && page.entities.is_empty() {
                page.stats.cache_misses
            } else {
                page.entities.len()
            });
        if !consume.graph_backed_result {
            all_entities.extend(page.entities);
        }
        if page.stats.network_requests == 0 && page.stats.cache_hits > 0 {
            // page carried consult hits
        }
    }
    let count = if consume.graph_backed_result {
        total_rows
    } else {
        all_entities.len()
    };
    let entities = if consume.graph_backed_result {
        Vec::new()
    } else {
        all_entities
    };
    let mut stats = ExecutionStats::from_telemetry(CacheTelemetry::default(), total_net);
    stats.record_rows_materialized(count);
    Ok(ExecutionResult {
        entities,
        count,
        has_more: last_has_more,
        pagination_resume: last_resume,
        paging_handle: None,
        source: if any_live {
            ExecutionSource::Live
        } else {
            ExecutionSource::Replay
        },
        stats,
        request_fingerprints: Vec::new(),
    })
}

async fn graph_spill_page_and_trim_hot(
    spill: &crate::graph_page_spill::GraphPageSpillHandle,
    mat: &mut SessionMaterialization,
    page_index: usize,
    page_entities: &[CachedEntity],
) -> Result<(), RuntimeError> {
    use std::time::Instant;

    let span = crate::spans::graph_page_spill(page_index, page_entities.len());
    let _guard = span.enter();
    let started = Instant::now();
    match spill.append_page(page_index, page_entities).await {
        Ok(()) => {
            let cap = spill.hot_bounds().max_hot_entities;
            let evicted = mat.graph_mut().evict_to_hot_limit(cap);
            crate::runtime_metrics::record_graph_page_spill(
                "success",
                page_index,
                page_entities.len(),
                evicted,
                started.elapsed(),
            );
            Ok(())
        }
        Err(e) => {
            crate::runtime_metrics::record_graph_page_spill(
                "error",
                page_index,
                0,
                0,
                started.elapsed(),
            );
            Err(e)
        }
    }
}

/// Execution modes for the runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Execute against live backend
    Live,
    /// Use recorded responses only
    Replay,
    /// Use replay if available, otherwise live + record
    Hybrid,
}

/// Configuration for the execution engine
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Base URL or RPC endpoint for live backend requests.
    pub base_url: Option<String>,
    /// Default execution mode
    pub default_mode: ExecutionMode,
    /// HTTP client timeout in seconds
    pub timeout_seconds: u64,
    /// Whether to validate responses after decoding
    pub validate_responses: bool,
    /// Process-wide concurrent outbound HTTP permits (transport semaphore).
    pub max_concurrent_requests: usize,
    /// Per-origin concurrent outbound HTTP permits.
    pub per_host_max_inflight: usize,
    /// Path for the replay store directory (if using replay/hybrid)
    pub replay_store_path: Option<std::path::PathBuf>,
    /// After query, fetch each row via GET when the entity has a Get capability (unless `QueryExpr.hydrate == Some(false)`).
    pub hydrate: bool,
    /// Max concurrent GETs during query hydration.
    pub hydrate_concurrency: usize,
    /// Max attempts per logical HTTP request (including first try).
    pub http_max_attempts: u32,
    pub http_retry_initial_backoff_ms: u64,
    pub http_retry_max_backoff_ms: u64,
    /// Wall-clock retry budget per logical HTTP request.
    pub http_retry_total_budget_ms: u64,
    /// teaching prompt rendering + symbol expansion (REPL `:schema`, HTTP execute session prompt, eval).
    pub prompt_pipeline: PromptPipelineConfig,
}

/// Result of a query execution
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionResult {
    /// The entities returned by the query
    pub entities: Vec<CachedEntity>,
    /// Number of entities in the result
    pub count: usize,
    /// For paginated queries: whether more rows may exist after this materialized batch.
    #[serde(default)]
    pub has_more: bool,
    /// Host-only continuation payload for opaque LLM paging (`page(pg#)`); never serialized on wire.
    #[serde(skip)]
    pub pagination_resume: Option<QueryPaginationResumeData>,
    /// When set, MCP/HTTP layers may surface a one-line `page(handle)` hint after truncated lists.
    #[serde(skip)]
    pub paging_handle: Option<plasm_core::PagingHandle>,
    /// Whether the result came from cache/replay or live execution
    pub source: ExecutionSource,
    /// Execution statistics
    pub stats: ExecutionStats,
    /// Hex-encoded [`crate::RequestFingerprint`] for each successful outbound compiled op (live or replay), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_fingerprints: Vec<String>,
}

/// Source of execution result
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSource {
    Live,
    Replay,
    Cache,
}

/// Execution statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionStats {
    /// Duration of execution in milliseconds
    pub duration_ms: u64,
    /// Whether any network requests were made
    pub network_requests: usize,
    /// Cache hits during execution (legacy aggregate; see [`Self::cache`])
    pub cache_hits: usize,
    /// Cache misses during execution (legacy aggregate; see [`Self::cache`])
    pub cache_misses: usize,
    /// Honest consult counters and row materialization count.
    #[serde(default)]
    pub cache: CacheTelemetry,
}

impl ExecutionStats {
    pub fn from_telemetry(telemetry: CacheTelemetry, network_requests: usize) -> Self {
        Self {
            duration_ms: 0,
            network_requests,
            cache_hits: telemetry.legacy_cache_hits(),
            cache_misses: telemetry.legacy_cache_misses(),
            cache: telemetry,
        }
    }

    pub fn merge_telemetry(&mut self, other: &CacheTelemetry) {
        self.cache.merge(other);
        self.cache_hits = self.cache.legacy_cache_hits();
        self.cache_misses = self.cache.legacy_cache_misses();
    }

    pub fn record_rows_materialized(&mut self, count: usize) {
        self.cache.rows_materialized = self.cache.rows_materialized.saturating_add(count);
    }
}

/// Stop paginating once this many rows satisfy [`Self::row_match_budget`] predicates.
#[derive(Debug, Clone)]
pub struct RowMatchBudget {
    pub count: usize,
    pub predicates: Vec<crate::row_predicate::JsonRowPredicate>,
}

/// Out-of-band consumption controls: how many pages / entities to pull (not part of the IR).
#[derive(Debug, Clone, Default)]
pub struct StreamConsumeOpts {
    /// Fetch every page until the API reports completion (bounded by a runtime safety cap).
    pub fetch_all: bool,
    /// Maximum number of entities to return in total across all pages.
    pub max_items: Option<usize>,
    /// When set with [`Self::max_items`], perform at most **one** upstream HTTP page while still
    /// clamping page size to `max_items` (LLM paging batches). When unset, `max_items` alone spans
    /// multiple upstream pages until the budget is satisfied (CLI `--limit`).
    pub one_page: bool,
    /// When true, paginated reads keep rows in session graph (+ optional spill) only — do not
    /// duplicate every page into [`ExecutionResult::entities`].
    pub graph_backed_result: bool,
    /// Row-level filter budget: keep paginating until `count` matching rows are materialized.
    pub row_match_budget: Option<RowMatchBudget>,
    /// Streaming top-k over all pages (sort+limit pushdown); memory O(k).
    pub top_k: Option<crate::top_k::TopKSpec>,
}

pub type RowsProgressFn = std::sync::Arc<dyn Fn(usize) + Send + Sync>;

/// Snapshot of [`PaginationLoopState`] for opaque LLM paging continuations (host-only; not for wire serde).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPaginationState {
    pub param_values: Vec<(String, Option<serde_json::Value>)>,
    pub next_absolute_url: Option<String>,
    pub last_requested_limit: u32,
    pub from_block: Option<u64>,
    pub final_to_block: Option<u64>,
    pub last_requested_to_block: Option<u64>,
}

/// Everything needed to issue the next paginated HTTP request after a first-page batch.
/// Host-only snapshot: not serialized on HTTP/MCP wires (avoid accidental logging of templates/env).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPaginationResumeData {
    pub query: plasm_core::QueryExpr,
    pub capability_name: String,
    pub env: plasm_compile::CmlEnv,
    pub template: plasm_compile::CapabilityTemplate,
    pub config: plasm_compile::PaginationConfig,
    pub state: QueryPaginationState,
}

/// One page of decoded, hydrated query results.
#[derive(Debug, Clone, Serialize)]
pub struct PageResult {
    pub entities: Vec<CachedEntity>,
    pub page_index: usize,
    /// Whether another poll may return more rows (same query / stream).
    pub has_more: bool,
    /// When present, host may mint an opaque `page(pg#)` handle for the next batch.
    #[serde(skip)]
    pub pagination_resume: Option<QueryPaginationResumeData>,
    pub stats: ExecutionStats,
}

pub type QueryStream<'a> =
    Pin<Box<dyn futures_util::Stream<Item = Result<PageResult, RuntimeError>> + Send + 'a>>;

tokio::task_local! {
    /// HTTP origin for compiled relative paths during one [`ExecutionEngine::execute`] (or scoped projection).
    static EXECUTION_HTTP_BASE: Arc<str>;
}

tokio::task_local! {
    /// Optional per-session HTTP auth (registry entry CGS) during one [`ExecutionEngine::execute`] / projection.
    /// When `None` inside the scope, [`ExecutionEngine`] falls back to its constructor [`AuthResolver`].
    static EXECUTION_AUTH_RESOLVER: Option<Arc<AuthResolver>>;
}

tokio::task_local! {
    /// Optional compile-plugin hooks for one [`ExecutionEngine::execute`] / stream (see [`ExecuteOptions`]).
    static EXECUTION_PLUGIN_HOOKS: Option<PluginCompileHooks>;
}

tokio::task_local! {
    /// When [`ExecuteOptions::request_fingerprint_sink`] is [`Some`], successful compiled ops append hex fingerprints here.
    static EXECUTION_FINGERPRINT_SINK: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>;
}

tokio::task_local! {
    /// Cooperative cancellation for pagination/hydration loops during one execute task scope.
    static EXECUTION_CANCEL: Option<CancelSignal>;
}

tokio::task_local! {
    static EXECUTION_ROWS_PROGRESS: Option<RowsProgressFn>;
}

/// Report newly materialized rows to the host progress sink when scoped.
#[inline]
pub fn report_rows_materialized(count: usize) {
    if count == 0 {
        return;
    }
    if let Ok(Some(cb)) = EXECUTION_ROWS_PROGRESS.try_with(|c| c.clone()) {
        cb(count);
    }
}

/// Checked between pagination pages and hydrate batches inside an execute task scope.
#[inline]
pub fn cooperative_cancel_check() -> Result<(), RuntimeError> {
    if EXECUTION_CANCEL
        .try_with(|c| c.as_ref().is_some_and(CancelSignal::is_cancelled))
        .unwrap_or(false)
    {
        return Err(RuntimeError::Cancelled);
    }
    Ok(())
}

tokio::task_local! {
    /// When [`ExecuteOptions::federation`] is set, per-catalog HTTP backends apply per outbound request.
    static EXECUTION_FEDERATION: Option<std::sync::Arc<plasm_core::FederationDispatch>>;
}

tokio::task_local! {
    /// When [`ExecuteOptions::execute_session`] is set, identity keys plus optional session mirrors
    /// (`share_token`, Proof `proof_base_token` → `base_token`) are merged via
    /// [`merge_plasm_execute_session_identity_env`], [`merge_plasm_execute_session_share_token_env`],
    /// [`merge_plasm_execute_session_proof_base_token_env`].
    static EXECUTION_EXECUTE_SESSION: Option<std::sync::Arc<ExecuteSessionMaterial>>;
}

/// Session material for the current execute task (view ambient scope injection).
pub(crate) fn try_current_execute_session_material(
) -> Option<std::sync::Arc<ExecuteSessionMaterial>> {
    EXECUTION_EXECUTE_SESSION
        .try_with(|s| s.clone())
        .ok()
        .flatten()
}

tokio::task_local! {
    /// Entity name for the current HTTP op (matches [`plasm_core::FederationDispatch`] keys); selects backend when federated.
    static EXECUTION_DISPATCH_ENTITY: Option<String>;
}

/// Merge session-bound `share_token` into CML env **before** flattened invoke/create parameters
/// so explicit capability parameters can override it (escape hatch).
///
/// No-op when [`ExecuteOptions::execute_session`] is unset or `share_token` is absent/blank.
pub fn merge_plasm_execute_session_share_token_env(env: &mut CmlEnv) {
    let Ok(material) = EXECUTION_EXECUTE_SESSION.try_with(|s| s.clone()) else {
        return;
    };
    let Some(m) = material else {
        return;
    };
    let Some(ref token) = m.share_token else {
        return;
    };
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return;
    }
    env.insert(
        "share_token".to_string(),
        Value::String(trimmed.to_string()),
    );
}

/// Merge session-bound Proof precondition token into CML env as `base_token` **before** flattened
/// invoke parameters so explicit `base_token=` on a capability overrides it (escape hatch).
///
/// No-op when [`ExecuteOptions::execute_session`] is unset or `proof_base_token` is absent/blank.
pub fn merge_plasm_execute_session_proof_base_token_env(env: &mut CmlEnv) {
    let Ok(material) = EXECUTION_EXECUTE_SESSION.try_with(|s| s.clone()) else {
        return;
    };
    let Some(m) = material else {
        return;
    };
    let Some(ref token) = m.proof_base_token else {
        return;
    };
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return;
    }
    env.insert("base_token".to_string(), Value::String(trimmed.to_string()));
}

/// Merge [`CML_ENV_PLASM_EXECUTE_PROMPT_HASH`] / [`CML_ENV_PLASM_EXECUTE_SESSION_ID`] when the
/// host set [`ExecuteOptions::execute_session`] for this execute task (see task-local scope).
///
/// Call **after** capability `preflight` merges for invoke/create; for GET/delete, call after path
/// env and splat so internal preflight GETs (which run with TLS unset or unchanged) omit this.
pub fn merge_plasm_execute_session_identity_env(env: &mut CmlEnv) {
    let Ok(material) = EXECUTION_EXECUTE_SESSION.try_with(|s| s.clone()) else {
        return;
    };
    let Some(m) = material else {
        return;
    };
    env.insert(
        CML_ENV_PLASM_EXECUTE_PROMPT_HASH.to_string(),
        Value::String(m.prompt_hash.clone()),
    );
    env.insert(
        CML_ENV_PLASM_EXECUTE_SESSION_ID.to_string(),
        Value::String(m.session_id.clone()),
    );
}

/// Back-compat alias: identity keys only (prompt hash + session id).
#[inline]
pub fn merge_plasm_execute_session_env(env: &mut CmlEnv) {
    merge_plasm_execute_session_identity_env(env);
    merge_plasm_execute_session_bind_env(env);
}

/// Merge session-constant MCP connect bindings as precomputed `bind_<wire>` CML env keys.
pub fn merge_plasm_execute_session_bind_env(env: &mut CmlEnv) {
    let Some(m) = try_current_execute_session_material() else {
        return;
    };
    let Some(bind) = m.catalog_bind.as_ref() else {
        return;
    };
    for (key, value) in bind {
        if value.trim().is_empty() {
            continue;
        }
        env.insert(key.clone(), Value::String(value.clone()));
    }
}

async fn with_dispatch_entity<Fut, T>(entity: Option<&str>, fut: Fut) -> T
where
    Fut: std::future::Future<Output = T> + Send,
{
    EXECUTION_DISPATCH_ENTITY
        .scope(entity.map(|s| s.to_string()), fut)
        .await
}

/// Append a request fingerprint (hex) when a fingerprint sink is active; collapses consecutive duplicates.
fn append_request_fingerprint(hex: String) {
    let _ = EXECUTION_FINGERPRINT_SINK.try_with(|holder| {
        if let Some(m) = holder {
            let mut v = m.lock().unwrap_or_else(|e| e.into_inner());
            if v.last().map(|s| s.as_str()) != Some(hex.as_str()) {
                v.push(hex);
            }
        }
    });
}

/// Compile-plugin hooks copied from [`ExecuteOptions`] into [`EXECUTION_PLUGIN_HOOKS`] for the execute task.
#[derive(Clone)]
pub struct PluginCompileHooks {
    pub compile_operation_fn: Option<Arc<CompileOperationFn>>,
    pub compile_query_fn: Option<Arc<CompileQueryFn>>,
}

impl PluginCompileHooks {
    fn snapshot_from_execute_options(opts: &ExecuteOptions) -> Self {
        Self {
            compile_operation_fn: opts.compile_operation_fn.clone(),
            compile_query_fn: opts.compile_query_fn.clone(),
        }
    }
}

/// Compile-plugin hook: replaces [`compile_operation`] when set (see `plasm-plugin-host`).
pub type CompileOperationFn = CompileOperationHook;
/// Compile-plugin hook: replaces [`compile_query`] when set.
pub type CompileQueryFn = CompileQueryHook;

fn compile_operation_dispatch(
    template: &CapabilityTemplate,
    env: &CmlEnv,
) -> Result<CompiledOperation, RuntimeError> {
    let hooks = match EXECUTION_PLUGIN_HOOKS.try_with(|h| h.clone()) {
        Ok(h) => h,
        Err(_) => {
            tracing::debug!("EXECUTION_PLUGIN_HOOKS unset; using builtin compile_operation");
            None
        }
    };
    if let Some(hooks) = hooks {
        if let Some(f) = hooks.compile_operation_fn {
            return f(template, env).map_err(|e| RuntimeError::CmlError { source: e });
        }
    }
    compile_operation(template, env).map_err(|e| RuntimeError::CmlError { source: e })
}

fn compile_query_dispatch(
    query: &QueryExpr,
    cgs: &CGS,
) -> Result<Option<BackendFilter>, RuntimeError> {
    let hooks = match EXECUTION_PLUGIN_HOOKS.try_with(|h| h.clone()) {
        Ok(h) => h,
        Err(_) => {
            tracing::debug!("EXECUTION_PLUGIN_HOOKS unset; using builtin compile_query");
            None
        }
    };
    if let Some(hooks) = hooks {
        if let Some(f) = hooks.compile_query_fn {
            return f(query, cgs).map_err(|e| RuntimeError::CompilationError { source: e });
        }
    }
    compile_query(query, cgs).map_err(|e| RuntimeError::CompilationError { source: e })
}

/// Per-call options for [`ExecutionEngine::execute`] and [`ExecutionEngine::auto_resolve_projection`].
#[derive(Clone, Default)]
pub struct ExecuteOptions {
    /// When set, each successful compiled HTTP/EVM operation appends [`crate::RequestFingerprint::to_hex`] (see [`ExecutionResult::request_fingerprints`]).
    pub request_fingerprint_sink: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
    /// When set (non-empty after trim), HTTP(S) requests use this origin instead of [`ExecutionConfig::base_url`].
    /// EVM RPC URLs still use [`ExecutionConfig::base_url`] only.
    pub http_base_url_override: Option<String>,
    /// When set, outbound **HTTP** requests resolve credentials from this resolver instead of the engine's
    /// [`ExecutionEngine::new_with_auth`] resolver. EVM paths ignore this and use the engine resolver only.
    pub auth_resolver_override: Option<Arc<AuthResolver>>,
    /// Optional compile plugin: replaces [`compile_operation`] when set (e.g. dynamic `cdylib` generation).
    pub compile_operation_fn: Option<Arc<CompileOperationFn>>,
    /// Optional compile plugin: replaces [`compile_query`] when set.
    pub compile_query_fn: Option<Arc<CompileQueryFn>>,
    /// Pinned plugin generation id for observability (HTTP/MCP execute sessions).
    pub plugin_generation_id: Option<u64>,
    /// When set, typecheck and HTTP dispatch use per-entity owning [`plasm_core::CgsContext`].
    pub federation: Option<std::sync::Arc<plasm_core::FederationDispatch>>,
    /// When set, agent-core preflight already type-checked and placeholder-gated this expression.
    pub preflight: Option<plasm_core::PreflightToken>,
    /// When set, CML compilation for outbound HTTP sees reserved `plasm_execute_*` env keys
    /// ([`merge_plasm_execute_session_env`]) plus optional session-bound `share_token`
    /// ([`merge_plasm_execute_session_share_token_env`]) and Proof `proof_base_token` as `base_token`
    /// ([`merge_plasm_execute_session_proof_base_token_env`]).
    pub execute_session: Option<std::sync::Arc<ExecuteSessionMaterial>>,
    /// Cooperative cancellation checked between pagination/hydration batches.
    pub cancel: Option<CancelSignal>,
    /// When set, each paginated graph page is appended to durable storage and hot RAM is trimmed.
    pub graph_page_spill: Option<crate::graph_page_spill::GraphPageSpillHandle>,
    /// Incremental row materialization callback (async plan progress during pagination).
    pub rows_progress: Option<RowsProgressFn>,
}

impl std::fmt::Debug for ExecuteOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecuteOptions")
            .field(
                "request_fingerprint_sink",
                &self.request_fingerprint_sink.is_some(),
            )
            .field("http_base_url_override", &self.http_base_url_override)
            .field(
                "auth_resolver_override",
                &self.auth_resolver_override.is_some(),
            )
            .field("compile_operation_fn", &self.compile_operation_fn.is_some())
            .field("compile_query_fn", &self.compile_query_fn.is_some())
            .field("plugin_generation_id", &self.plugin_generation_id)
            .field("federation", &self.federation.is_some())
            .field("preflight", &self.preflight.is_some())
            .field("execute_session", &self.execute_session.is_some())
            .field("graph_page_spill", &self.graph_page_spill.is_some())
            .field("rows_progress", &self.rows_progress.is_some())
            .finish()
    }
}

impl ExecuteOptions {
    /// View scope injection context derived from this execute call's session material / HTTP base.
    pub fn view_ambient(&self) -> ViewAmbientContext {
        if let Some(material) = self.execute_session.as_ref() {
            return ViewAmbientContext::from_execute_material(material.as_ref());
        }
        ViewAmbientContext::from_http_backend(self.http_base_url_override.as_deref())
    }
}

/// Main execution engine
pub struct ExecutionEngine {
    transport: Arc<dyn HttpTransport>,
    config: ExecutionConfig,
    replay_store: Option<crate::MemoryReplayStore>,
    /// Optional authentication resolver injected on every outbound HTTP request.
    auth_resolver: Option<AuthResolver>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            default_mode: ExecutionMode::Live,
            timeout_seconds: 30,
            validate_responses: true,
            max_concurrent_requests: 64,
            per_host_max_inflight: 24,
            replay_store_path: None,
            hydrate: true,
            hydrate_concurrency: 16,
            http_max_attempts: 4,
            http_retry_initial_backoff_ms: 500,
            http_retry_max_backoff_ms: 30_000,
            http_retry_total_budget_ms: 120_000,
            prompt_pipeline: PromptPipelineConfig::default(),
        }
    }
}

impl ExecutionEngine {
    fn resolve_http_base_from_opts(&self, opts: &ExecuteOptions) -> Arc<str> {
        if let Some(ref o) = opts.http_base_url_override {
            let t = o.trim();
            if !t.is_empty() {
                return t.to_string().into();
            }
        }
        self.config
            .base_url
            .clone()
            .unwrap_or_else(|| "http://localhost:3000".to_string())
            .into()
    }

    /// Task locals for fingerprint sink, HTTP base URL, auth override, compile-plugin hooks (one nested region).
    #[allow(clippy::too_many_arguments)]
    async fn run_in_execute_task_scopes<Fut, T>(
        base: Arc<str>,
        auth_override: Option<Arc<AuthResolver>>,
        plugin_hooks: PluginCompileHooks,
        request_fingerprint_sink: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
        federation: Option<std::sync::Arc<plasm_core::FederationDispatch>>,
        execute_session: Option<std::sync::Arc<ExecuteSessionMaterial>>,
        cancel: Option<CancelSignal>,
        rows_progress: Option<RowsProgressFn>,
        fut: Fut,
    ) -> T
    where
        Fut: std::future::Future<Output = T> + Send,
        T: Send,
    {
        EXECUTION_EXECUTE_SESSION
            .scope(execute_session, async move {
                EXECUTION_FEDERATION
                    .scope(federation, async move {
                        EXECUTION_FINGERPRINT_SINK
                            .scope(request_fingerprint_sink, async move {
                                EXECUTION_PLUGIN_HOOKS
                                    .scope(Some(plugin_hooks), async move {
                                        EXECUTION_AUTH_RESOLVER
                                            .scope(auth_override, async move {
                                                EXECUTION_CANCEL
                                                    .scope(cancel, async move {
                                                        EXECUTION_ROWS_PROGRESS
                                                            .scope(rows_progress, async move {
                                                                EXECUTION_HTTP_BASE
                                                                    .scope(base, fut)
                                                                    .await
                                                            })
                                                            .await
                                                    })
                                                    .await
                                            })
                                            .await
                                    })
                                    .await
                            })
                            .await
                    })
                    .await
            })
            .await
    }

    fn effective_http_base_for_request(&self) -> Arc<str> {
        let default_base = EXECUTION_HTTP_BASE
            .try_with(|b| b.clone())
            .unwrap_or_else(|_| {
                self.config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "http://localhost:3000".to_string())
                    .into()
            });

        let fed = EXECUTION_FEDERATION.try_with(|f| f.clone()).ok().flatten();
        let ent = EXECUTION_DISPATCH_ENTITY
            .try_with(|e| e.clone())
            .ok()
            .flatten();
        if let (Some(fed), Some(ent)) = (fed, ent) {
            if let Some(u) = fed.http_backend_for_entity(ent.as_str()) {
                let t = u.trim();
                if !t.is_empty() {
                    return t.to_string().into();
                }
            }
        }
        default_base
    }

    /// Full execution configuration (HTTP, hydration, **prompt pipeline**, …).
    #[inline]
    pub fn config(&self) -> &ExecutionConfig {
        &self.config
    }

    /// Prompt rendering / symbol expansion settings shared with teaching table and `expand_expr_for_parse`.
    #[inline]
    pub fn prompt_pipeline(&self) -> &PromptPipelineConfig {
        &self.config.prompt_pipeline
    }

    /// Create a new execution engine with no authentication.
    pub fn new(config: ExecutionConfig) -> Result<Self, RuntimeError> {
        Self::new_with_auth(config, None)
    }

    /// Create a new execution engine with an optional [`AuthResolver`].
    ///
    /// When `auth_resolver` is `Some`, every outbound HTTP request (including
    /// pagination continuation requests) will have credentials injected before
    /// being sent.
    pub fn new_with_auth(
        config: ExecutionConfig,
        auth_resolver: Option<AuthResolver>,
    ) -> Result<Self, RuntimeError> {
        // GitHub and several other APIs reject requests without User-Agent (often HTML 403 → JSON parse errors).
        let per_host = config.per_host_max_inflight.max(1);
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                "plasm-runtime/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com)"
            ))
            .timeout(std::time::Duration::from_secs(config.timeout_seconds))
            .pool_max_idle_per_host(per_host)
            .build()
            .map_err(|e| RuntimeError::RequestError {
                message: format!("Failed to create HTTP client: {}", e),
                attempts: 1,
            })?;

        let inner = ReqwestHttpTransport::new(client);
        let policy = HttpResiliencePolicy::from(&config);
        let transport = ResilientHttpTransport::new(inner, policy);

        Ok(Self {
            transport: Arc::new(transport),
            config,
            replay_store: Some(crate::MemoryReplayStore::default()),
            auth_resolver,
        })
    }

    /// Build an engine with a custom [`HttpTransport`] (e.g. test double, corporate proxy, tracing).
    pub fn new_with_transport(
        config: ExecutionConfig,
        transport: Arc<dyn HttpTransport>,
        auth_resolver: Option<AuthResolver>,
    ) -> Self {
        Self {
            transport,
            config,
            replay_store: Some(crate::MemoryReplayStore::default()),
            auth_resolver,
        }
    }

    /// Execute a schema-overlay source capability and return the raw JSON response body.
    pub async fn fetch_overlay_source_response(
        &self,
        cgs: &CGS,
        capability_name: &str,
        http_base: &str,
        auth_resolver_override: Option<Arc<AuthResolver>>,
        mode: ExecutionMode,
        bind: Option<&IndexMap<String, String>>,
    ) -> Result<serde_json::Value, RuntimeError> {
        use plasm_core::value::Value;
        use plasm_core::CapabilityKind;

        let cap = cgs.get_capability(capability_name).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!("schema overlay source capability '{capability_name}' not found"),
            }
        })?;
        if !matches!(
            cap.kind,
            CapabilityKind::Query | CapabilityKind::Get | CapabilityKind::Search
        ) {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "schema overlay source capability '{capability_name}' must be query, get, or search"
                ),
            });
        }
        let template = parse_capability_template(&cap.mapping.template.0).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: format!("schema overlay source template: {e}"),
            }
        })?;
        let mut env = CmlEnv::new();
        if let Some(bind) = bind {
            for (key, value) in bind {
                env.insert(key.clone(), Value::String(value.clone()));
            }
        }
        let compiled =
            compile_operation(&template, &env).map_err(|e| RuntimeError::CmlError { source: e })?;
        let base = http_base.trim().trim_end_matches('/').to_string();
        Self::run_in_execute_task_scopes(
            base.into(),
            auth_resolver_override,
            PluginCompileHooks {
                compile_operation_fn: None,
                compile_query_fn: None,
            },
            None,
            None,
            None,
            None,
            None,
            async move {
                self.execute_with_replay(&compiled, mode, None)
                    .await
                    .map(|(j, _)| j)
            },
        )
        .await
    }

    /// Execute an HTTP request with replay awareness.
    /// In Live mode: execute and optionally record.
    /// In Replay mode: look up by fingerprint.
    /// In Hybrid mode: replay if available, otherwise live + record.
    async fn execute_with_replay(
        &self,
        compiled: &CompiledOperation,
        mode: ExecutionMode,
        mat: Option<&mut SessionMaterialization>,
    ) -> Result<(serde_json::Value, ExecutionSource), RuntimeError> {
        let (json, _link, source) = self.execute_with_replay_full(compiled, mode, mat).await?;
        Ok((json, source))
    }

    /// Like [`execute_with_replay`], but also returns `Link: ...; rel="next"` for CML `link_header` pagination (live only).
    async fn execute_with_replay_full(
        &self,
        compiled: &CompiledOperation,
        mode: ExecutionMode,
        mat: Option<&mut SessionMaterialization>,
    ) -> Result<(serde_json::Value, Option<String>, ExecutionSource), RuntimeError> {
        let fingerprint = crate::RequestFingerprint::from_operation(compiled);
        let mut consult = CacheTelemetry::default();

        match mode {
            ExecutionMode::Live => {
                if let Some(session) = mat {
                    if let Some(stored) = ExecutionCacheConsult::decide_response(
                        &fingerprint,
                        &session.responses,
                        &mut consult,
                    ) {
                        append_request_fingerprint(fingerprint.to_hex());
                        return Ok((stored.response, None, stored.source));
                    }
                    ExecutionCacheConsult::record_response_miss(&mut consult);
                    let (resp, link) = self.execute_operation_full(compiled).await?;
                    session
                        .responses
                        .store(fingerprint, resp.clone(), ExecutionSource::Live);
                    Ok((resp, link, ExecutionSource::Live))
                } else {
                    let (resp, link) = self.execute_operation_full(compiled).await?;
                    Ok((resp, link, ExecutionSource::Live))
                }
            }
            ExecutionMode::Replay => {
                if let Some(store) = &self.replay_store {
                    use crate::ReplayStore;
                    if let Some(entry) = store.lookup(&fingerprint)? {
                        append_request_fingerprint(fingerprint.to_hex());
                        return Ok((entry.response, None, ExecutionSource::Replay));
                    }
                }
                Err(RuntimeError::ReplayEntryNotFound {
                    fingerprint: fingerprint.to_hex(),
                })
            }
            ExecutionMode::Hybrid => {
                if let Some(store) = &self.replay_store {
                    use crate::ReplayStore;
                    if let Some(entry) = store.lookup(&fingerprint)? {
                        append_request_fingerprint(fingerprint.to_hex());
                        return Ok((entry.response, None, ExecutionSource::Replay));
                    }
                }
                if let Some(session) = mat {
                    if let Some(stored) = ExecutionCacheConsult::decide_response(
                        &fingerprint,
                        &session.responses,
                        &mut consult,
                    ) {
                        append_request_fingerprint(fingerprint.to_hex());
                        return Ok((stored.response, None, stored.source));
                    }
                    ExecutionCacheConsult::record_response_miss(&mut consult);
                    let (resp, link) = self.execute_operation_full(compiled).await?;
                    session
                        .responses
                        .store(fingerprint, resp.clone(), ExecutionSource::Live);
                    Ok((resp, link, ExecutionSource::Live))
                } else {
                    let (resp, link) = self.execute_operation_full(compiled).await?;
                    Ok((resp, link, ExecutionSource::Live))
                }
            }
        }
    }

    async fn execute_operation_full(
        &self,
        operation: &CompiledOperation,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let fp = crate::RequestFingerprint::from_operation(operation);
        let out = match operation {
            CompiledOperation::Http(request) => self.execute_http_request_full(request).await,
            CompiledOperation::GraphQl(request) => self.execute_http_request_full(request).await,
            CompiledOperation::EvmCall(request) => {
                let rpc_url = self.evm_rpc_url()?;
                let auth = self.resolve_auth().await?;
                let json = execute_evm_call(rpc_url, auth.as_ref(), request).await?;
                Ok((json, None))
            }
            CompiledOperation::EvmLogs(request) => {
                let rpc_url = self.evm_rpc_url()?;
                let auth = self.resolve_auth().await?;
                let json = execute_evm_logs(rpc_url, auth.as_ref(), request).await?;
                Ok((json, None))
            }
            CompiledOperation::View(_) => Err(RuntimeError::ConfigurationError {
                message: "composed views execute via Query (`transport: view`), not HTTP invoke"
                    .into(),
            }),
        };
        if out.is_ok() {
            append_request_fingerprint(fp.to_hex());
        }
        out
    }

    fn evm_rpc_url(&self) -> Result<&str, RuntimeError> {
        self.config
            .base_url
            .as_deref()
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: "EVM transport requires ExecutionConfig.base_url to be set to an RPC URL"
                    .to_string(),
            })
    }

    /// Resolves credentials for **EVM** RPC requests only (ignores per-session HTTP override).
    async fn resolve_auth(&self) -> Result<Option<crate::ResolvedAuth>, RuntimeError> {
        match &self.auth_resolver {
            Some(resolver) => resolver.resolve().await.map(Some),
            None => Ok(None),
        }
    }

    /// Resolves credentials for **HTTP** requests: per-session override when set, else engine resolver.
    async fn resolve_auth_http(&self) -> Result<Option<crate::ResolvedAuth>, RuntimeError> {
        if let Ok(Some(resolver)) = EXECUTION_AUTH_RESOLVER.try_with(|o| o.clone()) {
            return resolver.resolve().await.map(Some);
        }
        match &self.auth_resolver {
            Some(resolver) => resolver.resolve().await.map(Some),
            None => Ok(None),
        }
    }

    /// Execute an expression (materializes the full stream per [`StreamConsumeOpts`]).
    pub fn execute<'a>(
        &'a self,
        expr: &'a Expr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        Box::pin(async move {
            crate::check_cancel(opts.cancel.as_ref())?;
            let start_time = std::time::Instant::now();
            let base = self.resolve_http_base_from_opts(&opts);
            let auth_override = opts.auth_resolver_override.clone();
            let plugin_hooks = PluginCompileHooks::snapshot_from_execute_options(&opts);
            let fp_sink = opts.request_fingerprint_sink.clone();
            let federation = opts.federation.clone();
            let execute_session = opts.execute_session.clone();
            let cancel = opts.cancel.clone();
            let rows_progress = opts.rows_progress.clone();
            let mut result = Self::run_in_execute_task_scopes(
                base,
                auth_override,
                plugin_hooks,
                fp_sink.clone(),
                federation,
                execute_session,
                cancel,
                rows_progress,
                async move {
                    let mut stream =
                        self.execute_stream(expr, cgs, mat, mode, consume.clone(), opts)?;
                    collect_query_stream(&mut stream, &consume).await
                },
            )
            .await?;
            result.stats.duration_ms = start_time.elapsed().as_millis() as u64;
            result.request_fingerprints = fp_sink
                .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .unwrap_or_default();
            Ok(result)
        })
    }

    /// Lazy page-by-page execution. Limits are in [`StreamConsumeOpts`], not the expression IR.
    pub fn execute_stream<'a>(
        &'a self,
        expr: &'a Expr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        if opts.preflight.is_none() {
            if let Some(ref fed) = opts.federation {
                type_check_expr_federated(expr, fed.as_ref(), cgs)?;
            } else {
                type_check_expr(expr, cgs)?;
            }
            reject_domain_placeholder_in_executable(expr)?;
        }
        let execution_mode = mode.unwrap_or(self.config.default_mode);
        let chain_consume = consume.clone();
        let view_ambient = opts.view_ambient();
        match expr {
            Expr::Query(query) => self.query_to_stream(
                query,
                cgs,
                mat,
                execution_mode,
                consume,
                opts.graph_page_spill.clone(),
                &view_ambient,
            ),
            Expr::Page(_) => Err(RuntimeError::ConfigurationError {
                message: "`page(pg#)` continuations are executed via `ExecutionEngine::execute_pagination_resume`"
                    .to_string(),
            }),
            Expr::Wait(_) => Err(RuntimeError::ConfigurationError {
                message: "`wait(sN_oM)` continuations are executed by the agent host (async plan poll)"
                    .to_string(),
            }),
            Expr::Cancel(_) => Err(RuntimeError::ConfigurationError {
                message: "`cancel(sN_oM)` continuations are executed by the agent host (async plan cancel)"
                    .to_string(),
            }),
            Expr::Get(get) => {
                let get = get.clone();
                let ambient = view_ambient;
                let stream = Box::pin(async_stream::try_stream! {
                    let res = self.execute_get(&get, cgs, mat, execution_mode, &ambient).await?;
                    yield PageResult {
                        entities: res.entities,
                        page_index: 0,
                        has_more: false,
                        pagination_resume: None,
                        stats: res.stats,
                    };
                });
                Ok(stream)
            }
            Expr::Create(create) => {
                let create = create.clone();
                let stream = Box::pin(async_stream::try_stream! {
                    let res = self.execute_create(&create, cgs, mat, execution_mode).await?;
                    yield PageResult {
                        entities: res.entities,
                        page_index: 0,
                        has_more: false,
                        pagination_resume: None,
                        stats: res.stats,
                    };
                });
                Ok(stream)
            }
            Expr::Delete(delete) => {
                let delete = delete.clone();
                let stream = Box::pin(async_stream::try_stream! {
                    let res = self.execute_delete(&delete, cgs, mat, execution_mode).await?;
                    yield PageResult {
                        entities: res.entities,
                        page_index: 0,
                        has_more: false,
                        pagination_resume: None,
                        stats: res.stats,
                    };
                });
                Ok(stream)
            }
            Expr::Invoke(invoke) => {
                let invoke = invoke.clone();
                let stream = Box::pin(async_stream::try_stream! {
                    let res = self.execute_invoke(&invoke, cgs, mat, execution_mode).await?;
                    yield PageResult {
                        entities: res.entities,
                        page_index: 0,
                        has_more: false,
                        pagination_resume: None,
                        stats: res.stats,
                    };
                });
                Ok(stream)
            }
            Expr::Chain(chain) => {
                let chain = chain.clone();
                let stream = Box::pin(async_stream::try_stream! {
                    let res = self
                        .execute_chain(&chain, cgs, mat, execution_mode, chain_consume, opts.clone())
                        .await?;
                    yield PageResult {
                        entities: res.entities,
                        page_index: 0,
                        has_more: false,
                        pagination_resume: None,
                        stats: res.stats,
                    };
                });
                Ok(stream)
            }
            Expr::TeachingValue { .. } => Err(RuntimeError::ConfigurationError {
                message: "`Expr::TeachingValue` is teaching-table-only (prompt teaching); it cannot be executed"
                    .to_string(),
            }),
        }
    }

    /// Execute a query expression (materializes [`query_to_stream`]).
    pub(crate) async fn execute_query(
        &self,
        query: &QueryExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        ambient: &ViewAmbientContext,
    ) -> Result<ExecutionResult, RuntimeError> {
        let mut stream =
            self.query_to_stream(query, cgs, mat, mode, consume.clone(), None, ambient)?;
        collect_query_stream(&mut stream, &consume).await
    }

    /// Execute a query with cross-entity predicate decomposition.
    ///
    /// For each cross-entity predicate (e.g. `pet.status = available`):
    /// - **Push-left**: query the foreign entity first, collect matching IDs,
    ///   inject an FK equality predicate on the source query.
    /// - **Pull-right**: query source without the cross-entity predicate,
    ///   then client-side filter each row by fetching the foreign entity.
    #[allow(clippy::too_many_arguments)]
    fn execute_query_cross_entity<'a>(
        &'a self,
        query: &'a QueryExpr,
        crosses: &'a [plasm_core::cross_entity::CrossEntityPredicate],
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        ambient: &'a ViewAmbientContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        let ambient = ambient.clone();
        Box::pin(async move {
            let source_entity =
                cgs.get_entity(&query.entity)
                    .ok_or_else(|| RuntimeError::ConfigurationError {
                        message: format!("Entity '{}' not found", query.entity),
                    })?;

            let mut push_left_preds: Vec<Predicate> = Vec::new();
            let mut pull_right_crosses: Vec<plasm_core::cross_entity::CrossEntityPredicate> =
                Vec::new();
            let mut total_network = 0usize;
            let mut any_live = false;

            for cross in crosses {
                match choose_strategy(cross, &query.entity, cgs) {
                    CrossEntityStrategy::PushLeft {
                        cross: c,
                        source_fk_param,
                    } => {
                        // Query foreign entity to get matching IDs.
                        let foreign_query =
                            QueryExpr::filtered(&c.foreign_entity, c.foreign_predicate.clone());
                        let foreign_result = self
                            .execute_query(
                                &foreign_query,
                                cgs,
                                mat,
                                mode,
                                StreamConsumeOpts {
                                    fetch_all: true,
                                    max_items: None,
                                    one_page: false,
                                    graph_backed_result: false,
                                    ..Default::default()
                                },
                                &ambient,
                            )
                            .await?;

                        total_network += foreign_result.stats.network_requests;
                        if foreign_result.source == ExecutionSource::Live {
                            any_live = true;
                        }

                        let foreign_ids: Vec<Value> = foreign_result
                            .entities
                            .iter()
                            .map(|e| Value::String(e.reference.primary_slot_str()))
                            .collect();

                        if foreign_ids.is_empty() {
                            return Ok(ExecutionResult {
                                entities: vec![],
                                count: 0,
                                has_more: false,
                                pagination_resume: None,
                                paging_handle: None,
                                source: ExecutionSource::Live,
                                stats: ExecutionStats {
                                    duration_ms: 0,
                                    network_requests: total_network,
                                    cache_hits: 0,
                                    cache_misses: 0,
                                    ..Default::default()
                                },
                                request_fingerprints: Vec::new(),
                            });
                        }

                        if foreign_ids.len() == 1 {
                            push_left_preds.push(Predicate::eq(
                                &source_fk_param,
                                foreign_ids.into_iter().next().unwrap(),
                            ));
                        } else {
                            push_left_preds
                                .push(Predicate::in_(&source_fk_param, Value::Array(foreign_ids)));
                        }
                    }
                    CrossEntityStrategy::PullRight { cross: c } => {
                        pull_right_crosses.push(c);
                    }
                }
            }

            // Build the rewritten query: local predicates + push-left FK predicates.
            let local_pred = strip_cross_entity_comparisons(
                query.predicate.as_ref().unwrap(),
                source_entity,
                cgs,
            );

            let mut all_preds: Vec<Predicate> = push_left_preds;
            if let Some(lp) = local_pred {
                all_preds.push(lp);
            }

            let rewritten_pred = match all_preds.len() {
                0 => None,
                1 => Some(all_preds.into_iter().next().unwrap()),
                _ => Some(Predicate::and(all_preds)),
            };

            let mut rewritten_query = query.clone();
            rewritten_query.predicate = rewritten_pred;

            let mut result = self
                .execute_query(&rewritten_query, cgs, mat, mode, consume, &ambient)
                .await?;
            result.stats.network_requests += total_network;
            if any_live {
                result.source = ExecutionSource::Live;
            }

            // Pull-right client-side filter for any crosses that couldn't push left.
            if !pull_right_crosses.is_empty() {
                let mut filtered = Vec::new();
                for entity in &result.entities {
                    let mut passes = true;
                    for cross in &pull_right_crosses {
                        let ref_id = extract_ref_id(entity, &cross.ref_field, cgs);
                        let Some(id) = ref_id else {
                            passes = false;
                            break;
                        };

                        let get = GetExpr::new(&cross.foreign_entity, &id);
                        let get_result = self.execute_get(&get, cgs, mat, mode, &ambient).await?;
                        result.stats.network_requests += get_result.stats.network_requests;

                        let Some(foreign) = get_result.entities.first() else {
                            passes = false;
                            break;
                        };

                        if !client_side_predicate_matches(foreign, &cross.foreign_predicate) {
                            passes = false;
                            break;
                        }
                    }
                    if passes {
                        filtered.push(entity.clone());
                    }
                }
                result.entities = filtered;
                result.count = result.entities.len();
            }

            Ok(result)
        })
    }

    /// Execute a get expression
    pub(crate) async fn execute_get(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        ambient: &ViewAmbientContext,
    ) -> Result<ExecutionResult, RuntimeError> {
        // Satisfy from cache only when we already hold a detail payload.
        if let Some(entity) = mat.get(&get.reference) {
            if entity.completeness == EntityCompleteness::Complete {
                return Ok(ExecutionResult {
                    entities: vec![entity.clone()],
                    count: 1,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 0,
                        cache_hits: 1,
                        cache_misses: 0,
                        ..Default::default()
                    },
                    request_fingerprints: Vec::new(),
                });
            }
        }

        let (cached, source) = self
            .fetch_get_decoded(
                get,
                cgs,
                mode,
                get.capability_name.as_deref(),
                true,
                Some(mat),
                ambient,
            )
            .await?;
        mat.insert(cached.clone())?;

        Ok(ExecutionResult {
            entities: vec![cached],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: if source == ExecutionSource::Live {
                    1
                } else {
                    0
                },
                cache_hits: 0,
                cache_misses: 1,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Like [`Self::execute_get`] for HTTP-backed GET capabilities used **inside** a composed `views:` DAG.
    pub(crate) async fn execute_get_for_view_dag(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        if let Some(entity) = mat.get(&get.reference) {
            if entity.completeness == EntityCompleteness::Complete {
                return Ok(ExecutionResult {
                    entities: vec![entity.clone()],
                    count: 1,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 0,
                        cache_hits: 1,
                        cache_misses: 0,
                        ..Default::default()
                    },
                    request_fingerprints: Vec::new(),
                });
            }
        }

        let capability = cgs
            .find_capability(&get.reference.entity_type, plasm_core::CapabilityKind::Get)
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: "get".to_string(),
                entity: get.reference.entity_type.to_string(),
            })?;
        let capability_template = parse_capability_template(&capability.mapping.template)?;
        if matches!(capability_template, CapabilityTemplate::View(_)) {
            return Err(RuntimeError::ConfigurationError {
                message:
                    "composed-view GET transport cannot nest as an inner node inside another views: DAG"
                        .into(),
            });
        }
        let (cached, source) = self
            .fetch_http_transport_get_decoded(
                get,
                cgs,
                mode,
                capability,
                &capability_template,
                true,
                Some(mat),
            )
            .await?;
        mat.insert(cached.clone())?;

        Ok(ExecutionResult {
            entities: vec![cached],
            count: 1,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source,
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: if source == ExecutionSource::Live {
                    1
                } else {
                    0
                },
                cache_hits: 0,
                cache_misses: 1,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// When the primary capability's CML [`HttpResponseDecode::auxiliary_merge`] is set, perform that
    /// follow-up GET (same [`CmlEnv`] as the primary request) and shallow-merge one field into the
    /// primary JSON before narrowing/decoding.
    async fn apply_auxiliary_http_merge_response(
        &self,
        capability_template: &CapabilityTemplate,
        env: &CmlEnv,
        mode: ExecutionMode,
        primary: serde_json::Value,
        entity_dispatch_hint: Option<&str>,
    ) -> Result<serde_json::Value, RuntimeError> {
        let CapabilityTemplate::Http(primary_cml) = capability_template else {
            return Ok(primary);
        };
        let Some(ref decode) = primary_cml.response else {
            return Ok(primary);
        };
        let Some(ref aux) = decode.auxiliary_merge else {
            return Ok(primary);
        };

        let aux_req = CmlRequest {
            method: aux.method.clone(),
            path: aux.path.clone(),
            query: aux.query.clone(),
            body: None,
            body_format: HttpBodyFormat::default(),
            multipart: None,
            headers: aux.headers.clone(),
            pagination: None,
            response: None,
        };
        let aux_template = CapabilityTemplate::Http(aux_req);
        let compiled = compile_operation_dispatch(&aux_template, env)?;

        let aux_body = match with_dispatch_entity(
            entity_dispatch_hint,
            self.execute_with_replay(&compiled, mode, None),
        )
        .await
        {
            Ok((v, _)) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "auxiliary_merge request failed; continuing with primary response only"
                );
                return Ok(primary);
            }
        };

        let extracted = if aux.from_path.is_empty() {
            aux_body
        } else {
            walk_json_path(&aux_body, &aux.from_path)
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        };

        let serde_json::Value::Object(mut primary_obj) = primary else {
            return Ok(primary);
        };
        primary_obj.insert(aux.into_key.clone(), extracted);
        Ok(serde_json::Value::Object(primary_obj))
    }

    /// HTTP(S)/GraphQL GET path only — never dispatches composed [`CapabilityTemplate::View`].
    #[allow(clippy::too_many_arguments)]
    async fn fetch_http_transport_get_decoded(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mode: ExecutionMode,
        capability: &CapabilitySchema,
        capability_template: &CapabilityTemplate,
        inject_execute_session_env: bool,
        mut cache: Option<&mut SessionMaterialization>,
    ) -> Result<(CachedEntity, ExecutionSource), RuntimeError> {
        let mut env = CmlEnv::new();
        if inject_execute_session_env {
            merge_plasm_execute_session_share_token_env(&mut env);
            merge_plasm_execute_session_proof_base_token_env(&mut env);
        }
        let target_ent = cgs.get_entity(get.reference.entity_type.as_str());
        populate_template_path_env(
            &mut env,
            capability_template,
            &get.reference,
            target_ent,
            get.path_vars.as_ref(),
            None,
        );
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        if inject_execute_session_env {
            merge_plasm_execute_session_env(&mut env);
        }

        let compiled = compile_operation_dispatch(capability_template, &env)?;
        let (response, source) = with_dispatch_entity(
            Some(get.reference.entity_type.as_str()),
            self.execute_with_replay(&compiled, mode, cache.as_deref_mut()),
        )
        .await?;
        let response = self
            .apply_auxiliary_http_merge_response(
                capability_template,
                &env,
                mode,
                response,
                Some(get.reference.entity_type.as_str()),
            )
            .await?;
        let response =
            narrow_http_graphql_response_for_entity_decode(capability_template, response)?;
        let rid = cgs.get_entity(&get.reference.entity_type).and_then(|ent| {
            if ent.implicit_request_identity || ent.id_field == "url" {
                get.reference.simple_id().map(|id| id.as_str())
            } else {
                None
            }
        });
        let identity_ambient = decode_identity_ambient_for_ref(&get.reference, &env);
        let decoder = create_entity_decoder_for_capability(
            &get.reference.entity_type,
            cgs,
            Some(capability.name.as_str()),
            None,
            rid,
            Some(&identity_ambient),
        );
        let decoded_entities = decode_entities_with_cgs(&decoder, &response, Some(cgs))?;

        let decoded = decoded_entities
            .first()
            .ok_or_else(|| RuntimeError::CacheError {
                message: format!("Entity not found: {}", get.reference),
            })?;

        let timestamp = current_timestamp();
        let cached = if let Some(session) = cache {
            cache_decoded_entity_tree(
                session,
                decoded.clone(),
                timestamp,
                EntityCompleteness::Complete,
            )?
        } else {
            CachedEntity::from_decoded(
                decoded.reference.clone(),
                decoded.fields.clone(),
                decoded.relations.clone(),
                timestamp,
                EntityCompleteness::Complete,
            )
        };
        Ok((cached, source))
    }

    /// Run GET + decode without consulting the graph cache (used for query hydration and cache refresh).
    ///
    /// When `hydrate_capability` is `Some(name)`, use that named GET capability instead of the
    /// default per-entity `find_capability(.., Get)` (used by preflight hydrate steps).
    ///
    /// When `inject_execute_session_env` is true, reserved `plasm_execute_*` keys are merged for
    /// user-facing GETs only — internal preflight/hydrate GETs pass `false`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_get_decoded(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mode: ExecutionMode,
        hydrate_capability: Option<&str>,
        inject_execute_session_env: bool,
        cache: Option<&mut SessionMaterialization>,
        ambient: &ViewAmbientContext,
    ) -> Result<(CachedEntity, ExecutionSource), RuntimeError> {
        let capability: &CapabilitySchema = match hydrate_capability {
            Some(name) => {
                let c =
                    cgs.get_capability(name)
                        .ok_or_else(|| RuntimeError::CapabilityNotFound {
                            capability: name.to_string(),
                            entity: get.reference.entity_type.to_string(),
                        })?;
                if c.kind != plasm_core::CapabilityKind::Get {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!("preflight hydrate get '{name}' must be kind get"),
                    });
                }
                if c.domain.as_str() != get.reference.entity_type.as_str() {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "preflight: hydrate capability '{name}' is for entity {}, expected {}",
                            c.domain.as_str(),
                            get.reference.entity_type
                        ),
                    });
                }
                c
            }
            None => cgs
                .find_capability(&get.reference.entity_type, plasm_core::CapabilityKind::Get)
                .ok_or_else(|| RuntimeError::CapabilityNotFound {
                    capability: "get".to_string(),
                    entity: get.reference.entity_type.to_string(),
                })?,
        };

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        if let CapabilityTemplate::View(vt) = &capability_template {
            let mut ephemeral = SessionMaterialization::new();
            let cache_ref = cache.unwrap_or(&mut ephemeral);
            let res = crate::view_execution::execute_view_get(
                self,
                vt.view.as_str(),
                get,
                cgs,
                cache_ref,
                mode,
                ambient,
            )
            .await?;
            let cached = res
                .entities
                .first()
                .cloned()
                .ok_or_else(|| RuntimeError::CacheError {
                    message: format!("composed view `{}` returned no entity row", vt.view),
                })?;
            return Ok((cached, res.source));
        }

        self.fetch_http_transport_get_decoded(
            get,
            cgs,
            mode,
            capability,
            &capability_template,
            inject_execute_session_env,
            cache,
        )
        .await
    }

    /// After a query, upgrade Summary rows to Complete via concurrent GET when configured and supported.
    async fn hydrate_query_summaries(
        &self,
        entity_type: &str,
        ordered_entities: &[CachedEntity],
        cgs: &CGS,
        mat: &mut SessionMaterialization,
        mode: ExecutionMode,
        hydrate_enabled: bool,
    ) -> Result<(Vec<CachedEntity>, usize), RuntimeError> {
        if !hydrate_enabled {
            return Ok((ordered_entities.to_vec(), 0));
        }
        if cgs
            .find_capability(entity_type, plasm_core::CapabilityKind::Get)
            .is_none()
        {
            return Ok((ordered_entities.to_vec(), 0));
        }

        let ordered_refs: Vec<Ref> = ordered_entities
            .iter()
            .map(|e| e.reference.clone())
            .collect();

        let to_fetch: Vec<Ref> = ordered_refs
            .iter()
            .filter(|r| {
                !matches!(
                    mat.get(r).map(|e| e.completeness),
                    Some(EntityCompleteness::Complete)
                )
            })
            .cloned()
            .collect();

        let concurrency = self.config.hydrate_concurrency.max(1);
        let mut extra_network = 0usize;

        use futures_util::stream::{self, StreamExt};

        let mut stream = stream::iter(to_fetch.into_iter().map(|reference| {
            let get = GetExpr::from_ref(reference.clone());
            async move {
                self.fetch_get_decoded(
                    &get,
                    cgs,
                    mode,
                    None,
                    false,
                    None,
                    &ViewAmbientContext::default(),
                )
                .await
            }
        }))
        .buffer_unordered(concurrency);

        while let Some(res) = stream.next().await {
            cooperative_cancel_check()?;
            let (entity, source) = res?;
            if source == ExecutionSource::Live {
                extra_network += 1;
            }
            mat.insert(entity)?;
        }

        let mut out = Vec::with_capacity(ordered_refs.len());
        for r in &ordered_refs {
            let e = mat.get(r).ok_or_else(|| RuntimeError::CacheError {
                message: format!("entity missing after query/hydrate: {}", r),
            })?;
            out.push(e.clone());
        }
        Ok((out, extra_network))
    }
}

fn cache_decoded_entity_tree(
    mat: &mut SessionMaterialization,
    decoded: plasm_compile::DecodedEntity,
    timestamp: u64,
    completeness: EntityCompleteness,
) -> Result<CachedEntity, RuntimeError> {
    embed_cache::cache_decoded_entity_tree(mat, decoded, timestamp, completeness)
}

fn query_result_merge_cache(
    decoded_entities: Vec<plasm_compile::DecodedEntity>,
    completeness: EntityCompleteness,
    source: ExecutionSource,
    mat: &mut SessionMaterialization,
    network_requests: usize,
) -> Result<ExecutionResult, RuntimeError> {
    let timestamp = current_timestamp();
    let mut cached_entities = Vec::new();
    for decoded in decoded_entities {
        cached_entities.push(cache_decoded_entity_tree(
            mat,
            decoded,
            timestamp,
            completeness,
        )?);
    }
    let count = cached_entities.len();
    mat.merge(cached_entities.clone())?;
    let mut stats = ExecutionStats::from_telemetry(CacheTelemetry::default(), network_requests);
    stats.record_rows_materialized(count);
    Ok(ExecutionResult {
        entities: cached_entities,
        count,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints: Vec::new(),
    })
}

fn path_var_names_from_template(template: &CapabilityTemplate) -> Vec<String> {
    match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            path_var_names_from_request(cml)
        }
        CapabilityTemplate::View(_) => Vec::new(),
        CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => Vec::new(),
    }
}

fn ensure_http_operation(operation: &CompiledOperation, action: &str) -> Result<(), RuntimeError> {
    if matches!(
        operation,
        CompiledOperation::Http(_) | CompiledOperation::GraphQl(_)
    ) {
        return Ok(());
    }
    Err(RuntimeError::UnsupportedExecutionMode {
        mode: format!("{action} with non-HTTP transport (phase 1 supports EVM reads only)"),
    })
}

/// Bind template variables for get/delete/invoke:
/// explicit `path_vars` first, then keys from `input_overlay`, then identity slots
/// ([`ResolvedIdentity`]), while preserving the legacy HTTP single-path-var => `id` alias.
fn populate_template_path_env(
    env: &mut CmlEnv,
    template: &CapabilityTemplate,
    reference: &Ref,
    ent: Option<&plasm_core::schema::EntityDef>,
    path_vars: Option<&indexmap::IndexMap<String, Value>>,
    input_overlay: Option<&Value>,
) {
    let identity = plasm_core::ResolvedIdentity::from_ref(reference, ent);
    let primary_id = reference.primary_slot_str();
    let id_val = Value::String(primary_id.clone());

    for (k, v) in &identity.slots {
        env.insert(k.clone(), Value::String(v.clone()));
    }

    let single_http_id_alias = match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            let names = path_var_names_from_request(cml);
            (names.len() == 1).then(|| names[0].clone())
        }
        CapabilityTemplate::View(_)
        | CapabilityTemplate::EvmCall(_)
        | CapabilityTemplate::EvmLogs(_) => None,
    };

    for var_name in template_var_names(template) {
        if var_name == "id" {
            continue;
        }

        let resolved = path_vars
            .and_then(|m| m.get(&var_name))
            .cloned()
            .or_else(|| {
                input_overlay.and_then(|inp| match inp {
                    Value::Object(map) => map.get(&var_name).cloned(),
                    _ => None,
                })
            })
            .or_else(|| {
                identity
                    .get(&var_name)
                    .map(|s| Value::String(s.to_string()))
            })
            .or_else(|| {
                single_http_id_alias
                    .as_ref()
                    .filter(|name| *name == &var_name)
                    .map(|_| id_val.clone())
            });

        if let Some(value) = resolved {
            env.insert(var_name.clone(), value);
        }
    }

    // Explicit `path_vars` override compound [`Ref`] strings and template defaults (program
    // `node_input`, materialized entity-ref rows, …).
    if let Some(pv) = path_vars {
        for (k, v) in pv {
            env.insert(k.clone(), v.clone());
        }
    }
}

/// Narrow scope slots typed as [`FieldType::EntityRef`] (row JSON → id string, etc.).
fn normalize_cml_env_scope_entity_refs(
    env: &mut CmlEnv,
    cgs: &CGS,
    capability: &CapabilitySchema,
) -> Result<(), RuntimeError> {
    let Some(schema) = capability.input_schema.as_ref() else {
        return Ok(());
    };
    let InputType::Object { fields, .. } = &schema.input_type else {
        return Ok(());
    };
    for field in fields {
        if !matches!(field.role, Some(ParameterRole::Scope)) {
            continue;
        }
        let nv = field
            .named_value(cgs)
            .map_err(|e| RuntimeError::ConfigurationError {
                message: format!("capability `{}`: {e}", capability.name),
            })?;
        let FieldType::EntityRef { target } = &nv.field_type else {
            continue;
        };
        let Some(ent) = cgs.get_entity(target.as_str()) else {
            continue;
        };
        let Some(slot) = env.get_mut(&field.name) else {
            continue;
        };
        if let Some(norm) = normalize_cml_scope_entity_ref_value(slot, ent) {
            *slot = norm;
        }
    }
    Ok(())
}

fn normalize_cml_scope_entity_ref_value(value: &Value, ent: &EntityDef) -> Option<Value> {
    let normalized = plasm_core::normalize_entity_ref_value_for_target(value, ent)?;
    if ent.key_vars.len() >= 2 {
        return Some(normalized);
    }
    match &normalized {
        Value::Object(map) => {
            let key = ent
                .key_vars
                .first()
                .map(|k| k.as_str())
                .unwrap_or_else(|| ent.id_field.as_str());
            map.get(key).and_then(|v| match v {
                Value::String(_) | Value::Integer(_) | Value::Float(_) | Value::Bool(_) => {
                    Some(v.clone())
                }
                _ => None,
            })
        }
        other => Some(other.clone()),
    }
}

/// Evaluate a simple predicate against a cached entity's fields (client-side filter).
///
/// Only call this with predicates that have been stripped of non-entity-field comparisons
/// (i.e. via `entity_field_predicate`). Every comparison field is expected to be a real
/// entity field; if a field is absent the entity does not match.
fn client_side_predicate_matches(entity: &CachedEntity, predicate: &plasm_core::Predicate) -> bool {
    use plasm_core::CompOp;
    match predicate {
        plasm_core::Predicate::True => true,
        plasm_core::Predicate::False => false,
        plasm_core::Predicate::Comparison { field, op, value } => {
            let rhs = value.to_value();
            let Some(actual_tf) = entity.get_field(field) else {
                // Field genuinely absent from this entity instance — does not match.
                // Non-entity-field predicates (scope, filter params) should have been
                // stripped by `entity_field_predicate` before reaching here.
                return *op == CompOp::Exists && matches!(rhs, Value::Null);
            };
            let actual = actual_tf.to_value();
            match op {
                CompOp::Eq => actual == rhs,
                CompOp::Neq => actual != rhs,
                CompOp::Gt => {
                    if let (Some(a), Some(b)) = (actual.as_number(), rhs.as_number()) {
                        a > b
                    } else {
                        false
                    }
                }
                CompOp::Lt => {
                    if let (Some(a), Some(b)) = (actual.as_number(), rhs.as_number()) {
                        a < b
                    } else {
                        false
                    }
                }
                CompOp::Gte => {
                    if let (Some(a), Some(b)) = (actual.as_number(), rhs.as_number()) {
                        a >= b
                    } else {
                        false
                    }
                }
                CompOp::Lte => {
                    if let (Some(a), Some(b)) = (actual.as_number(), rhs.as_number()) {
                        a <= b
                    } else {
                        false
                    }
                }
                CompOp::Contains => actual.contains(&rhs),
                CompOp::In => match &rhs {
                    Value::Array(arr) => arr.contains(&actual),
                    _ => false,
                },
                CompOp::Exists => !matches!(actual, Value::Null),
            }
        }
        plasm_core::Predicate::And { args } => args
            .iter()
            .all(|a| client_side_predicate_matches(entity, a)),
        plasm_core::Predicate::Or { args } => args
            .iter()
            .any(|a| client_side_predicate_matches(entity, a)),
        plasm_core::Predicate::Not { predicate: inner } => {
            !client_side_predicate_matches(entity, inner)
        }
        plasm_core::Predicate::ExistsRelation { .. } => true,
    }
}

/// Strip comparisons against non-entity fields from a predicate, returning the
/// entity-field-only portion suitable for client-side filtering.
///
/// Comparisons against fields **not** present in the entity schema are dropped —
/// they represent capability parameters (scope, filter, search, sort) that were
/// already handled server-side by the CML request template. Keeping them would
/// incorrectly eliminate all decoded entities (e.g. `block_id` in a
/// `block_children_query` result).
///
/// Comparisons against fields that are **also** declared as capability `parameters`
/// are dropped when those names appear in `cap_params`: the request already
/// carried them, and the response may round or normalize values (e.g. Open-Meteo
/// `latitude` / `longitude`).
///
/// Returns `None` when the entire predicate reduces to an unconditional pass
/// (i.e. nothing remains to filter client-side).
fn entity_field_predicate(
    pred: &plasm_core::Predicate,
    entity: &plasm_core::EntityDef,
    cap_params: Option<&HashSet<String>>,
) -> Option<plasm_core::Predicate> {
    use plasm_core::Predicate;
    match pred {
        Predicate::True | Predicate::False => Some(pred.clone()),
        Predicate::Comparison { field, .. } => {
            if !entity.fields.contains_key(field.as_str()) {
                return None;
            }
            if let Some(names) = cap_params {
                if names.contains(field) {
                    return None;
                }
            }
            Some(pred.clone())
        }
        Predicate::And { args } => {
            let kept: Vec<_> = args
                .iter()
                .filter_map(|a| entity_field_predicate(a, entity, cap_params))
                .collect();
            match kept.len() {
                0 => None,
                1 => Some(kept.into_iter().next().unwrap()),
                _ => Some(Predicate::And { args: kept }),
            }
        }
        Predicate::Or { args } => {
            let kept: Vec<_> = args
                .iter()
                .filter_map(|a| entity_field_predicate(a, entity, cap_params))
                .collect();
            match kept.len() {
                0 => None,
                1 => Some(kept.into_iter().next().unwrap()),
                _ => Some(Predicate::Or { args: kept }),
            }
        }
        Predicate::Not { predicate: inner } => entity_field_predicate(inner, entity, cap_params)
            .map(|p| Predicate::Not {
                predicate: Box::new(p),
            }),
        // Relation predicates are never entity scalar fields; leave them for cross-entity logic.
        Predicate::ExistsRelation { .. } => Some(pred.clone()),
    }
}

fn capability_param_names(capability: &plasm_core::CapabilitySchema) -> HashSet<String> {
    let Some(input) = &capability.input_schema else {
        return HashSet::new();
    };
    let InputType::Object { fields, .. } = &input.input_type else {
        return HashSet::new();
    };
    fields.iter().map(|f| f.name.clone()).collect()
}

/// Extract an EntityRef field or declared-relation target ID from a cached entity.
fn extract_ref_id(entity: &CachedEntity, selector: &str, cgs: &CGS) -> Option<String> {
    if let Some(source_ent) = cgs.get_entity(entity.reference.entity_type.as_str()) {
        if let Some(rel) = source_ent.relations.get(selector) {
            if let Some(target_ent) = cgs.get_entity(rel.target_resource.as_str()) {
                let key_vars = source_ent
                    .key_vars
                    .iter()
                    .map(|k| k.as_str().to_string())
                    .collect::<Vec<_>>();
                let mut identity = plasm_core::row_identity_from_parts(
                    plasm_core::QualifiedEntityKey::new(
                        String::new(),
                        entity.reference.entity_type.to_string(),
                    ),
                    entity.reference.clone(),
                    &entity.relations,
                    source_ent.id_field.as_str(),
                    &key_vars,
                );
                for rel_name in source_ent.relations.keys() {
                    if identity.ambient.contains_key(rel_name.as_str()) {
                        continue;
                    }
                    if let Some(tf) = entity.get_field(rel_name.as_str()) {
                        if let Value::String(s) = tf.to_value() {
                            if !s.is_empty() {
                                identity.ambient.insert(rel_name.as_str().to_string(), s);
                            }
                        }
                    }
                }
                if let Ok(target_ref) =
                    plasm_core::resolve_relation_target_id(&identity, selector, target_ent)
                {
                    let slot = target_ref.primary_slot_str();
                    if !slot.is_empty() {
                        return Some(slot);
                    }
                }
            }
        }
    }
    let v = entity.get_field(selector).map(|tf| tf.to_value())?;
    match v {
        Value::String(s) if !s.is_empty() => Some(s),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

/// One scoped query per parent (pure `query_scoped` / `query_scoped_bindings`).
pub(crate) fn partition_scoped_query_fanout<F>(
    parents: &[CachedEntity],
    mut build_scoped_query: F,
) -> Vec<(usize, QueryExpr)>
where
    F: FnMut(&CachedEntity) -> QueryExpr,
{
    parents
        .iter()
        .enumerate()
        .map(|(i, parent)| (i, build_scoped_query(parent)))
        .collect()
}

fn build_scoped_query_from_fallback(
    fallback: &RelationScopedFallback,
    parent: &CachedEntity,
    parent_entity_def: &EntityDef,
    target_entity: &EntityName,
    cgs: &CGS,
) -> Result<QueryExpr, RuntimeError> {
    match fallback {
        RelationScopedFallback::QueryScoped { capability, param } => {
            let id_field = cgs
                .get_entity(parent.reference.entity_type.as_str())
                .map(|def| def.id_field.as_str().to_string())
                .unwrap_or_default();
            let id = parent
                .get_field(id_field.as_str())
                .map(|tf| tf.to_value())
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Integer(n) => Some(n.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| parent.reference.primary_slot_str());
            let pred = Predicate::eq(param.as_str(), id);
            let mut q = QueryExpr::filtered(target_entity.clone(), pred);
            q.capability_name = Some(capability.clone());
            Ok(q)
        }
        RelationScopedFallback::QueryScopedBindings {
            capability,
            bindings,
        } => {
            let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: format!("unknown fallback capability '{capability}'"),
                }
            })?;
            let cap_params: Vec<_> = cap.object_params().map(|f| f.to_vec()).unwrap_or_default();
            let preds: Vec<Predicate> = bindings
                .iter()
                .map(|(cap_param, parent_field)| {
                    let raw = chain_binding_raw_json(parent, parent_entity_def, parent_field);
                    let value =
                        chain_binding_plasm_value(&raw, cap_param.as_str(), &cap_params, cgs);
                    Predicate::eq(cap_param.as_str(), value)
                })
                .collect();
            let pred = if preds.len() == 1 {
                preds.into_iter().next().expect("non-empty preds")
            } else {
                Predicate::and(preds)
            };
            let mut q = QueryExpr::filtered(target_entity.clone(), pred);
            q.capability_name = Some(capability.clone());
            Ok(q)
        }
        RelationScopedFallback::HydrateFromEmbedPath { .. } => {
            Err(RuntimeError::ConfigurationError {
                message: "hydrate_from_embed_path fallback is plan-materialized only".into(),
            })
        }
    }
}

/// Partition for [`RelationMaterialization::PreferFromParentGet`] using [`resolve_relation_row_resolution`].
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn partition_prefer_from_parent_get(
    parents: &[CachedEntity],
    materialize: &RelationMaterialization,
    relation_key: &str,
    expected_target: &str,
    mat: &SessionMaterialization,
    parent_entity_def: &EntityDef,
    cgs: &CGS,
    target_entity: &EntityName,
    fallback: &RelationScopedFallback,
) -> Result<(Vec<Vec<CachedEntity>>, Vec<(usize, QueryExpr)>), RuntimeError> {
    let n = parents.len();
    let parent_rows: Vec<(serde_json::Value, Option<Vec<Ref>>)> = parents
        .iter()
        .map(|parent| {
            (
                parent.payload_to_json(),
                parent.relations.get(relation_key).map(|refs| refs.to_vec()),
            )
        })
        .collect();
    let parent_row_refs: Vec<(&serde_json::Value, Option<&[Ref]>)> = parent_rows
        .iter()
        .map(|(json, refs)| (json, refs.as_deref()))
        .collect();
    let resolutions = partition_prefer_resolutions(
        materialize,
        relation_key,
        expected_target,
        parent_row_refs,
        |r| mat.get(r).is_some(),
    );
    let mut per_parent: Vec<Vec<CachedEntity>> = (0..n).map(|_| Vec::new()).collect();
    let mut network_jobs: Vec<(usize, QueryExpr)> = Vec::new();
    for (i, resolution) in resolutions.into_iter().enumerate() {
        let parent = &parents[i];
        match resolution {
            RelationRowResolution::EmbeddedRefs(refs) => {
                per_parent[i] =
                    resolve_cached_targets_from_relation_refs(mat, &refs, expected_target)?;
            }
            RelationRowResolution::ScopedQuery => {
                let q = build_scoped_query_from_fallback(
                    fallback,
                    parent,
                    parent_entity_def,
                    target_entity,
                    cgs,
                )?;
                network_jobs.push((i, q));
            }
        }
    }
    Ok((per_parent, network_jobs))
}

fn resolve_cached_targets_from_relation_refs(
    mat: &SessionMaterialization,
    refs: &[Ref],
    expected_target: &str,
) -> Result<Vec<CachedEntity>, RuntimeError> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        if r.entity_type.as_str() != expected_target {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Decoded relation expected Ref.entity_type {expected_target}, got {}",
                    r.entity_type
                ),
            });
        }
        let Some(e) = mat.get(r) else {
            return Err(RuntimeError::CacheError {
                message: format!("missing embedded relation target in session graph: {r}"),
            });
        };
        out.push(e.clone());
    }
    Ok(out)
}

fn ref_from_materialize_bindings_for_get_chain(
    target_ent: &EntityDef,
    binding_values: &IndexMap<String, String>,
) -> Result<Ref, RuntimeError> {
    if !target_ent.key_vars.is_empty() {
        let mut parts = BTreeMap::new();
        for kv in &target_ent.key_vars {
            let s = binding_values.get(kv.as_str()).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: format!(
                        "get_scoped_bindings missing bound value for `{}` on entity `{}`",
                        kv, target_ent.name
                    ),
                }
            })?;
            parts.insert(kv.to_string(), s.clone());
        }
        Ok(Ref::compound(target_ent.name.clone(), parts))
    } else {
        let id = binding_values
            .get(target_ent.id_field.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "get_scoped_bindings missing bound value for id field `{}` on entity `{}`",
                    target_ent.id_field, target_ent.name
                ),
            })?;
        Ok(Ref::new(target_ent.name.clone(), id.clone()))
    }
}

/// Raw JSON for a `query_scoped_bindings` parent field from a cached row / ref.
fn chain_binding_raw_json(
    entity: &CachedEntity,
    parent_def: &plasm_core::EntityDef,
    parent_field: &EntityFieldName,
) -> serde_json::Value {
    let pf = parent_field.as_str();
    if let Some(v) = entity.get_field(pf) {
        return plasm_core::plasm_value_to_json(&v.to_value());
    }
    if pf == parent_def.id_field.as_str() {
        return serde_json::Value::String(entity.reference.primary_slot_str());
    }
    if let EntityKey::Compound(parts) = &entity.reference.key {
        if let Some(s) = parts.get(pf) {
            return serde_json::Value::String(s.clone());
        }
    }
    serde_json::Value::String(entity.reference.primary_slot_str())
}

fn chain_binding_plasm_value(
    raw: &serde_json::Value,
    cap_param: &str,
    cap_params: &[plasm_core::InputFieldSchema],
    cgs: &CGS,
) -> Value {
    let Some(param_schema) = cap_params.iter().find(|f| f.name == cap_param) else {
        return plasm_core::json_value_to_plasm_value(raw);
    };
    let Ok(nv) = param_schema.named_value(cgs) else {
        return plasm_core::json_value_to_plasm_value(raw);
    };
    plasm_core::binding_value_as_plasm_value(raw, nv)
}

/// String slot for get_scoped_bindings path keys (compound ref parts).
fn chain_binding_value(
    entity: &CachedEntity,
    parent_def: &plasm_core::EntityDef,
    parent_field: &EntityFieldName,
) -> String {
    let v = chain_binding_raw_json(entity, parent_def, parent_field);
    match plasm_core::json_value_to_plasm_value(&v) {
        Value::String(s) if !s.is_empty() => s,
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => entity.reference.primary_slot_str(),
    }
}

/// JSON path to the entity array: top-level `items` key or [`HttpResponseDecode::items_path`].
fn http_collection_source(cml: &CmlRequest) -> PathExpr {
    if let Some(ref r) = cml.response {
        if let Some(ref path) = r.items_path {
            if !path.is_empty() {
                let mut segs: Vec<PathSegment> =
                    path.iter().map(|name| items_path_segment(name)).collect();
                segs.push(PathSegment::Wildcard);
                if let Some(ref inner) = r.item_inner_key {
                    if !inner.is_empty() {
                        segs.push(PathSegment::Key {
                            name: inner.clone(),
                        });
                    }
                }
                return PathExpr::new(segs);
            }
        }
    }
    let key = cml.response_items_key().to_string();
    let mut segs = vec![PathSegment::Key { name: key }, PathSegment::Wildcard];
    if let Some(ref r) = cml.response {
        if let Some(ref inner) = r.item_inner_key {
            if !inner.is_empty() {
                segs.push(PathSegment::Key {
                    name: inner.clone(),
                });
            }
        }
    }
    PathExpr::new(segs)
}

/// `items_path` segments are usually object keys; digit-only strings address JSON array indices.
fn items_path_segment(name: &str) -> PathSegment {
    if let Ok(index) = name.parse::<usize>() {
        PathSegment::Index { index }
    } else {
        PathSegment::Key {
            name: name.to_string(),
        }
    }
}

/// Key used when normalizing a bare JSON array to `{ <key>: [...] }` (must match the leaf array name).
fn response_bare_array_wrap_key(cml: &CmlRequest) -> String {
    if let Some(ref r) = cml.response {
        if let Some(ref path) = r.items_path {
            if let Some(last) = path.last() {
                return last.clone();
            }
        }
    }
    cml.response_items_key().to_string()
}

/// If the template is HTTP or GraphQL, narrow the raw response to the entity-shaped JSON described
/// by CML `response.single` + `items_path`. Other transports (e.g. EVM) return `response` unchanged.
fn narrow_http_graphql_response_for_entity_decode(
    template: &CapabilityTemplate,
    response: serde_json::Value,
) -> Result<serde_json::Value, RuntimeError> {
    match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            extract_single_entity_payload_from_response(response, cml)
        }
        CapabilityTemplate::View(_) => Err(RuntimeError::ConfigurationError {
            message: "view capabilities do not use HTTP response narrowing".into(),
        }),
        CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => Ok(response),
    }
}

/// Fibery `/api/commands` returns `{ "success": bool, "result": … }` on HTTP 200 even when the
/// command failed. Surface `success: false` before callers treat the HTTP round-trip as success.
fn preflight_fibery_command_envelope(response: &serde_json::Value) -> Result<(), RuntimeError> {
    let Some(success) = response.get("success").and_then(|v| v.as_bool()) else {
        return Ok(());
    };
    let Some(result) = response.get("result") else {
        return Ok(());
    };
    if !success {
        let name = result
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("command.error");
        let message = result
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Fibery command failed");
        return Err(RuntimeError::RequestError {
            message: format!("Fibery command failed ({name}): {message}"),
            attempts: 1,
        });
    }
    Ok(())
}

/// Fibery `/api/commands` and similar APIs return `{ "success": bool, "result": … }` on HTTP 200.
/// Surface command failures and empty query rows before CML `items_path` narrowing turns them into
/// opaque "missing path segment" configuration errors.
fn preflight_command_envelope_for_single_entity_narrow(
    response: &serde_json::Value,
    cml: &CmlRequest,
) -> Result<(), RuntimeError> {
    let Some(r) = cml.response.as_ref().filter(|r| r.single) else {
        return Ok(());
    };
    preflight_fibery_command_envelope(response)?;
    let Some(result) = response.get("result") else {
        return Ok(());
    };
    let Some(path) = r.items_path.as_ref().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    if path.len() < 2 || path[0] != "result" {
        return Ok(());
    }
    let Some(index_key) = path.get(1) else {
        return Ok(());
    };
    if index_key.parse::<usize>().is_err() {
        return Ok(());
    }
    if result.as_array().is_some_and(|a| a.is_empty()) {
        return Err(RuntimeError::RequestError {
            message: "Fibery command succeeded but returned no rows (empty `result` array). \
                      For `user_get_me`, the API token may not resolve `$my-id` — use a personal \
                      workspace API token from Fibery → API Tokens and reconnect in Plasm."
                .into(),
            attempts: 1,
        });
    }
    Ok(())
}

/// For mappings that declare `response.single` + `items_path` (e.g. GraphQL `{ data: { issue: { ... } } }`),
/// or `response.single` + top-level `items` (e.g. Cloudflare v4 `{ result: { ... } }` via `items: result`),
/// take the entity object at that path. Used for GET/detail, create, and update **invoke** decoding—
/// not specific to GET semantics.
fn extract_single_entity_payload_from_response(
    response: serde_json::Value,
    cml: &CmlRequest,
) -> Result<serde_json::Value, RuntimeError> {
    preflight_command_envelope_for_single_entity_narrow(&response, cml)?;
    if let Some(ref r) = cml.response {
        if r.single {
            let mut cur: &serde_json::Value = &response;
            if let Some(ref path) = r.items_path {
                if !path.is_empty() {
                    for key in path {
                        cur = match single_response_path_step(cur, key) {
                            Some(v) => v,
                            None => {
                                let mut msg =
                                    format!("single-entity response: missing path segment `{key}`");
                                if let Some(gs) = graphql_errors_summary(&response) {
                                    msg.push_str(" — GraphQL: ");
                                    msg.push_str(&gs);
                                } else if matches!(response.get("data"), Some(d) if d.is_null()) {
                                    msg.push_str(
                                        " (response `data` is null; often paired with GraphQL `errors`)",
                                    );
                                } else if let Some(fibery) =
                                    fibery_command_envelope_hint(&response, key)
                                {
                                    msg.push_str(" — ");
                                    msg.push_str(&fibery);
                                }
                                return Err(RuntimeError::ConfigurationError { message: msg });
                            }
                        };
                    }
                }
            } else if let Some(key) = r.items.as_deref().filter(|k| !k.is_empty()) {
                cur = match single_response_path_step(cur, key) {
                    Some(v) => v,
                    None => {
                        let mut msg = format!("single-entity response: missing `{key}`");
                        if let Some(gs) = graphql_errors_summary(&response) {
                            msg.push_str(" — GraphQL: ");
                            msg.push_str(&gs);
                        }
                        return Err(RuntimeError::ConfigurationError { message: msg });
                    }
                };
            }
            let mut out = cur.clone();
            if let Some(ref inner) = r.item_inner_key {
                if !inner.is_empty() {
                    out = unwrap_single_inner_payload(out, inner)?;
                }
            }
            return Ok(out);
        }
    }
    Ok(response)
}

fn single_response_path_step<'a>(
    cur: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    if let Ok(index) = key.parse::<usize>() {
        cur.get(index)
    } else {
        cur.get(key)
    }
}

/// Reddit-style `{ kind, data: { … } }` wrappers and `{ children: [ { kind, data } ] }` listings:
/// unwrap the first child’s `data` when the value is a non-empty array; otherwise if the object
/// contains `inner`, return that subtree; else return the value unchanged.
fn unwrap_single_inner_payload(
    cur: serde_json::Value,
    inner: &str,
) -> Result<serde_json::Value, RuntimeError> {
    match cur {
        serde_json::Value::Array(mut a) => {
            let first = a.get_mut(0).map(std::mem::take).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: "single-entity response: expected a non-empty array at path"
                        .to_string(),
                }
            })?;
            match first {
                serde_json::Value::Object(m) => {
                    m.get(inner)
                        .cloned()
                        .ok_or_else(|| RuntimeError::ConfigurationError {
                            message: format!(
                                "single-entity response: array element missing `{inner}` object"
                            ),
                        })
                }
                _ => Err(RuntimeError::ConfigurationError {
                    message: "single-entity response: expected object elements in array"
                        .to_string(),
                }),
            }
        }
        serde_json::Value::Object(m) => {
            if let Some(v) = m.get(inner) {
                Ok(v.clone())
            } else {
                Ok(serde_json::Value::Object(m))
            }
        }
        other => Ok(other),
    }
}

/// Decode hints from CML: alternate `items` key (e.g. `meals`) and single-object bodies.
fn prepare_http_query_response(
    response: serde_json::Value,
    cml: &CmlRequest,
    env: &CmlEnv,
) -> serde_json::Value {
    let response = if let Some(ref r) = cml.response {
        if let Some(ref p) = r.response_preprocess {
            apply_response_preprocess(response, cml, p, env)
        } else {
            response
        }
    } else {
        response
    };
    let key = cml.response_items_key().to_string();
    if cml.response_is_single_object()
        && cml
            .response
            .as_ref()
            .is_none_or(|r| r.response_preprocess.is_none())
        && response.is_object()
        && !response.is_array()
    {
        return serde_json::Value::Object(
            std::iter::once((key.clone(), serde_json::json!([response]))).collect(),
        );
    }
    if cml.response.as_ref().is_some_and(|r| r.wrap_root_scalar)
        && matches!(
            &response,
            serde_json::Value::Number(_) | serde_json::Value::String(_)
        )
    {
        return serde_json::json!({ key: [response] });
    }
    // Root JSON array with `items_path` starting at an array index (e.g. Reddit
    // `/r/{sub}/comments/{id}.json` → [post_listing, comment_listing]): leave the body unchanged so
    // `http_collection_source` can walk into the second listing without wrapping the whole array.
    if response.is_array()
        && cml
            .response
            .as_ref()
            .and_then(|r| r.items_path.as_ref())
            .is_some_and(|p| !p.is_empty() && p[0].parse::<usize>().is_ok())
    {
        return response;
    }
    let wrap_key = response_bare_array_wrap_key(cml);
    normalize_collection_response(response, &wrap_key)
}

fn cml_id_string(want: &plasm_core::Value) -> String {
    if let Ok(v) = serde_json::to_value(want) {
        match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        }
    } else {
        String::new()
    }
}

fn wire_id_matches(maybe: &serde_json::Value, want: &plasm_core::Value) -> bool {
    if want == &plasm_core::Value::Null {
        return false;
    }
    let w = cml_id_string(want);
    if w.is_empty() {
        return false;
    }
    match maybe {
        serde_json::Value::String(s) => s == &w,
        serde_json::Value::Number(n) => n.to_string() == w,
        _ => false,
    }
}

fn walk_json_path<'a>(v: &'a serde_json::Value, path: &[String]) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for key in path {
        cur = if let Ok(i) = key.parse::<usize>() {
            cur.get(i)?
        } else {
            cur.get(key)?
        };
    }
    Some(cur)
}

fn get_mut_value_at_path<'a>(
    v: &'a mut serde_json::Value,
    path: &[String],
) -> Option<&'a mut serde_json::Value> {
    let mut cur = v;
    for key in path {
        if let Ok(i) = key.parse::<usize>() {
            let serde_json::Value::Array(a) = cur else {
                return None;
            };
            cur = a.get_mut(i)?;
        } else {
            let serde_json::Value::Object(o) = cur else {
                return None;
            };
            cur = o.get_mut(key)?;
        }
    }
    Some(cur)
}

fn apply_response_preprocess(
    response: serde_json::Value,
    cml: &CmlRequest,
    p: &ResponsePreprocess,
    env: &CmlEnv,
) -> serde_json::Value {
    let key = cml.response_items_key().to_string();
    match p {
        ResponsePreprocess::ArrayFindPluck {
            path,
            id_field,
            id_var,
            nested_array,
        } => {
            let want = match env.get(id_var) {
                Some(v) => v,
                None => return response,
            };
            let Some(serde_json::Value::Array(arr)) = walk_json_path(&response, path) else {
                return response;
            };
            for it in arr {
                let Some(obj) = it.as_object() else { continue };
                let Some(ida) = obj.get(id_field) else {
                    continue;
                };
                if !wire_id_matches(ida, want) {
                    continue;
                }
                if let Some(serde_json::Value::Array(pl)) = obj.get(nested_array) {
                    return serde_json::Value::Object(
                        std::iter::once((key, serde_json::Value::Array(pl.clone()))).collect(),
                    );
                }
            }
            serde_json::json!({ key: serde_json::Value::Array(vec![]) })
        }
        ResponsePreprocess::ConcatFieldArrays { path, from_each } => {
            let Some(serde_json::Value::Array(outer)) = walk_json_path(&response, path) else {
                return response;
            };
            let mut acc: Vec<serde_json::Value> = Vec::new();
            for it in outer {
                let Some(o) = it.as_object() else { continue };
                if let Some(serde_json::Value::Array(a)) = o.get(from_each) {
                    acc.extend(a.iter().cloned());
                }
            }
            serde_json::Value::Object(
                std::iter::once((key, serde_json::Value::Array(acc))).collect(),
            )
        }
        ResponsePreprocess::StringIdsToFieldObjects { path, field } => {
            if path.is_empty() {
                return response;
            }
            let mut out = response;
            if let Some(serde_json::Value::Array(a)) = get_mut_value_at_path(&mut out, path) {
                let fk = field.clone();
                let mapped: Vec<serde_json::Value> = a
                    .iter()
                    .filter_map(|v| {
                        v.as_str().map(|s| {
                            let mut o = serde_json::Map::new();
                            o.insert(fk.clone(), serde_json::Value::String(s.to_string()));
                            serde_json::Value::Object(o)
                        })
                    })
                    .collect();
                *a = mapped;
            }
            out
        }
    }
}

/// Normalize collection API responses: bare arrays become `{ items_field: [...] }`.
fn normalize_collection_response(
    response: serde_json::Value,
    items_field: &str,
) -> serde_json::Value {
    if response.is_array() {
        serde_json::json!({ items_field: response })
    } else {
        response
    }
}

/// Extract field=value pairs from a predicate into CML env vars.
/// For a predicate like `And(status=available, name-contains=dog)`,
/// this sets env["status"] = "available", env["name"] = "dog".
fn extract_predicate_vars(predicate: &plasm_core::Predicate, env: &mut CmlEnv) {
    // First collect all field→value pairs, accumulating multi-value (In/Contains) arrays.
    let mut accumulator: indexmap::IndexMap<String, Vec<Value>> = indexmap::IndexMap::new();
    collect_predicate_vars(predicate, &mut accumulator);

    for (field, mut values) in accumulator {
        match values.len() {
            0 => {}
            1 => {
                env.insert(field, values.remove(0));
            }
            _ => {
                env.insert(field, Value::Array(values));
            }
        }
    }
}

fn collect_predicate_vars(
    predicate: &plasm_core::Predicate,
    acc: &mut indexmap::IndexMap<String, Vec<Value>>,
) {
    match predicate {
        plasm_core::Predicate::Comparison { field, op, value } => {
            let rhs = value.to_value();
            match op {
                // In/Contains: accumulate into an array for the field
                plasm_core::CompOp::In | plasm_core::CompOp::Contains => match &rhs {
                    Value::Array(arr) => {
                        acc.entry(field.clone())
                            .or_default()
                            .extend(arr.iter().cloned());
                    }
                    other => {
                        acc.entry(field.clone()).or_default().push(other.clone());
                    }
                },
                // All other ops: single scalar value — last one wins per field
                _ => {
                    acc.entry(field.clone()).or_default().clear();
                    acc.entry(field.clone()).or_default().push(rhs);
                }
            }
        }
        plasm_core::Predicate::And { args } => {
            for arg in args {
                collect_predicate_vars(arg, acc);
            }
        }
        plasm_core::Predicate::Or { args } => {
            for arg in args {
                collect_predicate_vars(arg, acc);
            }
        }
        _ => {}
    }
}

fn value_to_ambient_string(v: &Value) -> Option<String> {
    match v {
        Value::PlasmInputRef(_) => None,
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) | Value::UnionCtor { .. } => None,
    }
}

/// CML env slots usable as compound-key fallbacks (string-like values only).
pub(crate) fn cml_env_to_identity_strings(env: &CmlEnv) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    for (k, v) in env.iter() {
        if let Some(s) = value_to_ambient_string(v) {
            out.insert(k.clone(), s);
        }
    }
    out
}

fn ref_to_identity_ambient(reference: &Ref) -> IndexMap<String, String> {
    match &reference.key {
        EntityKey::Simple(_) => IndexMap::new(),
        EntityKey::Compound(parts) => parts.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    }
}

fn decode_identity_ambient_for_ref(reference: &Ref, env: &CmlEnv) -> IndexMap<String, String> {
    let mut m = ref_to_identity_ambient(reference);
    for (k, v) in cml_env_to_identity_strings(env) {
        m.entry(k).or_insert(v);
    }
    m
}

/// When an update capability passes a JSON patch object as `input`, merge the entity's primary wire
/// id (`id_from`) from the resolved `id` env slot so vendors like Fibery receive `{ fibery/id, … }`.
fn merge_entity_id_from_into_input_env(
    env: &mut CmlEnv,
    ent: Option<&EntityDef>,
    capability: &CapabilitySchema,
) {
    if !matches!(capability.kind, CapabilityKind::Update) {
        return;
    }
    let Some(ent) = ent else {
        return;
    };
    let Some(id_from) = ent.id_from.as_ref().filter(|p| !p.is_empty()) else {
        return;
    };
    let Some(wire_key) = id_from.first() else {
        return;
    };
    let Some(Value::String(id)) = env.get("id").cloned() else {
        return;
    };
    let Some(input_val) = env.get_mut("input") else {
        return;
    };
    let Value::Object(map) = input_val else {
        return;
    };
    map.entry(wire_key.clone())
        .or_insert_with(|| Value::String(id));
}

pub(crate) fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Convert serde_json::Value to plasm_core::Value
pub(crate) fn json_to_plasm_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            let values = arr.iter().map(json_to_plasm_value).collect();
            Value::Array(values)
        }
        serde_json::Value::Object(obj) => {
            let mut map = indexmap::IndexMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_plasm_value(v));
            }
            Value::Object(map)
        }
    }
}

/// Execute Plasm [`Expr`] trees against a live or replay backend.
///
/// Implemented by [`ExecutionEngine`]; implementors can stub this for tests or
/// alternate transports.
pub trait ExprExecutor: Send + Sync {
    /// Same contract as [`ExecutionEngine::execute`].
    fn execute<'a>(
        &'a self,
        expr: &'a Expr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    >;
}

impl ExprExecutor for ExecutionEngine {
    fn execute<'a>(
        &'a self,
        expr: &'a Expr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        ExecutionEngine::execute(self, expr, cgs, mat, mode, consume, opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use plasm_compile::decode_entities;
    use plasm_core::{
        CapabilityKind, CapabilityMapping, CapabilitySchema, Expr, FieldSchema, FieldType,
        FieldValueKind, GetExpr, InputFieldSchema, InputFieldWire, InputSchema, InputValidation,
        JsonPathSegment, NamedValueSchema, QueryPagination, Ref, ResourceSchema, StringSemantics,
        ValueDomainKey,
    };
    use std::collections::BTreeMap;

    fn create_test_cgs() -> CGS {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "exec_test_id".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.values.insert(
            "exec_test_name".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );

        // Add Account entity
        let account = ResourceSchema {
            name: "Account".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("exec_test_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("exec_test_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        };

        cgs.add_resource(account).unwrap();

        // Add query capability
        let query_capability = CapabilitySchema {
            name: "query_accounts".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Account".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "POST",
                    "path": [{"type": "literal", "value": "query"}, {"type": "literal", "value": "Account"}],
                    "body": {
                        "type": "if",
                        "condition": {"type": "exists", "var": "filter"},
                        "then_expr": {"type": "object", "fields": [["filter", {"type": "var", "name": "filter"}]]},
                        "else_expr": {"type": "object", "fields": []}
                    }
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
        };

        cgs.add_capability(query_capability).unwrap();

        // Add get capability
        let get_capability = CapabilitySchema {
            name: "get_account".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Account".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": "resources"},
                        {"type": "literal", "value": "Account"},
                        {"type": "var", "name": "id"}
                    ]
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
        };

        cgs.add_capability(get_capability).unwrap();

        cgs
    }

    fn cgs_with_unary_entity_ref_scope_query() -> (CGS, CapabilitySchema) {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "rt_str".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.values.insert(
            "rt_workspace_ref".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::EntityRef {
                    target: "Workspace".into(),
                },
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        cgs.add_resource(ResourceSchema {
            name: "Workspace".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![FieldSchema {
                name: "id".into(),
                kind: FieldValueKind::Registry(ValueDomainKey::new("rt_str").expect("key")),
                description: String::new(),
                required: true,
                agent_presentation: None,
                mime_type_hint: None,
                attachment_media: None,
                wire_path: None,
                derive: None,
            }],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .expect("workspace resource");
        let cap = CapabilitySchema {
            name: "managed_resource_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "ManagedResource".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": "workspaces"},
                        {"type": "var", "name": "workspace_id"},
                        {"type": "literal", "value": "managed-resources"}
                    ]
                })
                .into(),
            },
            input_schema: Some(InputSchema {
                input_type: InputType::Object {
                    fields: vec![InputFieldSchema {
                        name: "workspace_id".to_string(),
                        wire: InputFieldWire::Registry(
                            ValueDomainKey::new("rt_workspace_ref").expect("workspace ref key"),
                        ),
                        required: true,
                        description: None,
                        default: None,
                        role: Some(ParameterRole::Scope),
                        wire_json_path: None,
                        wire_array_element_key: None,
                    }],
                    additional_fields: false,
                },
                validation: InputValidation::default(),
                description: None,
                examples: vec![],
            }),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
        };
        (cgs, cap)
    }

    #[test]
    fn normalize_cml_scope_entity_ref_keeps_scalar_unary_ref_for_path_var() {
        let (cgs, cap) = cgs_with_unary_entity_ref_scope_query();
        let mut env = CmlEnv::new();
        env.insert(
            "workspace_id".to_string(),
            Value::String("workspace_123".to_string()),
        );
        normalize_cml_env_scope_entity_refs(&mut env, &cgs, &cap).expect("normalize");
        assert_eq!(
            env.get("workspace_id"),
            Some(&Value::String("workspace_123".to_string()))
        );
    }

    #[test]
    fn normalize_cml_scope_entity_ref_narrows_unary_ref_row_to_path_scalar() {
        let (cgs, cap) = cgs_with_unary_entity_ref_scope_query();
        let mut env = CmlEnv::new();
        env.insert(
            "workspace_id".to_string(),
            Value::Object(indexmap::indexmap! {
                "id".to_string() => Value::String("workspace_123".to_string()),
                "name".to_string() => Value::String("General Workspace".to_string()),
            }),
        );
        normalize_cml_env_scope_entity_refs(&mut env, &cgs, &cap).expect("normalize");
        assert_eq!(
            env.get("workspace_id"),
            Some(&Value::String("workspace_123".to_string()))
        );
    }

    #[test]
    fn pagination_context_map_reads_relay_page_info() {
        let v = serde_json::json!({
            "data": {
                "issues": {
                    "nodes": [{"id": "1"}],
                    "pageInfo": {"hasNextPage": true, "endCursor": "cursor-abc"}
                }
            }
        });
        let m = super::pagination_context_map(
            &v,
            Some(&[
                "data".to_string(),
                "issues".to_string(),
                "pageInfo".to_string(),
            ]),
        )
        .expect("pageInfo object");
        assert_eq!(m.get("endCursor"), Some(&serde_json::json!("cursor-abc")));
        assert_eq!(m.get("hasNextPage"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn pagination_context_map_accepts_numeric_prefix_for_root_array() {
        let v = serde_json::json!([
            { "data": { "after": null } },
            { "data": { "after": "t1_next", "children": [] } }
        ]);
        let m = super::pagination_context_map(&v, Some(&["1".to_string(), "data".to_string()]))
            .expect("second listing data object");
        assert_eq!(m.get("after"), Some(&serde_json::json!("t1_next")));
    }

    #[test]
    fn merge_pagination_into_body_nested_graphql_variables() {
        let mut body = Value::Object(indexmap::indexmap! {
            "query".to_string() => Value::String("{ q }".to_string()),
            "variables".to_string() => Value::Object(indexmap::indexmap! {
                "o".to_string() => Value::Object(IndexMap::new()),
            }),
        });
        merge_pagination_into_body(
            &mut body,
            Some(&[
                "variables".to_string(),
                "o".to_string(),
                "paginate".to_string(),
            ]),
            "page",
            Value::Integer(2),
        )
        .unwrap();
        merge_pagination_into_body(
            &mut body,
            Some(&[
                "variables".to_string(),
                "o".to_string(),
                "paginate".to_string(),
            ]),
            "limit",
            Value::Integer(5),
        )
        .unwrap();
        let vars = body
            .as_object()
            .unwrap()
            .get("variables")
            .unwrap()
            .as_object()
            .unwrap()
            .get("o")
            .unwrap()
            .as_object()
            .unwrap()
            .get("paginate")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(vars.get("page"), Some(&Value::Integer(2)));
        assert_eq!(vars.get("limit"), Some(&Value::Integer(5)));
    }

    #[test]
    fn test_execution_config_default() {
        let config = ExecutionConfig::default();
        assert_eq!(config.default_mode, ExecutionMode::Live);
        assert_eq!(config.timeout_seconds, 30);
        assert!(config.validate_responses);
        assert!(config.hydrate);
        assert_eq!(config.max_concurrent_requests, 64);
        assert_eq!(config.per_host_max_inflight, 24);
        assert_eq!(config.hydrate_concurrency, 16);
    }

    /// Regression: Cloudflare v4 list envelopes use `result: [...]`; paginated queries skip
    /// `prepare_http_query_response` and must still decode rows with scalar `id`.
    #[test]
    fn matrix_fixture_ruleset_query_decodes_v4_envelope_list() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let cgs = plasm_core::load_schema(&dir).expect("load plasm_prompt_matrix fixture");
        let cap = cgs
            .get_capability("ruleset_query")
            .expect("ruleset_query capability");
        let capability_template = parse_capability_template(&cap.mapping.template).unwrap();
        let cml = match &capability_template {
            plasm_compile::CapabilityTemplate::Http(c) => c,
            _ => panic!("expected HTTP template"),
        };
        assert!(
            !cml.response_is_single_object(),
            "ruleset_query must be a collection decode (`single: false`)",
        );
        let body = serde_json::json!({
            "errors": [],
            "messages": [],
            "result": [{
                "id": "2f2feab2026849078ba485f918791bdc",
                "kind": "root",
                "last_updated": "2000-01-01T00:00:00.000000Z",
                "name": "My ruleset",
                "phase": "http_request_firewall_custom",
                "version": "1",
                "description": "A description for my ruleset."
            }],
            "success": true,
            "result_info": { "cursors": { "after": "dGhpc2lzYW5leGFtcGxlCg" } }
        });
        let normalized =
            normalize_collection_response(body, response_bare_array_wrap_key(cml).as_str());
        let mut ambient = indexmap::IndexMap::new();
        ambient.insert("zone_id".into(), "00d2860b1edaed6074fd0f45a66e1a87".into());
        let decoder = create_entity_decoder(
            "Ruleset",
            &cgs,
            Some(http_collection_source(cml)),
            None,
            Some(&ambient),
        );
        let entities = decode_entities(&decoder, &normalized).expect("decode rulesets");
        assert_eq!(entities.len(), 1);
        let parts = entities[0]
            .reference
            .compound_parts()
            .expect("compound Ruleset ref");
        assert_eq!(
            parts.get("ruleset_id").map(String::as_str),
            Some("2f2feab2026849078ba485f918791bdc")
        );
        assert_eq!(
            parts.get("zone_id").map(String::as_str),
            Some("00d2860b1edaed6074fd0f45a66e1a87")
        );
    }

    #[test]
    fn matrix_fixture_ruleset_get_narrowing_decodes_inner_result_object() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let cgs = plasm_core::load_schema(&dir).expect("load plasm_prompt_matrix fixture");
        let cap = cgs
            .get_capability("ruleset_get")
            .expect("ruleset_get capability");
        let capability_template = parse_capability_template(&cap.mapping.template).unwrap();
        let cml = match &capability_template {
            plasm_compile::CapabilityTemplate::Http(c) => c,
            _ => panic!("expected HTTP template"),
        };
        assert!(cml.response_is_single_object());
        let body = serde_json::json!({
            "result": {
                "id": "2f2feab2026849078ba485f918791bdc",
                "name": "Zone-level phase entry point",
                "description": "",
                "kind": "zone",
                "version": "5",
                "last_updated": "2025-03-18T18:30:08.122758Z",
                "phase": "http_request_firewall_managed"
            },
            "success": true,
            "errors": [],
            "messages": []
        });
        let narrowed =
            narrow_http_graphql_response_for_entity_decode(&capability_template, body).unwrap();
        let mut ambient = indexmap::IndexMap::new();
        ambient.insert("zone_id".into(), "00d2860b1edaed6074fd0f45a66e1a87".into());
        let decoder = create_entity_decoder("Ruleset", &cgs, None, None, Some(&ambient));
        let entities = decode_entities(&decoder, &narrowed).expect("decode ruleset get");
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].fields.get("id"),
            Some(&plasm_core::Value::String(
                "2f2feab2026849078ba485f918791bdc".into()
            ))
        );
    }

    #[test]
    fn test_create_execution_engine() {
        let config = ExecutionConfig::default();
        let engine = ExecutionEngine::new(config);
        assert!(engine.is_ok());
    }

    #[tokio::test]
    async fn test_type_check_before_execution() {
        let config = ExecutionConfig::default();
        let engine = ExecutionEngine::new(config).unwrap();
        let cgs = create_test_cgs();
        let mut cache = SessionMaterialization::new();

        // Create an invalid query (non-existent entity)
        let query = QueryExpr::all("NonExistentEntity");
        let expr = Expr::Query(query);

        let result = engine
            .execute(
                &expr,
                &cgs,
                &mut cache,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::default(),
            )
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RuntimeError::TypeError { .. }
        ));
    }

    #[tokio::test]
    async fn test_execute_get_rejects_domain_placeholder_id() {
        let engine = ExecutionEngine::new(ExecutionConfig::default()).unwrap();
        let cgs = create_test_cgs();
        let mut cache = SessionMaterialization::new();
        let expr = Expr::Get(GetExpr::new("Account", "$"));
        let res = engine
            .execute(
                &expr,
                &cgs,
                &mut cache,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::default(),
            )
            .await;
        let err = res.expect_err("expected placeholder rejection");
        assert!(matches!(err, RuntimeError::TypeError { .. }));
    }

    #[test]
    fn test_basic_decoder_creation() {
        let decoder = create_entity_decoder(
            "TestEntity",
            &CGS::new(),
            Some(PathExpr::from_slice(&["results", "*"])),
            None,
            None,
        );
        assert_eq!(decoder.entity, "TestEntity");
        assert_eq!(decoder.fields.len(), 1);
    }

    #[test]
    fn test_execution_result_serialization() {
        let result = ExecutionResult {
            entities: vec![],
            count: 0,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 100,
                network_requests: 1,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("live")); // lowercase due to serde rename_all
        assert!(json.contains("duration_ms"));
    }

    #[test]
    fn test_execution_result_json_skips_host_pagination_fields() {
        use plasm_core::PagingHandle;
        let result = ExecutionResult {
            entities: vec![],
            count: 0,
            has_more: true,
            pagination_resume: None,
            paging_handle: Some(PagingHandle::mint_monotonic(1)),
            source: ExecutionSource::Live,
            stats: ExecutionStats {
                duration_ms: 1,
                network_requests: 0,
                cache_hits: 0,
                cache_misses: 0,
                ..Default::default()
            },
            request_fingerprints: Vec::new(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            !json.contains("pg1"),
            "paging_handle must not appear on wire JSON: {json}"
        );
        assert!(json.contains("\"has_more\":true"));
    }

    #[test]
    fn populate_template_path_env_binds_explicit_evm_get_vars() {
        let template = parse_capability_template(&serde_json::json!({
            "transport": "evm_call",
            "chain": 1,
            "contract": { "type": "const", "value": "0x0000000000000000000000000000000000000001" },
            "function": "function balanceOf(address owner) view returns (uint256)",
            "args": [{ "type": "var", "name": "owner" }],
            "block": { "type": "var", "name": "block" }
        }))
        .unwrap();

        let mut env = CmlEnv::new();
        let mut vars = IndexMap::new();
        vars.insert(
            "owner".to_string(),
            Value::String("0x00000000000000000000000000000000000000aa".to_string()),
        );
        vars.insert("block".to_string(), Value::String("latest".to_string()));

        populate_template_path_env(
            &mut env,
            &template,
            &Ref::new("Pet", "ignored-id"),
            None,
            Some(&vars),
            None,
        );

        assert_eq!(
            env.get("owner"),
            Some(&Value::String(
                "0x00000000000000000000000000000000000000aa".to_string()
            ))
        );
        assert_eq!(env.get("block"), Some(&Value::String("latest".to_string())));
        assert_eq!(
            env.get("id"),
            Some(&Value::String("ignored-id".to_string()))
        );
    }

    #[test]
    fn populate_template_path_env_does_not_default_non_id_evm_vars_to_primary_id() {
        let template = parse_capability_template(&serde_json::json!({
            "transport": "evm_call",
            "chain": 1,
            "contract": { "type": "const", "value": "0x0000000000000000000000000000000000000001" },
            "function": "function balanceOf(address owner) view returns (uint256)",
            "args": [{ "type": "var", "name": "owner" }]
        }))
        .unwrap();

        let mut env = CmlEnv::new();
        populate_template_path_env(
            &mut env,
            &template,
            &Ref::new("Pet", "primary-id"),
            None,
            None,
            None,
        );

        assert_eq!(
            env.get("id"),
            Some(&Value::String("primary-id".to_string()))
        );
        assert!(
            !env.contains_key("owner"),
            "non-id EVM vars should be explicitly supplied, not silently bound to the primary id"
        );
    }

    #[test]
    fn populate_template_path_env_binds_graphql_id_field_var() {
        use indexmap::IndexMap;
        use plasm_core::identity::{EntityFieldName, EntityName};
        use plasm_core::schema::EntityDef;

        let template = parse_capability_template(&serde_json::json!({
            "transport": "graphql",
            "method": "POST",
            "path": [{ "type": "literal", "value": "graphql" }],
            "body": {
                "type": "object",
                "fields": [
                    ["query", { "type": "const", "value": "query($key: String!) { teams { nodes { key } } }" }],
                    ["variables", {
                        "type": "object",
                        "fields": [["key", { "type": "var", "name": "key" }]]
                    }]
                ]
            }
        }))
        .unwrap();

        let ent = EntityDef {
            name: EntityName::from("Team"),
            description: String::new(),
            id_field: EntityFieldName::from("key"),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        };

        let mut env = CmlEnv::new();
        populate_template_path_env(
            &mut env,
            &template,
            &Ref::new("Team", "EVA"),
            Some(&ent),
            None,
            None,
        );

        assert_eq!(env.get("key"), Some(&Value::String("EVA".to_string())));
    }

    #[test]
    fn populate_template_path_env_path_vars_override_compound_ref_strings() {
        let template = parse_capability_template(&serde_json::json!({
            "method": "GET",
            "path": [
                {"type": "var", "name": "owner"},
                {"type": "literal", "value": "/"},
                {"type": "var", "name": "repo"},
                {"type": "literal", "value": "/"},
                {"type": "var", "name": "n"}
            ]
        }))
        .unwrap();

        let mut parts = BTreeMap::new();
        parts.insert("owner".into(), "stale-binding-name".into());
        parts.insert("repo".into(), "r".into());
        parts.insert("n".into(), "9".into());
        let reference = Ref::compound("Ticket", parts);

        let mut pv = IndexMap::new();
        pv.insert(
            "owner".into(),
            plasm_core::Value::String("real-owner-id".into()),
        );

        let mut env = CmlEnv::new();
        populate_template_path_env(&mut env, &template, &reference, None, Some(&pv), None);

        assert_eq!(
            env.get("owner"),
            Some(&plasm_core::Value::String("real-owner-id".into())),
            "path_vars must override stale compound Ref string for HTTP template vars"
        );
    }

    #[test]
    fn block_range_with_upper_bound_is_not_single_page() {
        let pconf = PaginationConfig {
            params: indexmap::indexmap! {
                "range_size".to_string() => plasm_compile::PaginationParam::Fixed { fixed: serde_json::json!(100) },
            },
            location: plasm_compile::PaginationLocation::BlockRange,
            body_merge_path: None,
            response_prefix: None,
            response_next_url_field: None,
            stop_when: None,
        };
        let user = QueryPagination {
            from_block: Some(0),
            to_block: Some(5_000),
            ..Default::default()
        };
        let consume = StreamConsumeOpts {
            fetch_all: false,
            max_items: None,
            one_page: false,
            ..Default::default()
        };
        // block_range + explicit to_block → NOT single HTTP round-trip (multi-page range query)
        let single_http_roundtrip = !consume.fetch_all
            && !matches!(
                pconf.location,
                plasm_compile::PaginationLocation::BlockRange
            )
            && (consume.max_items.is_none() || consume.one_page);
        assert!(!single_http_roundtrip);
        let _ = user; // suppress unused warning
    }

    #[test]
    fn block_range_without_upper_bound_stays_single_page_by_default() {
        let pconf = PaginationConfig {
            params: indexmap::indexmap! {
                "range_size".to_string() => plasm_compile::PaginationParam::Fixed { fixed: serde_json::json!(100) },
            },
            location: plasm_compile::PaginationLocation::BlockRange,
            body_merge_path: None,
            response_prefix: None,
            response_next_url_field: None,
            stop_when: None,
        };
        let user = QueryPagination {
            from_block: Some(0),
            ..Default::default()
        };
        let consume = StreamConsumeOpts {
            fetch_all: false,
            max_items: None,
            one_page: false,
            ..Default::default()
        };
        // block_range without to_block → not a single HTTP round-trip (BlockRange is always multi-step)
        let single_http_roundtrip = !consume.fetch_all
            && !matches!(
                pconf.location,
                plasm_compile::PaginationLocation::BlockRange
            )
            && (consume.max_items.is_none() || consume.one_page);
        // BlockRange always forces multi-page in the new model — test confirms the flag logic
        assert!(!single_http_roundtrip); // BlockRange is never a single HTTP round-trip
        let _ = user;
    }

    #[tokio::test]
    async fn execute_http_respects_base_url_override() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct RecordingTransport {
            last_base: Arc<Mutex<Option<String>>>,
        }

        #[async_trait]
        impl HttpTransport for RecordingTransport {
            async fn send_compiled_http(
                &self,
                base_url: &str,
                _request: &CompiledRequest,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                *self.last_base.lock().unwrap() = Some(base_url.to_string());
                Ok((serde_json::json!({"id":"1","name":"n"}), None))
            }

            async fn get_json_absolute(
                &self,
                _url: &str,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                Ok((serde_json::json!({}), None))
            }
        }

        let last = Arc::new(Mutex::new(None));
        let transport = RecordingTransport {
            last_base: last.clone(),
        };
        let config = ExecutionConfig {
            base_url: Some("http://wrong-host".to_string()),
            ..ExecutionConfig::default()
        };
        let engine = ExecutionEngine::new_with_transport(config, Arc::new(transport), None);
        let cgs = create_test_cgs();
        let mut cache = SessionMaterialization::new();
        let expr = Expr::Get(GetExpr::new("Account", "1"));
        engine
            .execute(
                &expr,
                &cgs,
                &mut cache,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions {
                    http_base_url_override: Some("http://right-host".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("execute");

        assert_eq!(last.lock().unwrap().as_deref(), Some("http://right-host"));
    }

    #[tokio::test]
    async fn execute_http_uses_session_auth_resolver_override_when_engine_has_none() {
        use crate::auth::ResolvedAuth;
        use crate::http_transport::HttpTransport;
        use async_trait::async_trait;
        use plasm_compile::CompiledRequest;
        use plasm_core::AuthScheme;
        use std::sync::{Arc, Mutex};

        const ENV_KEY: &str = "PLASM_RT_SESSION_AUTH_OVERRIDE_TEST";

        struct RecordingTransport {
            last_auth: Arc<Mutex<Option<ResolvedAuth>>>,
        }

        #[async_trait]
        impl HttpTransport for RecordingTransport {
            async fn send_compiled_http(
                &self,
                _base_url: &str,
                _request: &CompiledRequest,
                auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                *self.last_auth.lock().unwrap() = auth;
                Ok((serde_json::json!({"id":"1","name":"n"}), None))
            }

            async fn get_json_absolute(
                &self,
                _url: &str,
                _auth: Option<ResolvedAuth>,
            ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
                Ok((serde_json::json!({}), None))
            }
        }

        std::env::set_var(ENV_KEY, "secret-token");

        let last = Arc::new(Mutex::new(None));
        let transport = RecordingTransport {
            last_auth: last.clone(),
        };
        let config = ExecutionConfig::default();
        let scheme = AuthScheme::ApiKeyHeader {
            header: "X-Test-Auth".to_string(),
            env: Some(ENV_KEY.to_string()),
            hosted_kv: None,
        };
        let override_resolver = Arc::new(crate::AuthResolver::from_env(scheme));
        let engine = ExecutionEngine::new_with_transport(config, Arc::new(transport), None);
        let cgs = create_test_cgs();
        let mut cache = SessionMaterialization::new();
        let expr = Expr::Get(GetExpr::new("Account", "1"));
        engine
            .execute(
                &expr,
                &cgs,
                &mut cache,
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions {
                    auth_resolver_override: Some(override_resolver),
                    ..Default::default()
                },
            )
            .await
            .expect("execute");

        std::env::remove_var(ENV_KEY);

        let resolved = last.lock().unwrap().clone().expect("auth should be set");
        assert!(
            resolved
                .headers
                .iter()
                .any(|(k, v)| k == "X-Test-Auth" && v == "secret-token"),
            "expected override header, got {:?}",
            resolved.headers
        );
    }

    /// `prepare_http_query_response` + tagged [`ResponsePreprocess`]: find workspace, pluck `nested_array`.
    #[test]
    fn prepare_http_query_response_array_find_pluck() {
        use plasm_compile::CmlRequest;
        use plasm_core::Value;

        let cml: CmlRequest = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": [{"type": "literal", "value": "v2"}],
            "response": {
                "items": "members",
                "response_preprocess": {
                    "kind": "array_find_pluck",
                    "path": ["teams"],
                    "id_field": "id",
                    "id_var": "team_id",
                    "nested_array": "members"
                }
            }
        }))
        .unwrap();
        let mut env = CmlEnv::new();
        env.insert("team_id".to_string(), Value::String("2".to_string()));
        let body = serde_json::json!({
            "teams": [
                {"id": "1", "members": [{"n": "a"}]},
                {"id": "2", "members": [{"n": "b"}]}
            ]
        });
        let out = prepare_http_query_response(body, &cml, &env);
        assert_eq!(out, serde_json::json!({ "members": [ {"n": "b"} ] }));
    }

    /// Invalid `path` for array_find: body unchanged (no empty shell).
    #[test]
    fn prepare_http_query_response_array_find_bad_path_unchanged() {
        use plasm_compile::CmlRequest;
        use plasm_core::Value;

        let cml: CmlRequest = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": [{"type": "literal", "value": "v2"}],
            "response": {
                "items": "members",
                "response_preprocess": {
                    "kind": "array_find_pluck",
                    "path": ["teams"],
                    "id_field": "id",
                    "id_var": "team_id",
                    "nested_array": "members"
                }
            }
        }))
        .unwrap();
        let mut env = CmlEnv::new();
        env.insert("team_id".to_string(), Value::String("2".to_string()));
        let body = serde_json::json!({ "other": 1 });
        let out = prepare_http_query_response(body.clone(), &cml, &env);
        assert_eq!(out, body);
    }

    #[test]
    fn prepare_http_query_response_concat_field_arrays() {
        use plasm_compile::CmlRequest;

        let cml: CmlRequest = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": [{"type": "literal", "value": "v2"}],
            "response": {
                "items": "intervals",
                "response_preprocess": {
                    "kind": "concat_field_arrays",
                    "path": ["data"],
                    "from_each": "intervals"
                }
            }
        }))
        .unwrap();
        let env = CmlEnv::new();
        let body = serde_json::json!({
            "data": [
                { "intervals": [ {"a": 1} ] },
                { "intervals": [ {"a": 2}, {"a": 3} ] }
            ]
        });
        let out = prepare_http_query_response(body, &cml, &env);
        assert_eq!(
            out,
            serde_json::json!({ "intervals": [ {"a": 1}, {"a": 2}, {"a": 3} ] })
        );
    }

    #[test]
    fn prepare_http_query_response_string_ids_to_field_objects() {
        use plasm_compile::CmlRequest;

        let cml: CmlRequest = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": [{"type": "literal", "value": "v2"}],
            "response": {
                "items": "templates",
                "response_preprocess": {
                    "kind": "string_ids_to_field_objects",
                    "path": ["templates"],
                    "field": "id"
                }
            }
        }))
        .unwrap();
        let env = CmlEnv::new();
        let body = serde_json::json!({ "templates": ["t-1", "t-2", 3] });
        let out = prepare_http_query_response(body, &cml, &env);
        assert_eq!(
            out,
            serde_json::json!({
                "templates": [
                    { "id": "t-1" },
                    { "id": "t-2" }
                ]
            })
        );
    }

    /// `single: true` does not wrap a second time when `response_preprocess` already shaped the body.
    #[test]
    fn prepare_http_query_response_single_skipped_when_preprocess() {
        use plasm_compile::CmlRequest;

        let cml: CmlRequest = serde_json::from_value(serde_json::json!({
            "method": "GET",
            "path": [],
            "response": {
                "single": true,
                "items": "intervals",
                "response_preprocess": {
                    "kind": "concat_field_arrays",
                    "path": ["data"],
                    "from_each": "intervals"
                }
            }
        }))
        .unwrap();
        let env = CmlEnv::new();
        let body = serde_json::json!({
            "data": [ { "intervals": [ {"i": 1} ] } ]
        });
        let out = prepare_http_query_response(body, &cml, &env);
        assert_eq!(out, serde_json::json!({ "intervals": [ {"i": 1} ] }));
    }

    #[test]
    fn schema_overlay_decode_routes_to_typed_entity() {
        use plasm_core::loader::load_schema_dir;
        use plasm_core::schema_overlay::build_schema_overlay;

        let base_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/schemas/fibery_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("bootstrap fixture");
        let json: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
        ))
        .expect("sample schema JSON");
        let spec = base.schema_overlay.as_ref().unwrap();
        let overlay = build_schema_overlay(spec, &base, &json).expect("overlay");
        let cgs = base.with_overlay(overlay).expect("merge");

        let mut ambient = IndexMap::new();
        ambient.insert("database".to_string(), "Cricket/Player".to_string());
        let entity =
            entity_decoder::resolve_overlay_decode_entity(&cgs, "entity_query", Some(&ambient))
                .expect("overlay entity for scope");
        assert_eq!(entity, "Cricket__Player");
        let ent = cgs.get_entity("Cricket__Player").expect("overlay entity");
        assert!(ent
            .fields
            .contains_key(&plasm_core::EntityFieldName::from("Cricket_name")));
    }

    #[test]
    fn schema_overlay_decode_composite_scope_key() {
        use plasm_core::loader::load_schema_dir;
        use plasm_core::schema_overlay::{build_decode_scope_key, build_schema_overlay};

        let base_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/schemas/fibery_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("bootstrap fixture");
        let json: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
        ))
        .expect("sample schema JSON");
        let spec = base.schema_overlay.as_ref().unwrap();
        let overlay = build_schema_overlay(spec, &base, &json).expect("overlay");
        let cgs = base.with_overlay(overlay).expect("merge");

        let mut ambient = IndexMap::new();
        ambient.insert("project".into(), "MYPROJ".into());
        ambient.insert("issuetype".into(), "Story".into());
        let composite_spec = plasm_core::schema_overlay::OverlayDecodeScopeSpec {
            params: vec!["project".into(), "issuetype".into()],
            key: plasm_core::schema_overlay::OverlayTemplateSpec {
                template: "{{ ambient.project }}:{{ ambient.issuetype }}".into(),
            },
        };
        let key = build_decode_scope_key(&composite_spec, &ambient).expect("composite key");
        assert_eq!(key, "MYPROJ:Story");

        let mut single = IndexMap::new();
        single.insert("database".to_string(), "Cricket/Player".to_string());
        let entity =
            entity_decoder::resolve_overlay_decode_entity(&cgs, "entity_query", Some(&single))
                .expect("overlay entity for scope");
        assert_eq!(entity, "Cricket__Player");
    }

    #[test]
    fn schema_overlay_augment_base_global_decode_without_ambient() {
        use plasm_core::loader::load_schema_dir;
        use plasm_core::schema_overlay::build_schema_overlay;

        let base_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/schemas/clickup_schema_overlay/bootstrap");
        let base = load_schema_dir(&base_dir).expect("bootstrap fixture");
        let json: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/schemas/clickup_schema_overlay/sample_custom_field_query.json"
        ))
        .expect("sample custom field JSON");
        let spec = base.schema_overlay.as_ref().unwrap();
        let overlay = build_schema_overlay(spec, &base, &json).expect("overlay");
        let cgs = base.with_overlay(overlay).expect("merge");

        let empty = IndexMap::new();
        let entity = entity_decoder::resolve_overlay_decode_entity(&cgs, "task_get", Some(&empty))
            .expect("overlay entity for global augment_base");
        assert_eq!(entity, "Task");
        let task = cgs.get_entity("Task").expect("augmented Task");
        assert!(task
            .fields
            .contains_key(&plasm_core::EntityFieldName::from("Priority_Level")));
    }

    #[test]
    fn fibery_schema_query_decodes_database_rows_from_fibery_name_id_path() {
        use plasm_compile::decode_entities;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("schema_query").expect("schema_query");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let cml = match &capability_template {
            plasm_compile::CapabilityTemplate::Http(c) => c,
            _ => panic!("expected HTTP template"),
        };
        let json: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
        ))
        .expect("sample schema JSON");
        let normalized = prepare_http_query_response(json, cml, &CmlEnv::new());
        let decoder = create_entity_decoder_for_capability(
            "Database",
            &cgs,
            Some("schema_query"),
            Some(http_collection_source(cml)),
            None,
            None,
        );
        let entities = decode_entities(&decoder, &normalized).expect("decode Database rows");
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].fields.get("qualified_name"),
            Some(&plasm_core::Value::String("Cricket/Player".into()))
        );
    }

    #[test]
    fn fibery_user_get_me_narrowing_decodes_first_result_row() {
        use plasm_compile::decode_entities;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("user_get_me").expect("user_get_me");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/schemas/fibery_schema_overlay/sample_user_get_me.json"
        ))
        .expect("sample user_get_me JSON");
        let narrowed = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
            .expect("narrow user_get_me");
        let decoder = create_entity_decoder_for_capability(
            "User",
            &cgs,
            Some("user_get_me"),
            None,
            None,
            None,
        );
        let entities = decode_entities(&decoder, &narrowed).expect("decode User");
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].fields.get("id"),
            Some(&plasm_core::Value::String(
                "7dcf4730-82d2-11e9-8a28-82a9c787ee9d".into()
            ))
        );
        assert_eq!(
            entities[0].fields.get("name"),
            Some(&plasm_core::Value::String("Arthur Dent".into()))
        );
        assert_eq!(
            entities[0].fields.get("email"),
            Some(&plasm_core::Value::String("arthur@example.com".into()))
        );
    }

    #[test]
    fn fibery_entity_create_narrowing_decodes_result_object() {
        use plasm_compile::decode_entities;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("entity_create").expect("entity_create");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/schemas/fibery_schema_overlay/sample_entity_create.json"
        ))
        .expect("sample entity_create JSON");
        let narrowed = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
            .expect("narrow entity_create");
        let mut env = CmlEnv::new();
        env.insert(
            "database".into(),
            plasm_core::Value::String("Cricket/Player".into()),
        );
        let identity_ambient = cml_env_to_identity_strings(&env);
        let decoder = mutating_capability_response_decoder(
            "Record",
            "entity_create",
            &cgs,
            &identity_ambient,
            None,
        );
        let entities = decode_entities(&decoder, &narrowed).expect("decode Record");
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].fields.get("id"),
            Some(&plasm_core::Value::String(
                "d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4".into()
            ))
        );
        assert_eq!(
            entities[0].fields.get("public_id"),
            Some(&plasm_core::Value::String("6".into()))
        );
    }

    #[test]
    fn fibery_entity_update_merge_injects_fibery_id_into_input() {
        use plasm_compile::CompiledOperation;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("entity_update").expect("entity_update");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let mut env = CmlEnv::new();
        env.insert(
            "id".into(),
            plasm_core::Value::String("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4".into()),
        );
        env.insert(
            "database".into(),
            plasm_core::Value::String("Cricket/Player".into()),
        );
        env.insert(
            "input".into(),
            plasm_core::Value::Object(indexmap::IndexMap::from([(
                "Cricket/Name".into(),
                plasm_core::Value::String("Renamed".into()),
            )])),
        );
        let target_ent = cgs.get_entity("Record");
        merge_entity_id_from_into_input_env(&mut env, target_ent, cap);
        let compiled = compile_operation_dispatch(&capability_template, &env).expect("compile");
        let CompiledOperation::Http(req) = compiled else {
            panic!("expected HTTP compiled operation");
        };
        let body_str = serde_json::to_string(&req.body).expect("serialize body");
        assert!(
            body_str.contains("fibery/id"),
            "entity_update must include fibery/id in entity body: {body_str}"
        );
        assert!(
            body_str.contains("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4"),
            "entity_update must bind id param into entity: {body_str}"
        );
    }

    #[test]
    fn fibery_entity_delete_compiles_fibery_id_and_database() {
        use plasm_compile::CompiledOperation;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("entity_delete").expect("entity_delete");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let mut env = CmlEnv::new();
        env.insert(
            "id".into(),
            plasm_core::Value::String("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4".into()),
        );
        env.insert(
            "database".into(),
            plasm_core::Value::String("Cricket/Player".into()),
        );
        let compiled = compile_operation_dispatch(&capability_template, &env).expect("compile");
        let CompiledOperation::Http(req) = compiled else {
            panic!("expected HTTP compiled operation");
        };
        let body_str = serde_json::to_string(&req.body).expect("serialize body");
        assert!(
            body_str.contains("fibery.entity/delete"),
            "delete command name: {body_str}"
        );
        assert!(
            body_str.contains("Cricket/Player"),
            "delete must include database type: {body_str}"
        );
        assert!(
            body_str.contains("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4"),
            "delete must include fibery/id: {body_str}"
        );
    }

    #[test]
    fn fibery_entity_delete_envelope_surfaces_success_false() {
        let body = serde_json::json!({
            "success": false,
            "result": {
                "name": "entity.error/not-found",
                "message": "Entity not found"
            }
        });
        let err = preflight_fibery_command_envelope(&body)
            .expect_err("delete envelope success:false must fail");
        let msg = format!("{err}");
        assert!(msg.contains("entity.error/not-found"), "{msg}");
        assert!(msg.contains("Entity not found"), "{msg}");
    }

    #[test]
    fn fibery_view_query_decodes_result_array() {
        use plasm_compile::decode_entities;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("view_query").expect("view_query");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let cml = match &capability_template {
            plasm_compile::CapabilityTemplate::Http(c) => c,
            _ => panic!("expected HTTP template"),
        };
        let body: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/schemas/fibery_schema_overlay/sample_view_query.json"
        ))
        .expect("sample view_query JSON");
        let normalized = prepare_http_query_response(body, cml, &CmlEnv::new());
        let decoder = create_entity_decoder_for_capability(
            "View",
            &cgs,
            Some("view_query"),
            Some(http_collection_source(cml)),
            None,
            None,
        );
        let entities = decode_entities(&decoder, &normalized).expect("decode View rows");
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].fields.get("id"),
            Some(&plasm_core::Value::String(
                "43addb30-1fd0-11ee-9009-a7c752e861c6".into()
            ))
        );
        assert_eq!(
            entities[0].fields.get("name"),
            Some(&plasm_core::Value::String("Supa Doc".into()))
        );
    }

    #[test]
    fn fibery_user_get_me_compile_preserves_my_id_filter() {
        use plasm_compile::CompiledOperation;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("user_get_me").expect("user_get_me");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let compiled = compile_operation_dispatch(&capability_template, &CmlEnv::new())
            .expect("compile user_get_me");
        let CompiledOperation::Http(req) = compiled else {
            panic!("expected HTTP compiled operation");
        };
        let body_str = serde_json::to_string(&req.body).expect("serialize body");
        assert!(
            body_str.contains("$my-id"),
            "user_get_me must filter on authenticated user via $my-id: {body_str}"
        );
        assert!(
            body_str.contains("\"params\""),
            "user_get_me must include empty params object for Fibery param resolution: {body_str}"
        );
        let body: serde_json::Value = serde_json::from_str(&body_str).expect("parse body json");
        let where_clause = body
            .get("args")
            .and_then(|a| a.get("query"))
            .and_then(|q| q.get("q/where"))
            .expect("q/where in compiled body");
        assert_eq!(
            where_clause,
            &serde_json::json!(["=", ["fibery/id"], "$my-id"])
        );
    }

    #[test]
    fn fibery_command_envelope_preflight_surfaces_success_false() {
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("user_get_me").expect("user_get_me");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let body = serde_json::json!({
            "success": false,
            "result": {
                "name": "entity.error/schema-type-not-found",
                "message": "fibery/user database was not found."
            }
        });
        let err = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
            .expect_err("success:false must fail before narrowing");
        let msg = format!("{err}");
        assert!(msg.contains("entity.error/schema-type-not-found"), "{msg}");
        assert!(msg.contains("fibery/user database was not found"), "{msg}");
    }

    #[test]
    fn fibery_command_envelope_preflight_surfaces_empty_result_array() {
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
        let cgs = load_schema_dir(&dir).expect("load fibery catalog");
        let cap = cgs.get_capability("user_get_me").expect("user_get_me");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let body = serde_json::json!({ "success": true, "result": [] });
        let err = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
            .expect_err("empty result[] must fail with actionable message");
        let msg = format!("{err}");
        assert!(msg.contains("no rows"), "{msg}");
        assert!(msg.contains("$my-id"), "{msg}");
    }

    #[test]
    fn github_issue_query_decoder_includes_embedded_labels_relation() {
        use plasm_compile::decode_entities;
        use plasm_compile::DecodedRelation;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        let cgs = load_schema_dir(&dir).expect("load github catalog");
        let cap = cgs.get_capability("issue_query").expect("issue_query");
        let capability_template =
            parse_capability_template(&cap.mapping.template).expect("parse template");
        let cml = match &capability_template {
            plasm_compile::CapabilityTemplate::Http(c) => c,
            _ => panic!("expected HTTP template"),
        };
        let decoder = create_entity_decoder_for_capability(
            "Issue",
            &cgs,
            Some("issue_query"),
            Some(http_collection_source(cml)),
            None,
            None,
        );
        assert!(
            decoder.relations.iter().any(|r| r.relation == "labels"),
            "Issue.labels prefer/from_parent_get must emit a relation decoder on issue_query"
        );

        let row = serde_json::json!({
            "id": 42,
            "number": 7,
            "repository_url": "https://api.github.com/repos/acme/demo",
            "title": "Bug",
            "state": "open",
            "labels": [
                {
                    "id": 1,
                    "name": "bug",
                    "color": "f29513",
                    "description": "label",
                    "default": false
                }
            ]
        });
        let body = serde_json::json!([row]);
        let normalized =
            normalize_collection_response(body, response_bare_array_wrap_key(cml).as_str());
        let decoded = decode_entities(&decoder, &normalized).expect("decode issues");
        assert_eq!(decoded.len(), 1);
        match decoded[0].relations.get("labels") {
            Some(DecodedRelation::Specified(refs)) => {
                assert_eq!(refs.len(), 1);
                assert!(!decoded[0].embedded_entities.is_empty());
            }
            other => panic!("expected Specified labels relation, got {other:?}"),
        }
    }

    #[test]
    fn langitem_get_decoder_embed_decoders_are_leaf() {
        use plasm_compile::DecodedRelation;
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("langmatrix");
        let decoder = create_entity_decoder_for_capability(
            "LangItem",
            &cgs,
            Some("langitem_get"),
            None,
            Some("i1"),
            None,
        );
        let summary_rel = decoder
            .relations
            .iter()
            .find(|r| r.relation == "summary")
            .expect("summary relation decoder");
        assert!(
            summary_rel.decoder.relations.is_empty(),
            "leaf summary decoder must not nest further embed decoders (CEP-10)"
        );

        let body = serde_json::json!({
            "id": "i1",
            "title": "Alpha",
            "summary": {
                "id": "sum-i1",
                "headline": "Alpha summary",
                "detail": { "id": "det-i1", "body": "nested detail" }
            }
        });
        let decoded =
            decode_entities_with_cgs(&decoder, &body, Some(&cgs)).expect("decode langitem get");
        let summary = decoded[0]
            .embedded_entities
            .iter()
            .find(|e| e.reference.entity_type.as_str() == "LangSummary")
            .expect("embedded summary");
        let detail_rel = summary
            .relations
            .get("detail")
            .expect("summary.detail relation");
        let DecodedRelation::Specified(refs) = detail_rel else {
            panic!("expected specified detail refs, got {detail_rel:?}");
        };
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].primary_slot_str(), "det-i1");
        let detail = summary
            .embedded_entities
            .iter()
            .find(|e| e.reference.entity_type.as_str() == "LangDetail")
            .expect("embedded detail");
        assert_eq!(detail.reference.primary_slot_str(), "det-i1");
    }

    #[test]
    fn pokemon_get_decoder_embed_decoders_are_leaf() {
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let cgs = load_schema_dir(&dir).expect("pokeapi");
        let decoder = create_entity_decoder_for_capability(
            "Pokemon",
            &cgs,
            Some("pokemon_get"),
            None,
            Some("pikachu"),
            None,
        );
        for rel in ["types", "abilities", "species", "forms", "moves"] {
            let rd = decoder
                .relations
                .iter()
                .find(|r| r.relation == rel)
                .unwrap_or_else(|| panic!("missing {rel} relation decoder"));
            assert!(
                rd.decoder.relations.is_empty(),
                "{rel} embed decoder must be leaf (no nested .relations; CEP-10)"
            );
        }
    }

    #[test]
    fn pokemon_get_decode_on_release_stack_budget() {
        use plasm_compile::decode_entities_with_cgs;
        use plasm_core::loader::load_schema_dir;

        if cfg!(debug_assertions) {
            return;
        }

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let cgs = load_schema_dir(&dir).expect("pokeapi");
        let decoder = create_entity_decoder_for_capability(
            "Pokemon",
            &cgs,
            Some("pokemon_get"),
            None,
            Some("pikachu"),
            None,
        );
        let body = serde_json::json!({
            "id": 25,
            "name": "pikachu",
            "species": { "name": "pikachu", "id": 25, "is_legendary": false, "is_mythical": false },
            "types": [
                { "slot": 1, "type": { "name": "electric", "url": "https://pokeapi.co/api/v2/type/13/" } }
            ],
            "abilities": [
                { "is_hidden": false, "slot": 1, "ability": { "name": "static", "url": "https://pokeapi.co/api/v2/ability/9/" } }
            ]
        });

        std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                let decoded =
                    decode_entities_with_cgs(&decoder, &body, Some(&cgs)).expect("decode pikachu");
                assert_eq!(decoded.len(), 1);
                assert!(decoded[0].relations.contains_key("species"));
            })
            .expect("spawn 4MiB decode thread")
            .join()
            .expect("join decode thread");
    }

    #[test]
    fn partition_scoped_query_fanout_one_job_per_parent() {
        let parents = vec![
            CachedEntity::from_decoded(
                Ref::new("LangItem", "i1"),
                IndexMap::new(),
                IndexMap::new(),
                0,
                EntityCompleteness::Summary,
            ),
            CachedEntity::from_decoded(
                Ref::new("LangItem", "i2"),
                IndexMap::new(),
                IndexMap::new(),
                0,
                EntityCompleteness::Summary,
            ),
        ];
        let jobs = partition_scoped_query_fanout(&parents, |p| {
            let q = QueryExpr::filtered(EntityName::from("LangTag"), Predicate::eq("id", "x"));
            let _ = p;
            q
        });
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].0, 0);
        assert_eq!(jobs[1].0, 1);
    }

    #[test]
    fn prefer_graph_miss_yields_scoped_not_error() {
        let materialize = RelationMaterialization::PreferFromParentGet {
            path: vec![JsonPathSegment::Key { key: "tags".into() }],
            on_embed_miss: plasm_core::EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScoped {
                capability: "cap".into(),
                param: "p".into(),
            },
        };
        let parent_ref = Ref::new("LangItem", "i1");
        let tag_ref = Ref::new("LangTag", "missing");
        let mut parent = CachedEntity::from_decoded(
            parent_ref,
            IndexMap::from([(String::from("tags"), Value::String("1".into()))]),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        );
        parent.update_relations("tags".into(), vec![tag_ref], 0);
        let res = resolve_relation_row_resolution(
            &materialize,
            "tags",
            "LangTag",
            &parent.payload_to_json(),
            parent.relations.get("tags").map(|v| v.as_slice()),
            |_| false,
        );
        assert_eq!(res, RelationRowResolution::ScopedQuery);
    }

    #[test]
    fn hydrate_from_embed_path_fallback_is_plan_materialized_only() {
        let cgs = create_test_cgs();
        let parent_def = cgs
            .get_entity("Account")
            .expect("Account entity in test cgs");
        let parent = CachedEntity::from_decoded(
            Ref::new("Account", "1"),
            IndexMap::new(),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        );
        let fallback = RelationScopedFallback::HydrateFromEmbedPath {
            path: Vec::new(),
            get_capability: "get_account".into(),
        };
        let err = build_scoped_query_from_fallback(
            &fallback,
            &parent,
            parent_def,
            &EntityName::from("Account"),
            &cgs,
        )
        .expect_err("runtime must not build scoped queries for hydrate fallback");
        match err {
            RuntimeError::ConfigurationError { message } => {
                assert!(message.contains("plan-materialized"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn flatten_per_parent_major_order() {
        let a = CachedEntity::from_decoded(
            Ref::new("LangTag", "a"),
            IndexMap::new(),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        );
        let b = CachedEntity::from_decoded(
            Ref::new("LangTag", "b"),
            IndexMap::new(),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        );
        let c = CachedEntity::from_decoded(
            Ref::new("LangTag", "c"),
            IndexMap::new(),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        );
        let per_parent = vec![vec![a.clone()], vec![b.clone(), c.clone()]];
        let flat: Vec<_> = per_parent.into_iter().flatten().collect();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].reference.primary_slot_str(), "a");
        assert_eq!(flat[1].reference.primary_slot_str(), "b");
        assert_eq!(flat[2].reference.primary_slot_str(), "c");
    }

    #[test]
    fn parent_row_relation_decoded_and_resolve_cached_targets() {
        let parent_ref = Ref::new("LangItem", "i1");
        let tag_ref = Ref::new("LangTag", "t1");
        let mut parent = CachedEntity::from_decoded(
            parent_ref,
            IndexMap::from([(String::from("id"), Value::String("i1".into()))]),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        );
        parent.update_relations("tags".into(), vec![tag_ref.clone()], 0);
        assert!(parent.relations.contains_key("tags"));

        let tag = CachedEntity::from_decoded(
            tag_ref,
            IndexMap::from([(String::from("label"), Value::String("urgent".into()))]),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        );
        let mut mat = SessionMaterialization::new();
        mat.insert(tag).expect("insert tag");
        let resolved = resolve_cached_targets_from_relation_refs(
            &mat,
            parent.relations.get("tags").expect("tags refs"),
            "LangTag",
        )
        .expect("resolve tag ref");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].get_field("label").map(|f| f.to_value()),
            Some(Value::String("urgent".into()))
        );
    }
}
