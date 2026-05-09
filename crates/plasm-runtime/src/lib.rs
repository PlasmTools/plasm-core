//! # plasm-runtime
//!
//! Execution engine for Plasm with graph cache, record/replay, and response normalization.
//!
//! This crate orchestrates the full execution pipeline:
//!
//! ```text
//! Expr → type check → normalize predicate → compile to BackendFilter
//!      → build CML environment → compile CML to HTTP request
//!      → execute (live / replay / hybrid) → normalize response
//!      → decode via schema-driven decoder → merge into graph cache
//! ```
//!
//! ## Operational Semantics
//!
//! ### Expression Evaluation
//!
//! Given an [`Expr`](plasm_core::Expr) and a [`CGS`](plasm_core::CGS):
//!
//! 1. **Type check**: validate the expression against the schema
//!    ([`type_check_expr`](plasm_core::type_check_expr))
//! 2. **Resolve capability**: find the matching capability in the CGS
//!    (e.g. `find_capability("Pet", Query)`)
//! 3. **Build CML environment**: populate variables from the expression
//!    - Query: predicate field=value pairs (for query-param APIs) + compiled filter
//!    - Get: `id` + all path template variable names
//!    - Create: `input` object
//!    - Delete/Invoke: `id` + path vars + optional `input`
//!    - When the host sets [`ExecuteOptions::execute_session`](execution::ExecuteOptions), reserved
//!      `plasm_execute_prompt_hash` / `plasm_execute_session_id` keys are merged before compile (see
//!      [`merge_plasm_execute_session_env`](execution::merge_plasm_execute_session_env)), and optional
//!      session-bound mirrors (`share_token` for `?token=`, Proof `proof_base_token` as `base_token` for `/ops`)
//!      may be merged earlier so mappings stay token-free (see [`merge_plasm_execute_session_share_token_env`](execution::merge_plasm_execute_session_share_token_env),
//!      [`merge_plasm_execute_session_proof_base_token_env`](execution::merge_plasm_execute_session_proof_base_token_env)).
//! 4. **Compile CML template**: evaluate the capability's mapping template against
//!    the environment to produce a concrete HTTP request (method, path, query, body)
//! 5. **Execute**: dispatch based on [`ExecutionMode`]
//!    - Live: send HTTP request, return response
//!    - Replay: look up by blake3 fingerprint in replay store
//!    - Hybrid: try replay, fall through to live on miss
//! 6. **Normalize response**: wrap bare arrays in `{"results": [...]}`
//! 7. **Decode**: schema-driven decoder extracts typed entities from JSON response
//! 8. **Cache**: merge decoded entities into the graph cache by stable reference
//!
//! ### Graph Cache
//!
//! The [`GraphCache`] is a materialized subgraph of the backend. Entities are stored
//! by [`Ref`](plasm_core::Ref) (entity_type + ID) with merge semantics:
//!
//! - **Identity stability**: same Ref always refers to the same entity
//! - **Merge**: newer data overwrites older, field-by-field for same-timestamp
//! - **Invalidation**: entities can be removed by reference or by predicate
//! - **Type index**: efficient lookup of all entities of a given type
//!
//! The cache is not a TTL cache — it's a partial materialisation of the API's entity
//! graph. Entities persist until explicitly invalidated or the cache is cleared.
//!
//! **Semi-formal invariants** (I1–I7: identity, type index, monotonic timestamps, single-writer rule,
//! clone/merge policy) are documented in the [`cache`] module; a tabular summary lives in
//! [`README.md`](../README.md) next to this crate’s `Cargo.toml`.
//!
//! ### Record/Replay
//!
//! Every compiled HTTP request gets a deterministic fingerprint via blake3:
//!
//! ```text
//! fingerprint = blake3(method + path + normalized_body + normalized_query)
//! ```
//!
//! The [`ReplayStore`] trait supports file-system and in-memory implementations.
//! A [`ReplayEntry`] stores the request, response, decoded entities, and schema
//! snapshot, enabling:
//!
//! - **Deterministic testing**: same fingerprint → same response, no network
//! - **Schema drift detection**: compare decoded entities across versions
//! - **Time-travel debugging**: replay exact queries from earlier sessions
//!
//! ### Response Normalization
//!
//! Real APIs return varied response shapes. The runtime normalizes before decoding:
//!
//! - Bare JSON array `[...]` → `{"results": [...]}`
//! - Object with `results` key → pass through
//! - Single object → decoded as single entity
//!
//! ### Schema-Driven Decoding
//!
//! The decoder is built from the CGS entity definition at runtime:
//!
//! - Each entity field becomes a [`FieldDecoder`](plasm_compile::FieldDecoder)
//!   that extracts a value from the JSON response by field name
//! - Collection responses use `results[*]` as the source path
//! - Single-entity responses use the root object
//!
//! This replaces the need for hand-written decoders per API.
//!
//! ## Traits
//!
//! - [`ExprExecutor`](crate::ExprExecutor): abstract expression execution (implemented by [`ExecutionEngine`](crate::ExecutionEngine)).
//! - [`CacheStore`](crate::CacheStore): pluggable entity cache (implemented by [`GraphCache`](crate::GraphCache)).

pub mod api_error_detail;
pub mod auth;
pub mod auth_resolution;
pub mod cache;
pub mod error;
pub mod evm;
pub mod execution;
pub mod hosted_oauth_kv;
pub mod http_trace;
pub mod http_transport;
pub mod invoke_preflight;
pub mod mockserver;
pub mod oauth_client;
pub mod oauth_token_debug;
pub mod replay;
pub mod runtime_error_render;
pub mod session_graph_cache;

mod view_execution;

mod runtime_metrics;
mod spans;

pub use api_error_detail::{
    cap_detail, graphql_errors_summary, json_api_error_lines, sanitize_preview_chars,
    summarize_json_api_error_for_http, summarize_json_error_body, summarize_text_error_body,
    MAX_API_ERROR_DETAIL_CHARS, MAX_DEBUG_BODY_PREVIEW_CHARS,
};
pub use auth::*;
pub use auth_resolution::{
    auth_resolution_mode_from_env, auth_resolution_mode_from_str, validate_principal_for_mode,
    AuthResolutionMode,
};
pub use cache::*;
pub use error::*;
pub use evm::*;
pub use execution::*;
pub use hosted_oauth_kv::{
    build_oauth_token_http_client, classify_hosted_bearer_utf8, parse_outbound_oauth_kv_v1,
    post_oauth_token_form_json, resolve_hosted_bearer_default_no_refresh,
    runtime_error_is_oauth_invalid_grant, ApplyTokenError, HostedBearerResolution,
    OAuthTokenEndpointError, OutboundOAuthKvParseError, OutboundOAuthKvV1,
    HOSTED_OAUTH_EXPIRY_SKEW_SECS, OUTBOUND_OAUTH_KV_VERSION,
};
pub use http_transport::{HttpTransport, ReqwestHttpTransport};
pub use mockserver::*;
pub use oauth_client::{
    begin_authorization_code_pkce, exchange_authorization_code, OAuthAuthorizationStart,
    OAuthConnectError,
};
pub use oauth_token_debug::TokenEndpointResponseSummary;
pub use replay::*;
pub use runtime_error_render::step_error_from_runtime;
pub use session_graph_cache::MutexGraphCacheSession;
