//! Compilation layer for Plasm: CML, predicate compiler, and decoder DSL.
//!
//! This crate transforms typed predicates into backend-specific requests
//! and provides declarative response decoding.
//!
//! CML template types and transport live in [`plasm_cml`].

pub mod backend_filter;
pub mod decoder;
pub mod error;
pub mod json_path;
pub mod predicate_compiler;

pub use plasm_cml::{
    compile_operation, compile_request, eval_cml, eval_cond, parse_capability_template,
    path_var_names_from_request, template_pagination, template_var_names, AuxiliaryHttpMerge,
    CapabilityTemplate, CmlCond, CmlEnv, CmlExpr, CmlRequest, CmlType, CompiledMultipartBody,
    CompiledMultipartPart, CompiledOperation, CompiledRequest, HttpBodyFormat, HttpMethod,
    HttpResponseDecode, MultipartBodySpec, MultipartPartSpec, PaginationConfig, PaginationLocation,
    PaginationParam, PaginationStop, PathSegment as CmlPathSegment, ResponsePreprocess,
    ViewCompiled, ViewTemplate,
};

#[cfg(feature = "evm")]
pub use plasm_cml::evm_transport::*;

pub use backend_filter::*;
pub use decoder::*;
pub use error::{CompileError, DecodeError};
pub use json_path::path_expr_from_json_segments;
pub use plasm_cml::CmlError;
pub use predicate_compiler::*;

use plasm_core::{CapabilitySchema, QueryExpr, CGS};

/// Canonical compile-plugin hook trait objects (shared by `plasm-runtime` and `plasm-plugin-host`).
///
/// [`CmlEnv`] remains a map of [`plasm_core::value::Value`]. Invoke/create IR may hold structured
/// [`plasm_core::InvokeInputPayload`] internally; execution lowers that to [`Value`] before building the
/// env, so plugins see the same wire shapes as the built-in [`compile_operation`] path.
pub type CompileOperationHook =
    dyn Fn(&CapabilityTemplate, &CmlEnv) -> Result<CompiledOperation, CmlError> + Send + Sync;
pub type CompileQueryHook =
    dyn Fn(&QueryExpr, &CGS) -> Result<Option<BackendFilter>, CompileError> + Send + Sync;

/// Ensure every capability's CML mapping template parses (HTTP or EVM transport).
///
/// Call after loading a [`plasm_core::CGS`] so invalid templates fail at validation time
/// instead of first execution.
pub fn validate_cgs_capability_templates(cgs: &plasm_core::CGS) -> Result<(), CmlError> {
    for (name, cap) in &cgs.capabilities {
        parse_capability_template(&cap.mapping.template).map_err(|e| {
            CmlError::InvalidTemplate {
                message: format!("capability `{name}`: {e}"),
            }
        })?;
    }
    Ok(())
}

/// Parse one capability template and return its composable pagination stanza, when present.
///
/// Shared by CLI generation and tool-model projection so both surfaces interpret pagination
/// from the exact same CML parsing path.
pub fn pagination_config_for_capability(cap: &CapabilitySchema) -> Option<PaginationConfig> {
    parse_capability_template(&cap.mapping.template)
        .ok()
        .and_then(|template| template_pagination(&template).cloned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use plasm_core::apply_entity_ref_scope_splat;
    use plasm_core::load_schema;
    use plasm_core::value::Value;

    use super::*;

    fn github_cgs() -> plasm_core::CGS {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        load_schema(&root.join("../../apis/github")).expect("load github schema")
    }

    fn compile_github_repo_scoped_path(capability: &str, repository: &str) -> String {
        let cgs = github_cgs();
        let cap = cgs
            .get_capability(capability)
            .unwrap_or_else(|| panic!("missing capability {capability}"));
        let mut env = CmlEnv::new();
        env.insert(
            "repository".to_string(),
            Value::String(repository.to_string()),
        );
        apply_entity_ref_scope_splat(&mut env, &cgs, cap).expect("scope splat");
        let template = parse_capability_template(&cap.mapping.template)
            .unwrap_or_else(|e| panic!("parse {capability}: {e}"));
        let CompiledOperation::Http(req) = compile_operation(&template, &env)
            .unwrap_or_else(|e| panic!("compile {capability}: {e}"))
        else {
            panic!("{capability} should compile to HTTP");
        };
        req.path
    }

    #[test]
    fn github_repository_ref_splats_into_repo_scoped_list_paths() {
        for (capability, suffix) in [
            ("commit_query", "/commits"),
            ("branch_query", "/branches"),
            ("contributor_query", "/contributors"),
        ] {
            let path = compile_github_repo_scoped_path(capability, "ryan-s-roberts/plasm-core");
            assert_eq!(path, format!("/repos/ryan-s-roberts/plasm-core{suffix}"));
            assert!(
                !path.contains("%2F") && !path.contains("//"),
                "{capability} built malformed path {path}"
            );
        }
    }

    #[test]
    fn github_commit_query_provides_same_modeled_fields_as_get() {
        let cgs = github_cgs();
        let query = cgs.get_capability("commit_query").expect("commit_query");
        let get = cgs.get_capability("commit_get").expect("commit_get");
        assert_eq!(cgs.effective_provides(query), cgs.effective_provides(get));
    }

    #[test]
    fn github_repository_ref_without_owner_fails_before_malformed_path() {
        let cgs = github_cgs();
        let cap = cgs.get_capability("commit_query").expect("commit_query");
        let mut env = CmlEnv::new();
        env.insert("repository".to_string(), Value::String("plasm-core".into()));
        let splat_err = apply_entity_ref_scope_splat(&mut env, &cgs, cap).expect_err("splat");
        assert!(
            splat_err.to_string().contains("cannot normalize")
                || splat_err.to_string().contains("key_vars"),
            "{splat_err}"
        );
    }
}
