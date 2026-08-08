//! Plasm trace sink: HTTP ingest backed by Iceberg (Parquet + SqlCatalog).
//! Wire/serde request and response DTOs come from [`plasm_trace_wire`]; this crate holds
//! product storage/projection (Iceberg, Postgres) and only adds sink-only row types in [`model`].

pub mod append_port;
pub mod config;
pub mod http;
pub mod iceberg_writer;
mod metrics;
pub mod model;
pub mod persisted;
pub(crate) mod projection;
pub mod projector;
mod spans;
pub mod state;
mod trace_event_decode;
mod trace_totals;

#[cfg(test)]
#[path = "code_plan_projection_tests.rs"]
mod code_plan_projection_tests;

pub use append_port::{AuditSpanReader, AuditSpanStore, AuditSpanWriter};
pub use config::{
    CatalogConnectionString, Config, IcebergConnectParams, S3WarehouseUri, WarehouseLocation,
};
pub use model::{BillingUsageResponse, TraceGetResponse};
pub use persisted::PersistedTraceSink;
pub use state::AppState;
