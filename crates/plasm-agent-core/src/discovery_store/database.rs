//! Admission belongs outside SQLx's timed pool queue. Every database operation
//! owns a permit until its connection (including rollback) is back in the pool.

use sqlx::{pool::PoolConnection, postgres::PgPoolOptions, PgConnection, PgPool, Postgres};
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub(super) struct DiscoveryDatabase {
    pool: PgPool,
    admission: Arc<Semaphore>,
}

impl DiscoveryDatabase {
    pub(super) async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        Self::with_options(url, PgPoolOptions::new().max_connections(8)).await
    }

    async fn with_options(url: &str, options: PgPoolOptions) -> Result<Self, sqlx::Error> {
        let capacity = options.get_max_connections() as usize;
        let pool = options.connect(url).await?;
        Ok(Self {
            pool,
            admission: Arc::new(Semaphore::new(capacity)),
        })
    }

    pub(super) async fn acquire(&self) -> Result<DiscoveryConnection, sqlx::Error> {
        let permit = self
            .admission
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| sqlx::Error::PoolClosed)?;
        let connection = self.pool.acquire().await?;
        Ok(DiscoveryConnection {
            connection,
            permit: Some(permit),
        })
    }
}

pub(super) struct DiscoveryConnection {
    connection: PoolConnection<Postgres>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Deref for DiscoveryConnection {
    type Target = PgConnection;
    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for DiscoveryConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

impl Drop for DiscoveryConnection {
    fn drop(&mut self) {
        // SQLx returns connections asynchronously. Releasing admission first
        // would let the next caller race that return and re-enter its timed queue.
        let returned = self.connection.return_to_pool();
        let permit = self.permit.take();
        tokio::spawn(async move {
            returned.await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;
    use std::time::Duration;

    #[tokio::test]
    #[ignore = "requires PLASM_TEST_POSTGRES_URL"]
    async fn discovery_database_burst_waits_without_pool_timeouts() {
        let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
        let db = DiscoveryDatabase::with_options(
            &url,
            PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_millis(100)),
        )
        .await
        .unwrap();
        let mut held = db.acquire().await.unwrap();
        let mut tx = held.begin().await.unwrap();
        // Reproduce the original failure through the unadmitted pool.
        assert!(matches!(
            db.pool.acquire().await,
            Err(sqlx::Error::PoolTimedOut)
        ));
        assert!(sqlx::query("SELECT 1 / 0").execute(&mut *tx).await.is_err());
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..96 {
            let db = db.clone();
            tasks.spawn(async move {
                sqlx::query_scalar::<_, i32>("SELECT 1")
                    .fetch_one(&mut *db.acquire().await?)
                    .await
            });
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            tasks.try_join_next().is_none(),
            "queued calls must not hit SQLx's acquisition deadline"
        );
        drop(tx); // Uncommitted transaction must roll back before admission resumes.
        drop(held);
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(result) = tasks.join_next().await {
                assert_eq!(result.unwrap().unwrap(), 1);
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires PLASM_TEST_POSTGRES_URL"]
    async fn discovery_database_cancellation_and_errors_release_admission() {
        let db = DiscoveryDatabase::with_options(
            &std::env::var("PLASM_TEST_POSTGRES_URL").unwrap(),
            PgPoolOptions::new().max_connections(1),
        )
        .await
        .unwrap();
        let held = db.acquire().await.unwrap();
        let waiter = tokio::spawn({
            let db = db.clone();
            async move { db.acquire().await }
        });
        tokio::task::yield_now().await;
        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        drop(held);
        let mut connection = tokio::time::timeout(Duration::from_secs(2), db.acquire())
            .await
            .unwrap()
            .unwrap();
        assert!(sqlx::query("SELECT 1 / 0")
            .execute(&mut *connection)
            .await
            .is_err());
        drop(connection);
        let (started, ready) = tokio::sync::oneshot::channel();
        let holder = tokio::spawn({
            let db = db.clone();
            async move {
                let _connection = db.acquire().await.unwrap();
                started.send(()).unwrap();
                std::future::pending::<()>().await;
            }
        });
        ready.await.unwrap();
        holder.abort();
        let _ = holder.await;
        let _connection = tokio::time::timeout(Duration::from_secs(2), db.acquire())
            .await
            .unwrap()
            .unwrap();
    }
}
