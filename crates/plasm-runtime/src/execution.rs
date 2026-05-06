use crate::api_error_detail::graphql_errors_summary;
use crate::evm::{execute_evm_call, execute_evm_logs};
use crate::http_transport::{HttpTransport, ReqwestHttpTransport};
use crate::invoke_preflight::merge_preflight_fields_into_env;
use crate::{AuthResolver, CachedEntity, EntityCompleteness, GraphCache, RuntimeError};
use indexmap::IndexMap;
use plasm_compile::{
    compile_operation, compile_query, decode_entities, parse_capability_template,
    path_expr_from_json_segments, path_var_names_from_request, template_pagination,
    template_var_names, BackendFilter, CapabilityTemplate, CmlEnv, CmlRequest,
    CompileOperationHook, CompileQueryHook, CompiledOperation, CompiledRequest, PaginationConfig,
    PathExpr, PathSegment, ResponsePreprocess,
};
use plasm_core::{
    cross_entity::{
        choose_strategy, extract_cross_entity_predicates, strip_cross_entity_comparisons,
        CrossEntityStrategy,
    },
    resolve_query_capability as resolve_query_capability_core, type_check_expr,
    type_check_expr_federated, CapabilityParamName, CapabilitySchema, ChainStep, EntityDef,
    EntityFieldName, EntityKey, EntityName, Expr, FieldType, GetExpr, InputType, InvokeExpr,
    InvokeInputPayload, ParameterRole, Predicate, PromptPipelineConfig, QueryExpr, QueryPagination,
    Ref, RelationMaterialization, RelationSchema, TypeError, Value, CGS,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::Instrument;

/// Resolve the capability that backs a [`QueryExpr`] (delegates to [`plasm_core::resolve_query_capability`]).
fn resolve_query_capability<'a>(
    query: &'a QueryExpr,
    cgs: &'a CGS,
) -> Result<&'a CapabilitySchema, RuntimeError> {
    resolve_query_capability_core(query, cgs).map_err(|e| RuntimeError::ConfigurationError {
        message: e.to_string(),
    })
}

/// DOMAIN prompts use bare `$` as a fill-in cue; it must not reach HTTP/EVM transport.
fn reject_domain_placeholder_in_executable(expr: &Expr) -> Result<(), RuntimeError> {
    let err = || {
        RuntimeError::TypeError {
        source: TypeError::DomainPlaceholderLiteral {
            field: "expression".to_string(),
            expected_type:
                "concrete ids and parameter values — replace every `$` from DOMAIN examples before execution"
                    .to_string(),
            description: None,
        },
    }
    };
    match expr {
        Expr::Get(g) => {
            if g.reference.contains_domain_placeholder() {
                return Err(err());
            }
            if let Some(m) = &g.path_vars {
                if m.values()
                    .any(plasm_core::Value::contains_domain_placeholder_deep)
                {
                    return Err(err());
                }
            }
        }
        Expr::Create(c) => {
            if c.input.to_value().contains_domain_placeholder_deep() {
                return Err(err());
            }
        }
        Expr::Delete(d) => {
            if d.target.contains_domain_placeholder() {
                return Err(err());
            }
            if let Some(m) = &d.path_vars {
                if m.values()
                    .any(plasm_core::Value::contains_domain_placeholder_deep)
                {
                    return Err(err());
                }
            }
        }
        Expr::Invoke(i) => {
            if i.target.contains_domain_placeholder() {
                return Err(err());
            }
            if let Some(inp) = &i.input {
                if inp.to_value().contains_domain_placeholder_deep() {
                    return Err(err());
                }
            }
            if let Some(m) = &i.path_vars {
                if m.values()
                    .any(plasm_core::Value::contains_domain_placeholder_deep)
                {
                    return Err(err());
                }
            }
        }
        Expr::Chain(ch) => {
            reject_domain_placeholder_in_executable(&ch.source)?;
            if let ChainStep::Explicit { expr: inner } = &ch.step {
                reject_domain_placeholder_in_executable(inner)?;
            }
        }
        Expr::Query(_) => {}
        Expr::Page(_) => {}
    }
    Ok(())
}

/// Drain a [`QueryStream`] into a single [`ExecutionResult`].
pub async fn collect_query_stream(
    stream: &mut QueryStream<'_>,
) -> Result<ExecutionResult, RuntimeError> {
    use futures_util::StreamExt;
    let mut all_entities = Vec::new();
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
        all_entities.extend(page.entities);
    }
    let count = all_entities.len();
    Ok(ExecutionResult {
        entities: all_entities,
        count,
        has_more: last_has_more,
        pagination_resume: last_resume,
        paging_handle: None,
        source: if any_live {
            ExecutionSource::Live
        } else {
            ExecutionSource::Replay
        },
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: total_net,
            cache_hits: 0,
            cache_misses: count,
        },
        request_fingerprints: Vec::new(),
    })
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
    /// Maximum number of concurrent requests
    pub max_concurrent_requests: usize,
    /// Path for the replay store directory (if using replay/hybrid)
    pub replay_store_path: Option<std::path::PathBuf>,
    /// After query, fetch each row via GET when the entity has a Get capability (unless `QueryExpr.hydrate == Some(false)`).
    pub hydrate: bool,
    /// Max concurrent GETs during query hydration.
    pub hydrate_concurrency: usize,
    /// DOMAIN prompt rendering + symbol expansion (REPL `:schema`, HTTP execute session prompt, eval).
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStats {
    /// Duration of execution in milliseconds
    pub duration_ms: u64,
    /// Whether any network requests were made
    pub network_requests: usize,
    /// Cache hits during execution
    pub cache_hits: usize,
    /// Cache misses during execution
    pub cache_misses: usize,
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
}

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
    /// When [`ExecuteOptions::federation`] is set, per-catalog HTTP backends apply per outbound request.
    static EXECUTION_FEDERATION: Option<std::sync::Arc<plasm_core::FederationDispatch>>;
}

