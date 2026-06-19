//! Bounded concurrent map used by plan row jobs and relation GET hydrate.

use std::sync::Arc;

use futures::stream::{self, StreamExt};
use tokio::sync::Semaphore;

/// Shared HTTP concurrency for plan row jobs and relation GET hydrate.
#[must_use]
pub(crate) fn plan_http_concurrency() -> usize {
    std::env::var("PLASM_PLAN_HTTP_CONCURRENCY")
        .or_else(|_| std::env::var("PLASM_HTTP_HYDRATE_CONCURRENCY"))
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(16)
}

pub(crate) struct BoundedParallelConfig {
    pub concurrency: usize,
}

impl BoundedParallelConfig {
    pub(crate) fn for_plan_http(concurrency_override: Option<usize>) -> Self {
        Self {
            concurrency: concurrency_override.unwrap_or_else(plan_http_concurrency),
        }
    }
}

/// Run `f` concurrently with a semaphore cap.
pub(crate) async fn bounded_parallel_map<I, Fut, T>(
    items: Vec<I>,
    cfg: BoundedParallelConfig,
    f: impl Fn(I) -> Fut + Send + Sync + Clone,
) -> Result<Vec<T>, String>
where
    Fut: std::future::Future<Output = Result<T, String>> + Send,
    I: Send + 'static,
    T: Send + 'static,
{
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if items.len() == 1 {
        let item = items.into_iter().next().expect("one item");
        return Ok(vec![f(item).await?]);
    }

    let semaphore = Arc::new(Semaphore::new(cfg.concurrency));
    let f = Arc::new(f);
    stream::iter(items)
        .map(move |item| {
            let f = Arc::clone(&f);
            let semaphore = Arc::clone(&semaphore);
            async move {
                let _permit = semaphore.acquire_owned().await.map_err(|e| e.to_string())?;
                f(item).await
            }
        })
        .buffer_unordered(cfg.concurrency)
        .collect::<Vec<Result<T, String>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<T>, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_parallel_map_completes_when_batch_exceeds_concurrency() {
        let cfg = BoundedParallelConfig { concurrency: 4 };
        let items: Vec<usize> = (0..20).collect();
        let out = bounded_parallel_map(items, cfg, |i| async move {
            tokio::task::yield_now().await;
            Ok(i * 2)
        })
        .await
        .expect("parallel map");
        assert_eq!(out.len(), 20);
        let mut sorted = out;
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).map(|i| i * 2).collect::<Vec<_>>());
    }
}
