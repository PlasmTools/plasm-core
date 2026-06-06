//! Generic runtime schema overlay: execute `source` pipeline and merge projected entities.
//!
//! Wired at session open for HTTP execute ([`crate::http_execute`]), MCP `plasm_context`
//! (same execute path), federated catalog attach, and local [`plasm_repl`](../../plasm-repl).

use indexmap::IndexMap;
use plasm_core::{
    build_schema_overlay, overlay_bind_cache_suffix, overlay_collect_rows,
    overlay_merge_step_response, overlay_pipeline_cache_suffix, resolve_overlay_row_bind,
    SchemaOverlaySpec, CGS,
};
use plasm_runtime::{AuthResolver, ExecutionEngine, ExecutionMode, RuntimeError, SecretProvider};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const ENV_SCHEMA_OVERLAY_TTL_SECS: &str = "PLASM_SCHEMA_OVERLAY_TTL_SECS";
const DEFAULT_OVERLAY_TTL_SECS: u64 = 600;

type OverlayCacheKey = (String, String, String, String);
type OverlayCacheEntry = (Instant, Arc<CGS>);

fn overlay_cache() -> &'static Mutex<HashMap<OverlayCacheKey, OverlayCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<OverlayCacheKey, OverlayCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn overlay_ttl() -> Duration {
    env::var(ENV_SCHEMA_OVERLAY_TTL_SECS)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(DEFAULT_OVERLAY_TTL_SECS))
}

async fn fetch_overlay_source_response(
    engine: &ExecutionEngine,
    base: &CGS,
    capability: &str,
    http_base: &str,
    auth_resolver: Option<Arc<AuthResolver>>,
    mode: ExecutionMode,
    bind: Option<&IndexMap<String, String>>,
) -> Result<JsonValue, String> {
    engine
        .fetch_overlay_source_response(base, capability, http_base, auth_resolver, mode, bind)
        .await
        .map_err(|e: RuntimeError| e.to_string())
}

async fn fetch_overlay_merged_response(
    engine: &ExecutionEngine,
    base: &CGS,
    spec: &SchemaOverlaySpec,
    http_base: &str,
    auth_resolver: Option<Arc<AuthResolver>>,
    mode: ExecutionMode,
) -> Result<(JsonValue, String), String> {
    let source = &spec.source;
    if source.steps.is_empty() {
        let bind = if source.bind.is_empty() {
            None
        } else {
            Some(&source.bind)
        };
        let response = fetch_overlay_source_response(
            engine,
            base,
            source.capability.as_str(),
            http_base,
            auth_resolver.clone(),
            mode,
            bind,
        )
        .await?;
        let suffix = overlay_bind_cache_suffix(&source.bind);
        return Ok((response, suffix));
    }

    let mut collections: IndexMap<String, Vec<JsonValue>> = IndexMap::new();
    let mut merged = serde_json::json!({});
    let mut pipeline_responses: Vec<JsonValue> = Vec::new();

    for step in &source.steps {
        if let Some(collect_name) = &step.collect {
            let items_path = step.items_path.as_ref().expect("validated items_path");
            let response = fetch_overlay_source_response(
                engine,
                base,
                step.capability.as_str(),
                http_base,
                auth_resolver.clone(),
                mode,
                None,
            )
            .await?;
            pipeline_responses.push(response.clone());
            let rows = overlay_collect_rows(&response, items_path)?;
            collections.insert(collect_name.clone(), rows);
            continue;
        }

        let for_each_name = step.for_each.as_ref().expect("validated for_each");
        let rows = collections
            .get(for_each_name)
            .ok_or_else(|| format!("overlay pipeline missing collect '{for_each_name}'"))?;
        let merge = step.merge.as_ref().expect("validated merge");
        for row in rows {
            let bind =
                resolve_overlay_row_bind(&step.bind, row, None, None).map_err(|e| e.to_string())?;
            let response = fetch_overlay_source_response(
                engine,
                base,
                step.capability.as_str(),
                http_base,
                auth_resolver.clone(),
                mode,
                Some(&bind),
            )
            .await?;
            pipeline_responses.push(response.clone());
            overlay_merge_step_response(&mut merged, merge, &response)
                .map_err(|e| e.to_string())?;
        }
    }

    let suffix = overlay_pipeline_cache_suffix(&pipeline_responses);
    Ok((merged, suffix))
}

