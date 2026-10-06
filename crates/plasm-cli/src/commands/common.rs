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
    Schema(#[from] Box<plasm_core::loader::SchemaLoadError>),
    #[error(transparent)]
    Compilation(#[from] plasm_compile::CmlError),
}

impl From<plasm_core::loader::SchemaLoadError> for CgsCommandLoadError {
    fn from(source: plasm_core::loader::SchemaLoadError) -> Self {
        Self::Schema(Box::new(source))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn load_error_has_bounded_footprint_and_preserves_schema_source() {
        assert!(std::mem::size_of::<CgsCommandLoadError>() < 128);
        let error = CgsCommandLoadError::from(plasm_core::loader::SchemaLoadError::Validation(
            plasm_core::SchemaError::UnknownValueDomain {
                key: "missing".into(),
                context: "Fixture.id".into(),
            },
        ));
        assert!(matches!(
            &error,
            CgsCommandLoadError::Schema(source)
                if matches!(source.as_ref(), plasm_core::loader::SchemaLoadError::Validation(_))
        ));
        assert!(error.source().unwrap().is::<plasm_core::SchemaError>());
    }
}
