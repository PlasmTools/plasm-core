//! Iceberg `audit_events` + `trace_spans` via JanKaul [`iceberg-sql-catalog`] (Postgres in dev) and
//! [`datafusion_iceberg`] for Parquet reads/writes.

use std::collections::HashSet;
use std::sync::Arc;

use crate::append_port::{
    AuditSpanReader, AuditSpanWriter, TenantId, TimeWindow, TraceListFilter, TraceListStatusFilter,
};
use crate::config::IcebergConnectParams;
use crate::config::WarehouseLocation;
use crate::metrics::record_iceberg_detail_prune_fallback;
use crate::model::{
    year_month_bucket_utc, year_month_buckets_for_trace_ms, AuditEvent, DurableTraceDetail,
    TraceDetailRecord, TraceHeadRow, TraceSpanRow, TraceSummary, TraceTotals,
    AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT,
};
use crate::trace_totals::{head_needs_segment_recompute, trace_totals_from_head_or_records};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use datafusion::arrow::array::Array;
use datafusion::arrow::array::{
    ArrayRef, BooleanArray, Int32Array, Int64Array, StringArray, TimestampMicrosecondArray,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::dataframe::DataFrameWriteOptions;
use datafusion::prelude::SessionContext;
use datafusion_iceberg::catalog::catalog::IcebergCatalog;
use iceberg_rust::catalog::identifier::Identifier;
use iceberg_rust::catalog::namespace::Namespace;
use iceberg_rust::catalog::Catalog;
use iceberg_rust::object_store::ObjectStoreBuilder;
use iceberg_rust::spec::partition::{PartitionField, PartitionSpec, Transform};
use iceberg_rust::spec::schema::Schema as IcebergSchema;
use iceberg_rust::spec::types::{PrimitiveType, StructField, Type};
use iceberg_rust::table::Table;
use iceberg_sql_catalog::SqlCatalog;
use object_store::local::LocalFileSystem;
use plasm_trace::{session_data_from_ordered_events, totals_from_session_data, TraceEvent};
use tokio::sync::Mutex;
use uuid::Uuid;

const NS: &str = "plasm";
const AUDIT: &str = "audit_events";
const TRACE: &str = "trace_spans";
const TRACE_HEADS: &str = "trace_heads";
const DF_CATALOG: &str = "warehouse";

fn ts_micros(dt: chrono::DateTime<chrono::Utc>) -> i64 {
    dt.timestamp_micros()
}

fn iceberg_struct_field(id: i32, name: &str, required: bool, t: PrimitiveType) -> StructField {
    StructField::new(id, name, required, Type::Primitive(t), None)
}

fn utc_timestamp_micros_array(v: Vec<i64>) -> TimestampMicrosecondArray {
    TimestampMicrosecondArray::from(v).with_timezone_opt(Some("UTC".to_string()))
}

fn audit_iceberg_schema_versioned(schema_id: i32) -> IcebergSchema {
    IcebergSchema::builder()
        .with_schema_id(schema_id)
        .with_struct_field(iceberg_struct_field(
            1,
            "event_id",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            2,
            "schema_version",
            true,
            PrimitiveType::Int,
        ))
        .with_struct_field(iceberg_struct_field(
            3,
            "emitted_at",
            true,
            PrimitiveType::Timestamptz,
        ))
        .with_struct_field(iceberg_struct_field(
            4,
            "ingested_at",
            true,
            PrimitiveType::Timestamptz,
        ))
        .with_struct_field(iceberg_struct_field(
            5,
            "trace_id",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            6,
            "mcp_session_id",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            7,
            "plasm_prompt_hash",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            8,
            "plasm_execute_session",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            9,
            "run_id",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            10,
            "call_index",
            false,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            11,
            "line_index",
            false,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            12,
            "tenant_id",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            13,
            "principal_sub",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            14,
            "tenant_partition",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            15,
            "event_kind",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            16,
            "request_units",
            true,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            17,
            "payload_json",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            18,
            "workspace_slug",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            19,
            "project_slug",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            20,
            "year_month_bucket",
            true,
            PrimitiveType::Int,
        ))
        .build()
        .expect("audit schema")
}

fn audit_iceberg_schema() -> IcebergSchema {
    audit_iceberg_schema_versioned(0)
}

fn trace_iceberg_schema_versioned(schema_id: i32) -> IcebergSchema {
    IcebergSchema::builder()
        .with_schema_id(schema_id)
        .with_struct_field(iceberg_struct_field(
            1,
            "span_id",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            2,
            "event_id",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            3,
            "trace_id",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            4,
            "emitted_at",
            true,
            PrimitiveType::Timestamptz,
        ))
        .with_struct_field(iceberg_struct_field(
            5,
            "tenant_partition",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            6,
            "mcp_session_id",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            7,
            "plasm_prompt_hash",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            8,
            "plasm_execute_session",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            9,
            "run_id",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            10,
            "call_index",
            false,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            11,
            "line_index",
            false,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            12,
            "span_name",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            13,
            "is_billing_event",
            true,
            PrimitiveType::Boolean,
        ))
        .with_struct_field(iceberg_struct_field(
            14,
            "billing_event_type",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            15,
            "request_units",
            true,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            16,
            "duration_ms",
            false,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            17,
            "attributes_json",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            18,
            "api_entry_id",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            19,
            "capability",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            20,
            "year_month_bucket",
            true,
            PrimitiveType::Int,
        ))
        .build()
        .expect("trace schema")
}

fn trace_iceberg_schema() -> IcebergSchema {
    trace_iceberg_schema_versioned(0)
}

/// Full `trace_heads` Iceberg schema for a given **schema id** (new tables use `0`).
fn trace_heads_iceberg_schema_versioned(schema_id: i32) -> IcebergSchema {
    IcebergSchema::builder()
        .with_schema_id(schema_id)
        .with_struct_field(iceberg_struct_field(
            1,
            "trace_id",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            2,
            "tenant_partition",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            3,
            "tenant_id",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            4,
            "project_slug",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            5,
            "mcp_session_id",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            6,
            "status",
            true,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            7,
            "started_at_ms",
            true,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            8,
            "ended_at_ms",
            false,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            9,
            "updated_at_ms",
            true,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            10,
            "expression_lines",
            true,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            11,
            "max_call_index",
            false,
            PrimitiveType::Long,
        ))
        .with_struct_field(iceberg_struct_field(
            12,
            "totals_json",
            false,
            PrimitiveType::String,
        ))
        .with_struct_field(iceberg_struct_field(
            13,
            "workspace_slug",
            false,
            PrimitiveType::String,
        ))
        .build()
        .expect("trace_heads schema")
}

fn trace_heads_iceberg_schema() -> IcebergSchema {
    trace_heads_iceberg_schema_versioned(0)
}

fn audit_partition_spec() -> PartitionSpec {
    // Identity on `tenant_partition` + `year_month_bucket` (YYYYMM UTC) for pruning.
    PartitionSpec::builder()
        .with_partition_field(PartitionField::new(
            14,
            1000,
            "tenant_partition",
            Transform::Identity,
        ))
        .with_partition_field(PartitionField::new(
            20,
            1001,
            "year_month_bucket",
            Transform::Identity,
        ))
        .build()
        .expect("audit partition spec")
}

fn trace_partition_spec() -> PartitionSpec {
    PartitionSpec::builder()
        .with_partition_field(PartitionField::new(
            5,
            1000,
            "tenant_partition",
            Transform::Identity,
        ))
        .with_partition_field(PartitionField::new(
            20,
            1001,
            "year_month_bucket",
            Transform::Identity,
        ))
        .build()
        .expect("trace partition spec")
}

fn trace_heads_partition_spec() -> PartitionSpec {
    PartitionSpec::builder()
        .with_partition_field(PartitionField::new(
            2,
            1000,
            "tenant_partition",
            Transform::Identity,
        ))
        .build()
        .expect("trace_heads partition spec")
}

fn audit_arrow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("event_id", DataType::Utf8, false),
        Field::new("schema_version", DataType::Int32, false),
        Field::new(
            "emitted_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "ingested_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("trace_id", DataType::Utf8, false),
        Field::new("mcp_session_id", DataType::Utf8, true),
        Field::new("plasm_prompt_hash", DataType::Utf8, true),
        Field::new("plasm_execute_session", DataType::Utf8, true),
        Field::new("run_id", DataType::Utf8, true),
        Field::new("call_index", DataType::Int64, true),
        Field::new("line_index", DataType::Int64, true),
        Field::new("tenant_id", DataType::Utf8, true),
        Field::new("principal_sub", DataType::Utf8, true),
        Field::new("tenant_partition", DataType::Utf8, false),
        Field::new("event_kind", DataType::Utf8, false),
        Field::new("request_units", DataType::Int64, false),
        Field::new("payload_json", DataType::Utf8, false),
        Field::new("workspace_slug", DataType::Utf8, true),
        Field::new("project_slug", DataType::Utf8, true),
        Field::new("year_month_bucket", DataType::Int32, false),
    ]))
}