/// When `cgs.schema_overlay` is set, fetch workspace schema via the declared source pipeline
/// and return a merged CGS (with optional TTL cache keyed by entry, base hash, backend, and pipeline digest).
pub async fn resolve_schema_overlay_cgs(
    base: Arc<CGS>,
    engine: &ExecutionEngine,
    mode: ExecutionMode,
    http_base: &str,
    auth_resolver: Option<Arc<AuthResolver>>,
    entry_id: &str,
) -> Result<Arc<CGS>, String> {
    let spec = match base.schema_overlay_spec() {
        Some(s) => s,
        None => return Ok(base),
    };

    let base_hash = base.catalog_cgs_hash_hex();
    let (response, cache_suffix) = fetch_overlay_merged_response(
        engine,
        base.as_ref(),
        spec,
        http_base,
        auth_resolver.clone(),
        mode,
    )
    .await?;

    let cache_key = (
        entry_id.to_string(),
        base_hash.clone(),
        http_base.trim().to_string(),
        cache_suffix,
    );
    let ttl = overlay_ttl();
    if ttl.as_secs() > 0 {
        if let Ok(guard) = overlay_cache().lock() {
            if let Some((fetched_at, cached)) = guard.get(&cache_key) {
                if fetched_at.elapsed() <= ttl {
                    return Ok(cached.clone());
                }
            }
        }
    }

    let overlay = build_schema_overlay(spec, base.as_ref(), &response)?;
    let merged = base
        .with_overlay(overlay)
        .map_err(|e| format!("schema overlay merge: {e}"))?;
    let effective = Arc::new(merged);

    if ttl.as_secs() > 0 {
        if let Ok(mut guard) = overlay_cache().lock() {
            guard.insert(cache_key, (Instant::now(), effective.clone()));
        }
    }

    Ok(effective)
}

/// Host-path overlay resolve (HTTP execute, MCP, federated attach): engine + outbound secrets.
pub async fn resolve_schema_overlay_for_host(
    engine: &ExecutionEngine,
    mode: ExecutionMode,
    secret_provider: Arc<dyn SecretProvider>,
    base: Arc<CGS>,
    http_backend: &str,
    entry_id: &str,
) -> Result<Arc<CGS>, String> {
    let auth = base
        .auth
        .clone()
        .map(|scheme| Arc::new(AuthResolver::new(scheme, secret_provider)));
    resolve_schema_overlay_cgs(base, engine, mode, http_backend, auth, entry_id).await
}

/// Local REPL / dev CLI path: overlay via process env secrets.
pub async fn resolve_schema_overlay_for_local(
    engine: &ExecutionEngine,
    mode: ExecutionMode,
    base: Arc<CGS>,
    http_backend: &str,
    entry_id: &str,
) -> Result<Arc<CGS>, String> {
    let auth = base
        .auth
        .clone()
        .map(|scheme| Arc::new(AuthResolver::from_env(scheme)));
    resolve_schema_overlay_cgs(base, engine, mode, http_backend, auth, entry_id).await
}

/// Log merged overlay stats to stderr (REPL / local dev).
pub fn eprint_schema_overlay_status(cgs: &CGS) {
    if let Some(ref hash) = cgs.schema_overlay_hash {
        eprintln!(
            "schema overlay: {} typed entities merged (digest {hash})",
            cgs.schema_overlay_scope_index.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_none_passes_through() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let base = Arc::new(CGS::new());
        let engine = ExecutionEngine::new(Default::default()).expect("engine");
        let out = rt
            .block_on(resolve_schema_overlay_cgs(
                base.clone(),
                &engine,
                ExecutionMode::Live,
                "https://x.fibery.io",
                None,
                "fibery",
            ))
            .unwrap();
        assert!(Arc::ptr_eq(&base, &out));
    }
}
