//! ExecutionEngine construction and primary execute entrypoints.

use super::*;
use crate::http_resilience::{HttpResiliencePolicy, ResilientHttpTransport};
use crate::http_transport::{HttpTransport, ReqwestHttpTransport};
use crate::materialization::SessionMaterialization;
use crate::preflight::{apply_preflight_steps, PreflightInvoke};
use crate::{AuthResolver, RuntimeError};
use plasm_core::{type_check_expr, type_check_expr_federated, Expr, CGS};
use std::sync::Arc;
use tracing::Instrument;

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
    /// When set, typecheck and HTTP dispatch use per-entity owning [`plasm_core::CgsContext`].
    pub federation: Option<std::sync::Arc<plasm_core::FederationDispatch>>,
    /// When set, agent-core preflight already type-checked and placeholder-gated this expression.
    pub preflight: Option<plasm_core::PreflightToken>,
    /// When set, CML compilation for outbound HTTP sees reserved `plasm_execute_*` env keys
    /// ([`merge_plasm_execute_session_env`]) plus Proof `proof_base_token` as `base_token`
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
    pub(crate) transport: Arc<dyn HttpTransport>,
    pub(crate) config: ExecutionConfig,
    pub(crate) replay_store: Option<crate::MemoryReplayStore>,
    /// Optional authentication resolver injected on every outbound HTTP request.
    pub(crate) auth_resolver: Option<AuthResolver>,
}

impl ExecutionEngine {
    pub(crate) fn resolve_http_base_from_opts(&self, opts: &ExecuteOptions) -> Arc<str> {
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

    /// Task locals for fingerprint sink, HTTP base URL, auth override (one nested region).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_in_execute_task_scopes<Fut, T>(
        base: Arc<str>,
        auth_override: Option<Arc<AuthResolver>>,
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
                                EXECUTION_AUTH_RESOLVER
                                    .scope(auth_override, async move {
                                        EXECUTION_CANCEL
                                            .scope(cancel, async move {
                                                EXECUTION_ROWS_PROGRESS
                                                    .scope(rows_progress, async move {
                                                        EXECUTION_HTTP_BASE.scope(base, fut).await
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

    pub(crate) fn effective_http_base_for_request(&self) -> Arc<str> {
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

    /// Prompt rendering / symbol expansion settings shared with teaching table and `wire_surface_for_parse`.
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
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!(
                "plasm-runtime/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com)"
            ))
            .timeout(std::time::Duration::from_secs(config.timeout_seconds))
            .pool_max_idle_per_host(per_host);
        // Opt out of macOS SCDynamicStore / system proxy probes (and env proxies).
        // Tests and locked-down agents set `PLASM_HTTP_NO_SYSTEM_PROXY=1`.
        if std::env::var_os("PLASM_HTTP_NO_SYSTEM_PROXY").is_some_and(|v| {
            matches!(
                v.to_str().unwrap_or(""),
                "1" | "true" | "TRUE" | "yes" | "YES"
            )
        }) {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|e| RuntimeError::RequestError {
            message: format!("Failed to create HTTP client: {e}"),
            attempts: 1,
            status: None,
            body: None,
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
        let mapping = cap.mapping.as_ref().ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!(
                "schema overlay source capability '{capability_name}' has no CML mapping"
            ),
        })?;
        let template = parse_capability_template(&mapping.template.0).map_err(|e| {
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
    pub(crate) async fn execute_with_replay(
        &self,
        compiled: &CompiledOperation,
        mode: ExecutionMode,
        mat: Option<&mut SessionMaterialization>,
    ) -> Result<(serde_json::Value, ExecutionSource), RuntimeError> {
        let (json, _link, source) = self.execute_with_replay_full(compiled, mode, mat).await?;
        Ok((json, source))
    }

    /// Like [`execute_with_replay`], but also returns `Link: ...; rel="next"` for CML `link_header` pagination (live only).
    pub(crate) async fn execute_with_replay_full(
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

    pub(crate) async fn execute_operation_full(
        &self,
        operation: &CompiledOperation,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.execute_operation_full_inner(operation)
            .instrument(crate::spans::execute_operation())
            .await
    }

    pub(crate) async fn execute_operation_full_inner(
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

    pub(crate) fn evm_rpc_url(&self) -> Result<&str, RuntimeError> {
        self.config
            .base_url
            .as_deref()
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: "EVM transport requires ExecutionConfig.base_url to be set to an RPC URL"
                    .to_string(),
            })
    }

    /// Resolves credentials for **EVM** RPC requests only (ignores per-session HTTP override).
    pub(crate) async fn resolve_auth(&self) -> Result<Option<crate::ResolvedAuth>, RuntimeError> {
        match &self.auth_resolver {
            Some(resolver) => resolver.resolve().await.map(Some),
            None => Ok(None),
        }
    }

    /// Resolves credentials for **HTTP** requests: per-session override when set, else engine resolver.
    pub(crate) async fn resolve_auth_http(
        &self,
    ) -> Result<Option<crate::ResolvedAuth>, RuntimeError> {
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
            let fp_sink = opts.request_fingerprint_sink.clone();
            let federation = opts.federation.clone();
            let execute_session = opts.execute_session.clone();
            let cancel = opts.cancel.clone();
            let rows_progress = opts.rows_progress.clone();
            let mut result = Self::run_in_execute_task_scopes(
                base,
                auth_override,
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
}
