use thiserror::Error;

#[derive(Error, Debug)]
pub enum MockError {
    #[error("Entity '{entity}' not found")]
    EntityNotFound { entity: String },

    #[error("Resource with ID '{id}' not found in entity '{entity}'")]
    ResourceNotFound { entity: String, id: String },

    #[error("Filter type error: {source}")]
    FilterType {
        #[from]
        source: plasm_core::TypeError,
    },
    #[error("Filter compilation error: {source}")]
    FilterCompilation {
        #[from]
        source: plasm_compile::CompileError,
    },

    #[error("Relation '{relation}' not found in entity '{entity}'")]
    RelationNotFound { relation: String, entity: String },

    #[error("Serialization error: {source}")]
    SerializationError {
        #[from]
        source: serde_json::Error,
    },
}
