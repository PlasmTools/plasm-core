//! Compilation layer for Plasm: CML, predicate compiler, and decoder DSL.
//!
//! This crate transforms typed predicates into backend-specific requests
//! and provides declarative response decoding.
//!
//! CML template types and transport live in [`plasm_cml`].

pub mod backend_filter;
pub mod capability_pagination;
pub mod decoder;
pub mod embed_decode;
pub mod embed_target_decoder;
pub mod embed_tree;
pub mod error;
pub mod json_path;
pub mod predicate_compiler;

pub use plasm_cml::{
    compile_operation, compile_request, eval_cml, eval_cond, parse_capability_template,
    path_var_names_from_request, template_pagination, template_var_names, AuxiliaryHttpMerge,
    CapabilityTemplate, CmlCond, CmlEnv, CmlExpr, CmlRequest, CmlType, CompiledMultipartBody,
    CompiledMultipartPart, CompiledOperation, CompiledRequest, ConcatArraySource, CredentialSource,
    HttpBodyFormat, HttpMethod, HttpResponseDecode, MultipartBodySpec, MultipartPartSpec,
    PaginationConfig, PaginationLocation, PaginationParam, PaginationParamRole, PaginationStop,
    PaginationStrategyKind, PathSegment as CmlPathSegment, ResponsePreprocess, ValidatedPagination,
    ViewCompiled, ViewTemplate,
};

#[cfg(feature = "evm")]
pub use plasm_cml::evm_transport::*;

pub use backend_filter::*;
pub use capability_pagination::{
    emit_paginated_list_missing_cml_pagination_warnings, is_pagination_wire_param,
    paginated_list_missing_cml_pagination_warnings, pagination_contract_validation_errors,
};
pub use decoder::*;
pub use embed_decode::{decode_entities, decode_entities_with_cgs};
pub use embed_target_decoder::entity_decoder_for_from_parent_get_target;
pub use embed_tree::flatten_decoded_embed_descendants;
pub use error::{CompileError, DecodeError};
pub use json_path::path_expr_from_json_segments;
pub use plasm_cml::CmlError;
pub use predicate_compiler::*;

use plasm_core::{QueryExpr, CGS};

/// Canonical compile hook trait objects (shared by `plasm-runtime`).
///
/// [`CmlEnv`] remains a map of [`plasm_core::value::Value`]. Invoke/create IR may hold structured
/// [`plasm_core::InvokeInputPayload`] internally; execution lowers that to [`Value`] before building the
/// env, so plugins see the same wire shapes as the built-in [`compile_operation`] path.
pub type CompileOperationHook =
    dyn Fn(&CapabilityTemplate, &CmlEnv) -> Result<CompiledOperation, CmlError> + Send + Sync;
pub type CompileQueryHook =
    dyn Fn(&QueryExpr, &CGS) -> Result<Option<BackendFilter>, CompileError> + Send + Sync;

pub mod validate_templates;
pub use validate_templates::{
    compile_cgs_capability_templates, load_compiled_catalog_artifact,
    pagination_config_for_capability, validate_cgs_capability_templates, validate_cgs_views,
    CompiledCatalog,
};
