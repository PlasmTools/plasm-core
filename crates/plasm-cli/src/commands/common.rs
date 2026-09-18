//! Shared helpers for CLI commands that load CGS files.

use plasm_compile::validate_cgs_capability_templates;
use plasm_core::CGS;
use std::path::Path;

/// Load a CGS from a path and ensure every capability CML template parses.
pub fn load_cgs(path: &Path) -> Result<CGS, String> {
    let cgs = plasm_core::loader::load_schema(path)?;
    validate_cgs_capability_templates(&cgs).map_err(|e| e.to_string())?;
    let directory = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    plasm_compile::validate_catalog_openapi_pagination(&cgs, directory)
        .map_err(|e| e.to_string())?;
    Ok(cgs)
}