fn trace_arrow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("span_id", DataType::Utf8, false),
        Field::new("event_id", DataType::Utf8, false),
        Field::new("trace_id", DataType::Utf8, false),
        Field::new(
            "emitted_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("tenant_partition", DataType::Utf8, false),
        Field::new("mcp_session_id", DataType::Utf8, true),
        Field::new("plasm_prompt_hash", DataType::Utf8, true),
        Field::new("plasm_execute_session", DataType::Utf8, true),
        Field::new("run_id", DataType::Utf8, true),
        Field::new("call_index", DataType::Int64, true),
        Field::new("line_index", DataType::Int64, true),
        Field::new("span_name", DataType::Utf8, false),
        Field::new("is_billing_event", DataType::Boolean, false),
        Field::new("billing_event_type", DataType::Utf8, true),
        Field::new("request_units", DataType::Int64, false),
        Field::new("duration_ms", DataType::Int64, true),
        Field::new("attributes_json", DataType::Utf8, false),
        Field::new("api_entry_id", DataType::Utf8, true),
        Field::new("capability", DataType::Utf8, true),
        Field::new("year_month_bucket", DataType::Int32, false),
    ]))
}

fn trace_heads_arrow_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("trace_id", DataType::Utf8, false),
        Field::new("tenant_partition", DataType::Utf8, false),
        Field::new("tenant_id", DataType::Utf8, false),
        Field::new("project_slug", DataType::Utf8, false),
        Field::new("mcp_session_id", DataType::Utf8, true),
        Field::new("status", DataType::Utf8, false),
        Field::new("started_at_ms", DataType::Int64, false),
        Field::new("ended_at_ms", DataType::Int64, true),
        Field::new("updated_at_ms", DataType::Int64, false),
        Field::new("expression_lines", DataType::Int64, false),
        Field::new("max_call_index", DataType::Int64, true),
        Field::new("totals_json", DataType::Utf8, true),
        Field::new("workspace_slug", DataType::Utf8, true),
    ]))
}

fn audit_batch(events: &[AuditEvent]) -> anyhow::Result<RecordBatch> {
    let n = events.len();
    let mut event_id = Vec::with_capacity(n);
    let mut schema_version = Vec::with_capacity(n);
    let mut emitted_at = Vec::with_capacity(n);
    let mut ingested_at = Vec::with_capacity(n);
    let mut trace_id = Vec::with_capacity(n);
    let mut mcp_session_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut plasm_prompt_hash: Vec<Option<String>> = Vec::with_capacity(n);
    let mut plasm_execute_session: Vec<Option<String>> = Vec::with_capacity(n);
    let mut run_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut call_index: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut line_index: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut tenant_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut principal_sub: Vec<Option<String>> = Vec::with_capacity(n);
    let mut tenant_partition = Vec::with_capacity(n);
    let mut event_kind = Vec::with_capacity(n);
    let mut request_units = Vec::with_capacity(n);
    let mut payload_json = Vec::with_capacity(n);
    let mut workspace_slug: Vec<Option<String>> = Vec::with_capacity(n);
    let mut project_slug: Vec<Option<String>> = Vec::with_capacity(n);
    let mut year_month_bucket = Vec::with_capacity(n);

    for ev in events {
        event_id.push(ev.event_id.to_string());
        schema_version.push(ev.schema_version);
        emitted_at.push(ts_micros(ev.emitted_at));
        ingested_at.push(ts_micros(ev.ingested_at));
        trace_id.push(ev.trace_id.to_string());
        mcp_session_id.push(ev.mcp_session_id.clone());
        plasm_prompt_hash.push(ev.plasm_prompt_hash.clone());
        plasm_execute_session.push(ev.plasm_execute_session.clone());
        run_id.push(ev.run_id.clone());
        call_index.push(ev.call_index);
        line_index.push(ev.line_index);
        tenant_id.push(ev.tenant_id.clone());
        principal_sub.push(ev.principal_sub.clone());
        tenant_partition.push(ev.tenant_partition());
        event_kind.push(ev.event_kind.clone());
        request_units.push(ev.request_units);
        payload_json.push(serde_json::to_string(&ev.payload).unwrap_or_else(|_| "{}".to_string()));
        let ws = ev.audit_workspace_slug();
        workspace_slug.push((!ws.is_empty()).then_some(ws));
        let ps = ev.audit_project_slug();
        project_slug.push((!ps.is_empty()).then_some(ps));
        year_month_bucket.push(year_month_bucket_utc(ev.emitted_at));
    }

    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(event_id)),
        Arc::new(Int32Array::from(schema_version)),
        Arc::new(utc_timestamp_micros_array(emitted_at)),
        Arc::new(utc_timestamp_micros_array(ingested_at)),
        Arc::new(StringArray::from(trace_id)),
        Arc::new(StringArray::from_iter(
            mcp_session_id.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            plasm_prompt_hash.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            plasm_execute_session.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(run_id.iter().map(|s| s.as_deref()))),
        Arc::new(Int64Array::from_iter(call_index.iter().copied())),
        Arc::new(Int64Array::from_iter(line_index.iter().copied())),
        Arc::new(StringArray::from_iter(
            tenant_id.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            principal_sub.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from(tenant_partition)),
        Arc::new(StringArray::from(event_kind)),
        Arc::new(Int64Array::from(request_units)),
        Arc::new(StringArray::from(payload_json)),
        Arc::new(StringArray::from_iter(
            workspace_slug.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            project_slug.iter().map(|s| s.as_deref()),
        )),
        Arc::new(Int32Array::from(year_month_bucket)),
    ];

    RecordBatch::try_new(audit_arrow_schema(), cols).map_err(|e| anyhow::anyhow!("{e}"))
}

