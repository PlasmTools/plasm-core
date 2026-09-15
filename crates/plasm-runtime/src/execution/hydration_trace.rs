//! Select with `RUST_LOG=warn,plasm_runtime::hydration=trace`.
//! Values and error messages are
//! deliberately excluded; only field names/types and opaque correlation IDs are emitted.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
tokio::task_local! { static ATTEMPT: u64; }

pub(crate) fn enabled() -> bool {
    tracing::enabled!(target: "plasm_runtime::hydration", tracing::Level::TRACE)
}

pub(crate) async fn scope<T>(id: u64, future: impl std::future::Future<Output = T>) -> T {
    ATTEMPT.scope(id, future).await
}

pub(crate) fn next_id() -> Option<u64> {
    enabled().then(|| NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn active() -> bool {
    ATTEMPT.try_with(|_| ()).is_ok()
}

pub(crate) fn emit(stage: &str, facts: Value) {
    if let Ok(id) = ATTEMPT.try_with(|id| *id) {
        emit_for(id, stage, facts);
    }
}

pub(crate) fn emit_for(id: u64, stage: &str, facts: Value) {
    tracing::trace!(target: "plasm_runtime::hydration", attempt = id, stage, facts = %facts, "hydration boundary");
}

/// Schema-level shape only. Neither scalar values nor nested arbitrary map keys escape.
pub(crate) fn shape(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), summary(v)))
                .collect(),
        ),
        _ => summary(value),
    }
}
fn summary(value: &Value) -> Value {
    match value {
        Value::String(s) => json!({"type":"string", "bytes":s.len()}),
        Value::Array(a) => json!({"type":"array", "items":a.len()}),
        _ => json!({"type":kind(value)}),
    }
}
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(crate) fn failure(error: &crate::RuntimeError) -> Value {
    use crate::RuntimeError::*;
    match error {
        RequestError { status, .. } => json!({"kind":"request", "status":status}),
        DecodeError { .. } => json!({"kind":"decode"}),
        CacheError { .. } => json!({"kind":"cache"}),
        RateLimited { status, .. } => json!({"kind":"rate_limit", "status":status}),
        _ => json!({"kind":"other"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hydration_trace_shape_excludes_values() {
        assert_eq!(
            shape(&json!({"content":"secret", "token":"secret", "empty":"", "missing":null})),
            json!({"content":{"type":"string","bytes":6}, "token":{"type":"string","bytes":6}, "empty":{"type":"string","bytes":0}, "missing":{"type":"null"}})
        );
    }
    #[tokio::test]
    async fn hydration_trace_scope_isolated_and_restored() {
        let (a, b) = tokio::join!(
            scope(11, async {
                tokio::task::yield_now().await;
                ATTEMPT.with(|x| *x)
            }),
            scope(22, async {
                tokio::task::yield_now().await;
                ATTEMPT.with(|x| *x)
            })
        );
        assert_eq!((a, b), (11, 22));
        assert!(!active());
    }
}
