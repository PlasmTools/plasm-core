//! Semantic tracing spans for `plasm-core`.
//!
//! Span names are a **stable contract** (`plasm_core.<domain>.<operation>`): they describe
//! observable phases of the schema / prompt / expression surface, not Rust module paths.
//! Refactors that move code between files must keep these names unless the observability
//! contract is intentionally versioned.

use std::fmt::Debug;
use std::path::Path;
use tracing::Span;

// --- Schema load / assemble --------------------------------------------------

#[inline]
pub(crate) fn schema_load_split(domain_path: &Path, mappings_path: &Path) -> Span {
    tracing::info_span!(
        "plasm_core.schema.load_split",
        domain_path = %domain_path.display(),
        mappings_path = %mappings_path.display(),
    )
}

#[inline]
pub(crate) fn schema_load_directory(dir: &Path) -> Span {
    tracing::debug_span!("plasm_core.schema.load_directory", dir = %dir.display())
}

#[inline]
pub(crate) fn schema_load_path(path: &Path) -> Span {
    tracing::info_span!("plasm_core.schema.load_path", path = %path.display())
}

#[inline]
pub(crate) fn schema_assemble(entity_count: usize, capability_count: usize) -> Span {
    tracing::debug_span!(
        "plasm_core.schema.assemble",
        entity_count = entity_count,
        capability_count = capability_count,
    )
}

// --- teaching prompt render ----------------------------------------------------

#[inline]
pub(crate) fn prompt_domain_bundle<F: Debug>(
    focus: &F,
    symbol_tuning: bool,
    include_domain_execution_model: bool,
) -> Span {
    tracing::debug_span!(
        "plasm_core.prompt.domain_bundle",
        focus = ?focus,
        symbol_tuning = symbol_tuning,
        include_domain_execution_model = include_domain_execution_model,
        cache.hit = tracing::field::Empty,
    )
}

#[inline]
pub(crate) fn prompt_domain_bundle_exposure(incremental: bool, symbol_tuning: bool) -> Span {
    tracing::debug_span!(
        "plasm_core.prompt.domain_bundle_exposure",
        incremental = incremental,
        symbol_tuning = symbol_tuning,
        cache.hit = tracing::field::Empty,
    )
}

#[inline]
pub(crate) fn prompt_domain_bundle_exposure_federated(
    incremental: bool,
    symbol_tuning: bool,
) -> Span {
    tracing::debug_span!(
        "plasm_core.prompt.domain_bundle_exposure_federated",
        incremental = incremental,
        symbol_tuning = symbol_tuning,
        cache.hit = tracing::field::Empty,
    )
}

// --- Parse / typecheck / discovery / catalog ---------------------------------

#[inline]
pub(crate) fn parse_program(source_len: usize) -> Span {
    tracing::debug_span!("plasm_core.parse.program", source_len = source_len)
}

#[inline]
pub(crate) fn typecheck_expr() -> Span {
    tracing::debug_span!("plasm_core.typecheck.expr")
}

#[inline]
pub(crate) fn discovery_discover() -> Span {
    tracing::debug_span!(
        "plasm_core.discovery.discover",
        candidate_count = tracing::field::Empty,
        result_count = tracing::field::Empty,
    )
}

#[inline]
pub(crate) fn catalog_load_il(byte_len: usize) -> Span {
    tracing::debug_span!("plasm_core.catalog.load_il", byte_len = byte_len)
}

#[inline]
pub(crate) fn schema_validate(entity_count: usize, capability_count: usize) -> Span {
    tracing::debug_span!(
        "plasm_core.schema.validate",
        entity_count = entity_count,
        capability_count = capability_count,
    )
}