fn trace_batch(rows: &[TraceSpanRow]) -> anyhow::Result<RecordBatch> {
    let n = rows.len();
    let mut span_id = Vec::with_capacity(n);
    let mut event_id = Vec::with_capacity(n);
    let mut trace_id = Vec::with_capacity(n);
    let mut emitted_at = Vec::with_capacity(n);
    let mut tenant_partition = Vec::with_capacity(n);
    let mut mcp_session_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut plasm_prompt_hash: Vec<Option<String>> = Vec::with_capacity(n);
    let mut plasm_execute_session: Vec<Option<String>> = Vec::with_capacity(n);
    let mut run_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut call_index: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut line_index: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut span_name = Vec::with_capacity(n);
    let mut is_billing_event = Vec::with_capacity(n);
    let mut billing_event_type: Vec<Option<String>> = Vec::with_capacity(n);
    let mut request_units = Vec::with_capacity(n);
    let mut duration_ms: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut attributes_json = Vec::with_capacity(n);
    let mut api_entry_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut capability: Vec<Option<String>> = Vec::with_capacity(n);
    let mut year_month_bucket = Vec::with_capacity(n);

    for row in rows {
        span_id.push(row.span_id.to_string());
        event_id.push(row.event_id.to_string());
        trace_id.push(row.trace_id.to_string());
        emitted_at.push(ts_micros(row.emitted_at));
        tenant_partition.push(row.tenant_partition.clone());
        mcp_session_id.push(row.mcp_session_id.clone());
        plasm_prompt_hash.push(row.plasm_prompt_hash.clone());
        plasm_execute_session.push(row.plasm_execute_session.clone());
        run_id.push(row.run_id.clone());
        call_index.push(row.call_index);
        line_index.push(row.line_index);
        span_name.push(row.span_name.clone());
        is_billing_event.push(row.is_billing_event);
        billing_event_type.push(row.billing_event_type.clone());
        request_units.push(row.request_units);
        duration_ms.push(row.duration_ms);
        attributes_json
            .push(serde_json::to_string(&row.attributes_json).unwrap_or_else(|_| "{}".to_string()));
        api_entry_id.push(row.api_entry_id.clone());
        capability.push(row.capability.clone());
        year_month_bucket.push(year_month_bucket_utc(row.emitted_at));
    }

    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(span_id)),
        Arc::new(StringArray::from(event_id)),
        Arc::new(StringArray::from(trace_id)),
        Arc::new(utc_timestamp_micros_array(emitted_at)),
        Arc::new(StringArray::from(tenant_partition)),
        Arc::new(StringArray::from_iter(
            mcp_session_id.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            plasm_prompt_hash.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            plasm_execute_session.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(run_id.iter().map(|s| s.as_deref()))),
        Arc::new(Int64Array::from_iter(call_index.iter().copied())),
        Arc::new(Int64Array::from_iter(line_index.iter().copied())),
        Arc::new(StringArray::from(span_name)),
        Arc::new(BooleanArray::from(is_billing_event)),
        Arc::new(StringArray::from_iter(
            billing_event_type.iter().map(|s| s.as_deref()),
        )),
        Arc::new(Int64Array::from(request_units)),
        Arc::new(Int64Array::from_iter(duration_ms.iter().copied())),
        Arc::new(StringArray::from(attributes_json)),
        Arc::new(StringArray::from_iter(
            api_entry_id.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            capability.iter().map(|s| s.as_deref()),
        )),
        Arc::new(Int32Array::from(year_month_bucket)),
    ];

    RecordBatch::try_new(trace_arrow_schema(), cols).map_err(|e| anyhow::anyhow!("{e}"))
}

fn trace_heads_batch(rows: &[TraceHeadRow]) -> anyhow::Result<RecordBatch> {
    let n = rows.len();
    let mut trace_id = Vec::with_capacity(n);
    let mut tenant_partition = Vec::with_capacity(n);
    let mut tenant_id = Vec::with_capacity(n);
    let mut project_slug = Vec::with_capacity(n);
    let mut mcp_session_id: Vec<Option<String>> = Vec::with_capacity(n);
    let mut status = Vec::with_capacity(n);
    let mut started_at_ms = Vec::with_capacity(n);
    let mut ended_at_ms: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut updated_at_ms = Vec::with_capacity(n);
    let mut expression_lines = Vec::with_capacity(n);
    let mut max_call_index: Vec<Option<i64>> = Vec::with_capacity(n);
    let mut totals_json: Vec<Option<String>> = Vec::with_capacity(n);
    let mut workspace_slug: Vec<Option<String>> = Vec::with_capacity(n);
    for row in rows {
        trace_id.push(row.trace_id.to_string());
        tenant_partition.push(row.tenant_partition.clone());
        tenant_id.push(row.tenant_id.clone());
        project_slug.push(row.project_slug.clone());
        mcp_session_id.push(row.mcp_session_id.clone());
        status.push(row.status.clone());
        started_at_ms.push(row.started_at_ms);
        ended_at_ms.push(row.ended_at_ms);
        updated_at_ms.push(row.updated_at_ms);
        expression_lines.push(row.expression_lines);
        max_call_index.push(row.max_call_index);
        totals_json.push(Some(row.totals_json.clone()));
        let ws = row.workspace_slug.trim();
        workspace_slug.push((!ws.is_empty()).then(|| ws.to_string()));
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(trace_id)),
        Arc::new(StringArray::from(tenant_partition)),
        Arc::new(StringArray::from(tenant_id)),
        Arc::new(StringArray::from(project_slug)),
        Arc::new(StringArray::from_iter(
            mcp_session_id.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from(status)),
        Arc::new(Int64Array::from(started_at_ms)),
        Arc::new(Int64Array::from_iter(ended_at_ms.iter().copied())),
        Arc::new(Int64Array::from(updated_at_ms)),
        Arc::new(Int64Array::from(expression_lines)),
        Arc::new(Int64Array::from_iter(max_call_index.iter().copied())),
        Arc::new(StringArray::from_iter(
            totals_json.iter().map(|s| s.as_deref()),
        )),
        Arc::new(StringArray::from_iter(
            workspace_slug.iter().map(|s| s.as_deref()),
        )),
    ];
    RecordBatch::try_new(trace_heads_arrow_schema(), cols).map_err(|e| anyhow::anyhow!("{e}"))
}

async fn ensure_table(
    catalog: Arc<dyn Catalog>,
    warehouse: &WarehouseLocation,
    name: &str,
    schema: IcebergSchema,
    partition_spec: PartitionSpec,
) -> anyhow::Result<()> {
    let ident = Identifier::new(&[NS.to_string()], name);
    if catalog.tabular_exists(&ident).await? {
        return Ok(());
    }
    let loc_str = match warehouse {
        WarehouseLocation::Filesystem(root) => {
            let loc = root.join(name);
            std::fs::create_dir_all(&loc)?;
            loc.canonicalize()?.to_string_lossy().to_string()
        }
        WarehouseLocation::S3 { base_url } => {
            let base = base_url.trim_end_matches('/');
            format!("{base}/{name}")
        }
    };
    Table::builder()
        .with_name(name)
        .with_location(loc_str)
        .with_schema(schema)
        .with_partition_spec(partition_spec)
        .build(&[NS.to_string()], catalog)
        .await?;
    Ok(())
}

/// JanKaul SqlCatalog + DataFusion append (Parquet data files on local FS or S3-compatible storage).
pub struct IcebergSink {
    ctx: Arc<Mutex<SessionContext>>,
    audit_fqn: String,
    trace_fqn: String,
    trace_heads_fqn: String,
}

impl IcebergSink {
    fn sql_quote(s: &str) -> String {
        s.replace('\'', "''")
    }

