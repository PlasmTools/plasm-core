use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChainError {
    #[error("field `{entity}.{field}` is {actual}, not EntityRef")]
    FieldNotEntityRef {
        entity: String,
        field: String,
        actual: plasm_core::FieldType,
    },
    #[error("relation `{entity}.{relation}` has no chain materialization")]
    MaterializationMissing { entity: String, relation: String },
    #[error("relation `{entity}.{relation}` get_scoped_bindings requires cardinality one")]
    GetBindingsCardinality { entity: String, relation: String },
    #[error("relation `{entity}.{relation}` query-scoped materialization is invalid for cardinality one")]
    QueryBindingsCardinality { entity: String, relation: String },
    #[error("relation `{entity}.{relation}` parent-get/view embedding requires cardinality many")]
    ParentEmbeddingCardinality { entity: String, relation: String },
    #[error("chain selector `{selector}` is not an EntityRef field or relation on `{entity}`")]
    SelectorMissing { entity: String, selector: String },
    #[error(
        "chain capability `{capability}` domain `{actual}` does not match target `{expected}`"
    )]
    CapabilityDomainMismatch {
        capability: String,
        expected: String,
        actual: String,
    },
    #[error("relation `{relation}` requires PreferFromParentGet materialization")]
    PreferParentGetRequired { relation: String },
}
