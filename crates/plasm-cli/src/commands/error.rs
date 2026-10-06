use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Schema,
    OpenApiSpec,
    ReplayDirectory,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("{kind:?} input not found: {}", path.display())]
    InputMissing { kind: InputKind, path: PathBuf },
    #[error("No compatible entity found")]
    NoCompatibleEntity {
        fields: Vec<String>,
        relations: Vec<String>,
    },
    #[error("{failed} replay tests failed")]
    ReplayTestsFailed { failed: usize },
    #[error("Hermit conformance incomplete: {failures} failures, {warnings} warnings")]
    ConformanceIncomplete { failures: usize, warnings: usize },
    #[error("{} OpenAPI operations have no catalog mapping:\n{}", operations.len(), operations.join("\n"))]
    UncoveredOpenApiOperations { operations: Vec<String> },
    #[error(transparent)]
    CatalogLoad(#[from] super::common::CgsCommandLoadError),
    #[error(transparent)]
    Schema(#[from] plasm_core::SchemaError),
    #[error(transparent)]
    PredicateType(#[from] plasm_core::TypeError),
    #[error(transparent)]
    Compile(#[from] plasm_compile::CompileError),
    #[error(transparent)]
    CatalogTemplate(#[from] plasm_compile::CatalogTemplateError),
    #[error(transparent)]
    Runtime(#[from] plasm_runtime::RuntimeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_rejections_retain_structured_metadata() {
        let error = CommandError::InputMissing {
            kind: InputKind::Schema,
            path: PathBuf::from("missing/domain.yaml"),
        };
        assert!(
            matches!(error, CommandError::InputMissing { kind: InputKind::Schema, path }
            if path == PathBuf::from("missing/domain.yaml"))
        );
        assert!(matches!(CommandError::NoCompatibleEntity {
            fields: vec!["score".into()], relations: vec!["children".into()],
        }, CommandError::NoCompatibleEntity { fields, relations }
            if fields == ["score"] && relations == ["children"]));
        assert!(matches!(
            CommandError::ReplayTestsFailed { failed: 3 },
            CommandError::ReplayTestsFailed { failed: 3 }
        ));
        assert!(matches!(
            CommandError::ConformanceIncomplete {
                failures: 2,
                warnings: 1
            },
            CommandError::ConformanceIncomplete {
                failures: 2,
                warnings: 1
            }
        ));
    }

    #[test]
    fn concrete_causes_survive_the_main_box_boundary() {
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let boxed: Box<dyn std::error::Error> = Box::new(CommandError::Json(source));
        assert!(
            matches!(boxed.downcast_ref::<CommandError>(), Some(CommandError::Json(source)) if source.is_eof())
        );
    }
}
