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
/// Heap-bound job futures on both paths: a single hydration job can contain the
/// entire nested execution tree and must not inflate every caller poll frame.
pub(crate) async fn bounded_parallel_map<I, Fut, T, E>(
    items: Vec<I>,
    cfg: BoundedParallelConfig,
    f: impl Fn(I) -> Fut + Send + Sync + Clone,
) -> Result<Vec<T>, E>
where
    Fut: std::future::Future<Output = Result<T, E>> + Send,
    E: From<String> + Send,
    I: Send + 'static,
    T: Send + 'static,
{
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if items.len() == 1 {
        let item = items.into_iter().next().expect("one item");
        return Ok(vec![Box::pin(f(item)).await?]);
    }

    let semaphore = Arc::new(Semaphore::new(cfg.concurrency));
    let f = Arc::new(f);
    stream::iter(items)
        .map(move |item| {
            let f = Arc::clone(&f);
            let semaphore = Arc::clone(&semaphore);
            async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|e| E::from(e.to_string()))?;
                Box::pin(f(item)).await
            }
        })
        .buffer_unordered(cfg.concurrency)
        .collect::<Vec<Result<T, E>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<T>, _>>()
}

/// Admission is semantic, independent of the configured concurrency limit.
#[derive(Clone, Copy)]
pub(crate) enum BatchAdmission {
    ReadAll,
    OrderedEffects,
}

/// Retain job failures beside completed results. Ordered effects admit one job
/// at a time and never dispatch the suffix after a failure.
/// The outer `Err` is only for the concurrency permit itself.
pub(crate) async fn bounded_parallel_map_partition<I, Fut, T, E>(
    items: Vec<I>,
    cfg: BoundedParallelConfig,
    admission: BatchAdmission,
    f: impl Fn(I) -> Fut + Send + Sync + Clone,
) -> Result<(Vec<T>, Vec<E>), String>
where
    Fut: std::future::Future<Output = Result<T, E>> + Send,
    I: Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    if matches!(admission, BatchAdmission::OrderedEffects) {
        let mut completed = Vec::new();
        for item in items {
            match Box::pin(f(item)).await {
                Ok(value) => completed.push(value),
                Err(failure) => return Ok((completed, vec![failure])),
            }
        }
        return Ok((completed, Vec::new()));
    }
    if items.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if items.len() == 1 {
        let item = items.into_iter().next().expect("one item");
        return match Box::pin(f(item)).await {
            Ok(ok) => Ok((vec![ok], Vec::new())),
            Err(err) => Ok((Vec::new(), vec![err])),
        };
    }

    let semaphore = Arc::new(Semaphore::new(cfg.concurrency));
    let f = Arc::new(f);
    let outcomes: Vec<Result<Box<Result<T, E>>, String>> = stream::iter(items)
        .map(move |item| {
            let f = Arc::clone(&f);
            let semaphore = Arc::clone(&semaphore);
            async move {
                let _permit = semaphore.acquire_owned().await.map_err(|e| e.to_string())?;
                Ok(Box::new(Box::pin(f(item)).await))
            }
        })
        .buffer_unordered(cfg.concurrency)
        .collect()
        .await;
    let mut completed = Vec::new();
    let mut failures = Vec::new();
    for outcome in outcomes {
        match *outcome? {
            Ok(ok) => completed.push(ok),
            Err(err) => failures.push(err),
        }
    }
    Ok((completed, failures))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ordered_effects_leave_the_failed_occurrence_suffix_undispatched() {
        for count in 1..8 {
            for failed in 0..count {
                let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
                let observed = Arc::clone(&calls);
                let (completed, failures) = bounded_parallel_map_partition(
                    (0..count).collect(),
                    BoundedParallelConfig { concurrency: 8 },
                    BatchAdmission::OrderedEffects,
                    move |index| {
                        observed.lock().unwrap().push(index);
                        async move {
                            tokio::task::yield_now().await;
                            if index == failed {
                                Err(index)
                            } else {
                                Ok(index)
                            }
                        }
                    },
                )
                .await
                .unwrap();
                assert_eq!(*calls.lock().unwrap(), (0..=failed).collect::<Vec<_>>());
                assert_eq!(completed, (0..failed).collect::<Vec<_>>());
                assert_eq!(failures, vec![failed]);
            }
        }
    }

    #[tokio::test]
    async fn serial_reads_still_drain_after_a_failure() {
        let (completed, failures) = bounded_parallel_map_partition(
            vec![0, 1, 2],
            BoundedParallelConfig { concurrency: 1 },
            BatchAdmission::ReadAll,
            |index| async move {
                if index == 1 {
                    Err(index)
                } else {
                    Ok(index)
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(completed, vec![0, 2]);
        assert_eq!(failures, vec![1]);
    }

    #[tokio::test]
    async fn bounded_parallel_map_completes_when_batch_exceeds_concurrency() {
        let cfg = BoundedParallelConfig { concurrency: 4 };
        let items: Vec<usize> = (0..20).collect();
        let out = bounded_parallel_map(items, cfg, |i| async move {
            tokio::task::yield_now().await;
            Ok::<_, String>(i * 2)
        })
        .await
        .expect("parallel map");
        assert_eq!(out.len(), 20);
        let mut sorted = out;
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).map(|i| i * 2).collect::<Vec<_>>());
    }
}
