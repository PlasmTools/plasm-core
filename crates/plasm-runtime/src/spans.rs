//! Semantic tracing spans for `plasm-runtime`.
//!
//! Names follow `plasm_runtime.<domain>.<operation>` so traces stay stable when execution
//! code is reorganized across types and files.

use tracing::Span;

/// Outbound compiled HTTP request (method + URL length; avoid full URL cardinality in span names).
#[inline]
pub(crate) fn http_compiled_request(method: &'static str, url_len: usize) -> Span {
    tracing::debug_span!(
        "plasm_runtime.http.compiled_request",
        http.method = method,
        url_len = url_len,
    )
}

/// Absolute URL GET (pagination / link continuations).
#[inline]
pub(crate) fn http_absolute_get(url_len: usize) -> Span {
    tracing::debug_span!("plasm_runtime.http.absolute_get", url_len = url_len)
}

/// Resilient retry loop around outbound HTTP (record `attempt` as the loop advances).
#[inline]
pub(crate) fn http_retry() -> Span {
    tracing::debug_span!("plasm_runtime.http.retry", attempt = tracing::field::Empty,)
}

/// Execute a query expression (materialize stream).
#[inline]
pub(crate) fn execute_query() -> Span {
    tracing::debug_span!("plasm_runtime.execute.query")
}

/// Execute a get expression.
#[inline]
pub(crate) fn execute_get() -> Span {
    tracing::debug_span!("plasm_runtime.execute.get")
}

/// Execute a compiled operation (HTTP / GraphQL / EVM).
#[inline]
pub(crate) fn execute_operation() -> Span {
    tracing::debug_span!("plasm_runtime.execute.operation")
}

/// Hydration pass that invokes provider capabilities to fill projected fields.
#[inline]
pub(crate) fn projection_hydrate(entity_type: &str, provider_count: usize) -> Span {
    tracing::debug_span!(
        "plasm_runtime.projection.hydrate",
        entity_type = entity_type,
        provider_group_count = provider_count,
    )
}

/// Spill one paginated graph page to durable storage and trim the in-process hot cache.
#[inline]
pub(crate) fn graph_page_spill(page_index: usize, entity_count: usize) -> Span {
    tracing::debug_span!(
        "plasm_runtime.graph.page_spill",
        page_index = page_index,
        entity_count = entity_count,
    )
}
