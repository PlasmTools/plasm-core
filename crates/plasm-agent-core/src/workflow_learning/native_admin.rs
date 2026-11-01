//! Explicit host administration; never part of an agent's review tool set.
use super::{model::LibraryScope, LearningError, LearningMode, WorkflowLearning};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromotionCounts {
    pub queued: u64,
    pub leased: u64,
    pub completed: u64,
    pub failed: u64,
    pub awaiting_approval: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PromotionDrain {
    Settled {
        jobs: PromotionCounts,
    },
    TimedOut {
        jobs: PromotionCounts,
    },
    ProviderUnavailable {
        jobs: PromotionCounts,
    },
    /// No authoritative job snapshot was obtainable within the host deadline.
    DeadlineExceeded,
}

/// The caller supplies an experiment-owned scope. The same durable worker is
/// used after producer restart; it never executes business operations.
pub async fn drain_promotion(
    learning: &WorkflowLearning,
    scope: &LibraryScope,
    timeout: Duration,
) -> Result<PromotionDrain, LearningError> {
    if learning.mode != LearningMode::Shadow
        || timeout.is_zero()
        || timeout > Duration::from_secs(3600)
    {
        return Err(LearningError::Evidence(
            "promotion drain requires collect mode and a 1..3600 second budget".into(),
        ));
    }
    tokio::time::timeout(timeout, drain_with_snapshot(learning, scope, timeout))
        .await
        .unwrap_or(Ok(PromotionDrain::DeadlineExceeded))
}

async fn drain_with_snapshot(
    learning: &WorkflowLearning,
    scope: &LibraryScope,
    timeout: Duration,
) -> Result<PromotionDrain, LearningError> {
    let repo = learning.repository().await?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let jobs = repo.promotion_counts(scope).await?;
        if jobs.queued == 0 && jobs.leased == 0 {
            return Ok(PromotionDrain::Settled { jobs });
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(PromotionDrain::TimedOut { jobs });
        }
        if std::env::var("OPENROUTER_API_KEY")
            .ok()
            .is_none_or(|key| key.trim().is_empty())
        {
            return Ok(PromotionDrain::ProviderUnavailable { jobs });
        }
        match tokio::time::timeout_at(deadline, super::worker::tick_scoped(learning, Some(scope)))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(target:"plasm_agent::workflow_learning", %error, "promotion attempt retained for retry or terminal failure")
            }
            Err(_) => {
                return Ok(PromotionDrain::TimedOut {
                    jobs: repo.promotion_counts(scope).await?,
                })
            }
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(1)),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn postgres_promotion_deadline_includes_blocked_status_queries() {
        let Ok(url) = std::env::var("PLASM_TEST_POSTGRES_URL") else {
            return;
        };
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("promotion_deadline_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let scoped = format!("{url}{separator}options=-csearch_path%3D{schema}");
        let learning = WorkflowLearning::configured(LearningMode::Shadow, Some(scoped));
        learning.repository().await.unwrap();
        let mut blocker = admin.begin().await.unwrap();
        sqlx::query(&format!(
            "LOCK TABLE {schema}.workflow_learning_reviews IN ACCESS EXCLUSIVE MODE"
        ))
        .execute(&mut *blocker)
        .await
        .unwrap();
        let scope = LibraryScope {
            tenant: "tenant:deadline".into(),
            project: "test".into(),
        };
        let result = drain_promotion(&learning, &scope, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(result, PromotionDrain::DeadlineExceeded));
        blocker.rollback().await.unwrap();
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .unwrap();
    }
}