    /// Open SqlCatalog, ensure `plasm` namespace and Iceberg tables, register DataFusion catalog.
    ///
    /// Prefer resolving [`IcebergConnectParams`] via [`crate::config::Config::iceberg_connect_params`]
    /// so Kubernetes / warehouse URL validation stays in one place.
    ///
    /// **Schema:** `audit_events`, `trace_spans`, and `trace_heads` use a single canonical layout
    /// (`audit_iceberg_schema`, `trace_iceberg_schema`, `trace_heads_iceberg_schema`). If an older
    /// warehouse or JDBC catalog row disagrees (mixed Parquet generations), clear object storage
    /// and reset catalog metadata, then redeploy—there is no in-place additive migration path.
    pub async fn connect(params: &IcebergConnectParams) -> anyhow::Result<Self> {
        let catalog_url = params.catalog.as_str();
        let warehouse = &params.warehouse;
        let object_store = match warehouse {
            WarehouseLocation::Filesystem(_) => {
                ObjectStoreBuilder::Filesystem(Arc::new(LocalFileSystem::new()))
            }
            WarehouseLocation::S3 { .. } => ObjectStoreBuilder::s3(),
        };
        let catalog: Arc<dyn Catalog> = Arc::new(
            SqlCatalog::new(catalog_url, "warehouse", object_store)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );

        let ns = Namespace::try_new(&[NS.to_string()])?;
        let namespaces = catalog
            .list_namespaces(None)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if !namespaces.iter().any(|n| n == &ns) {
            catalog
                .create_namespace(&ns, None)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        let audit_schema = audit_iceberg_schema();
        let trace_schema = trace_iceberg_schema();
        let trace_heads_schema = trace_heads_iceberg_schema();
        ensure_table(
            catalog.clone(),
            warehouse,
            AUDIT,
            audit_schema,
            audit_partition_spec(),
        )
        .await?;
        ensure_table(
            catalog.clone(),
            warehouse,
            TRACE,
            trace_schema,
            trace_partition_spec(),
        )
        .await?;
        ensure_table(
            catalog.clone(),
            warehouse,
            TRACE_HEADS,
            trace_heads_schema,
            trace_heads_partition_spec(),
        )
        .await?;

        let ctx = SessionContext::new();
        let df_cat = IcebergCatalog::new(catalog.clone(), None)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        ctx.register_catalog(DF_CATALOG, Arc::new(df_cat));

        let audit_fqn = format!("{DF_CATALOG}.{NS}.{AUDIT}");
        let trace_fqn = format!("{DF_CATALOG}.{NS}.{TRACE}");
        let trace_heads_fqn = format!("{DF_CATALOG}.{NS}.{TRACE_HEADS}");

        Ok(Self {
            ctx: Arc::new(Mutex::new(ctx)),
            audit_fqn,
            trace_fqn,
            trace_heads_fqn,
        })
    }

    pub async fn append_audit_events(&self, events: &[AuditEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let batch = audit_batch(events)?;
        let ctx = self.ctx.lock().await;
        let df = ctx.read_batch(batch)?;
        df.write_table(&self.audit_fqn, DataFrameWriteOptions::default())
            .await?;
        Ok(())
    }

    pub async fn append_trace_spans(&self, rows: &[TraceSpanRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let batch = trace_batch(rows)?;
        let ctx = self.ctx.lock().await;
        let df = ctx.read_batch(batch)?;
        df.write_table(&self.trace_fqn, DataFrameWriteOptions::default())
            .await?;
        Ok(())
    }

    pub async fn append_audit_events_with_trace_spans(
        &self,
        events: &[AuditEvent],
        spans: &[TraceSpanRow],
    ) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let audit_b = audit_batch(events)?;
        let ctx = self.ctx.lock().await;
        let df = ctx.read_batch(audit_b)?;
        df.write_table(&self.audit_fqn, DataFrameWriteOptions::default())
            .await?;
        if !spans.is_empty() {
            let trace_b = trace_batch(spans)?;
            let df2 = ctx.read_batch(trace_b)?;
            df2.write_table(&self.trace_fqn, DataFrameWriteOptions::default())
                .await?;
        }
        Ok(())
    }

    pub async fn append_trace_heads(&self, rows: &[TraceHeadRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let batch = trace_heads_batch(rows)?;
        let ctx = self.ctx.lock().await;
        let df = ctx.read_batch(batch)?;
        df.write_table(&self.trace_heads_fqn, DataFrameWriteOptions::default())
            .await?;
        Ok(())
    }

    pub async fn scan_audit_row_count(&self) -> anyhow::Result<usize> {
        self.count_star(&self.audit_fqn).await
    }

    pub async fn scan_trace_row_count(&self) -> anyhow::Result<usize> {
        self.count_star(&self.trace_fqn).await
    }

    async fn count_star(&self, fqn: &str) -> anyhow::Result<usize> {
        let sql = format!("SELECT COUNT(*) AS c FROM {fqn}");
        let ctx = self.ctx.lock().await;
        let df = ctx.sql(&sql).await?;
        let batches = df.collect().await?;
        let mut n = 0usize;
        for b in batches {
            if b.num_rows() > 0 {
                let col = b.column(0);
                if let Some(arr) = col
                    .as_any()
                    .downcast_ref::<datafusion::arrow::array::Int64Array>()
                {
                    n += arr.value(0) as usize;
                } else if let Some(arr) = col
                    .as_any()
                    .downcast_ref::<datafusion::arrow::array::UInt64Array>()
                {
                    n += arr.value(0) as usize;
                } else {
                    anyhow::bail!("unexpected count column type");
                }
            }
        }
        Ok(n)
    }

    async fn sql_batches(&self, sql: &str) -> anyhow::Result<Vec<RecordBatch>> {
        let ctx = self.ctx.lock().await;
        let df = ctx.sql(sql).await?;
        df.collect().await.map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Max distinct `tenant_partition` literals in the idempotency SQL `IN` list; above this, omit the filter.
    pub const EXISTING_EVENT_IDS_PARTITION_FILTER_CAP: usize = 64;

    /// Subset of `ids` already present in `audit_events`.
    pub async fn existing_event_ids(
        &self,
        ids: &[Uuid],
        tenant_partitions: Option<&[String]>,
    ) -> anyhow::Result<HashSet<Uuid>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let list = ids
            .iter()
            .map(|u| format!("'{u}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let partition_filter = match tenant_partitions {
            Some(parts)
                if !parts.is_empty()
                    && parts.len() <= Self::EXISTING_EVENT_IDS_PARTITION_FILTER_CAP =>
            {
                let in_list = parts
                    .iter()
                    .map(|p| format!("'{}'", Self::sql_quote(p)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" AND tenant_partition IN ({in_list})")
            }
            Some(parts) if parts.len() > Self::EXISTING_EVENT_IDS_PARTITION_FILTER_CAP => {
                tracing::debug!(
                    target: "plasm_trace_sink.iceberg",
                    partition_count = parts.len(),
                    cap = Self::EXISTING_EVENT_IDS_PARTITION_FILTER_CAP,
                    "existing_event_ids: skipping tenant_partition filter (too many distinct partitions)"
                );
                String::new()
            }
            _ => String::new(),
        };
        let sql = format!(
            "SELECT event_id FROM {} WHERE event_id IN ({list}){partition_filter}",
            self.audit_fqn
        );
        let batches = self.sql_batches(&sql).await?;
        let mut out = HashSet::new();
        for b in batches {
            let col = b
                .column_by_name("event_id")
                .ok_or_else(|| anyhow::anyhow!("missing event_id column"))?;
            let sa = col
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| anyhow::anyhow!("event_id not Utf8"))?;
            for i in 0..b.num_rows() {
                let s = sa.value(i);
                out.insert(Uuid::parse_str(s).map_err(|e| anyhow::anyhow!("{e}"))?);
            }
        }
        Ok(out)
    }

    pub async fn load_trace_events(&self, trace_id: Uuid) -> anyhow::Result<Vec<AuditEvent>> {
        self.load_trace_events_with_where(&format!("trace_id = '{trace_id}'"))
            .await
    }

    pub async fn load_trace_events_for_tenant(
        &self,
        tenant_partition: &str,
        trace_id: Uuid,
    ) -> anyhow::Result<Vec<AuditEvent>> {
        self.load_trace_events_for_tenant_with_head(tenant_partition, trace_id, None)
            .await
    }

    pub async fn load_trace_events_for_tenant_with_head(
        &self,
        tenant_partition: &str,
        trace_id: Uuid,
        head_hint: Option<&TraceHeadRow>,
    ) -> anyhow::Result<Vec<AuditEvent>> {
        let head = match head_hint {
            Some(h) => Some(h.clone()),
            None => {
                self.load_trace_head_row_for_tenant(tenant_partition, trace_id)
                    .await?
            }
        };
        let buckets = head
            .as_ref()
            .map(|h| year_month_buckets_for_trace_ms(h.started_at_ms, h.ended_at_ms));
        let (events, _) = self
            .load_trace_events_for_tenant_pruned(
                tenant_partition,
                trace_id,
                buckets.as_deref(),
                head.is_some(),
            )
            .await?;
        Ok(events)
    }

    /// Load `mcp_trace_segment` audit rows for one trace, optionally pruning by `year_month_bucket`.
    ///
    /// When `year_month_buckets` is non-empty and `retry_on_empty` is true, an empty pruned result
    /// triggers one full-tenant retry (partition-prune flake guard, same pattern as billing).
    pub async fn load_trace_events_for_tenant_pruned(
        &self,
        tenant_partition: &str,
        trace_id: Uuid,
        year_month_buckets: Option<&[i32]>,
        retry_on_empty: bool,
    ) -> anyhow::Result<(Vec<AuditEvent>, IcebergDetailLoadMeta)> {
        let pruned_buckets = year_month_buckets.filter(|b| !b.is_empty());
        let where_clause =
            audit_trace_detail_where(tenant_partition, trace_id, pruned_buckets, true);
        let mut events = self.load_trace_events_with_where(&where_clause).await?;
        let mut prune_fallback = false;
        if events.is_empty() && retry_on_empty && pruned_buckets.is_some() {
            tracing::debug!(
                target: "plasm_trace_sink.iceberg",
                %trace_id,
                tenant_partition,
                year_month_bucket_count = pruned_buckets.map(|b| b.len()).unwrap_or(0),
                "load_trace_events_for_tenant_pruned: empty pruned scan; retrying without year_month_bucket"
            );
            record_iceberg_detail_prune_fallback();
            let where_clause = audit_trace_detail_where(tenant_partition, trace_id, None, true);
            events = self.load_trace_events_with_where(&where_clause).await?;
            prune_fallback = true;
        }
        let meta = IcebergDetailLoadMeta {
            iceberg_detail_pruned: pruned_buckets.is_some() && !prune_fallback,
            year_month_bucket_count: pruned_buckets.map(|b| b.len()).unwrap_or(0),
            iceberg_detail_prune_fallback: prune_fallback,
        };
        Ok((events, meta))
    }

    async fn load_trace_head_row_for_tenant(
        &self,
        tenant_partition: &str,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<TraceHeadRow>> {
        let heads = self.load_latest_trace_heads(&[trace_id]).await?;
        Ok(heads
            .into_iter()
            .find(|h| h.tenant_partition == tenant_partition))
    }

    async fn load_trace_events_with_where(
        &self,
        where_clause: &str,
    ) -> anyhow::Result<Vec<AuditEvent>> {
        let sql = format!(
            "SELECT event_id, schema_version, emitted_at, ingested_at, trace_id, mcp_session_id, \
             plasm_prompt_hash, plasm_execute_session, run_id, call_index, line_index, tenant_id, \
             principal_sub, tenant_partition, event_kind, request_units, payload_json, \
             workspace_slug, project_slug, year_month_bucket \
             FROM {} WHERE {} \
             ORDER BY emitted_at ASC NULLS LAST, call_index ASC NULLS LAST, line_index ASC NULLS LAST",
            self.audit_fqn, where_clause
        );
        let batches = self.sql_batches(&sql).await?;
        let mut events = Vec::new();
        for b in batches {
            for row in 0..b.num_rows() {
                events.push(decode_audit_row(&b, row)?);
            }
        }
        Ok(events)
    }

    async fn batch_load_mcp_trace_segment_json(
        &self,
        tenant_partition: &str,
        trace_ids: &[uuid::Uuid],
    ) -> anyhow::Result<std::collections::HashMap<uuid::Uuid, Vec<serde_json::Value>>> {
        use std::collections::HashMap;
        if trace_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let trace_in = trace_ids
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let tenant_q = Self::sql_quote(tenant_partition);
        let kind_q = Self::sql_quote(AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT);
        let where_clause = format!(
            "tenant_partition = '{tenant_q}' AND trace_id IN ({trace_in}) AND event_kind = '{kind_q}'"
        );
        let events = self.load_trace_events_with_where(&where_clause).await?;
        let mut out: HashMap<uuid::Uuid, Vec<serde_json::Value>> = HashMap::new();
        for e in events {
            if let Some(rec) = trace_detail_record_from_audit_event(&e) {
                out.entry(e.trace_id).or_default().push(rec.record);
            }
        }
        Ok(out)
    }

    /// Full scan of `trace_heads` (used to seed SQL projections on first deploy).
    pub async fn scan_all_trace_heads(&self) -> anyhow::Result<Vec<TraceHeadRow>> {
        let sql = format!(
            "SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, status, \
             started_at_ms, ended_at_ms, updated_at_ms, expression_lines, max_call_index, totals_json, \
             workspace_slug \
             FROM {}",
            self.trace_heads_fqn
        );
        let batches = self.sql_batches(&sql).await?;
        let mut out = Vec::new();
        for b in batches {
            for row in 0..b.num_rows() {
                out.push(decode_trace_head_row(&b, row)?);
            }
        }
        Ok(out)
    }

    pub async fn load_latest_trace_heads(
        &self,
        trace_ids: &[Uuid],
    ) -> anyhow::Result<Vec<TraceHeadRow>> {
        if trace_ids.is_empty() {
            return Ok(Vec::new());
        }
        let trace_in = trace_ids
            .iter()
            .map(|u| format!("'{}'", Self::sql_quote(&u.to_string())))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, status, \
             started_at_ms, ended_at_ms, updated_at_ms, expression_lines, max_call_index, totals_json, \
             workspace_slug \
             FROM {} WHERE trace_id IN ({}) ORDER BY updated_at_ms DESC",
            self.trace_heads_fqn, trace_in
        );
        let batches = self.sql_batches(&sql).await?;
        use std::collections::HashMap;
        let mut by_trace: HashMap<Uuid, TraceHeadRow> = HashMap::new();
        for b in batches {
            for row in 0..b.num_rows() {
                let h = decode_trace_head_row(&b, row)?;
                by_trace.entry(h.trace_id).or_insert(h);
            }
        }
        Ok(by_trace.into_values().collect())
    }

    pub async fn load_billing_usage_scoped(
        &self,
        tenant: &TenantId,
        window: TimeWindow,
    ) -> anyhow::Result<Vec<TraceSpanRow>> {
        // Scan billing spans without `tenant_partition = ...` in SQL: partition-pruned scans on
        // identity-partitioned Iceberg tables have been observed to flake empty under DataFusion
        // while `COUNT(*)` still sees rows. Filter tenant + window in Rust (tables stay small).
        let sql = format!(
            "SELECT span_id, event_id, trace_id, emitted_at, tenant_partition, mcp_session_id, \
             plasm_prompt_hash, plasm_execute_session, run_id, call_index, line_index, span_name, \
             is_billing_event, billing_event_type, request_units, duration_ms, attributes_json, \
             api_entry_id, capability, year_month_bucket \
             FROM {} WHERE is_billing_event = true",
            self.trace_fqn
        );
        let batches = self.sql_batches(&sql).await?;
        let to_inclusive = window.to + chrono::Duration::microseconds(999_999);
        let want = tenant.as_str();
        let mut rows = Vec::new();
        for b in batches {
            for row in 0..b.num_rows() {
                let r = decode_trace_row(&b, row)?;
                if r.tenant_partition == want
                    && r.emitted_at >= window.from
                    && r.emitted_at <= to_inclusive
                {
                    rows.push(r);
                }
            }
        }
        Ok(rows)
    }

    pub async fn load_billing_usage_global(
        &self,
        window: TimeWindow,
    ) -> anyhow::Result<Vec<TraceSpanRow>> {
        let sql = format!(
            "SELECT span_id, event_id, trace_id, emitted_at, tenant_partition, mcp_session_id, \
             plasm_prompt_hash, plasm_execute_session, run_id, call_index, line_index, span_name, \
             is_billing_event, billing_event_type, request_units, duration_ms, attributes_json, \
             api_entry_id, capability, year_month_bucket \
             FROM {} WHERE is_billing_event = true",
            self.trace_fqn
        );
        let batches = self.sql_batches(&sql).await?;
        let to_inclusive = window.to + chrono::Duration::microseconds(999_999);
        let mut rows = Vec::new();
        for b in batches {
            for row in 0..b.num_rows() {
                let r = decode_trace_row(&b, row)?;
                if r.emitted_at >= window.from && r.emitted_at <= to_inclusive {
                    rows.push(r);
                }
            }
        }
        Ok(rows)
    }

    pub async fn list_trace_summaries(
        &self,
        filter: TraceListFilter<'_>,
    ) -> anyhow::Result<Vec<TraceSummary>> {
        let tenant_q = Self::sql_quote(filter.tenant.as_str());
        let project_clause = match filter.project_slug {
            Some(ps) if !ps.is_empty() => format!(" AND project_slug = '{}'", Self::sql_quote(ps)),
            _ => String::new(),
        };
        let status_clause = match filter.status {
            TraceListStatusFilter::All => String::new(),
            TraceListStatusFilter::Live => " AND status = 'live'".to_string(),
            TraceListStatusFilter::Completed => " AND status = 'completed'".to_string(),
        };
        let sql = format!(
            "SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, status, \
             started_at_ms, ended_at_ms, updated_at_ms, expression_lines, max_call_index, totals_json, \
             workspace_slug \
             FROM ( \
               SELECT trace_id, tenant_partition, tenant_id, project_slug, mcp_session_id, status, \
                      started_at_ms, ended_at_ms, updated_at_ms, expression_lines, max_call_index, totals_json, \
                      workspace_slug, \
                      ROW_NUMBER() OVER (PARTITION BY trace_id ORDER BY updated_at_ms DESC) AS rn \
               FROM {} WHERE tenant_partition = '{}'{}{} \
             ) latest \
             WHERE rn = 1 \
             ORDER BY started_at_ms DESC \
             OFFSET {} LIMIT {}",
            self.trace_heads_fqn,
            tenant_q,
            project_clause,
            status_clause,
            filter.offset,
            filter.limit.clamp(1, 500)
        );
        let batches = self.sql_batches(&sql).await?;
        let mut heads = Vec::new();
        for b in batches {
            for row in 0..b.num_rows() {
                let h = decode_trace_head_row(&b, row)?;
                heads.push(h);
            }
        }
        let sparse_ids: Vec<uuid::Uuid> = heads
            .iter()
            .filter(|h| head_needs_segment_recompute(h))
            .map(|h| h.trace_id)
            .collect();
        let segments_by_trace = if sparse_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            self.batch_load_mcp_trace_segment_json(filter.tenant.as_str(), &sparse_ids)
                .await?
        };
        let out = heads
            .into_iter()
            .map(|h| {
                let records = segments_by_trace
                    .get(&h.trace_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let totals = trace_totals_from_head_or_records(&h, records);
                TraceSummary {
                    trace_id: h.trace_id,
                    mcp_session_id: h.mcp_session_id.unwrap_or_default(),
                    status: h.status,
                    started_at_ms: h.started_at_ms.max(0) as u64,
                    ended_at_ms: h.ended_at_ms.map(|v| v.max(0) as u64),
                    project_slug: h.project_slug,
                    tenant_id: h.tenant_id,
                    totals,
                }
            })
            .collect::<Vec<_>>();
        Ok(out)
    }

    pub async fn load_trace_detail(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<DurableTraceDetail>> {
        self.load_trace_detail_with_head(tenant, trace_id, None)
            .await
    }

    /// Cold-path detail read with optional SQL `trace_heads` hint for month pruning.
    pub async fn load_trace_detail_with_head(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
        head_hint: Option<&TraceHeadRow>,
    ) -> anyhow::Result<Option<DurableTraceDetail>> {
        let tenant_partition = tenant.as_str();
        let events = self
            .load_trace_events_for_tenant_with_head(tenant_partition, trace_id, head_hint)
            .await?;
        if events.is_empty() {
            return Ok(None);
        }
        let detail = durable_detail_from_events(trace_id, events, tenant_partition.to_string());
        Ok(Some(detail))
    }
}

/// Observability for head-guided Iceberg detail scans.
#[derive(Debug, Clone, Copy)]
pub struct IcebergDetailLoadMeta {
    pub iceberg_detail_pruned: bool,
    pub year_month_bucket_count: usize,
    pub iceberg_detail_prune_fallback: bool,
}

fn audit_trace_detail_where(
    tenant_partition: &str,
    trace_id: Uuid,
    year_month_buckets: Option<&[i32]>,
    mcp_trace_segment_only: bool,
) -> String {
    let tenant_q = IcebergSink::sql_quote(tenant_partition);
    let mut parts = vec![
        format!("tenant_partition = '{tenant_q}'"),
        format!("trace_id = '{trace_id}'"),
    ];
    if mcp_trace_segment_only {
        parts.push(format!(
            "event_kind = '{}'",
            IcebergSink::sql_quote(AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT)
        ));
    }
    if let Some(buckets) = year_month_buckets {
        if !buckets.is_empty() {
            let list = buckets
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("year_month_bucket IN ({list})"));
        }
    }
    parts.join(" AND ")
}

/// Build one HTTP detail row from an ingested `mcp_trace_segment` audit event.
pub fn trace_detail_record_from_audit_event(e: &AuditEvent) -> Option<TraceDetailRecord> {
    if e.event_kind != AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT {
        return None;
    }
    let trace = serde_json::from_value::<plasm_trace::TraceEvent>(e.payload.clone()).ok()?;
    let mut record = serde_json::to_value(&trace).ok()?;
    if let serde_json::Value::Object(ref mut map) = record {
        map.insert(
            "event_id".to_string(),
            serde_json::Value::String(e.event_id.to_string()),
        );
        map.insert(
            "emitted_at".to_string(),
            serde_json::Value::String(e.emitted_at.to_rfc3339()),
        );
    }
    Some(TraceDetailRecord {
        kind: AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT.to_string(),
        record,
    })
}

pub fn durable_detail_from_events(
    trace_id: Uuid,
    mut events: Vec<AuditEvent>,
    tenant_id_fallback: String,
) -> DurableTraceDetail {
    sort_audit_events(&mut events);
    let first = events.first();
    let last = events.last();
    let started_at_ms = first
        .map(|e| e.emitted_at.timestamp_millis().max(0) as u64)
        .unwrap_or(0);
    let ended_at_ms = last.map(|e| e.emitted_at.timestamp_millis().max(0) as u64);
    let mcp_session_id = first
        .and_then(|e| e.mcp_session_id.clone())
        .unwrap_or_default();
    let tenant_id = first
        .and_then(|e| e.tenant_id.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or(tenant_id_fallback);
    let project_slug = first
        .map(|e| e.audit_project_slug())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "main".to_string());

    struct McpTraceRow {
        sort_key: usize,
        trace: TraceEvent,
    }

    let mut mcp_rows: Vec<McpTraceRow> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        if e.event_kind != AUDIT_EVENT_KIND_MCP_TRACE_SEGMENT {
            continue;
        }
        let Ok(trace) = serde_json::from_value::<TraceEvent>(e.payload.clone()) else {
            continue;
        };
        mcp_rows.push(McpTraceRow { sort_key: i, trace });
    }
    let records: Vec<TraceDetailRecord> = events
        .iter()
        .filter_map(trace_detail_record_from_audit_event)
        .collect();

    let mut session_ix: Vec<usize> = (0..mcp_rows.len()).collect();
    session_ix.sort_by(|&ia, &ib| {
        mcp_rows[ia]
            .trace
            .emitted_at_ms
            .cmp(&mcp_rows[ib].trace.emitted_at_ms)
            .then_with(|| mcp_rows[ia].sort_key.cmp(&mcp_rows[ib].sort_key))
    });
    let session_traces: Vec<TraceEvent> = session_ix
        .into_iter()
        .map(|i| mcp_rows[i].trace.clone())
        .collect();
    let session = session_data_from_ordered_events(mcp_session_id.as_str(), session_traces);
    let totals: TraceTotals = totals_from_session_data(&session).into();

    DurableTraceDetail {
        summary: TraceSummary {
            trace_id,
            mcp_session_id,
            status: "completed".to_string(),
            started_at_ms,
            ended_at_ms,
            project_slug,
            tenant_id,
            totals,
        },
        records,
    }
}

pub fn sort_audit_events(events: &mut [AuditEvent]) {
    events.sort_by(|a, b| {
        a.emitted_at
            .cmp(&b.emitted_at)
            .then_with(|| a.call_index.cmp(&b.call_index))
            .then_with(|| a.line_index.cmp(&b.line_index))
    });
}

fn decode_audit_row(batch: &RecordBatch, row: usize) -> anyhow::Result<AuditEvent> {
    let event_id = uuid_col(batch, "event_id", row)?;
    let schema_version = i32_col(batch, "schema_version", row)?;
    let emitted_at = ts_col(batch, "emitted_at", row)?;
    let ingested_at = ts_col(batch, "ingested_at", row)?;
    let trace_id = uuid_col(batch, "trace_id", row)?;
    let mcp_session_id = opt_string_col(batch, "mcp_session_id", row);
    let plasm_prompt_hash = opt_string_col(batch, "plasm_prompt_hash", row);
    let plasm_execute_session = opt_string_col(batch, "plasm_execute_session", row);
    let run_id = opt_string_col(batch, "run_id", row);
    let call_index = opt_i64_col(batch, "call_index", row);
    let line_index = opt_i64_col(batch, "line_index", row);
    let tenant_id = opt_string_col(batch, "tenant_id", row);
    let principal_sub = opt_string_col(batch, "principal_sub", row);
    let _tenant_partition = string_col(batch, "tenant_partition", row)?;
    let event_kind = string_col(batch, "event_kind", row)?;
    let request_units = i64_col(batch, "request_units", row)?;
    let payload_s = string_col(batch, "payload_json", row)?;
    let payload: serde_json::Value =
        serde_json::from_str(&payload_s).unwrap_or_else(|_| serde_json::json!({}));

    let workspace_slug = opt_string_col(batch, "workspace_slug", row).filter(|s| !s.is_empty());
    let project_slug = opt_string_col(batch, "project_slug", row).filter(|s| !s.is_empty());

    Ok(AuditEvent {
        event_id,
        schema_version,
        emitted_at,
        ingested_at,
        trace_id,
        mcp_session_id,
        plasm_prompt_hash,
        plasm_execute_session,
        run_id,
        call_index,
        line_index,
        tenant_id,
        principal_sub,
        workspace_slug,
        project_slug,
        event_kind,
        request_units,
        payload,
    })
}

fn decode_trace_row(batch: &RecordBatch, row: usize) -> anyhow::Result<TraceSpanRow> {
    let span_id = uuid_col(batch, "span_id", row)?;
    let event_id = uuid_col(batch, "event_id", row)?;
    let trace_id = uuid_col(batch, "trace_id", row)?;
    let emitted_at = ts_col(batch, "emitted_at", row)?;
    let tenant_partition = string_col(batch, "tenant_partition", row)?;
    let mcp_session_id = opt_string_col(batch, "mcp_session_id", row);
    let plasm_prompt_hash = opt_string_col(batch, "plasm_prompt_hash", row);
    let plasm_execute_session = opt_string_col(batch, "plasm_execute_session", row);
    let run_id = opt_string_col(batch, "run_id", row);
    let call_index = opt_i64_col(batch, "call_index", row);
    let line_index = opt_i64_col(batch, "line_index", row);
    let span_name = string_col(batch, "span_name", row)?;
    let is_billing_event = bool_col(batch, "is_billing_event", row)?;
    let billing_event_type = opt_string_col(batch, "billing_event_type", row);
    let request_units = i64_col(batch, "request_units", row)?;
    let duration_ms = opt_i64_col(batch, "duration_ms", row);
    let attr_s = string_col(batch, "attributes_json", row)?;
    let attributes_json: serde_json::Value =
        serde_json::from_str(&attr_s).unwrap_or_else(|_| serde_json::json!({}));

    let api_entry_id = opt_string_col(batch, "api_entry_id", row);
    let capability = opt_string_col(batch, "capability", row);

    Ok(TraceSpanRow {
        span_id,
        event_id,
        trace_id,
        emitted_at,
        tenant_partition,
        mcp_session_id,
        plasm_prompt_hash,
        plasm_execute_session,
        run_id,
        call_index,
        line_index,
        span_name,
        is_billing_event,
        billing_event_type,
        request_units,
        duration_ms,
        api_entry_id,
        capability,
        attributes_json,
    })
}

fn decode_trace_head_row(batch: &RecordBatch, row: usize) -> anyhow::Result<TraceHeadRow> {
    let totals_json = if batch.column_by_name("totals_json").is_some() {
        opt_string_col(batch, "totals_json", row).unwrap_or_default()
    } else {
        String::new()
    };
    let workspace_slug = if batch.column_by_name("workspace_slug").is_some() {
        opt_string_col(batch, "workspace_slug", row).unwrap_or_default()
    } else {
        String::new()
    };
    Ok(TraceHeadRow {
        trace_id: uuid_col(batch, "trace_id", row)?,
        tenant_partition: string_col(batch, "tenant_partition", row)?,
        tenant_id: string_col(batch, "tenant_id", row)?,
        project_slug: string_col(batch, "project_slug", row)?,
        mcp_session_id: opt_string_col(batch, "mcp_session_id", row),
        status: string_col(batch, "status", row)?,
        started_at_ms: i64_col(batch, "started_at_ms", row)?,
        ended_at_ms: opt_i64_col(batch, "ended_at_ms", row),
        updated_at_ms: i64_col(batch, "updated_at_ms", row)?,
        expression_lines: i64_col(batch, "expression_lines", row)?,
        max_call_index: opt_i64_col(batch, "max_call_index", row),
        totals_json,
        workspace_slug,
    })
}

fn col_named<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a ArrayRef> {
    batch
        .column_by_name(name)
        .ok_or_else(|| anyhow::anyhow!("missing column {name}"))
}

fn opt_string_col(batch: &RecordBatch, name: &str, row: usize) -> Option<String> {
    let col = batch.column_by_name(name)?;
    let sa = col.as_any().downcast_ref::<StringArray>()?;
    if sa.is_null(row) {
        None
    } else {
        Some(sa.value(row).to_string())
    }
}

fn string_col(batch: &RecordBatch, name: &str, row: usize) -> anyhow::Result<String> {
    opt_string_col(batch, name, row).ok_or_else(|| anyhow::anyhow!("null or missing {name}"))
}

fn i32_col(batch: &RecordBatch, name: &str, row: usize) -> anyhow::Result<i32> {
    let col = col_named(batch, name)?;
    let a = col
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow::anyhow!("{name} not Int32"))?;
    if a.is_null(row) {
        anyhow::bail!("null {name}");
    }
    Ok(a.value(row))
}

fn i64_col(batch: &RecordBatch, name: &str, row: usize) -> anyhow::Result<i64> {
    let col = col_named(batch, name)?;
    let a = col
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow::anyhow!("{name} not Int64"))?;
    if a.is_null(row) {
        anyhow::bail!("null {name}");
    }
    Ok(a.value(row))
}

fn opt_i64_col(batch: &RecordBatch, name: &str, row: usize) -> Option<i64> {
    let col = batch.column_by_name(name)?;
    let a = col.as_any().downcast_ref::<Int64Array>()?;
    if a.is_null(row) {
        None
    } else {
        Some(a.value(row))
    }
}

fn bool_col(batch: &RecordBatch, name: &str, row: usize) -> anyhow::Result<bool> {
    let col = col_named(batch, name)?;
    let a = col
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow::anyhow!("{name} not Boolean"))?;
    if a.is_null(row) {
        anyhow::bail!("null {name}");
    }
    Ok(a.value(row))
}

