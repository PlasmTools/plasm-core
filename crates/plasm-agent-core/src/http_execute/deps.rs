//! Explicit shared imports for `http_execute` submodules.

#![allow(unused_imports)]

pub(crate) use axum::body::Bytes;
pub(crate) use axum::extract::{Extension, Path, Query};
pub(crate) use axum::http::header::{ACCEPT, CONTENT_TYPE, LOCATION};
pub(crate) use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
pub(crate) use axum::response::sse::{Event, KeepAlive, Sse};
pub(crate) use axum::response::{IntoResponse, Response};
pub(crate) use axum::routing::{get, post};
pub(crate) use axum::{Json, Router};
pub(crate) use futures_util::stream::{self, Stream, StreamExt};
pub(crate) use http_problem::prelude::{StatusCode as ProblemStatus, Uri};
pub(crate) use http_problem::Problem;
pub(crate) use indexmap::IndexMap;
pub(crate) use plasm_core::discovery::{CgsCatalog, DiscoveryError};
pub(crate) use plasm_core::error_render::{render_parse_error_with_feedback, FeedbackStyle};
pub(crate) use plasm_core::{
    expr_parser::{self, ParsedExpr},
    normalize_expr_query_capabilities, normalize_expr_query_capabilities_federated,
    teaching_tsv_from_wrapped_prompt, AuthScheme, CgsContext, Expr, SymbolMap, TeachingFenceSlice,
    CGS,
};
pub(crate) use plasm_runtime::{
    auth_resolution_mode_from_env, entity_to_agent_row_json, validate_principal_for_mode,
    AuthResolutionMode, AuthResolver, CompileOperationFn, CompileQueryFn, ExecuteOptions,
    ExecuteSessionMaterial, ExecutionResult, ExecutionSource, ExecutionStats,
    QueryPaginationResumeData, RuntimeError,
};
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use std::collections::{BTreeSet, HashMap};
pub(crate) use std::convert::Infallible;
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::time::{Instant, SystemTime, UNIX_EPOCH};
pub(crate) use tracing::Instrument;
pub(crate) use uuid::Uuid;

pub(crate) use crate::execute_path_ids::{ExecuteSessionId, PromptHashHex};
pub(crate) use crate::execute_session::{ExecuteSession, SessionReuseKey, SessionRunSummary};
pub(crate) use crate::http_problem_util::problem_response;
pub(crate) use crate::http_problem_util::problem_types;
pub(crate) use crate::incoming_auth::{
    incoming_auth_problem, session_allows_principal, tenant_scope, IncomingPrincipal,
};
pub(crate) use crate::mcp_run_markdown::execute_expression_preview;
pub(crate) use crate::output::{
    format_result_with_cgs, http_execute_results_value, reference_only_omitted_field_names,
    OutputFormat,
};
pub(crate) use crate::run_artifacts::{
    artifact_http_path, plasm_run_resource_uri, plasm_session_short_resource_uri,
    plasm_short_resource_uri, plasm_short_resource_uri_logical, RunArtifactHandle, RunArtifactId,
    RunArtifactWire,
};
pub(crate) use crate::server_state::PlasmHostState;
pub(crate) use crate::trace_hub::{
    McpPlasmTraceSink, PlasmLineTraceMeta, TraceEvent, TraceSegment,
};
pub(crate) use crate::trace_sink_emit::{McpTraceAuditFields, PlasmTraceContext};
