use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum CompileError {
    #[error("Type error during compilation: {source}")]
    TypeError {
        #[from]
        source: plasm_core::TypeError,
    },

    #[error("Normalization error: {source}")]
    NormalizationError {
        #[from]
        source: plasm_core::NormalizationError,
    },

    #[error("No capability found for entity '{entity}' with kind '{kind:?}'")]
    CapabilityNotFound { entity: String, kind: String },

    #[error("Field type '{field_type:?}' is not supported by this backend")]
    UnsupportedFieldType { field_type: String },

    #[error(
        "Operator '{operator:?}' is not supported for field type '{field_type:?}' by this backend"
    )]
    UnsupportedOperator {
        operator: String,
        field_type: String,
    },

    #[error("Compilation failed: {message}")]
    CompilationFailed { message: String },
}

#[derive(Error, Debug, Clone)]
pub enum DecodeError {
    #[error("response field `{field}` violates its declared type: {reason}")]
    FieldContract { field: String, reason: String },

    #[error("Path '{path}' not found in response")]
    PathNotFound { path: String },

    #[error("Type mismatch: expected '{expected}', found '{found}' at path '{path}'")]
    TypeMismatch {
        path: String,
        expected: String,
        found: String,
    },

    #[error("Transform '{transform}' failed on value '{value}': {reason}")]
    TransformFailed {
        transform: String,
        value: String,
        reason: String,
    },

    #[error("Invalid response structure: {message}")]
    InvalidStructure { message: String },

    #[error("Decoding failed: {message}")]
    DecodingFailed { message: String },
}