fn ts_col(batch: &RecordBatch, name: &str, row: usize) -> anyhow::Result<DateTime<Utc>> {
    let col = col_named(batch, name)?;
    let a = col
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .ok_or_else(|| anyhow::anyhow!("{name} not TimestampMicrosecond"))?;
    if a.is_null(row) {
        anyhow::bail!("null {name}");
    }
    let micros = a.value(row);
    DateTime::from_timestamp_micros(micros).ok_or_else(|| anyhow::anyhow!("timestamp out of range"))
}

fn uuid_col(batch: &RecordBatch, name: &str, row: usize) -> anyhow::Result<Uuid> {
    let s = string_col(batch, name, row)?;
    Uuid::parse_str(&s).map_err(|e| anyhow::anyhow!("{e}"))
}

#[async_trait]
impl AuditSpanWriter for IcebergSink {
    async fn append_audit_events(&self, events: &[AuditEvent]) -> anyhow::Result<()> {
        IcebergSink::append_audit_events(self, events).await
    }

    async fn append_trace_spans(&self, rows: &[TraceSpanRow]) -> anyhow::Result<()> {
        IcebergSink::append_trace_spans(self, rows).await
    }

    async fn append_audit_events_with_trace_spans(
        &self,
        events: &[AuditEvent],
        spans: &[TraceSpanRow],
    ) -> anyhow::Result<()> {
        IcebergSink::append_audit_events_with_trace_spans(self, events, spans).await
    }

