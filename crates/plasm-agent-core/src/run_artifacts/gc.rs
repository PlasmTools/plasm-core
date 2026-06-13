use object_store::path::Path as StorePath;
use object_store::{ObjectStore, ObjectStoreExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::MissedTickBehavior;

pub(crate) fn retention_from_env() -> Duration {
    let secs: u64 = std::env::var("PLASM_RUN_ARTIFACTS_RETENTION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(604_800);
    Duration::from_secs(secs.max(60))
}

pub(crate) fn gc_interval_from_env() -> Duration {
    let secs: u64 = std::env::var("PLASM_RUN_ARTIFACTS_GC_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    Duration::from_secs(secs.max(60))
}

pub(crate) fn spawn_run_artifact_gc_task(
    store: Arc<dyn ObjectStore>,
    list_prefix: StorePath,
    retention: Duration,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = run_artifact_gc_pass(store.as_ref(), &list_prefix, retention).await {
                tracing::warn!(error = %e, "run artifact GC pass failed");
            }
        }
    });
}

async fn run_artifact_gc_pass(
    store: &dyn ObjectStore,
    list_prefix: &StorePath,
    retention: Duration,
) -> Result<(), object_store::Error> {
    use chrono::Utc;
    use futures_util::TryStreamExt;
    let secs = retention.as_secs().min(i64::MAX as u64) as i64;
    let cutoff = Utc::now() - chrono::Duration::seconds(secs);
    let mut stream = store.list(Some(list_prefix));
    while let Some(meta) = stream.try_next().await? {
        if !object_store_path_is_run_snapshot_gc_eligible(meta.location.as_ref()) {
            continue;
        }
        if meta.last_modified < cutoff {
            store.delete(&meta.location).await?;
            tracing::debug!(path = %meta.location, "run artifact GC deleted object");
        }
    }
    Ok(())
}

/// Time-GC applies only to execute run snapshot blobs, not evidence sidecars or code plans.
pub(crate) fn object_store_path_is_run_snapshot_gc_eligible(location: &str) -> bool {
    (location.contains("/execute/") || location.starts_with("execute/"))
        && !location.contains("/evidence/")
}
