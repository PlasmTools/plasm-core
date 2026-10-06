use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum CompileError {
    #[error("Type error during compilation: {source}")]
    TypeError {
        #[from]
        source: Box<plasm_core::TypeError>,
    },

    #[error("Normalization error: {source}")]
    NormalizationError {
        #[from]
        source: plasm_core::NormalizationError,
    },

    #[error(transparent)]
    TemporalInput(#[from] plasm_core::temporal_input::TemporalInputError),

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

    #[error(transparent)]
    Schema(#[from] Box<plasm_core::SchemaError>),
    #[error(transparent)]
    QueryResolution(#[from] plasm_core::QueryCapabilityResolveError),
}

#[derive(Error, Debug, Clone)]
pub enum DecodeError {
    #[error(transparent)]
    JsonPath(#[from] crate::json_path::JsonPathError),

    #[error("response field `{field}` violates its declared type: {source}")]
    FieldContract {
        field: String,
        #[source]
        source: plasm_core::DecodeFieldCause,
    },

    #[error("Path '{path}' not found in response")]
    PathNotFound { path: String },

    #[error("Type mismatch: expected '{expected}', found '{found}' at path '{path}'")]
    TypeMismatch {
        path: String,
        expected: String,
        found: String,
    },

    #[error("to_string requires a string, number or boolean")]
    StringTransformInput,
    #[error("to_number requires a number or string")]
    NumberTransformInput,
    #[error("JSON number cannot be represented as i64 or f64")]
    NumberRepresentation,
    #[error("numeric string cannot be parsed as i64 or f64: {source}")]
    NumberParse {
        integer: std::num::ParseIntError,
        #[source]
        source: std::num::ParseFloatError,
    },
    #[error("to_bool requires a boolean or string")]
    BooleanTransformInput,
    #[error("boolean string must be true/yes/1 or false/no/0")]
    BooleanLiteral,
    #[error("map_enum requires a string")]
    EnumTransformInput,
    #[error("segments_after_prefix requires a JSON string")]
    SegmentDeriveInput,
    #[error("segments_after_prefix value does not match a declared prefix")]
    SegmentPrefixMissing,
    #[error("segments_after_prefix index {index} out of range ({segments} segments)")]
    SegmentIndex { index: usize, segments: usize },
    #[error("name_value_array_lookup requires a JSON array")]
    NameValueDeriveInput,
    #[error("object_key_lookup requires a JSON object")]
    ObjectKeyDeriveInput,
    #[error("relation `{relation}` embed decoder must be a leaf")]
    NestedRelationDecoder { relation: String },
    #[error("id_path matched no value")]
    IdentityPathEmpty,
    #[error("id_path must resolve to a string or number")]
    IdentityScalarRequired,
    #[error("entity decode source must be an object or string/number identity scalar")]
    EntitySourceShape,
    #[error("identity field `{field}` has no declared value type")]
    IdentityFieldTypeMissing { field: String },
    #[error("identity field `{field}` has an invalid declared value type: {source}")]
    IdentityFieldContract {
        field: String,
        #[source]
        source: Box<plasm_core::SchemaError>,
    },
    #[error("compound key part `{part}` missing for entity `{entity}`")]
    CompoundKeyPartMissing { entity: String, part: String },
    #[error("exhaustive embedded relation `{parent}.{relation}` requires a nonempty path")]
    ExhaustiveEmbedPathEmpty { parent: String, relation: String },
    #[error("exhaustive embedded relation `{parent}.{relation}` has a missing, null or malformed branch")]
    ExhaustiveEmbedBranch { parent: String, relation: String },
    #[error("embedded relation membership could not be recorded")]
    RelationMembership {
        #[source]
        source: Box<plasm_core::collection_codec::CollectionFault>,
    },
    #[error("No valid ID field found in source object")]
    IdentityMissing,
}

impl From<plasm_core::TypeError> for CompileError {
    fn from(source: plasm_core::TypeError) -> Self {
        Self::TypeError {
            source: Box::new(source),
        }
    }
}

impl From<plasm_core::SchemaError> for CompileError {
    fn from(source: plasm_core::SchemaError) -> Self {
        Self::Schema(Box::new(source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn compiler_errors_have_bounded_stack_footprints() {
        assert!(std::mem::size_of::<CompileError>() < 128);
        assert!(std::mem::size_of::<DecodeError>() < 128);
    }

    #[test]
    fn boxed_compile_and_decode_causes_remain_concrete() {
        let error = CompileError::from(plasm_core::TypeError::EntityNotFound {
            entity: "Fixture".into(),
        });
        assert!(matches!(
            error.source().unwrap().downcast_ref::<Box<plasm_core::TypeError>>().map(Box::as_ref),
            Some(plasm_core::TypeError::EntityNotFound { entity }) if entity == "Fixture"
        ));
        let error = DecodeError::IdentityFieldContract {
            field: "id".into(),
            source: Box::new(plasm_core::SchemaError::UnknownValueDomain {
                key: "missing".into(),
                context: "Fixture.id".into(),
            }),
        };
        for error in [error.clone(), error] {
            let source = error
                .source()
                .unwrap()
                .downcast_ref::<Box<plasm_core::SchemaError>>()
                .unwrap()
                .as_ref();
            assert!(matches!(source,
                plasm_core::SchemaError::UnknownValueDomain { key, context }
                    if key == "missing" && context == "Fixture.id"
            ));
        }
    }

    #[test]
    fn boxed_compile_cause_preserves_nested_error_chain() {
        let error = CompileError::from(plasm_core::TypeError::CoercionFailure {
            field: "count".into(),
            source: plasm_core::CoercionError::MissingTemporalFormat,
        });
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<plasm_core::TypeError>>()
            .unwrap()
            .as_ref();
        assert!(
            matches!(source, plasm_core::TypeError::CoercionFailure { field, .. } if field == "count")
        );
        assert!(matches!(
            source
                .source()
                .unwrap()
                .downcast_ref::<plasm_core::CoercionError>(),
            Some(plasm_core::CoercionError::MissingTemporalFormat)
        ));
    }
}