    async fn append_trace_heads(&self, rows: &[TraceHeadRow]) -> anyhow::Result<()> {
        IcebergSink::append_trace_heads(self, rows).await
    }
}

#[async_trait]
impl AuditSpanReader for IcebergSink {
    async fn existing_event_ids(
        &self,
        ids: &[Uuid],
        tenant_partitions: Option<&[String]>,
    ) -> anyhow::Result<HashSet<Uuid>> {
        IcebergSink::existing_event_ids(self, ids, tenant_partitions).await
    }

    async fn load_trace_events(&self, trace_id: Uuid) -> anyhow::Result<Vec<AuditEvent>> {
        IcebergSink::load_trace_events(self, trace_id).await
    }

    async fn load_trace_events_for_tenant(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
    ) -> anyhow::Result<Vec<AuditEvent>> {
        IcebergSink::load_trace_events_for_tenant(self, tenant.as_str(), trace_id).await
    }

    async fn load_latest_trace_heads(
        &self,
        trace_ids: &[Uuid],
    ) -> anyhow::Result<Vec<TraceHeadRow>> {
        IcebergSink::load_latest_trace_heads(self, trace_ids).await
    }

    async fn load_billing_usage_scoped(
        &self,
        tenant: &TenantId,
        window: TimeWindow,
    ) -> anyhow::Result<Vec<TraceSpanRow>> {
        IcebergSink::load_billing_usage_scoped(self, tenant, window).await
    }

