//! Execute session material, CML session env merge, and dispatch helpers.

use super::*;

/// Resolve the capability that backs a [`QueryExpr`] (delegates to [`plasm_core::resolve_query_capability`]).
pub(crate) fn resolve_query_capability<'a>(
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
#[derive(Clone, Debug)]
pub struct ExecuteSessionMaterial {
    pub prompt_hash: String,
    pub session_id: String,
    pub catalog_revision: String,
    /// Typed request recipes pinned to `catalog_revision` before program execution.
    pub compiled_catalog: Arc<plasm_compile::CompiledCatalog>,
    pub credential_store: Option<Arc<dyn crate::credentials::SessionCredentialStore>>,
    /// Pinned HTTP(S) transport origin for this execute row (session backend override).
    pub transport_origin: Option<String>,
    /// UI / browse deeplink origin; defaults to [`Self::transport_origin`] when unset.
    pub ui_origin: Option<String>,
    /// MCP connect binding wire values for the active catalog row (merged as `bind_<wire>` CML env).
    pub catalog_bind: Option<indexmap::IndexMap<String, String>>,
    /// Secret-safe tail of the last action-`provides` `access_token` (HTTP-1 login compare).
    pub login_access_token_tail: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl ExecuteSessionMaterial {
    #[must_use]
    pub fn empty_login_access_token_tail() -> std::sync::Arc<std::sync::Mutex<Option<String>>> {
        std::sync::Arc::new(std::sync::Mutex::new(None))
    }

    pub fn note_login_access_token(&self, token: &str) {
        let tail = crate::http_auth_failure::credential_tail(
            crate::http_auth_failure::token_secret(token),
        );
        if tail.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.login_access_token_tail.lock() {
            *guard = Some(tail);
        }
    }
}

pub(crate) fn compiled_capability_template(
    capability: &CapabilitySchema,
) -> Result<CapabilityTemplate, RuntimeError> {
    EXECUTION_COMPILED_CATALOG
        .try_with(|compiled| {
            compiled
                .capability(capability.name.as_str())
                .cloned()
                .map_err(RuntimeError::from)
        })
        .map_err(|_| RuntimeError::ConfigurationError {
            message: "execution requires a pinned compiled catalog scope".into(),
        })?
}

pub(crate) fn compiled_conflict_rules(
    capability: &CapabilitySchema,
) -> Result<Vec<plasm_core::ConflictRule>, RuntimeError> {
    EXECUTION_COMPILED_CATALOG
        .try_with(|compiled| {
            compiled
                .conflict_rules(capability.name.as_str())
                .map(<[_]>::to_vec)
                .map_err(RuntimeError::from)
        })
        .map_err(|_| RuntimeError::ConfigurationError {
            message: "execution requires a pinned compiled catalog scope".into(),
        })?
}

/// Reserved CML env key: 64-char lowercase hex (rendered teaching prompt digest for the row).
pub const CML_ENV_PLASM_EXECUTE_PROMPT_HASH: &str = "plasm_execute_prompt_hash";
/// Reserved CML env key: 32-char lowercase hex UUID (simple form) for the execute row.
pub const CML_ENV_PLASM_EXECUTE_SESSION_ID: &str = "plasm_execute_session_id";

/// teaching prompts use bare `$` as a fill-in cue; it must not reach HTTP/EVM transport.
pub(crate) fn reject_domain_placeholder_in_executable(expr: &Expr) -> Result<(), RuntimeError> {
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
    let mut coverage = ResultCoverage::Unknown;
    let mut operations = OperationLedger::empty();
    while let Some(item) = stream.next().await {
        let page = item?;
        last_has_more = page.has_more;
        coverage = page.coverage;
        if page.pagination_resume.is_some() {
            last_resume = page.pagination_resume.clone();
        }
        operations.merge(&page.operations);
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
        coverage,
        pagination_resume: last_resume,
        paging_handle: None,
        source: if any_live {
            ExecutionSource::Live
        } else {
            ExecutionSource::Replay
        },
        stats,
        request_fingerprints: Vec::new(),
        operations,
    })
}

pub(crate) async fn graph_spill_page_and_trim_hot(
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

/// Session material for the current execute task (view ambient scope injection).
pub(crate) fn try_current_execute_session_material(
) -> Option<std::sync::Arc<ExecuteSessionMaterial>> {
    EXECUTION_EXECUTE_SESSION
        .try_with(|s| s.clone())
        .ok()
        .flatten()
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

pub(crate) async fn with_dispatch_entity<Fut, T>(entity: Option<&str>, fut: Fut) -> T
where
    Fut: std::future::Future<Output = T> + Send,
{
    EXECUTION_DISPATCH_ENTITY
        .scope(entity.map(|s| s.to_string()), fut)
        .await
}

/// Append a request fingerprint (hex) when a fingerprint sink is active; collapses consecutive duplicates.
pub(crate) fn append_request_fingerprint(hex: String) {
    let _ = EXECUTION_FINGERPRINT_SINK.try_with(|holder| {
        if let Some(m) = holder {
            let mut v = m.lock().unwrap_or_else(|e| e.into_inner());
            if v.last().map(|s| s.as_str()) != Some(hex.as_str()) {
                v.push(hex);
            }
        }
    });
}

pub(crate) fn compile_operation_dispatch(
    template: &CapabilityTemplate,
    env: &CmlEnv,
) -> Result<CompiledOperation, RuntimeError> {
    compile_operation(template, env).map_err(|e| RuntimeError::CmlError { source: e })
}

pub(crate) fn compile_query_dispatch(
    query: &QueryExpr,
    cgs: &CGS,
) -> Result<Option<BackendFilter>, RuntimeError> {
    compile_query(query, cgs).map_err(|e| RuntimeError::CompilationError { source: e })
}
