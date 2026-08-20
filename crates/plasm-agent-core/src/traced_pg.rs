//! OpenTelemetry-instrumented Postgres pool (`sqlx-tracing` full cutover).
//!
//! All durable sqlx repositories in this crate hold [`PgPool`] from this module, not raw
//! `sqlx::PgPool`. Construct via [`wrap`] after `PgPoolOptions::connect`.

use sqlx::Postgres;

/// Traced Postgres connection pool (queries become `sqlx.*` child spans).
pub type PgPool = sqlx_tracing::Pool<Postgres>;

/// Wrap a raw sqlx pool so every executor call is instrumented.
#[inline]
pub fn wrap(pool: sqlx::PgPool) -> PgPool {
    sqlx_tracing::Pool::from(pool)
}