    async fn load_billing_usage_global(
        &self,
        window: TimeWindow,
    ) -> anyhow::Result<Vec<TraceSpanRow>> {
        IcebergSink::load_billing_usage_global(self, window).await
    }

    async fn list_trace_summaries(
        &self,
        filter: TraceListFilter<'_>,
    ) -> anyhow::Result<Vec<TraceSummary>> {
        IcebergSink::list_trace_summaries(self, filter).await
    }

    async fn load_trace_detail(
        &self,
        tenant: &TenantId,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<DurableTraceDetail>> {
        IcebergSink::load_trace_detail(self, tenant, trace_id).await
    }
}

#[cfg(test)]
mod detail_where_tests {
    use super::*;

    #[test]
    fn audit_trace_detail_where_includes_tenant_trace_kind_and_months() {
        let trace_id = Uuid::new_v4();
        let w = audit_trace_detail_where("tenant-a", trace_id, Some(&[202603, 202604]), true);
        assert!(w.contains("tenant_partition = 'tenant-a'"));
        assert!(w.contains(&format!("trace_id = '{trace_id}'")));
        assert!(w.contains("event_kind = 'mcp_trace_segment'"));
        assert!(w.contains("year_month_bucket IN (202603, 202604)"));
    }

    #[test]
    fn audit_trace_detail_where_omits_months_when_none() {
        let trace_id = Uuid::new_v4();
        let w = audit_trace_detail_where("tenant-a", trace_id, None, true);
        assert!(!w.contains("year_month_bucket"));
    }
}

#[cfg(test)]
mod schema_freeze_tests {
    use super::*;

    #[test]
    fn audit_iceberg_schema_has_pruning_and_slugs() {
        let s = audit_iceberg_schema();
        assert_eq!(s.iter().count(), 20);
        assert!(s.get_name("workspace_slug").is_some());
        assert!(s.get_name("project_slug").is_some());
        assert!(s.get_name("year_month_bucket").is_some());
    }

    #[test]
    fn trace_iceberg_schema_has_lineage_and_bucket() {
        let s = trace_iceberg_schema();
        assert_eq!(s.iter().count(), 20);
        assert!(s.get_name("api_entry_id").is_some());
        assert!(s.get_name("capability").is_some());
        assert!(s.get_name("year_month_bucket").is_some());
    }

    #[test]
    fn trace_heads_iceberg_schema_has_workspace() {
        let s = trace_heads_iceberg_schema();
        assert_eq!(s.iter().count(), 13);
        assert!(s.get_name("workspace_slug").is_some());
    }
}
