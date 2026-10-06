use thiserror::Error;

#[derive(Debug, Error)]
pub enum PreflightError {
    #[error("preflight hydration prefix must not be empty")]
    HydrationPrefixEmpty,
    #[error("preflight missing parameter `{param}`")]
    ParameterMissing { param: String },
    #[error("preflight parameter `{param}` object missing key `{key}`")]
    ParameterKeyMissing { param: String, key: String },
    #[error("preflight parameter `{param}` is not a scalar entity reference")]
    ParameterNotEntityRef { param: String },
    #[error("preflight get `{capability}` did not provide field `{field}`")]
    HydratedFieldMissing { capability: String, field: String },
    #[error(
        "preflight query_pick: {matches} rows match {field} == {equals_param} in `{capability}`"
    )]
    PickMatchCount {
        matches: usize,
        field: String,
        equals_param: String,
        capability: String,
    },
    #[error("preflight query_pick row missing field `{field}` for wire key `{wire_key}`")]
    PickFieldMissing { field: String, wire_key: String },
    #[error("preflight scope bind requires from_param, from_preflight, or literal")]
    ScopeBindMissing,
    #[error("preflight path must not be empty")]
    PathEmpty,
    #[error("preflight path missing key `{key}`")]
    PathKeyMissing { key: String },
    #[error("preflight path cannot descend into `{segment}`")]
    PathCannotDescend { segment: String },
    #[error("preflight label lookup returned {matches} matches")]
    LabelMatchCount { matches: usize },
    #[error("existence check requires a declared identity")]
    ExistenceIdentityUndeclared,
    #[error("existence check is missing identity field `{field}`")]
    ExistenceIdentityMissing { field: String },
    #[error("existence check response does not prove the requested identity")]
    ExistenceIdentityUnproven,
}
