use thiserror::Error;

#[derive(Debug, Error)]
pub enum ResponseNarrowError {
    #[error("view capabilities do not use HTTP response narrowing")]
    ViewTransport,
    #[error("single-entity response: missing path segment `{segment}`{hint}")]
    MissingSegment {
        segment: String,
        hint: ResponseNarrowHint,
    },
    #[error("single-entity response: expected a non-empty array at path")]
    EmptyArray,
    #[error("single-entity response: array element missing `{inner}` object")]
    ArrayInnerMissing { inner: String },
    #[error("single-entity response: expected object elements in array")]
    ArrayObjectRequired,
}

/// External error detail and structural evidence, rendered only with the fault.
#[derive(Debug)]
pub enum ResponseNarrowHint {
    None,
    GraphQl { detail: String },
    NullData,
    Command { name: String, message: String },
}

impl std::fmt::Display for ResponseNarrowHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => Ok(()),
            Self::GraphQl { detail } => write!(f, " — GraphQL: {detail}"),
            Self::NullData => write!(
                f,
                " (response `data` is null; often paired with GraphQL `errors`)"
            ),
            Self::Command { name, message } => {
                write!(f, " — Fibery command failed ({name}): {message}")
            }
        }
    }
}
