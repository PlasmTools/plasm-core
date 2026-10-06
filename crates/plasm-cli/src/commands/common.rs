//! Shared helpers for CLI commands that load CGS files.

use plasm_compile::validate_cgs_capability_templates;
use plasm_core::CGS;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CgsCommandLoadError {
    #[error(transparent)]
    CatalogTemplate(#[from] plasm_compile::CatalogTemplateError),
    #[error(transparent)]
    Schema(#[from] plasm_core::loader::SchemaLoadError),
    #[error(transparent)]
    Compilation(#[from] plasm_compile::CmlError),
}

/// Load a CGS from a path and ensure every capability CML template parses.
pub fn load_cgs(path: &Path) -> Result<CGS, CgsCommandLoadError> {
    let cgs = plasm_core::loader::load_schema(path)?;
    validate_cgs_capability_templates(&cgs)?;
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    plasm_compile::validate_catalog_openapi_pagination(&cgs, directory)?;
    Ok(cgs)
}
