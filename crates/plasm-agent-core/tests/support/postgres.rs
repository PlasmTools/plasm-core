//! Hermetic Postgres for integration tests: optional [`INTEGRATION_POSTGRES_URL_ENV`], else testcontainers.
//!
//! Other workspace integration tests include this file via `#[path = "..."]` (see `docs/env-profiles.md`).

use std::time::Duration;

use testcontainers_modules::testcontainers::{
    core::{error::TestcontainersError, wait::LogWaitStrategy, IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};

pub const INTEGRATION_POSTGRES_URL_ENV: &str = "PLASM_TEST_POSTGRES_URL";

/// Keeps a throwaway Postgres container alive until dropped.
pub struct PostgresKeepAlive(Option<ContainerAsync<GenericImage>>);

impl Drop for PostgresKeepAlive {
    fn drop(&mut self) {
        drop(self.0.take());
    }
}

async fn postgres_url_reachable(url: &str) -> bool {
    match tokio::time::timeout(
        Duration::from_secs(8),
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url),
    )
    .await
    {
        Ok(Ok(pool)) => {
            pool.close().await;
            true
        }
        Ok(Err(e)) => {
            eprintln!(
                "integration postgres: ignoring {INTEGRATION_POSTGRES_URL_ENV} ({e}); \
                 will use testcontainers or skip"
            );
            false
        }
        Err(_) => {
            eprintln!(
                "integration postgres: ignoring {INTEGRATION_POSTGRES_URL_ENV} (connect timed out); \
                 will use testcontainers or skip"
            );
            false
        }
    }
}

async fn start_postgres_container(
    start_timeout: Duration,
) -> Result<ContainerAsync<GenericImage>, String> {
    let fut = GenericImage::new("postgres", "16")
        .with_wait_for(WaitFor::log(
            // The temporary init server logs to stdout; only the final TCP server
            // emits this readiness message on stderr.
            LogWaitStrategy::stderr("database system is ready to accept connections"),
        ))
        .with_exposed_port(5432.tcp())
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .start();
    match tokio::time::timeout(start_timeout, fut).await {
        Ok(Ok(n)) => Ok(n),
        Ok(Err(e)) => Err(format!("Postgres testcontainer failed ({e})")),
        Err(_) => Err(format!(
            "Postgres testcontainer start timed out after {start_timeout:?}"
        )),
    }
}

/// Postgres URL for integration tests: env override (after connect probe), else Docker testcontainers.
pub async fn integration_postgres_url(
    start_timeout: Duration,
) -> Option<(PostgresKeepAlive, String)> {
    if let Ok(url) = std::env::var(INTEGRATION_POSTGRES_URL_ENV) {
        let url = url.trim().to_string();
        if !url.is_empty() && postgres_url_reachable(&url).await {
            return Some((PostgresKeepAlive(None), url));
        }
    }

    let startup_deadline = tokio::time::Instant::now() + start_timeout;
    let node = match start_postgres_container(start_timeout).await {
        Ok(n) => n,
        Err(msg) => {
            eprintln!(
                "integration postgres: {msg}. Set {INTEGRATION_POSTGRES_URL_ENV} \
                 or ensure Docker is running."
            );
            return None;
        }
    };
    // PostgreSQL readiness and Docker's host-port publication are separate
    // signals. Keep inspecting this container within the startup deadline.
    let port = match tokio::time::timeout_at(startup_deadline, async {
        loop {
            let ports = node.ports().await?;
            if let Some(port) = ports.map_to_host_port_ipv4(5432.tcp()) {
                break Ok::<_, TestcontainersError>(port);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            eprintln!("integration postgres: port mapping failed: {e}");
            return None;
        }
        Err(_) => {
            eprintln!("integration postgres: host port was not published within {start_timeout:?}");
            return None;
        }
    };
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    if !postgres_url_reachable(&url).await {
        eprintln!("integration postgres: testcontainer not ready ({url})");
        return None;
    }
    Some((PostgresKeepAlive(Some(node)), url))
}
