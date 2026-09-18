//! Conditional backend limits shared by all engine branches at HTTP dispatch.
use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

type Limits = Mutex<HashMap<(String, String), (usize, Weak<Semaphore>)>>;
static LIMITS: OnceLock<Limits> = OnceLock::new();

fn semaphore(origin: String, entry: String, cap: usize) -> Result<Arc<Semaphore>, RuntimeError> {
    let mut limits = LIMITS.get_or_init(Default::default).lock().map_err(|_| {
        RuntimeError::ConfigurationError {
            message: "backend HTTP limiter lock poisoned".into(),
        }
    })?;
    // Only active or waiting requests keep a pool alive. Discard idle origins
    // rather than retaining every backend ever visited by a long-lived host.
    limits.retain(|_, (_, pool)| pool.strong_count() > 0);
    let key = (origin, entry);
    if let Some((stored_cap, pool)) = limits.get(&key) {
        if let Some(pool) = pool.upgrade() {
            if *stored_cap != cap {
                return Err(RuntimeError::ConfigurationError {
                    message: "conflicting HTTP concurrency limits for the same backend".into(),
                });
            }
            return Ok(pool);
        }
    }
    let pool = Arc::new(Semaphore::new(cap));
    limits.insert(key, (cap, Arc::downgrade(&pool)));
    Ok(pool)
}

impl ExecutionEngine {
    pub(super) async fn acquire_backend_http_permit(
        &self,
        destination: &str,
    ) -> Result<Option<OwnedSemaphorePermit>, RuntimeError> {
        let entry = EXECUTION_COMPILED_CATALOG
            .try_with(|catalog| catalog.entry_id().map(str::to_owned))
            .ok()
            .flatten();
        let Some(entry) = entry else { return Ok(None) };
        let Some(cap) = self
            .config
            .backend_max_inflight
            .get(&entry)
            .copied()
            .filter(|cap| *cap > 0)
        else {
            return Ok(None);
        };
        let origin = url::Url::parse(destination)
            .map_err(|_| RuntimeError::ConfigurationError {
                message: "invalid HTTP backend limiter destination".into(),
            })?
            .origin()
            .ascii_serialization();
        let timeout = crate::http_resilience::ResilientHttpTransport::semaphore_acquire_timeout();
        let pool = semaphore(origin.clone(), entry, cap)?;
        let permit = tokio::time::timeout(timeout, pool.acquire_owned())
            .await
            .map_err(|_| RuntimeError::RateLimited {
                status: 429,
                host: origin,
                retry_after: Some(timeout),
                attempts: 0,
                message: "HTTP concurrency queue timeout waiting for backend".into(),
            })?
            .map_err(|_| RuntimeError::ConfigurationError {
                message: "backend HTTP limiter closed".into(),
            })?;
        Ok(Some(permit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct ObservedTransport {
        active: AtomicUsize,
        peak: AtomicUsize,
    }
    impl ObservedTransport {
        async fn observe(&self) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(count, Ordering::SeqCst);
            tokio::task::yield_now().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok((serde_json::json!([]), None))
        }
    }
    #[async_trait::async_trait]
    impl crate::HttpTransport for ObservedTransport {
        async fn send_compiled_http(
            &self,
            _: &str,
            _: &CompiledRequest,
            _: Option<crate::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            self.observe().await
        }
        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<crate::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            self.observe().await
        }
    }

    proptest! {
        #[test]
        fn backend_limit_bounds_independent_batches(cap in 1usize..5, batches in 2usize..6, width in 1usize..9, capped in proptest::bool::ANY) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                let origin = format!("https://{}.test", uuid::Uuid::new_v4());
                let mut cgs = plasm_core::loader::load_schema_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix")).unwrap();
                cgs.bind_registry_entry_id("fixture");
                let catalog = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
                let observed = Arc::new(ObservedTransport::default());
                let engine = Arc::new(ExecutionEngine::new_with_transport(ExecutionConfig {
                    base_url: Some(origin.clone()),
                    backend_max_inflight: if capped { HashMap::from([("fixture".into(), cap)]) } else { HashMap::new() },
                    ..Default::default()
                }, observed.clone(), None));
                let mut tasks = Vec::new();
                for i in 0..batches * width {
                    let engine = engine.clone();
                    let catalog = catalog.clone();
                    let origin = origin.clone();
                    tasks.push(tokio::spawn(EXECUTION_COMPILED_CATALOG.scope(catalog, async move {
                        if i % 2 == 0 {
                            let request = CompiledRequest {
                                credential: None, method: plasm_compile::HttpMethod::Get,
                                path: "/children".into(), query: None, body: None,
                                body_format: plasm_compile::HttpBodyFormat::Json,
                                multipart: None, headers: None,
                            };
                            engine.execute_http_request_full(&request).await.unwrap();
                        } else {
                            engine.get_json_absolute(&format!("{origin}/next")).await.unwrap();
                        }
                    })));
                }
                for task in tasks { task.await.unwrap(); }
                if capped {
                    assert!(observed.peak.load(Ordering::SeqCst) <= cap);
                } else {
                    assert!(observed.peak.load(Ordering::SeqCst) > 1, "unconfigured backends must remain concurrent");
                }
                assert_eq!(observed.active.load(Ordering::SeqCst), 0);
                if capped {
                    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                    let engine = engine.clone();
                    let destination = origin.clone();
                    let waiting = tokio::spawn(EXECUTION_COMPILED_CATALOG.scope(catalog.clone(), async move {
                        let _permit = engine.acquire_backend_http_permit(&destination).await.unwrap().unwrap();
                        ready_tx.send(()).unwrap();
                        std::future::pending::<()>().await;
                    }));
                    tokio::time::timeout(std::time::Duration::from_secs(1), ready_rx).await.unwrap().unwrap();
                    waiting.abort();
                    assert!(waiting.await.unwrap_err().is_cancelled());
                    let pool = semaphore(origin.clone(), "fixture".into(), cap).unwrap();
                    assert_eq!(pool.available_permits(), cap);
                    let permits = tokio::time::timeout(std::time::Duration::from_secs(1), pool.acquire_many_owned(cap as u32)).await.unwrap().unwrap();
                    drop(permits);
                }

                assert_eq!(semaphore(origin, "fixture".into(), cap).unwrap().available_permits(), cap);
            });
        }
    }
}
