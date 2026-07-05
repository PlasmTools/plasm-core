//! Memoized tool-model synthesis on the blocking compute pool.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use plasm_core::discovery::CatalogEntryMeta;
use plasm_core::schema::CGS;
use tokio::sync::Notify;

use crate::blocking_compute::{BlockingComputePool, ComputePoolError};
use crate::tool_model::{
    build_tool_model, normalize_tool_model_query, ToolModelBuildError, ToolModelQuery,
    ToolModelResponse,
};

const DEFAULT_TOOL_MODEL_CACHE_CAP: usize = 128;

#[derive(Clone, Debug, Eq)]
struct ToolModelCacheKey {
    entry_id: String,
    catalog_cgs_hash: String,
    focus: String,
    entity: Vec<String>,
}

impl PartialEq for ToolModelCacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.entry_id == other.entry_id
            && self.catalog_cgs_hash == other.catalog_cgs_hash
            && self.focus == other.focus
            && self.entity == other.entity
    }
}

impl Hash for ToolModelCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.entry_id.hash(state);
        self.catalog_cgs_hash.hash(state);
        self.focus.hash(state);
        self.entity.hash(state);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolModelServiceError {
    #[error(transparent)]
    Build(#[from] ToolModelBuildError),
    #[error(transparent)]
    Compute(#[from] ComputePoolError),
}

/// In-memory tool-model cache + blocking-pool dispatch.
pub struct ToolModelService {
    pool: Arc<BlockingComputePool>,
    cache: DashMap<ToolModelCacheKey, Arc<ToolModelResponse>>,
    /// Coalesces concurrent cache misses for the same key (Phoenix tool-model fan-out).
    inflight: DashMap<ToolModelCacheKey, Arc<Notify>>,
    cache_len: AtomicUsize,
    cache_cap: usize,
}

impl ToolModelService {
    #[must_use]
    pub fn new(pool: Arc<BlockingComputePool>) -> Self {
        Self {
            pool,
            cache: DashMap::new(),
            inflight: DashMap::new(),
            cache_len: AtomicUsize::new(0),
            cache_cap: tool_model_cache_cap(),
        }
    }

    pub fn clear_cache(&self) {
        self.cache.clear();
        self.inflight.clear();
        self.cache_len.store(0, Ordering::Relaxed);
    }

    pub async fn build(
        &self,
        cgs: Arc<CGS>,
        meta: CatalogEntryMeta,
        q: ToolModelQuery,
    ) -> Result<Arc<ToolModelResponse>, ToolModelServiceError> {
        let (focus, entity) = normalize_tool_model_query(&q)?;
        let key = ToolModelCacheKey {
            entry_id: meta.entry_id.clone(),
            catalog_cgs_hash: catalog_digest(&cgs, &meta),
            focus,
            entity,
        };

        if let Some(hit) = self.cache.get(&key) {
            return Ok(Arc::clone(hit.value()));
        }

        let leader_notify = loop {
            if let Some(hit) = self.cache.get(&key) {
                return Ok(Arc::clone(hit.value()));
            }
            match self.inflight.entry(key.clone()) {
                Entry::Occupied(entry) => {
                    let notify = Arc::clone(entry.get());
                    drop(entry);
                    notify.notified().await;
                }
                Entry::Vacant(entry) => {
                    let notify = Arc::new(Notify::new());
                    entry.insert(Arc::clone(&notify));
                    break notify;
                }
            }
        };

        let build_result = self
            .pool
            .run("build_tool_model", move || build_tool_model(cgs.as_ref(), &meta, &q))
            .await
            .map_err(ToolModelServiceError::Compute)
            .and_then(|r| r.map_err(ToolModelServiceError::Build));

        self.inflight.remove(&key);
        leader_notify.notify_waiters();

        match build_result {
            Ok(built) => {
                let arc = Arc::new(built);
                self.store_cache(key, Arc::clone(&arc));
                Ok(arc)
            }
            Err(e) => Err(e),
        }
    }

    fn store_cache(&self, key: ToolModelCacheKey, value: Arc<ToolModelResponse>) {
        if self.cache_len.load(Ordering::Relaxed) >= self.cache_cap {
            self.cache.clear();
            self.cache_len.store(0, Ordering::Relaxed);
        }
        if self.cache.insert(key, value).is_none() {
            self.cache_len.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn catalog_digest(cgs: &CGS, meta: &CatalogEntryMeta) -> String {
    if meta.catalog_cgs_hash.is_empty() {
        cgs.catalog_cgs_hash_hex()
    } else {
        meta.catalog_cgs_hash.clone()
    }
}

fn tool_model_cache_cap() -> usize {
    std::env::var("PLASM_TOOL_MODEL_CACHE_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n >= 8)
        .unwrap_or(DEFAULT_TOOL_MODEL_CACHE_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;
    use std::path::Path;

    #[tokio::test]
    async fn cache_hit_returns_same_arc() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let meta = CatalogEntryMeta {
            entry_id: "overshow".into(),
            label: "Overshow".into(),
            tags: vec![],
            aliases: vec![],
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let svc = ToolModelService::new(Arc::new(BlockingComputePool::with_max_inflight(2)));
        let q = ToolModelQuery {
            focus: "ALL".into(),
            entity: vec![],
        };
        let a = svc
            .build(Arc::clone(&cgs), meta.clone(), q.clone())
            .await
            .expect("first build");
        let b = svc.build(cgs, meta, q).await.expect("cache hit");
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn concurrent_miss_singleflight_builds_once() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let meta = CatalogEntryMeta {
            entry_id: "overshow".into(),
            label: "Overshow".into(),
            tags: vec![],
            aliases: vec![],
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
        };
        let svc = Arc::new(ToolModelService::new(Arc::new(
            BlockingComputePool::with_max_inflight(4),
        )));
        let q = ToolModelQuery {
            focus: "all".into(),
            entity: vec![],
        };

        let mut handles = Vec::new();
        for _ in 0..6 {
            let svc = Arc::clone(&svc);
            let cgs = Arc::clone(&cgs);
            let meta = meta.clone();
            let q = q.clone();
            handles.push(tokio::spawn(async move {
                svc.build(cgs, meta, q).await.expect("build")
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.expect("join"));
        }
        assert!(results.iter().all(|r| Arc::ptr_eq(&results[0], r)));
        assert_eq!(svc.cache_len.load(Ordering::Relaxed), 1);
    }
}
