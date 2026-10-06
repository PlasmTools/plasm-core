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
    Schema(#[from] Box<plasm_core::SchemaError>),
    #[error(transparent)]
    PredicateType(#[from] Box<plasm_core::TypeError>),
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

impl From<plasm_core::SchemaError> for CommandError {
    fn from(source: plasm_core::SchemaError) -> Self {
        Self::Schema(Box::new(source))
    }
}

impl From<plasm_core::TypeError> for CommandError {
    fn from(source: plasm_core::TypeError) -> Self {
        Self::PredicateType(Box::new(source))
    }
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
            if path == std::path::Path::new("missing/domain.yaml"))
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

    #[test]
    fn command_error_has_bounded_footprint_and_preserves_schema_source() {
        use std::error::Error;
        let bytes = std::mem::size_of::<CommandError>();
        assert!(bytes < 128, "CommandError is {bytes} bytes; expected <128");
        let error = CommandError::from(plasm_core::SchemaError::IdentityFieldTypeResolution {
            entity: "Fixture".into(),
            field: "id".into(),
            source: plasm_core::ParentFieldTypeError::UnknownParentField {
                entity: "Fixture".into(),
                field: "id".into(),
            },
        });
        assert!(matches!(
            &error,
            CommandError::Schema(source)
                if matches!(source.as_ref(), plasm_core::SchemaError::IdentityFieldTypeResolution { .. })
        ));
        assert!(error
            .source()
            .unwrap()
            .is::<plasm_core::ParentFieldTypeError>());
    }

    #[test]
    fn boxed_predicate_type_preserves_metadata_and_concrete_source() {
        use std::error::Error;
        let error = CommandError::from(plasm_core::TypeError::CoercionFailure {
            field: "score".into(),
            source: plasm_core::CoercionError::InvalidNumberLiteral {
                raw: "not-a-number".into(),
                source: "not-a-number".parse::<f64>().unwrap_err(),
            },
        });
        assert!(matches!(
            &error,
            CommandError::PredicateType(source)
                if matches!(source.as_ref(), plasm_core::TypeError::CoercionFailure { field, .. }
                    if field == "score")
        ));
        let source = error.source().unwrap();
        assert!(matches!(
            source.downcast_ref::<plasm_core::CoercionError>(),
            Some(plasm_core::CoercionError::InvalidNumberLiteral { raw, .. })
                if raw == "not-a-number"
        ));
        assert!(source.source().unwrap().is::<std::num::ParseFloatError>());
    }
}