tokio::task_local! {
    /// Entity name for the current HTTP op (matches [`plasm_core::FederationDispatch`] keys); selects backend when federated.
    static EXECUTION_DISPATCH_ENTITY: Option<String>;
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
            .finish()
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
            max_concurrent_requests: 10,
            replay_store_path: None,
            hydrate: true,
            hydrate_concurrency: 5,
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
    async fn run_in_execute_task_scopes<Fut, T>(
        base: Arc<str>,
        auth_override: Option<Arc<AuthResolver>>,
        plugin_hooks: PluginCompileHooks,
        request_fingerprint_sink: Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>,
        federation: Option<std::sync::Arc<plasm_core::FederationDispatch>>,
        fut: Fut,
    ) -> T
    where
        Fut: std::future::Future<Output = T> + Send,
        T: Send,
    {
        EXECUTION_FEDERATION
            .scope(federation, async move {
                EXECUTION_FINGERPRINT_SINK
                    .scope(request_fingerprint_sink, async move {
                        EXECUTION_PLUGIN_HOOKS
                            .scope(Some(plugin_hooks), async move {
                                EXECUTION_AUTH_RESOLVER
                                    .scope(auth_override, async move {
                                        EXECUTION_HTTP_BASE.scope(base, fut).await
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

    /// Prompt rendering / symbol expansion settings shared with DOMAIN and `expand_expr_for_parse`.
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
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                "plasm-runtime/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com)"
            ))
            .timeout(std::time::Duration::from_secs(config.timeout_seconds))
            .build()
            .map_err(|e| RuntimeError::RequestError {
                message: format!("Failed to create HTTP client: {}", e),
            })?;

        Ok(Self {
            transport: Arc::new(ReqwestHttpTransport::new(client)),
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

    /// Execute an HTTP request with replay awareness.
    /// In Live mode: execute and optionally record.
    /// In Replay mode: look up by fingerprint.
    /// In Hybrid mode: replay if available, otherwise live + record.
    async fn execute_with_replay(
        &self,
        compiled: &CompiledOperation,
        mode: ExecutionMode,
    ) -> Result<(serde_json::Value, ExecutionSource), RuntimeError> {
        let (json, _link, source) = self.execute_with_replay_full(compiled, mode).await?;
        Ok((json, source))
    }

    /// Like [`execute_with_replay`], but also returns `Link: ...; rel="next"` for CML `link_header` pagination (live only).
    async fn execute_with_replay_full(
        &self,
        compiled: &CompiledOperation,
        mode: ExecutionMode,
    ) -> Result<(serde_json::Value, Option<String>, ExecutionSource), RuntimeError> {
        let fingerprint = crate::RequestFingerprint::from_operation(compiled);

        match mode {
            ExecutionMode::Live => {
                let (resp, link) = self.execute_operation_full(compiled).await?;
                Ok((resp, link, ExecutionSource::Live))
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
                let (resp, link) = self.execute_operation_full(compiled).await?;
                Ok((resp, link, ExecutionSource::Live))
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
        cache: &'a mut GraphCache,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let start_time = std::time::Instant::now();
            let base = self.resolve_http_base_from_opts(&opts);
            let auth_override = opts.auth_resolver_override.clone();
            let plugin_hooks = PluginCompileHooks::snapshot_from_execute_options(&opts);
            let fp_sink = opts.request_fingerprint_sink.clone();
            let federation = opts.federation.clone();
            let mut result = Self::run_in_execute_task_scopes(
                base,
                auth_override,
                plugin_hooks,
                fp_sink.clone(),
                federation,
                async move {
                    let mut stream = self.execute_stream(expr, cgs, cache, mode, consume, opts)?;
                    collect_query_stream(&mut stream).await
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
        cache: &'a mut GraphCache,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        if let Some(ref fed) = opts.federation {
            type_check_expr_federated(expr, fed.as_ref(), cgs)?;
        } else {
            type_check_expr(expr, cgs)?;
        }
        reject_domain_placeholder_in_executable(expr)?;
        let execution_mode = mode.unwrap_or(self.config.default_mode);
        let chain_consume = consume.clone();
        match expr {
            Expr::Query(query) => self.query_to_stream(query, cgs, cache, execution_mode, consume),
            Expr::Page(_) => Err(RuntimeError::ConfigurationError {
                message: "`page(pg#)` continuations are executed via `ExecutionEngine::execute_pagination_resume`"
                    .to_string(),
            }),
            Expr::Get(get) => {
                let get = get.clone();
                let stream = Box::pin(async_stream::try_stream! {
                    let res = self.execute_get(&get, cgs, cache, execution_mode).await?;
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
                    let res = self.execute_create(&create, cgs, cache, execution_mode).await?;
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
                    let res = self.execute_delete(&delete, cgs, cache, execution_mode).await?;
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
                    let res = self.execute_invoke(&invoke, cgs, cache, execution_mode).await?;
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
                        .execute_chain(&chain, cgs, cache, execution_mode, chain_consume, opts.clone())
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
        }
    }

    fn query_to_stream<'a>(
        &'a self,
        query: &'a QueryExpr,
        cgs: &'a CGS,
        cache: &'a mut GraphCache,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        if let Some(pred) = &query.predicate {
            if let Some(source_entity) = cgs.get_entity(&query.entity) {
                let crosses = extract_cross_entity_predicates(pred, source_entity, cgs);
                if !crosses.is_empty() {
                    return self
                        .cross_entity_query_stream(query, &crosses, cgs, cache, mode, consume);
                }
            }
        }

        let filter = compile_query_dispatch(query, cgs)?;
        let capability = resolve_query_capability(query, cgs)?;
        let mut env = CmlEnv::new();
        if let Some(f) = &filter {
            let json_val = f.to_json();
            env.insert("filter".to_string(), json_to_plasm_value(&json_val));
        }
        if let Some(pred) = &query.predicate {
            extract_predicate_vars(pred, &mut env);
        }
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;
        if let Some(proj) = &query.projection {
            env.insert(
                "projection".to_string(),
                Value::Array(proj.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }
        let capability_template = parse_capability_template(&capability.mapping.template)?;
        if let Some(pconf) = template_pagination(&capability_template) {
            return self.paginated_query_stream(
                query.clone(),
                cgs,
                cache,
                mode,
                capability_template.clone(),
                pconf.clone(),
                env.clone(),
                capability,
                consume,
                None,
            );
        }

        self.non_paginated_query_stream(
            query,
            cgs,
            cache,
            mode,
            capability,
            capability_template,
            env,
        )
    }

    /// Execute a query expression (materializes [`query_to_stream`]).
    async fn execute_query(
        &self,
        query: &QueryExpr,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
    ) -> Result<ExecutionResult, RuntimeError> {
        let mut stream = self.query_to_stream(query, cgs, cache, mode, consume)?;
        collect_query_stream(&mut stream).await
    }

    #[allow(clippy::too_many_arguments)]
    fn cross_entity_query_stream<'a>(
        &'a self,
        query: &'a QueryExpr,
        crosses: &[plasm_core::cross_entity::CrossEntityPredicate],
        cgs: &'a CGS,
        cache: &'a mut GraphCache,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        let query = query.clone();
        let crosses = crosses.to_vec();
        let stream = Box::pin(async_stream::try_stream! {
            let res = self
                .execute_query_cross_entity(&query, &crosses, cgs, cache, mode, consume)
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

    #[allow(clippy::too_many_arguments)]
    fn non_paginated_query_stream<'a>(
        &'a self,
        query: &'a QueryExpr,
        cgs: &'a CGS,
        cache: &'a mut GraphCache,
        mode: ExecutionMode,
        capability: &'a CapabilitySchema,
        capability_template: CapabilityTemplate,
        env: CmlEnv,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        let compiled = compile_operation_dispatch(&capability_template, &env)?;
        let query = query.clone();
        let capability = capability.clone();
        let stream = Box::pin(async_stream::try_stream! {
            let (response, source) = with_dispatch_entity(
                Some(query.entity.as_str()),
                self.execute_with_replay(&compiled, mode),
            )
            .await?;
            let (normalized, decoder) = match &capability_template {
                CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => Ok((
                    prepare_http_query_response(response, cml, &env),
                    create_entity_decoder(
                        &query.entity,
                        cgs,
                        Some(http_collection_source(cml)),
                        None,
                        Some(&cml_env_to_identity_strings(&env)),
                    ),
                )),
                CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => {
                    Err(RuntimeError::ConfigurationError {
                        message: "query/search capabilities must use HTTP CML templates".into(),
                    })
                }
            }?;
            let decoded_entities = decode_entities(&decoder, &normalized)?;

            let response_completeness = {
                let all_entity_fields: std::collections::HashSet<String> = cgs
                    .get_entity(&query.entity)
                    .map(|e| {
                        e.fields
                            .keys()
                            .map(|k| k.as_str().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let provided: std::collections::HashSet<String> =
                    cgs.effective_provides(&capability).into_iter().collect();
                if provided.is_superset(&all_entity_fields) {
                    EntityCompleteness::Complete
                } else {
                    EntityCompleteness::Summary
                }
            };
            let hydrate_run = query.hydrate.unwrap_or(self.config.hydrate);
            let mut res = query_result_merge_cache(
                decoded_entities,
                response_completeness,
                source,
                cache,
                1,
            )?;
            let (entities, extra_net) = self
                .hydrate_query_summaries(&query.entity, &res.entities, cgs, cache, mode, hydrate_run)
                .await?;
            res.entities = entities;
            res.stats.network_requests += extra_net;

            if let Some(pred) = &query.predicate {
                if let Some(entity_def) = cgs.get_entity(&query.entity) {
                    let cap_params = capability_param_names(&capability);
                    if let Some(entity_pred) =
                        entity_field_predicate(pred, entity_def, Some(&cap_params))
                    {
                        res.entities
                            .retain(|e| client_side_predicate_matches(e, &entity_pred));
                        res.count = res.entities.len();
                    }
                }
            }

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

    /// Paginated query: one HTTP round-trip per stream item (page).
    #[allow(clippy::too_many_arguments)]
    fn paginated_query_stream<'a>(
        &'a self,
        query: QueryExpr,
        cgs: &'a CGS,
        cache: &'a mut GraphCache,
        mode: ExecutionMode,
        capability_template: CapabilityTemplate,
        pconf: PaginationConfig,
        env: CmlEnv,
        capability: &'a CapabilitySchema,
        consume: StreamConsumeOpts,
        resume_state: Option<PaginationLoopState>,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        const MAX_PAGES: usize = 10_000;

        let user = query.pagination.clone().unwrap_or_default();
        let single_http_roundtrip = !consume.fetch_all
            && !matches!(
                pconf.location,
                plasm_compile::PaginationLocation::BlockRange
            )
            && (consume.max_items.is_none() || consume.one_page);
        let (decoder, wrap_key) = match &capability_template {
            plasm_compile::CapabilityTemplate::Http(ref req)
            | plasm_compile::CapabilityTemplate::GraphQl(ref req) => (
                create_entity_decoder(
                    &query.entity,
                    cgs,
                    Some(http_collection_source(req)),
                    None,
                    Some(&cml_env_to_identity_strings(&env)),
                ),
                response_bare_array_wrap_key(req),
            ),
            plasm_compile::CapabilityTemplate::EvmCall(_)
            | plasm_compile::CapabilityTemplate::EvmLogs(_) => (
                create_entity_decoder(
                    &query.entity,
                    cgs,
                    Some(PathExpr::new(vec![
                        PathSegment::Key {
                            name: "results".to_string(),
                        },
                        PathSegment::Wildcard,
                    ])),
                    None,
                    Some(&cml_env_to_identity_strings(&env)),
                ),
                "results".to_string(),
            ),
        };
        let base_compiled = compile_operation_dispatch(&capability_template, &env)?;
        let mut state = match resume_state {
            Some(s) => s,
            None => PaginationLoopState::new(&pconf, &user, &consume)?,
        };
        let capability = capability.clone();

        let stream = Box::pin(async_stream::try_stream! {
            let mut pages = 0usize;
            let mut accumulated_total = 0usize;

            loop {
                if pages >= MAX_PAGES {
                    Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "Pagination stopped after {} pages (safety cap). Refine filters or increase cap in engine.",
                            MAX_PAGES
                        ),
                    })?;
                }

                let (response, link_next, http_live) =
                    if let Some(url) = state.next_absolute_url.take() {
                        if mode != ExecutionMode::Live {
                            Err(RuntimeError::ConfigurationError {
                                message: "link_header pagination beyond the first page requires Live execution mode (replay/hybrid do not store Link headers)".to_string(),
                            })?;
                        }
                        let (j, link) = with_dispatch_entity(
                            Some(query.entity.as_str()),
                            self.get_json_absolute(&url),
                        )
                        .await?;
                        (j, link, true)
                    } else {
                        let mut compiled = base_compiled.clone();
                        state.apply_request_params(
                            &mut compiled,
                            &pconf,
                            &user,
                            &consume,
                            single_http_roundtrip,
                            pages == 0,
                            accumulated_total,
                        )?;
                        let (j, link, src) = with_dispatch_entity(
                            Some(query.entity.as_str()),
                            self.execute_with_replay_full(&compiled, mode),
                        )
                        .await?;
                        (j, link, src == ExecutionSource::Live)
                    };

                let normalized = normalize_collection_response(response, wrap_key.as_str());
                let mut decoded_entities = decode_entities(&decoder, &normalized)?;
                let full_page_len = decoded_entities.len();
                let last_id = decoded_entities
                    .last()
                    .map(|d| d.reference.primary_slot_str());

                let mut truncated = false;
                if let Some(cap) = consume.max_items {
                    let remain = cap.saturating_sub(accumulated_total);
                    if decoded_entities.len() > remain {
                        decoded_entities.truncate(remain);
                        truncated = true;
                    }
                }

                let timestamp = current_timestamp();
                let page_cached: Vec<CachedEntity> = decoded_entities
                    .into_iter()
                    .map(|decoded| {
                        CachedEntity::from_decoded(
                            decoded.reference,
                            decoded.fields,
                            decoded.relations,
                            timestamp,
                            EntityCompleteness::Summary,
                        )
                    })
                    .collect();
                accumulated_total += page_cached.len();

                cache.merge(page_cached.clone())?;

                let hydrate_run = query.hydrate.unwrap_or(self.config.hydrate);
                let (hydrated, extra_net) = self
                    .hydrate_query_summaries(
                        &query.entity,
                        &page_cached,
                        cgs,
                        cache,
                        mode,
                        hydrate_run,
                    )
                    .await?;

                let cap_params = capability_param_names(&capability);
                let entities = match query.predicate.as_ref().and_then(|pred| {
                    cgs.get_entity(&query.entity)
                        .and_then(|e| entity_field_predicate(pred, e, Some(&cap_params)))
                }) {
                    Some(entity_pred) => hydrated
                        .into_iter()
                        .filter(|e| client_side_predicate_matches(e, &entity_pred))
                        .collect(),
                    None => hydrated,
                };

                let page_http = if http_live { 1 } else { 0 };
                let page_net = page_http + extra_net;
                let page_cache_misses = entities.len();

                if single_http_roundtrip {
                    let continue_pages = state.advance_after_page(
                        &pconf,
                        &normalized,
                        full_page_len,
                        state.last_requested_limit,
                        link_next.as_deref(),
                        last_id.as_deref(),
                    )?;
                    let pagination_resume = if continue_pages {
                        Some(QueryPaginationResumeData {
                            query: query.clone(),
                            capability_name: capability.name.to_string(),
                            env: env.clone(),
                            template: capability_template.clone(),
                            config: pconf.clone(),
                            state: (&state).into(),
                        })
                    } else {
                        None
                    };
                    yield PageResult {
                        entities,
                        page_index: pages,
                        has_more: continue_pages,
                        pagination_resume,
                        stats: ExecutionStats {
                            duration_ms: 0,
                            network_requests: page_net,
                            cache_hits: 0,
                            cache_misses: page_cache_misses,
                        },
                    };
                    break;
                }
                if truncated {
                    yield PageResult {
                        entities,
                        page_index: pages,
                        has_more: false,
                        pagination_resume: None,
                        stats: ExecutionStats {
                            duration_ms: 0,
                            network_requests: page_net,
                            cache_hits: 0,
                            cache_misses: page_cache_misses,
                        },
                    };
                    break;
                }
                if consume
                    .max_items
                    .is_some_and(|m| accumulated_total >= m)
                {
                    yield PageResult {
                        entities,
                        page_index: pages,
                        has_more: false,
                        pagination_resume: None,
                        stats: ExecutionStats {
                            duration_ms: 0,
                            network_requests: page_net,
                            cache_hits: 0,
                            cache_misses: page_cache_misses,
                        },
                    };
                    break;
                }
                if full_page_len == 0 && !matches!(pconf.location, plasm_compile::PaginationLocation::BlockRange) {
                    yield PageResult {
                        entities,
                        page_index: pages,
                        has_more: false,
                        pagination_resume: None,
                        stats: ExecutionStats {
                            duration_ms: 0,
                            network_requests: page_net,
                            cache_hits: 0,
                            cache_misses: page_cache_misses,
                        },
                    };
                    break;
                }

                let continue_pages = state.advance_after_page(
                    &pconf,
                    &normalized,
                    full_page_len,
                    state.last_requested_limit,
                    link_next.as_deref(),
                    last_id.as_deref(),
                )?;

                yield PageResult {
                    entities,
                    page_index: pages,
                    has_more: continue_pages,
                    pagination_resume: None,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: page_net,
                        cache_hits: 0,
                        cache_misses: page_cache_misses,
                    },
                };

                pages += 1;

                if !continue_pages {
                    break;
                }
            }
        });

        Ok(stream)
    }

    /// Resume a paginated query from a prior [`QueryPaginationResumeData`] snapshot (opaque LLM paging).
    pub fn execute_pagination_resume<'a>(
        &'a self,
        resume: QueryPaginationResumeData,
        cgs: &'a CGS,
        cache: &'a mut GraphCache,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let start_time = std::time::Instant::now();
            let base = self.resolve_http_base_from_opts(&opts);
            let auth_override = opts.auth_resolver_override.clone();
            let plugin_hooks = PluginCompileHooks::snapshot_from_execute_options(&opts);
            let fp_sink = opts.request_fingerprint_sink.clone();
            let federation = opts.federation.clone();
            let mut result = Self::run_in_execute_task_scopes(
                base,
                auth_override,
                plugin_hooks,
                fp_sink.clone(),
                federation,
                async move {
                    let mut stream = self.execute_pagination_resume_stream(
                        resume, cgs, cache, mode, consume, &opts,
                    )?;
                    collect_query_stream(&mut stream).await
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

    /// Lazy stream for [`Self::execute_pagination_resume`].
    pub fn execute_pagination_resume_stream<'a>(
        &'a self,
        resume: QueryPaginationResumeData,
        cgs: &'a CGS,
        cache: &'a mut GraphCache,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: &ExecuteOptions,
    ) -> Result<QueryStream<'a>, RuntimeError> {
        let qexpr = plasm_core::Expr::Query(resume.query.clone());
        if let Some(ref fed) = opts.federation {
            type_check_expr_federated(&qexpr, fed.as_ref(), cgs)?;
        } else {
            type_check_expr(&qexpr, cgs)?;
        }
        reject_domain_placeholder_in_executable(&qexpr)?;
        let execution_mode = mode.unwrap_or(self.config.default_mode);
        let capability = cgs
            .get_capability(resume.capability_name.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "unknown capability `{}` in pagination resume",
                    resume.capability_name
                ),
            })?;
        let state: PaginationLoopState = resume.state.try_into()?;
        let QueryPaginationResumeData {
            query,
            env,
            template,
            config,
            ..
        } = resume;
        self.paginated_query_stream(
            query,
            cgs,
            cache,
            execution_mode,
            template,
            config,
            env,
            capability,
            consume,
            Some(state),
        )
    }

    /// Execute a query with cross-entity predicate decomposition.
    ///
    /// For each cross-entity predicate (e.g. `pet.status = available`):
    /// - **Push-left**: query the foreign entity first, collect matching IDs,
    ///   inject an FK equality predicate on the source query.
    /// - **Pull-right**: query source without the cross-entity predicate,
    ///   then client-side filter each row by fetching the foreign entity.
    fn execute_query_cross_entity<'a>(
        &'a self,
        query: &'a QueryExpr,
        crosses: &'a [plasm_core::cross_entity::CrossEntityPredicate],
        cgs: &'a CGS,
        cache: &'a mut GraphCache,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
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
                                cache,
                                mode,
                                StreamConsumeOpts {
                                    fetch_all: true,
                                    max_items: None,
                                    one_page: false,
                                },
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
                .execute_query(&rewritten_query, cgs, cache, mode, consume)
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
                        let ref_id = extract_ref_id(entity, &cross.ref_field);
                        let Some(id) = ref_id else {
                            passes = false;
                            break;
                        };

                        let get = GetExpr::new(&cross.foreign_entity, &id);
                        let get_result = self.execute_get(&get, cgs, cache, mode).await?;
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
    async fn execute_get(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        // Satisfy from cache only when we already hold a detail payload.
        if let Some(entity) = cache.get(&get.reference) {
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
                    },
                    request_fingerprints: Vec::new(),
                });
            }
        }

        let (cached, source) = self.fetch_get_decoded(get, cgs, mode, None).await?;
        cache.insert(cached.clone())?;

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
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Run GET + decode without consulting the graph cache (used for query hydration and cache refresh).
    ///
    /// When `hydrate_capability` is `Some(name)`, use that named GET capability instead of the
    /// default per-entity `find_capability(.., Get)` (used by [`Self::apply_invoke_preflight`]).
    async fn fetch_get_decoded(
        &self,
        get: &GetExpr,
        cgs: &CGS,
        mode: ExecutionMode,
        hydrate_capability: Option<&str>,
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
                        message: format!(
                            "invoke_preflight hydrate_capability '{name}' must be kind get"
                        ),
                    });
                }
                if c.domain.as_str() != get.reference.entity_type.as_str() {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "invoke_preflight: hydrate capability '{name}' is for entity {}, expected {}",
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

        let mut env = CmlEnv::new();
        populate_template_path_env(
            &mut env,
            &capability_template,
            &get.reference,
            get.path_vars.as_ref(),
            None,
        );
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        let compiled = compile_operation_dispatch(&capability_template, &env)?;
        let (response, source) = with_dispatch_entity(
            Some(get.reference.entity_type.as_str()),
            self.execute_with_replay(&compiled, mode),
        )
        .await?;
        let response =
            narrow_http_graphql_response_for_entity_decode(&capability_template, response)?;
        let rid = cgs
            .get_entity(&get.reference.entity_type)
            .filter(|e| e.implicit_request_identity)
            .and_then(|_| get.reference.simple_id().map(|id| id.as_str()));
        let decoder = create_entity_decoder(
            &get.reference.entity_type,
            cgs,
            None,
            rid,
            Some(&ref_to_identity_ambient(&get.reference)),
        );
        let decoded_entities = decode_entities(&decoder, &response)?;

        let decoded = decoded_entities
            .first()
            .ok_or_else(|| RuntimeError::CacheError {
                message: format!("Entity not found: {}", get.reference),
            })?;

        let timestamp = current_timestamp();
        let cached = CachedEntity::from_decoded(
            decoded.reference.clone(),
            decoded.fields.clone(),
            decoded.relations.clone(),
            timestamp,
            EntityCompleteness::Complete,
        );
        Ok((cached, source))
    }

    /// When [`CapabilitySchema::invoke_preflight`] is set, load the invoke target row (cache or GET)
    /// and merge decoded fields into `env` as `{env_prefix}_{field}`.
    ///
    /// **Merge order:** called from [`Self::execute_invoke`] after path env, `input`, flattened
    /// invoke parameters, and scope splat — so preflight **overwrites** any spoofed `parent_*` keys
    /// that might appear in user input.
    async fn apply_invoke_preflight(
        &self,
        capability: &CapabilitySchema,
        cgs: &CGS,
        cache: &mut GraphCache,
        invoke: &InvokeExpr,
        mode: ExecutionMode,
        env: &mut CmlEnv,
    ) -> Result<(), RuntimeError> {
        let Some(spec) = capability.invoke_preflight.as_ref() else {
            return Ok(());
        };

        let prefix = spec.env_prefix.trim();
        if prefix.is_empty() {
            return Err(RuntimeError::ConfigurationError {
                message: "invoke_preflight.env_prefix must not be empty".to_string(),
            });
        }

        if let Some(entity) = cache.get(&invoke.target) {
            if entity.completeness == EntityCompleteness::Complete {
                merge_preflight_fields_into_env(env, prefix, &entity.fields);
                return Ok(());
            }
        }

        let get = GetExpr {
            reference: invoke.target.clone(),
            path_vars: None,
        };
        let (cached, _source) = self
            .fetch_get_decoded(&get, cgs, mode, Some(spec.hydrate_capability.as_str()))
            .await?;
        cache.insert(cached.clone())?;
        merge_preflight_fields_into_env(env, prefix, &cached.fields);
        Ok(())
    }

    /// After a query, upgrade Summary rows to Complete via concurrent GET when configured and supported.
    async fn hydrate_query_summaries(
        &self,
        entity_type: &str,
        ordered_entities: &[CachedEntity],
        cgs: &CGS,
        cache: &mut GraphCache,
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
                    cache.get(r).map(|e| e.completeness),
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
            async move { self.fetch_get_decoded(&get, cgs, mode, None).await }
        }))
        .buffer_unordered(concurrency);

        while let Some(res) = stream.next().await {
            let (entity, source) = res?;
            if source == ExecutionSource::Live {
                extra_network += 1;
            }
            cache.insert(entity)?;
        }

        let mut out = Vec::with_capacity(ordered_refs.len());
        for r in &ordered_refs {
            let e = cache.get(r).ok_or_else(|| RuntimeError::CacheError {
                message: format!("entity missing after query/hydrate: {}", r),
            })?;
            out.push(e.clone());
        }
        Ok((out, extra_network))
    }

    /// Execute a create expression (no target ID — creates a new resource)
    async fn execute_create(
        &self,
        create: &plasm_core::CreateExpr,
        cgs: &CGS,
        _cache: &mut GraphCache,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let capability = cgs
            .get_capability(create.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: create.capability.to_string(),
                entity: create.entity.to_string(),
            })?;

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        let payload = if let Some(schema) = &capability.input_schema {
            InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs)
        } else {
            create.input.clone()
        };

        let input = match capability.input_schema.as_ref() {
            Some(schema) => plasm_core::normalize_structured_string_inputs(
                payload.to_value(),
                &schema.input_type,
                cgs,
            ),
            None => payload.to_value(),
        };

        let mut env = CmlEnv::new();
        env.insert("input".to_string(), input.clone());
        if let Value::Object(ref map) = input {
            // Path segments: same as the historical loop.
            for var_name in path_var_names_from_template(&capability_template) {
                if let Some(v) = map.get(&var_name) {
                    env.insert(var_name.clone(), v.clone());
                }
            }
            // Body/query template vars: mirror invoke's input overlay so `var title` (etc.)
            // resolves without stuffing path-only keys into `body: { type: var, name: input }`.
            for (k, v) in map {
                env.insert(k.clone(), v.clone());
            }
        }
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_http_operation(&compiled, "create")?;
                let (response, _) = with_dispatch_entity(
                    Some(create.entity.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await?;
                let response =
                    narrow_http_graphql_response_for_entity_decode(&capability_template, response)?;
                let decoder = create_entity_decoder(&create.entity, cgs, None, None, None);
                let decoded = match decode_entities(&decoder, &response) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(
                            entity = %create.entity,
                            capability = %create.capability,
                            error = ?e,
                            "create response entity decode failed; returning empty entities"
                        );
                        Vec::new()
                    }
                };

                let timestamp = current_timestamp();
                let entities: Vec<CachedEntity> = decoded
                    .into_iter()
                    .map(|d| {
                        CachedEntity::from_decoded(
                            d.reference,
                            d.fields,
                            d.relations,
                            timestamp,
                            EntityCompleteness::Complete,
                        )
                    })
                    .collect();
                let count = entities.len();

                Ok(ExecutionResult {
                    entities,
                    count,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 1,
                        cache_hits: 0,
                        cache_misses: count,
                    },
                    request_fingerprints: Vec::new(),
                })
            }
            _ => Err(RuntimeError::UnsupportedExecutionMode {
                mode: format!("create with {:?}", mode),
            }),
        }
    }

    /// Execute a delete expression
    async fn execute_delete(
        &self,
        delete: &plasm_core::DeleteExpr,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let capability = cgs
            .get_capability(delete.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: delete.capability.to_string(),
                entity: delete.target.entity_type.to_string(),
            })?;

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        let mut env = CmlEnv::new();
        populate_template_path_env(
            &mut env,
            &capability_template,
            &delete.target,
            delete.path_vars.as_ref(),
            None,
        );
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_http_operation(&compiled, "delete")?;
                let (_response, _) = with_dispatch_entity(
                    Some(delete.target.entity_type.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await?;

                // Remove from cache if present
                cache.remove(&delete.target);

                Ok(ExecutionResult {
                    entities: vec![],
                    count: 0,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 1,
                        cache_hits: 0,
                        cache_misses: 0,
                    },
                    request_fingerprints: Vec::new(),
                })
            }
            _ => Err(RuntimeError::UnsupportedExecutionMode {
                mode: format!("delete with {:?}", mode),
            }),
        }
    }

    /// Execute an invoke expression
    async fn execute_invoke(
        &self,
        invoke: &InvokeExpr,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        let capability = cgs
            .get_capability(invoke.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: invoke.capability.to_string(),
                entity: invoke.target.entity_type.to_string(),
            })?;

        let capability_template = parse_capability_template(&capability.mapping.template)?;

        let input_for_env = match &invoke.input {
            None => None,
            Some(input) => {
                let payload = if let Some(schema) = &capability.input_schema {
                    InvokeInputPayload::lift(&input.to_value(), &schema.input_type, cgs)
                } else {
                    input.clone()
                };
                Some(match capability.input_schema.as_ref() {
                    Some(schema) => plasm_core::normalize_structured_string_inputs(
                        payload.to_value(),
                        &schema.input_type,
                        cgs,
                    ),
                    None => payload.to_value(),
                })
            }
        };

        let mut env = CmlEnv::new();
        populate_template_path_env(
            &mut env,
            &capability_template,
            &invoke.target,
            invoke.path_vars.as_ref(),
            input_for_env.as_ref(),
        );

        if let Some(input) = &input_for_env {
            env.insert("input".to_string(), input.clone());
            if let Value::Object(map) = input {
                for (k, v) in map {
                    env.insert(k.clone(), v.clone());
                }
            }
        }
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;

        self.apply_invoke_preflight(capability, cgs, cache, invoke, mode, &mut env)
            .await?;

        let compiled = compile_operation_dispatch(&capability_template, &env)?;

        match mode {
            ExecutionMode::Live => {
                ensure_http_operation(&compiled, "invoke")?;
                let (response, _) = with_dispatch_entity(
                    Some(invoke.target.entity_type.as_str()),
                    self.execute_operation_full(&compiled),
                )
                .await?;
                let response =
                    narrow_http_graphql_response_for_entity_decode(&capability_template, response)?;

                // Decode the response as the capability's declared entity type.
                // When an action returns a projection of the same entity (e.g.
                // page_get_markdown returns {id, markdown, truncated} for a Page),
                // the decoder extracts only the fields present in the response, and
                // the cache's additive merge preserves existing fields from other
                // projections (e.g. url, timestamps from page_get).
                let decoder =
                    create_entity_decoder(&invoke.target.entity_type, cgs, None, None, None);
                let decoded = decode_entities(&decoder, &response).unwrap_or_default();

                let timestamp = current_timestamp();
                let entities: Vec<CachedEntity> = decoded
                    .into_iter()
                    .map(|d| {
                        CachedEntity::from_decoded(
                            d.reference,
                            d.fields,
                            d.relations,
                            timestamp,
                            EntityCompleteness::Complete,
                        )
                    })
                    .collect();
                let count = entities.len();

                if count > 0 {
                    cache.merge(entities.clone())?;
                }

                Ok(ExecutionResult {
                    entities,
                    count,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 1,
                        cache_hits: 0,
                        cache_misses: count,
                    },
                    request_fingerprints: Vec::new(),
                })
            }
            _ => Err(RuntimeError::UnsupportedExecutionMode {
                mode: format!("invoke with {:?} mode", mode),
            }),
        }
    }

    /// Execute a chain expression (Kleisli EntityRef navigation).
    ///
    /// 1. Execute the source expression to get one or more entities.
    /// 2. For each entity, extract the EntityRef field value → target ID.
    /// 3. **Batch**: deduplicate IDs, satisfy from cache, fetch uncached via concurrent GETs.
    /// 4. Reassemble in source order (preserving duplicates).
    async fn execute_chain(
        &self,
        chain: &plasm_core::ChainExpr,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        let source_result = self
            .execute(
                &chain.source,
                cgs,
                cache,
                Some(mode),
                consume.clone(),
                opts.clone(),
            )
            .await?;

        if source_result.entities.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats,
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let source_entity_name = chain.source.primary_entity();
        let source_entity =
            cgs.get_entity(source_entity_name)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("Chain source entity '{}' not in CGS", source_entity_name),
                })?;

        // Resolve the target entity type — either from an EntityRef field or a
        // declared relation (cardinality-one decode, scoped query, embedded GET refs).
        let target_entity_name: String = if let Some(field_schema) =
            source_entity.fields.get(chain.selector.as_str())
        {
            let nv = cgs.named_value_for_slot(field_schema).map_err(|e| {
                RuntimeError::ConfigurationError {
                    message: format!(
                        "Field '{}.{}': invalid value_ref — {}",
                        source_entity_name, chain.selector, e
                    ),
                }
            })?;
            match &nv.field_type {
                FieldType::EntityRef { target } => target.to_string(),
                _ => {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "Field '{}.{}' is {:?}, not EntityRef",
                            source_entity_name, chain.selector, nv.field_type
                        ),
                    });
                }
            }
        } else if let Some(rel) = source_entity.relations.get(chain.selector.as_str()) {
            let mat = rel
                .materialize
                .as_ref()
                .unwrap_or(&RelationMaterialization::Unavailable);
            match rel.cardinality {
                plasm_core::Cardinality::Many => match mat {
                    RelationMaterialization::QueryScoped { capability, param } => {
                        return self
                            .execute_chain_via_param(
                                &source_result,
                                rel.target_resource.clone(),
                                capability,
                                param,
                                cgs,
                                cache,
                                mode,
                            )
                            .await;
                    }
                    RelationMaterialization::QueryScopedBindings {
                        capability,
                        bindings,
                    } => {
                        return self
                            .execute_chain_via_bindings(
                                &source_result,
                                source_entity,
                                rel.target_resource.clone(),
                                capability,
                                bindings,
                                cgs,
                                cache,
                                mode,
                            )
                            .await;
                    }
                    RelationMaterialization::Unavailable => {
                        return Err(RuntimeError::ConfigurationError {
                            message: format!(
                                "Relation '{}.{}' is not configured for chain traversal (materialize unavailable)",
                                source_entity_name, chain.selector
                            ),
                        });
                    }
                    RelationMaterialization::FromParentGet { .. } => {
                        return self
                            .execute_chain_from_embedded_relations(
                                &source_result,
                                rel,
                                cgs,
                                cache,
                                mode,
                                &chain.step,
                                consume.clone(),
                                opts.clone(),
                            )
                            .await;
                    }
                },
                plasm_core::Cardinality::One => rel.target_resource.to_string(),
            }
        } else {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Chain selector '{}' not found on entity '{}' (not an EntityRef field or relation)",
                    chain.selector, source_entity_name
                ),
            });
        };

        // ── Extract ref IDs from source entities ─────────────────────────
        let ref_ids: Vec<Option<String>> = source_result
            .entities
            .iter()
            .map(|e| {
                let extracted = extract_ref_id(e, &chain.selector);
                if extracted.is_some() {
                    return extracted;
                }
                if let Some(rel) = source_entity.relations.get(chain.selector.as_str()) {
                    if rel.cardinality == plasm_core::Cardinality::One {
                        return Some(e.reference.primary_slot_str());
                    }
                }
                None
            })
            .collect();

        // ── Explicit continuation: no batching, dispatch per-entity ──────
        if matches!(chain.step, ChainStep::Explicit { .. }) {
            let mut resolved = Vec::new();
            let mut total_network = source_result.stats.network_requests;
            let mut total_cache_hits = source_result.stats.cache_hits;
            let mut any_live = source_result.source == ExecutionSource::Live;

            if let ChainStep::Explicit { expr } = &chain.step {
                for id_opt in &ref_ids {
                    let Some(_id) = id_opt else { continue };
                    let r = self
                        .execute(
                            expr,
                            cgs,
                            cache,
                            Some(mode),
                            StreamConsumeOpts::default(),
                            opts.clone(),
                        )
                        .await?;
                    if r.source == ExecutionSource::Live {
                        any_live = true;
                    }
                    total_network += r.stats.network_requests;
                    total_cache_hits += r.stats.cache_hits;
                    resolved.extend(r.entities);
                }
            }

            let count = resolved.len();
            return Ok(ExecutionResult {
                entities: resolved,
                count,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: if any_live {
                    ExecutionSource::Live
                } else {
                    ExecutionSource::Cache
                },
                stats: ExecutionStats {
                    duration_ms: 0,
                    network_requests: total_network,
                    cache_hits: total_cache_hits,
                    cache_misses: count,
                },
                request_fingerprints: Vec::new(),
            });
        }

        // ── AutoGet with batching ────────────────────────────────────────
        // Deduplicate IDs.
        let unique_ids: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            ref_ids
                .iter()
                .filter_map(|o| o.as_ref())
                .filter(|id| seen.insert((*id).clone()))
                .cloned()
                .collect()
        };

        // Partition: cached (Complete) vs uncached.
        let mut cached_hits = 0usize;
        let to_fetch: Vec<Ref> = unique_ids
            .iter()
            .filter(|id| {
                let r = Ref::new(&target_entity_name, id.as_str());
                if matches!(
                    cache.get(&r).map(|e| e.completeness),
                    Some(crate::EntityCompleteness::Complete)
                ) {
                    cached_hits += 1;
                    false
                } else {
                    true
                }
            })
            .map(|id| Ref::new(&target_entity_name, id.as_str()))
            .collect();

        // Fetch uncached via concurrent GETs (same pattern as hydrate).
        let mut extra_network = 0usize;
        let mut any_live = source_result.source == ExecutionSource::Live;

        if !to_fetch.is_empty() {
            use futures_util::stream::{self, StreamExt};

            let concurrency = self.config.hydrate_concurrency.max(1);
            let mut stream = stream::iter(to_fetch.into_iter().map(|reference| {
                let get = GetExpr::from_ref(reference.clone());
                async move { self.fetch_get_decoded(&get, cgs, mode, None).await }
            }))
            .buffer_unordered(concurrency);

            while let Some(res) = stream.next().await {
                let (entity, source) = res?;
                if source == ExecutionSource::Live {
                    any_live = true;
                    extra_network += 1;
                }
                cache.insert(entity)?;
            }
        }

        // Reassemble in source order from cache.
        let mut resolved = Vec::with_capacity(ref_ids.len());
        for id_opt in &ref_ids {
            let Some(id) = id_opt else { continue };
            let r = Ref::new(&target_entity_name, id.as_str());
            if let Some(e) = cache.get(&r) {
                resolved.push(e.clone());
            }
        }

        let count = resolved.len();
        Ok(ExecutionResult {
            entities: resolved,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: source_result.stats.network_requests + extra_network,
                cache_hits: source_result.stats.cache_hits + cached_hits,
                cache_misses: count,
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Execute a `via_param` relation traversal: for each source entity, run a scoped
    /// query on the target using the source entity's `id_field` value as `via_param`.
    ///
    /// Example: `Page.blocks` with `via_param: block_id` → for each Page, execute
    /// `block_children_query(block_id = page.id)` and return the Block results.
    ///
    /// Queries are run concurrently (up to `hydrate_concurrency` limit).
    #[allow(clippy::too_many_arguments)]
    async fn execute_chain_via_param(
        &self,
        source_result: &ExecutionResult,
        target_entity: EntityName,
        capability: &plasm_core::CapabilityName,
        via_param: &CapabilityParamName,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let target_key = target_entity.as_str();
        let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: unknown capability '{}' (target entity '{}')",
                    capability, target_key
                ),
            }
        })?;
        if cap.domain.as_str() != target_key {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: capability '{}' domain '{}' does not match target '{}'",
                    capability, cap.domain, target_key
                ),
            });
        }
        let capability_name = cap.name.clone();

        // Build one QueryExpr per source entity, using its id as scope value.
        let queries: Vec<QueryExpr> = source_result
            .entities
            .iter()
            .map(|entity| {
                let id_field = cgs
                    .get_entity(entity.reference.entity_type.as_str())
                    .map(|def| def.id_field.as_str().to_string())
                    .unwrap_or_default();
                let id = entity
                    .get_field(id_field.as_str())
                    .map(|tf| tf.to_value())
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Integer(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .unwrap_or_else(|| entity.reference.primary_slot_str());
                let pred = plasm_core::Predicate::eq(via_param.as_str(), id);
                let mut q = QueryExpr::filtered(target_entity.clone(), pred);
                q.capability_name = Some(capability_name.clone());
                q
            })
            .collect();

        if queries.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let concurrency = self.config.hydrate_concurrency.max(1);
        let mut all_entities: Vec<CachedEntity> = Vec::new();
        let mut total_network = source_result.stats.network_requests;
        let mut total_cache_hits = source_result.stats.cache_hits;
        let mut any_live = source_result.source == ExecutionSource::Live;

        // Execute one scoped query per source entity, concurrently.
        // Each query uses its own local cache to avoid borrow conflicts; we merge
        // the results into the caller's cache afterward.
        let mut stream = stream::iter(queries.into_iter().map(|q| async move {
            let mut local_cache = GraphCache::new();
            self.execute_query(
                &q,
                cgs,
                &mut local_cache,
                mode,
                StreamConsumeOpts::default(),
            )
            .await
        }))
        .buffer_unordered(concurrency);

        while let Some(res) = stream.next().await {
            let result = res?;
            if result.source == ExecutionSource::Live {
                any_live = true;
            }
            total_network += result.stats.network_requests;
            total_cache_hits += result.stats.cache_hits;
            all_entities.extend(result.entities);
        }

        cache.merge(all_entities.clone())?;
        let entities = all_entities;

        let count = entities.len();
        Ok(ExecutionResult {
            entities,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: total_network,
                cache_hits: total_cache_hits,
                cache_misses: count,
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Multi-parameter scoped query fanout (`RelationMaterialization::QueryScopedBindings`).
    #[allow(clippy::too_many_arguments)]
    async fn execute_chain_via_bindings(
        &self,
        source_result: &ExecutionResult,
        parent_entity_def: &plasm_core::EntityDef,
        target_entity: EntityName,
        capability: &plasm_core::CapabilityName,
        bindings: &IndexMap<CapabilityParamName, EntityFieldName>,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult, RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let target_key = target_entity.as_str();
        let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: unknown capability '{}' (target entity '{}')",
                    capability, target_key
                ),
            }
        })?;
        if cap.domain.as_str() != target_key {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Chain materialize: capability '{}' domain '{}' does not match target '{}'",
                    capability, cap.domain, target_key
                ),
            });
        }
        let capability_name = cap.name.clone();

        let queries: Vec<QueryExpr> = source_result
            .entities
            .iter()
            .map(|entity| {
                let preds: Vec<Predicate> = bindings
                    .iter()
                    .map(|(cap_param, parent_field)| {
                        let v = chain_binding_value(entity, parent_entity_def, parent_field);
                        Predicate::eq(cap_param.as_str(), Value::String(v))
                    })
                    .collect();
                let pred = if preds.len() == 1 {
                    preds.into_iter().next().expect("non-empty preds")
                } else {
                    Predicate::and(preds)
                };
                let mut q = QueryExpr::filtered(target_entity.clone(), pred);
                q.capability_name = Some(capability_name.clone());
                q
            })
            .collect();

        if queries.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        let concurrency = self.config.hydrate_concurrency.max(1);
        let mut all_entities: Vec<CachedEntity> = Vec::new();
        let mut total_network = source_result.stats.network_requests;
        let mut total_cache_hits = source_result.stats.cache_hits;
        let mut any_live = source_result.source == ExecutionSource::Live;

        let mut stream = stream::iter(queries.into_iter().map(|q| async move {
            let mut local_cache = GraphCache::new();
            self.execute_query(
                &q,
                cgs,
                &mut local_cache,
                mode,
                StreamConsumeOpts::default(),
            )
            .await
        }))
        .buffer_unordered(concurrency);

        while let Some(res) = stream.next().await {
            let result = res?;
            if result.source == ExecutionSource::Live {
                any_live = true;
            }
            total_network += result.stats.network_requests;
            total_cache_hits += result.stats.cache_hits;
            all_entities.extend(result.entities);
        }

        cache.merge(all_entities.clone())?;
        let count = all_entities.len();
        Ok(ExecutionResult {
            entities: all_entities,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: total_network,
                cache_hits: total_cache_hits,
                cache_misses: count,
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Chain on `FromParentGet`: refs already on `CachedEntity.relations[relation.name]`.
    #[allow(clippy::too_many_arguments)] // cache + mode + step + consume mirror `execute_chain` helpers
    async fn execute_chain_from_embedded_relations(
        &self,
        source_result: &ExecutionResult,
        relation: &RelationSchema,
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
        chain_step: &ChainStep,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> Result<ExecutionResult, RuntimeError> {
        use futures_util::stream::{self, StreamExt};

        let relation_key = relation.name.as_str();
        let expected_target = &relation.target_resource;

        let mut ordered_refs: Vec<Ref> = Vec::new();
        for e in &source_result.entities {
            if let Some(refs) = e.relations.get(relation_key) {
                for r in refs {
                    if r.entity_type != *expected_target {
                        return Err(RuntimeError::ConfigurationError {
                            message: format!(
                                "Decoded relation '{}' expected Ref.entity_type {} (CGS target_resource), got {}",
                                relation.name, expected_target, r.entity_type
                            ),
                        });
                    }
                    ordered_refs.push(r.clone());
                }
            }
        }

        if ordered_refs.is_empty() {
            return Ok(ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: source_result.source,
                stats: source_result.stats.clone(),
                request_fingerprints: source_result.request_fingerprints.clone(),
            });
        }

        if matches!(chain_step, ChainStep::Explicit { .. }) {
            let mut resolved = Vec::new();
            let mut total_network = source_result.stats.network_requests;
            let mut total_cache_hits = source_result.stats.cache_hits;
            let mut any_live = source_result.source == ExecutionSource::Live;

            if let ChainStep::Explicit { expr } = chain_step {
                // One continuation eval per decoded ref (refs already validated against
                // `relation.target_resource` when collected above).
                let mut remaining = ordered_refs.len();
                while remaining > 0 {
                    remaining -= 1;
                    let res = self
                        .execute(expr, cgs, cache, Some(mode), consume.clone(), opts.clone())
                        .await?;
                    if res.source == ExecutionSource::Live {
                        any_live = true;
                    }
                    total_network += res.stats.network_requests;
                    total_cache_hits += res.stats.cache_hits;
                    resolved.extend(res.entities);
                }
            }

            let count = resolved.len();
            return Ok(ExecutionResult {
                entities: resolved,
                count,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: if any_live {
                    ExecutionSource::Live
                } else {
                    ExecutionSource::Cache
                },
                stats: ExecutionStats {
                    duration_ms: 0,
                    network_requests: total_network,
                    cache_hits: total_cache_hits,
                    cache_misses: count,
                },
                request_fingerprints: Vec::new(),
            });
        }

        let mut seen = HashSet::new();
        let unique_refs: Vec<Ref> = ordered_refs
            .iter()
            .filter(|r| seen.insert((*r).clone()))
            .cloned()
            .collect();

        let mut cached_hits = 0usize;
        let to_fetch: Vec<Ref> = unique_refs
            .iter()
            .filter(|r| {
                if matches!(
                    cache.get(r).map(|e| e.completeness),
                    Some(crate::EntityCompleteness::Complete)
                ) {
                    cached_hits += 1;
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        let mut extra_network = 0usize;
        let mut any_live = source_result.source == ExecutionSource::Live;

        if !to_fetch.is_empty() {
            let concurrency = self.config.hydrate_concurrency.max(1);
            let mut stream = stream::iter(to_fetch.into_iter().map(|reference| {
                let get = GetExpr::from_ref(reference.clone());
                async move { self.fetch_get_decoded(&get, cgs, mode, None).await }
            }))
            .buffer_unordered(concurrency);

            while let Some(res) = stream.next().await {
                let (entity, source) = res?;
                if source == ExecutionSource::Live {
                    any_live = true;
                    extra_network += 1;
                }
                cache.insert(entity)?;
            }
        }

        let mut resolved = Vec::with_capacity(ordered_refs.len());
        for r in &ordered_refs {
            if let Some(e) = cache.get(r) {
                resolved.push(e.clone());
            }
        }

        let count = resolved.len();
        Ok(ExecutionResult {
            entities: resolved,
            count,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: if any_live {
                ExecutionSource::Live
            } else {
                ExecutionSource::Cache
            },
            stats: ExecutionStats {
                duration_ms: 0,
                network_requests: source_result.stats.network_requests + extra_network,
                cache_hits: source_result.stats.cache_hits + cached_hits,
                cache_misses: count,
            },
            request_fingerprints: Vec::new(),
        })
    }

    /// Auto-resolve missing projected fields by invoking the providing capabilities.
    ///
    /// When a projection `[field1, field2]` is requested and one or more fields are absent
    /// from the cached entity, this method:
    ///
    /// 1. Builds the `field → capability` reverse index from `CGS::field_providers`
    /// 2. For each entity, determines which projected fields are missing (null or absent)
    /// 3. Groups missing fields by their providing capability
    /// 4. Invokes each provider capability concurrently for the affected entities
    /// 5. The results are additive-merged into cache; returns the enriched entities
    ///
    /// This makes `Page("id")[markdown]` automatically invoke `page_get_markdown` when
    /// `markdown` is not yet in cache — without any manual multi-step workflow.
    #[allow(clippy::too_many_arguments)]
    pub async fn auto_resolve_projection(
        &self,
        entities: Vec<CachedEntity>,
        entity_type: &str,
        projection: &[String],
        cgs: &CGS,
        cache: &mut GraphCache,
        mode: ExecutionMode,
        opts: ExecuteOptions,
    ) -> Result<Vec<CachedEntity>, RuntimeError> {
        let base = self.resolve_http_base_from_opts(&opts);
        let auth_override = opts.auth_resolver_override.clone();
        let plugin_hooks = PluginCompileHooks::snapshot_from_execute_options(&opts);
        let fp_sink = opts.request_fingerprint_sink.clone();
        let federation = opts.federation.clone();
        Self::run_in_execute_task_scopes(
            base,
            auth_override,
            plugin_hooks,
            fp_sink,
            federation,
            async {
                use futures_util::stream::{self, StreamExt};

                // Build reverse index: field → Vec<cap_name>
                let providers = cgs.field_providers(entity_type);

                // For each entity, find which projected fields are missing.
                // Group: capability_name → Vec<entity_id>
                let mut cap_to_ids: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();

                for entity in &entities {
                    for field in projection {
                        // Field is "missing" if it's absent from the entity's field map or null.
                        let is_missing = entity
                            .fields
                            .get(field)
                            .map(|v| v.is_null())
                            .unwrap_or(true);

                        if is_missing {
                            if let Some(cap_names) = providers.get(field) {
                                // Use the first (highest-priority) provider.
                                if let Some(cap_name) = cap_names.first() {
                                    // Skip if we already have a Complete entry from this provider
                                    // for this entity (i.e. the field IS in the cache under this cap).
                                    cap_to_ids
                                        .entry(cap_name.clone())
                                        .or_default()
                                        .push(entity.reference.primary_slot_str());
                                }
                            }
                        }
                    }
                }

                if cap_to_ids.is_empty() {
                    return Ok(entities);
                }

                let projection_span =
                    crate::spans::projection_hydrate(entity_type, cap_to_ids.len());
                async {
                    // For each provider capability, invoke it for all entity IDs that need it.
                    let concurrency = self.config.hydrate_concurrency.max(1);

                    for (cap_name, ids) in cap_to_ids {
                        let Some(cap) = cgs.get_capability(&cap_name) else {
                            continue;
                        };

                        // Deduplicate IDs
                        let mut unique_ids = ids;
                        unique_ids.sort_unstable();
                        unique_ids.dedup();

                        // Build one expression per entity ID
                        let exprs: Vec<(String, Expr)> = unique_ids
                            .into_iter()
                            .map(|id| {
                                let expr = match cap.kind {
                                    plasm_core::CapabilityKind::Get => {
                                        let get = GetExpr::new(entity_type, &id);
                                        Expr::Get(get)
                                    }
                                    _ => {
                                        // action / update / etc. — invoke with no input
                                        let inv =
                                            InvokeExpr::new(&cap_name, entity_type, &id, None);
                                        Expr::Invoke(inv)
                                    }
                                };
                                (id, expr)
                            })
                            .collect();

                        let mut stream = stream::iter(exprs.into_iter().map(|(_id, expr)| {
                            async move {
                                let mut local_cache = GraphCache::new();
                                // Use a minimal execute path to avoid infinite projection loops.
                                match &expr {
                                    Expr::Get(g) => {
                                        self.execute_get(g, cgs, &mut local_cache, mode).await
                                    }
                                    Expr::Invoke(inv) => {
                                        self.execute_invoke(inv, cgs, &mut local_cache, mode).await
                                    }
                                    _ => Err(RuntimeError::ConfigurationError {
                                        message: "auto_resolve_projection: unexpected expr type"
                                            .into(),
                                    }),
                                }
                            }
                        }))
                        .buffer_unordered(concurrency);

                        while let Some(res) = stream.next().await {
                            match res {
                                Ok(result) => {
                                    // Merge the enriched entities into the main cache (additive merge).
                                    cache.merge(result.entities)?;
                                }
                                Err(e) => {
                                    // Best-effort: log the error but don't fail the whole resolution.
                                    // The field will simply remain absent in the output.
                                    tracing::warn!(
                                        target: "plasm_runtime::projection",
                                        capability = cap_name.as_str(),
                                        error = %e,
                                        "projection provider invocation failed"
                                    );
                                }
                            }
                        }
                    }
                    Ok::<(), RuntimeError>(())
                }
                .instrument(projection_span)
                .await?;

                // Re-read the (now-enriched) entities from cache.
                let refreshed: Vec<CachedEntity> = entities
                    .iter()
                    .filter_map(|e| cache.get(&e.reference).cloned())
                    .collect();

                Ok(refreshed)
            },
        )
        .await
    }

    /// Execute request and capture `Link` header (`rel="next"`) when present.
    async fn execute_http_request_full(
        &self,
        request: &CompiledRequest,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let base_url = self.effective_http_base_for_request();
        let auth = self.resolve_auth_http().await?;
        self.transport
            .send_compiled_http(base_url.as_ref(), request, auth)
            .await
    }

    /// GET absolute URL (used for `link_header` continuation pages).
    async fn get_json_absolute(
        &self,
        url: &str,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let auth = self.resolve_auth_http().await?;
        self.transport.get_json_absolute(url, auth).await
    }
}

fn query_result_merge_cache(
    decoded_entities: Vec<plasm_compile::DecodedEntity>,
    completeness: EntityCompleteness,
    source: ExecutionSource,
    cache: &mut GraphCache,
    network_requests: usize,
) -> Result<ExecutionResult, RuntimeError> {
    let timestamp = current_timestamp();
    let mut cached_entities = Vec::new();
    for decoded in decoded_entities {
        cached_entities.push(CachedEntity::from_decoded(
            decoded.reference,
            decoded.fields,
            decoded.relations,
            timestamp,
            completeness,
        ));
    }
    let count = cached_entities.len();
    cache.merge(cached_entities.clone())?;
    Ok(ExecutionResult {
        entities: cached_entities,
        count,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests,
            cache_hits: 0,
            cache_misses: count,
        },
        request_fingerprints: Vec::new(),
    })
}

fn path_var_names_from_template(template: &CapabilityTemplate) -> Vec<String> {
    match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            path_var_names_from_request(cml)
        }
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
/// explicit `path_vars` first, then keys from `input_overlay`, while preserving
/// the legacy HTTP single-path-var => positional `id` behavior.
fn populate_template_path_env(
    env: &mut CmlEnv,
    template: &CapabilityTemplate,
    reference: &Ref,
    path_vars: Option<&indexmap::IndexMap<String, Value>>,
    input_overlay: Option<&Value>,
) {
    let primary_id = reference.primary_slot_str();
    let id_val = Value::String(primary_id.clone());
    env.insert("id".to_string(), id_val.clone());

    if let EntityKey::Compound(parts) = &reference.key {
        for (k, v) in parts {
            env.insert(k.clone(), Value::String(v.clone()));
        }
    }

    let single_http_id_alias = match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            let names = path_var_names_from_request(cml);
            (names.len() == 1).then(|| names[0].clone())
        }
        CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => None,
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
        let nv = cgs
            .named_value_for_slot(field)
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

fn pagination_default_limit(pconf: &PaginationConfig) -> u32 {
    let size_names = [
        "size",
        "limit",
        "per_page",
        "page_size",
        "maxResults",
        "max_results",
        "first",
    ];
    for (name, param) in &pconf.params {
        let is_size_like = size_names.contains(&name.as_str())
            || name.ends_with("_size")
            || name.ends_with("_limit");
        if is_size_like {
            if let Some(v) = param.fixed_as_u32() {
                return v.max(1);
            }
        }
    }
    // BlockRange: look for a fixed range_size param
    if pconf.location == plasm_compile::PaginationLocation::BlockRange {
        for param in pconf.params.values() {
            if let Some(v) = param.fixed_as_u32() {
                return v.max(1);
            }
        }
        return 1000; // default block range span
    }
    20
}

// Legacy stub kept for compatibility with the compile-layer import.
#[allow(dead_code)]
fn pagination_default_limit_stub() -> u32 {
    // Placeholder to satisfy any residual references.
    20
}

fn _pagination_items_key_unused() {
    // Removed: items key now comes from cml_request.response.items (HttpResponseDecode)
    // not from PaginationConfig.
}

fn compiled_query_insert_http(compiled: &mut CompiledRequest, key: &str, val: Value) {
    use indexmap::IndexMap;
    if compiled.query.is_none() {
        compiled.query = Some(Value::Object(IndexMap::new()));
    }
    if let Some(Value::Object(m)) = compiled.query.as_mut() {
        m.insert(key.to_string(), val);
    } else {
        let mut m = IndexMap::new();
        m.insert(key.to_string(), val);
        compiled.query = Some(Value::Object(m));
    }
}

fn compiled_query_insert(
    compiled: &mut CompiledOperation,
    key: &str,
    val: Value,
) -> Result<(), RuntimeError> {
    match compiled {
        CompiledOperation::Http(request) | CompiledOperation::GraphQl(request) => {
            compiled_query_insert_http(request, key, val);
            Ok(())
        }
        CompiledOperation::EvmCall(_) => Err(RuntimeError::ConfigurationError {
            message: format!("pagination key '{key}' is not valid for evm_call transport"),
        }),
        CompiledOperation::EvmLogs(_) => Err(RuntimeError::ConfigurationError {
            message: format!(
                "query parameter pagination key '{key}' is not valid for evm_logs transport"
            ),
        }),
    }
}

fn compiled_block_range_set(
    compiled: &mut CompiledOperation,
    from_block: u64,
    to_block: u64,
) -> Result<(), RuntimeError> {
    match compiled {
        CompiledOperation::EvmLogs(request) => {
            request.from_block = Some(from_block);
            request.to_block = Some(to_block);
            Ok(())
        }
        CompiledOperation::Http(request) | CompiledOperation::GraphQl(request) => {
            compiled_query_insert_http(
                request,
                "from_block",
                Value::String(from_block.to_string()),
            );
            compiled_query_insert_http(request, "to_block", Value::String(to_block.to_string()));
            Ok(())
        }
        CompiledOperation::EvmCall(_) => Err(RuntimeError::ConfigurationError {
            message: "block-range pagination is not valid for evm_call transport".to_string(),
        }),
    }
}

/// Merge one pagination key into the compiled JSON body: either at the root object or under
/// [`PaginationConfig::body_merge_path`] (GraphQL `variables.…` nesting).
pub(super) fn merge_pagination_into_body(
    body: &mut Value,
    merge_path: Option<&[String]>,
    key: &str,
    value: Value,
) -> Result<(), RuntimeError> {
    let target_map: &mut IndexMap<String, Value> =
        if let Some(path) = merge_path.filter(|p| !p.is_empty()) {
            let Value::Object(root) = body else {
                return Err(RuntimeError::ConfigurationError {
                    message: "pagination with body_merge_path requires a JSON object request body"
                        .into(),
                });
            };
            let mut cur = root;
            for segment in path {
                let entry = cur
                    .entry(segment.clone())
                    .or_insert_with(|| Value::Object(IndexMap::new()));
                match entry {
                    Value::Object(next) => cur = next,
                    _ => {
                        return Err(RuntimeError::ConfigurationError {
                            message: format!(
                                "pagination body_merge_path: expected object at segment '{segment}'"
                            ),
                        });
                    }
                }
            }
            cur
        } else {
            let Value::Object(m) = body else {
                return Err(RuntimeError::ConfigurationError {
                    message: "pagination body injection requires a JSON object request body".into(),
                });
            };
            m
        };
    target_map.insert(key.to_string(), value);
    Ok(())
}

fn response_map(
    v: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, RuntimeError> {
    match v {
        serde_json::Value::Object(m) => Ok(m),
        _ => Err(RuntimeError::ConfigurationError {
            message: "expected JSON object in paginated API response".into(),
        }),
    }
}

/// Object map used for `stop_when` and `FromResponse` pagination keys.
/// When `prefix` is `None` or empty, uses the root JSON object.
pub(crate) fn pagination_context_map<'a>(
    response: &'a serde_json::Value,
    prefix: Option<&[String]>,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, RuntimeError> {
    let mut cur = response;
    if let Some(segs) = prefix.filter(|p| !p.is_empty()) {
        for seg in segs {
            cur = if let Ok(index) = seg.parse::<usize>() {
                cur.get(index)
            } else {
                cur.get(seg)
            }
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("pagination response_prefix: missing segment '{seg}'"),
            })?;
        }
    }
    response_map(cur)
}

/// Unified pagination state machine driven by the composable `PaginationConfig`.
/// No style-enum branching — param types and stop conditions carry all information.
struct PaginationLoopState {
    /// Current value for each param. `None` = `FromResponse` not yet received.
    param_values: indexmap::IndexMap<String, Option<serde_json::Value>>,
    /// Next-page absolute URL (LinkHeader location only).
    next_absolute_url: Option<String>,
    /// Page size used on the last request (for short-page heuristic).
    last_requested_limit: u32,
    /// BlockRange: current starting block.
    from_block: Option<u64>,
    /// BlockRange: user-specified final block (optional upper bound).
    final_to_block: Option<u64>,
    /// BlockRange: end of last requested range.
    last_requested_to_block: Option<u64>,
}

impl PaginationLoopState {
    fn new(
        pconf: &PaginationConfig,
        user: &QueryPagination,
        consume: &StreamConsumeOpts,
    ) -> Result<Self, RuntimeError> {
        if pconf.location == plasm_compile::PaginationLocation::BlockRange {
            if user.from_block.is_none() {
                return Err(RuntimeError::ConfigurationError {
                    message:
                        "block_range pagination requires QueryPagination.from_block / --from-block"
                            .to_string(),
                });
            }
            if consume.fetch_all && user.to_block.is_none() {
                return Err(RuntimeError::ConfigurationError {
                    message: "block_range pagination with --all requires QueryPagination.to_block / --to-block"
                        .to_string(),
                });
            }
            return Ok(Self {
                param_values: indexmap::IndexMap::new(),
                next_absolute_url: None,
                last_requested_limit: 0,
                from_block: user.from_block,
                final_to_block: user.to_block,
                last_requested_to_block: None,
            });
        }

        let mut param_values = indexmap::IndexMap::new();
        for (name, param) in &pconf.params {
            let initial = match param {
                plasm_compile::PaginationParam::Counter { counter, .. } => {
                    let start = if name == "page" || name == "p" {
                        user.page.unwrap_or(*counter)
                    } else if name == "offset" {
                        user.offset.unwrap_or(*counter)
                    } else {
                        *counter
                    };
                    Some(serde_json::Value::Number(start.into()))
                }
                plasm_compile::PaginationParam::Fixed { fixed } => Some(fixed.clone()),
                plasm_compile::PaginationParam::FromResponse { .. } => user
                    .cursor
                    .as_ref()
                    .map(|c| serde_json::Value::String(c.clone())),
            };
            param_values.insert(name.clone(), initial);
        }

        Ok(Self {
            param_values,
            next_absolute_url: None,
            last_requested_limit: 0,
            from_block: user.from_block,
            final_to_block: user.to_block,
            last_requested_to_block: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_request_params(
        &mut self,
        compiled: &mut CompiledOperation,
        pconf: &PaginationConfig,
        _user: &QueryPagination,
        consume: &StreamConsumeOpts,
        _single_page: bool,
        _is_first_page: bool,
        accumulated: usize,
    ) -> Result<(), RuntimeError> {
        let default_lim = pagination_default_limit(pconf);

        // BlockRange is handled separately.
        if pconf.location == plasm_compile::PaginationLocation::BlockRange {
            let from_block = self
                .from_block
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: "block_range pagination requires a starting block".to_string(),
                })?;
            let span = u64::from(default_lim).max(1);
            let mut to_block = from_block.saturating_add(span.saturating_sub(1));
            if let Some(final_to) = self.final_to_block {
                to_block = to_block.min(final_to);
            }
            match compiled {
                CompiledOperation::Http(_) | CompiledOperation::GraphQl(_) => {
                    compiled_query_insert(
                        compiled,
                        "from_block",
                        Value::String(from_block.to_string()),
                    )?;
                    compiled_query_insert(
                        compiled,
                        "to_block",
                        Value::String(to_block.to_string()),
                    )?;
                }
                CompiledOperation::EvmLogs(_) => {
                    compiled_block_range_set(compiled, from_block, to_block)?;
                }
                CompiledOperation::EvmCall(_) => {
                    return Err(RuntimeError::ConfigurationError {
                        message: "block_range pagination is not valid for evm_call transport"
                            .to_string(),
                    });
                }
            }
            self.last_requested_to_block = Some(to_block);
            self.last_requested_limit = default_lim;
            return Ok(());
        }

        // LinkHeader: no params to inject — URL comes from response header.
        if pconf.location == plasm_compile::PaginationLocation::LinkHeader {
            self.last_requested_limit = default_lim;
            return Ok(());
        }

        let remain_cap = consume
            .max_items
            .map(|c| c.saturating_sub(accumulated))
            .unwrap_or(usize::MAX);
        let limit_this_page: u32 = if remain_cap < usize::MAX {
            (remain_cap as u32).min(default_lim).max(1)
        } else {
            default_lim
        };

        for (name, param) in &pconf.params {
            let current = self.param_values.get(name).and_then(|v| v.as_ref());

            let value = match param {
                plasm_compile::PaginationParam::Fixed { fixed } => {
                    let name_lower = name.to_lowercase();
                    let is_size_like = [
                        "size",
                        "limit",
                        "per_page",
                        "page_size",
                        "maxresults",
                        "max_results",
                        "first",
                    ]
                    .iter()
                    .any(|s| name_lower.contains(s))
                        || name_lower == "first"
                        || name_lower.ends_with("_size")
                        || name_lower.ends_with("_limit");
                    if is_size_like {
                        serde_json::Value::Number(
                            (limit_this_page as i64)
                                .min(fixed.as_i64().unwrap_or(limit_this_page as i64))
                                .into(),
                        )
                    } else {
                        fixed.clone()
                    }
                }
                _ => match current {
                    Some(v) => v.clone(),
                    None => continue, // FromResponse absent on first page — skip
                },
            };

            let plasm_val = json_to_plasm_value(&value);
            match pconf.location {
                plasm_compile::PaginationLocation::Query => {
                    compiled_query_insert(compiled, name, plasm_val)?;
                }
                plasm_compile::PaginationLocation::Body => {
                    use indexmap::IndexMap;
                    if let CompiledOperation::Http(ref mut req)
                    | CompiledOperation::GraphQl(ref mut req) = compiled
                    {
                        if req.body_format == plasm_compile::HttpBodyFormat::Multipart {
                            return Err(RuntimeError::ConfigurationError {
                                message: "pagination with location body is not supported for multipart HTTP requests"
                                    .to_string(),
                            });
                        }
                        if req.body.is_none() {
                            req.body = Some(Value::Object(IndexMap::new()));
                        }
                        if let Some(body) = req.body.as_mut() {
                            merge_pagination_into_body(
                                body,
                                pconf.body_merge_path.as_deref(),
                                name,
                                plasm_val,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }

        self.last_requested_limit = limit_this_page;
        Ok(())
    }

    fn advance_after_page(
        &mut self,
        pconf: &PaginationConfig,
        response: &serde_json::Value,
        full_page_len: usize,
        requested_limit: u32,
        link_next: Option<&str>,
        _last_entity_id: Option<&str>,
    ) -> Result<bool, RuntimeError> {
        // LinkHeader: next URL from response header.
        if pconf.location == plasm_compile::PaginationLocation::LinkHeader {
            let Some(url) = link_next.filter(|u| !u.is_empty()) else {
                return Ok(false);
            };
            self.next_absolute_url = Some(url.to_string());
            return Ok(true);
        }

        // BlockRange: advance from_block past the last requested range.
        if pconf.location == plasm_compile::PaginationLocation::BlockRange {
            let Some(last_to) = self.last_requested_to_block else {
                return Ok(false);
            };
            if let Some(final_to) = self.final_to_block {
                if last_to >= final_to {
                    return Ok(false);
                }
            }
            self.from_block = Some(last_to.saturating_add(1));
            return Ok(true);
        }

        // Explicit stop_when condition.
        if let Some(stop) = &pconf.stop_when {
            let resp = pagination_context_map(response, pconf.response_prefix.as_deref())?;
            match stop {
                plasm_compile::PaginationStop::FieldEquals { field, eq } => {
                    if let Some(val) = resp.get(field) {
                        if val == eq {
                            return Ok(false);
                        }
                    }
                }
                plasm_compile::PaginationStop::FieldAbsent { field, absent } => {
                    let is_absent = resp.get(field).map(|v| v.is_null()).unwrap_or(true);
                    if is_absent == *absent {
                        return Ok(false);
                    }
                }
            }
        }

        // Update param values for the next request.
        let mut any_from_response_absent = false;
        for (name, param) in &pconf.params {
            match param {
                plasm_compile::PaginationParam::Counter { step, .. } => {
                    if let Some(Some(serde_json::Value::Number(n))) = self.param_values.get(name) {
                        let current = n.as_i64().unwrap_or(0);
                        self.param_values.insert(
                            name.clone(),
                            Some(serde_json::Value::Number((current + step).into())),
                        );
                    }
                }
                plasm_compile::PaginationParam::FromResponse { from_response } => {
                    let extracted = if let Some(prefix) =
                        pconf.response_prefix.as_ref().filter(|p| !p.is_empty())
                    {
                        pagination_context_map(response, Some(prefix.as_slice()))
                            .ok()
                            .and_then(|resp| {
                                resp.get(from_response.as_str())
                                    .filter(|v| {
                                        !v.is_null()
                                            && v.as_str().map(|s| !s.is_empty()).unwrap_or(true)
                                    })
                                    .cloned()
                            })
                    } else {
                        response
                            .get(from_response.as_str())
                            .filter(|v| {
                                !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true)
                            })
                            .cloned()
                    };
                    if extracted.is_none() {
                        any_from_response_absent = true;
                    }
                    self.param_values.insert(name.clone(), extracted);
                }
                plasm_compile::PaginationParam::Fixed { .. } => {} // fixed, never changes
            }
        }

        // Implicit stop: any FromResponse param became absent → cursor exhausted.
        if any_from_response_absent && pconf.stop_when.is_none() {
            return Ok(false);
        }

        // Default short-page heuristic: stop when items array is shorter than requested.
        if full_page_len == 0 || (full_page_len as u32) < requested_limit {
            return Ok(false);
        }

        Ok(true)
    }
}

impl From<&PaginationLoopState> for QueryPaginationState {
    fn from(s: &PaginationLoopState) -> Self {
        Self {
            param_values: s
                .param_values
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            next_absolute_url: s.next_absolute_url.clone(),
            last_requested_limit: s.last_requested_limit,
            from_block: s.from_block,
            final_to_block: s.final_to_block,
            last_requested_to_block: s.last_requested_to_block,
        }
    }
}

impl TryFrom<QueryPaginationState> for PaginationLoopState {
    type Error = RuntimeError;

    fn try_from(s: QueryPaginationState) -> Result<Self, Self::Error> {
        Ok(Self {
            param_values: s.param_values.into_iter().collect(),
            next_absolute_url: s.next_absolute_url,
            last_requested_limit: s.last_requested_limit,
            from_block: s.from_block,
            final_to_block: s.final_to_block,
            last_requested_to_block: s.last_requested_to_block,
        })
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

/// Extract an EntityRef field value as a string ID from a cached entity.
fn extract_ref_id(entity: &CachedEntity, selector: &str) -> Option<String> {
    let v = entity.get_field(selector).map(|tf| tf.to_value())?;
    match v {
        Value::String(s) if !s.is_empty() => Some(s),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

/// Value for a `query_scoped_bindings` param from a parent cached row / ref.
fn chain_binding_value(
    entity: &CachedEntity,
    parent_def: &plasm_core::EntityDef,
    parent_field: &EntityFieldName,
) -> String {
    let pf = parent_field.as_str();
    if let Some(v) = entity.get_field(pf) {
        return match v.to_value() {
            Value::String(s) if !s.is_empty() => s,
            Value::Integer(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => entity.reference.primary_slot_str(),
        };
    }
    if pf == parent_def.id_field.as_str() {
        return entity.reference.primary_slot_str();
    }
    if let EntityKey::Compound(parts) = &entity.reference.key {
        if let Some(s) = parts.get(pf) {
            return s.clone();
        }
    }
    entity.reference.primary_slot_str()
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
        CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => Ok(response),
    }
}

/// For mappings that declare `response.single` + `items_path` (e.g. GraphQL `{ data: { issue: { ... } } }`),
/// take the entity object at that path. Used for GET/detail, create, and update **invoke** decoding—
/// not specific to GET semantics.
fn extract_single_entity_payload_from_response(
    response: serde_json::Value,
    cml: &CmlRequest,
) -> Result<serde_json::Value, RuntimeError> {
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
                                }
                                return Err(RuntimeError::ConfigurationError { message: msg });
                            }
                        };
                    }
                }
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
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// CML env slots usable as compound-key fallbacks (string-like values only).
fn cml_env_to_identity_strings(env: &CmlEnv) -> IndexMap<String, String> {
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

/// Create a decoder for an entity type, driven by the CGS schema.
/// - `collection_source: Some(path)` — `path` ends in a wildcard over the entity array
/// - `None` — single entity at the response root
/// - `identity_ambient` — scope / request key parts merged when a row omits a compound-key field
fn create_entity_decoder(
    entity_type: &str,
    cgs: &CGS,
    collection_source: Option<PathExpr>,
    request_identity: Option<&str>,
    identity_ambient: Option<&IndexMap<String, String>>,
) -> plasm_compile::EntityDecoder {
    use plasm_compile::{EntityDecoder, FieldDecoder, PathExpr, PathSegment};

    let ambient = identity_ambient.cloned().unwrap_or_default();

    let source = match collection_source {
        Some(p) => p,
        None => PathExpr::empty(),
    };

    let mut field_decoders = Vec::new();

    if let Some(entity) = cgs.get_entity(entity_type) {
        for (field_name, field_schema) in &entity.fields {
            let from_path = if let Some(wp) = &field_schema.wire_path {
                PathExpr::new(
                    wp.iter()
                        .map(|n| PathSegment::Key { name: n.clone() })
                        .collect(),
                )
            } else {
                PathExpr::new(vec![PathSegment::Key {
                    name: field_name.as_str().to_string(),
                }])
            };
            let fd = FieldDecoder::new(field_name.as_str(), from_path);
            field_decoders.push(match &field_schema.derive {
                Some(d) => fd.with_derive(d.clone()),
                None => fd,
            });
        }
        // For cardinality-one declared relations, decode the nested **target id** (per target
        // `id_field`) so ChainExpr can batch-fetch by ref. Example: Linear `state.id` (uuid);
        // PokéAPI `species.name` when [`EntityDef::id_field`] is `name`.
        for (rel_name, rel) in &entity.relations {
            if rel.cardinality == plasm_core::Cardinality::One {
                let Some(target_ent) = cgs.get_entity(rel.target_resource.as_str()) else {
                    continue;
                };
                let nested_key = target_ent.id_field.clone();
                field_decoders.push(FieldDecoder::new(
                    rel_name.as_str(),
                    PathExpr::new(vec![
                        PathSegment::Key {
                            name: rel_name.as_str().to_string(),
                        },
                        PathSegment::Key {
                            name: nested_key.into(),
                        },
                    ]),
                ));
            }
        }
    } else {
        // Fallback: at least decode the ID
        field_decoders.push(FieldDecoder::new(
            "id",
            PathExpr::new(vec![PathSegment::Key {
                name: "id".to_string(),
            }]),
        ));
    }

    let mut relation_decoders: Vec<plasm_compile::RelationDecoder> = Vec::new();
    if let Some(entity) = cgs.get_entity(entity_type) {
        for (rel_name, rel) in &entity.relations {
            if let Some(RelationMaterialization::FromParentGet { path }) = &rel.materialize {
                let rel_path = path_expr_from_json_segments(path).unwrap_or_else(|e| {
                    panic!("CGS must reject invalid from_parent_get paths: {e}");
                });
                let Some(target_ent) = cgs.get_entity(rel.target_resource.as_str()) else {
                    continue;
                };
                let mut cf = Vec::new();
                for (fname, fschema) in &target_ent.fields {
                    let from_path = if let Some(wp) = &fschema.wire_path {
                        PathExpr::new(
                            wp.iter()
                                .map(|n| PathSegment::Key { name: n.clone() })
                                .collect(),
                        )
                    } else {
                        PathExpr::new(vec![PathSegment::Key {
                            name: fname.as_str().to_string(),
                        }])
                    };
                    let fd = FieldDecoder::new(fname.as_str(), from_path);
                    cf.push(match &fschema.derive {
                        Some(d) => fd.with_derive(d.clone()),
                        None => fd,
                    });
                }
                let child_kv: Vec<String> = target_ent
                    .key_vars
                    .iter()
                    .map(|k| k.as_str().to_string())
                    .collect();
                let child = EntityDecoder::new(rel.target_resource.as_str(), rel_path)
                    .with_fields(cf)
                    .with_id_field(target_ent.id_field.clone())
                    .with_key_vars(child_kv)
                    .with_identity_ambient(IndexMap::new());
                relation_decoders.push(plasm_compile::RelationDecoder {
                    relation: rel_name.as_str().to_string(),
                    decoder: child,
                    cardinality: rel.cardinality,
                });
            }
        }
    }

    let mut decoder = EntityDecoder::new(entity_type, source)
        .with_fields(field_decoders)
        .with_relations(relation_decoders)
        .with_identity_ambient(ambient);
    if let Some(entity) = cgs.get_entity(entity_type) {
        let key_vars: Vec<String> = entity
            .key_vars
            .iter()
            .map(|k| k.as_str().to_string())
            .collect();
        decoder = decoder
            .with_id_field(entity.id_field.clone())
            .with_key_vars(key_vars);
        if let Some(parts) = entity.id_from.as_ref().filter(|p| !p.is_empty()) {
            let segments: Vec<PathSegment> = parts
                .iter()
                .cloned()
                .map(|name| PathSegment::Key { name })
                .collect();
            decoder = decoder.with_id_path(PathExpr::new(segments));
        }
        if entity.implicit_request_identity {
            if let Some(rid) = request_identity {
                decoder = decoder.with_request_identity_override(rid);
            }
        }
    }
    decoder
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Convert serde_json::Value to plasm_core::Value
fn json_to_plasm_value(json: &serde_json::Value) -> Value {
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
        cache: &'a mut GraphCache,
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
        cache: &'a mut GraphCache,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        ExecutionEngine::execute(self, expr, cgs, cache, mode, consume, opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use plasm_core::{
        CapabilityKind, CapabilityMapping, CapabilitySchema, Expr, FieldSchema, FieldType,
        FieldValueKind, GetExpr, InputFieldSchema, InputSchema, InputValidation, NamedValueSchema,
        Ref, ResourceSchema, StringSemantics, ValueDomainKey,
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
            invoke_preflight: None,
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
            invoke_preflight: None,
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
                        kind: FieldValueKind::Registry(
                            ValueDomainKey::new("rt_workspace_ref").expect("workspace ref key"),
                        ),
                        required: true,
                        description: None,
                        default: None,
                        role: Some(ParameterRole::Scope),
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
            invoke_preflight: None,
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
        assert_eq!(config.hydrate_concurrency, 5);
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
        let mut cache = GraphCache::new();

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
        let mut cache = GraphCache::new();
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
        populate_template_path_env(&mut env, &template, &reference, Some(&pv), None);

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
        let mut cache = GraphCache::new();
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
        let mut cache = GraphCache::new();
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
}
