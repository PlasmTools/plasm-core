//! Expression execution: engine, query/get paths, response prep, and session env.

// Shared prelude for sibling modules (`use super::*`). Composition root itself is re-exports only.
#![allow(unused_imports)]

use crate::api_error_detail::{
    fibery_command_envelope_hint, graphql_errors_summary, graphql_mutation_envelope_failure,
    response_path_step,
};
use crate::evm::{execute_evm_call, execute_evm_logs};
use crate::http_resilience::{HttpResiliencePolicy, ResilientHttpTransport};
use crate::http_transport::{HttpTransport, ReqwestHttpTransport};
use crate::materialization::{CacheTelemetry, ExecutionCacheConsult, SessionMaterialization};
use crate::preflight::{apply_preflight_steps, PreflightInvoke};
use crate::view_plan::ViewAmbientContext;
use crate::{AuthResolver, CachedEntity, CancelSignal, EntityCompleteness, RuntimeError};
use indexmap::IndexMap;
#[cfg(test)]
use plasm_compile::parse_capability_template;
use plasm_compile::{
    compile_operation, compile_query, decode_entities_with_cgs, path_var_names_from_request,
    template_pagination, template_var_names, BackendFilter, CapabilityTemplate, CmlEnv, CmlRequest,
    CompiledOperation, CompiledRequest, HttpBodyFormat, PaginationConfig, PathExpr, PathSegment,
    ResponsePreprocess,
};
use plasm_core::partition_prefer_resolutions;
use plasm_core::resolve_relation_row_resolution;
use plasm_core::{
    cross_entity::{
        choose_strategy, extract_cross_entity_predicates, strip_cross_entity_comparisons,
        CrossEntityStrategy,
    },
    reject_domain_placeholder_in_executable as reject_domain_placeholder_core,
    resolve_query_capability as resolve_query_capability_core, type_check_expr,
    type_check_expr_federated, CapabilityKind, CapabilityParamName, CapabilitySchema, ChainStep,
    EntityDef, EntityFieldName, EntityKey, EntityName, Expr, FieldType, GetExpr, InvokeExpr,
    InvokeInputPayload, Predicate, PromptPipelineConfig, QueryExpr, Ref, RelationMaterialization,
    RelationRowResolution, RelationSchema, RelationScopedFallback, Value, CGS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::Instrument;

mod cache_merge;
mod chain;
mod compile_preflight;
mod embed_cache;
mod engine;
mod engine_get;
mod engine_query;
mod entity_decoder;
mod expr_executor;
mod http_exec;
mod hydrate;
mod identity;
mod mutators;
mod pagination_driver;
mod pagination_state;
mod predicates;
mod projection;
mod query_stream;
mod response_prep;
mod resume;
mod scoped_fanout;
mod session;
mod task_scopes;
mod template_env;
mod types;

#[cfg(test)]
mod credential_tests;
#[cfg(test)]
mod tests;

pub(crate) use hydrate::{
    get_with_session_params, identity_keys_for_entity, stamp_entities_and_mat, synthesized_get,
    wrap_synthesized_get_error, CapabilityParamEnv,
};
pub(crate) use session::{compiled_capability_template, compiled_conflict_rules};

pub use pagination_driver::{PageAudit, PaginationDriver, PaginationTerminalReason};

use self::entity_decoder::{
    create_entity_decoder, create_entity_decoder_for_capability,
    mutating_capability_response_decoder,
};

pub use compile_preflight::preflight_compile_expr;

pub(crate) use pagination_state::PaginationLoopState;
#[cfg(test)]
pub(crate) use pagination_state::{merge_pagination_into_body, pagination_context_map};
pub(crate) use plasm_core::json_value_to_plasm_value as json_to_plasm_value;

pub use types::{
    ExecutionConfig, ExecutionMode, ExecutionResult, ExecutionSource, ExecutionStats, PageResult,
    QueryPaginationResumeData, QueryPaginationState, QueryStream, RowMatchBudget, RowsProgressFn,
    StreamConsumeOpts,
};

pub(crate) use task_scopes::{
    EXECUTION_AUTH_RESOLVER, EXECUTION_CANCEL, EXECUTION_COMPILED_CATALOG,
    EXECUTION_DISPATCH_ENTITY, EXECUTION_EXECUTE_SESSION, EXECUTION_FEDERATION,
    EXECUTION_FINGERPRINT_SINK, EXECUTION_HTTP_BASE, EXECUTION_ROWS_PROGRESS,
};

pub use session::{
    collect_query_stream, cooperative_cancel_check, merge_plasm_execute_session_bind_env,
    merge_plasm_execute_session_env, merge_plasm_execute_session_identity_env,
    report_rows_materialized, ExecuteSessionMaterial, CML_ENV_PLASM_EXECUTE_PROMPT_HASH,
    CML_ENV_PLASM_EXECUTE_SESSION_ID,
};

pub(crate) use session::{
    append_request_fingerprint, compile_operation_dispatch, compile_query_dispatch,
    graph_spill_page_and_trim_hot, reject_domain_placeholder_in_executable,
    resolve_query_capability, try_current_execute_session_material, with_dispatch_entity,
};

pub use engine::{ExecuteOptions, ExecutionEngine, OverlaySourceOptions};

pub(crate) use cache_merge::query_result_merge_cache;
pub(crate) use embed_cache::cache_decoded_entity_tree;
pub use expr_executor::ExprExecutor;
pub(crate) use identity::{
    cml_env_to_identity_strings, current_timestamp, decode_identity_ambient_for_ref,
    merge_entity_id_from_into_input_env, ref_to_identity_ambient, value_to_ambient_string,
};
pub(crate) use predicates::{
    capability_param_names, client_side_predicate_matches, collect_predicate_vars,
    entity_field_predicate, extract_predicate_vars, extract_ref_id, filter_entities_by_predicate,
};
pub(crate) use response_prep::{
    apply_response_preprocess, cml_id_string, extract_single_entity_payload_from_response,
    get_mut_value_at_path, http_collection_source, items_path_segment,
    narrow_http_graphql_response_for_entity_decode, normalize_collection_response,
    preflight_command_envelope_for_single_entity_narrow, preflight_fibery_command_envelope,
    prepare_http_query_response, response_bare_array_wrap_key, single_response_path_step,
    unwrap_single_inner_payload, walk_json_path, wire_id_matches,
};
pub(crate) use scoped_fanout::{
    build_scoped_query_from_fallback, chain_binding_plasm_value, chain_binding_raw_json,
    chain_binding_value, partition_prefer_from_parent_get, partition_scoped_query_fanout,
    ref_from_materialize_bindings_for_get_chain, resolve_cached_targets_from_relation_refs,
};
pub(crate) use template_env::{
    ensure_mutating_operation, normalize_cml_env_scope_entity_refs,
    normalize_cml_scope_entity_ref_value, populate_template_path_env,
};
