#[derive(Debug, Clone)]
pub enum McpOAuthError {
    MetadataRetrieval {
        source: std::sync::Arc<CimdRetrievalError>,
    },
    Internal {
        source: std::sync::Arc<OAuthFailure>,
    },
    OAuth {
        error: String,
        description: String,
    },
    InvalidTarget {
        description: String,
    },
    AccessDenied {
        description: String,
    },
    Unavailable {
        description: String,
    },
    RateLimited {
        description: String,
    },
}

impl McpOAuthError {
    pub fn bad_request(error: &str, description: &str) -> Self {
        Self::OAuth {
            error: error.to_string(),
            description: description.to_string(),
        }
    }

    pub fn invalid_target(description: &str) -> Self {
        Self::InvalidTarget {
            description: description.to_string(),
        }
    }

    pub fn unavailable(description: &str) -> Self {
        Self::Unavailable {
            description: description.to_string(),
        }
    }

    pub fn oauth_error_code(&self) -> &str {
        match self {
            Self::MetadataRetrieval { source } => {
                if source.is_capacity_failure() {
                    "temporarily_unavailable"
                } else {
                    "invalid_client_metadata"
                }
            }
            Self::Internal { .. } => "server_error",
            Self::OAuth { error, .. } => error,
            Self::InvalidTarget { .. } => "invalid_target",
            Self::AccessDenied { .. } => "access_denied",
            Self::Unavailable { .. } => "temporarily_unavailable",
            Self::RateLimited { .. } => "invalid_client_metadata",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::MetadataRetrieval { .. } => "client metadata retrieval failed",
            Self::Internal { .. } => "OAuth operation failed",
            Self::OAuth { description, .. }
            | Self::InvalidTarget { description }
            | Self::AccessDenied { description }
            | Self::Unavailable { description }
            | Self::RateLimited { description } => description,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthFailure {
    #[error("OAuth credential storage failed")]
    Storage(#[from] crate::secret_store::SecretStoreError),
    #[error("OAuth record serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("OAuth JWT operation failed")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("OAuth HTTP initialization failed")]
    Http(#[from] reqwest::Error),
    #[error("OAuth DNS task failed")]
    DnsTask(#[from] tokio::task::JoinError),
}
#[derive(Debug, thiserror::Error)]
pub enum CimdRetrievalError {
    #[error("client metadata HTTP retrieval failed")]
    Http(#[from] reqwest::Error),
    #[error("client metadata DNS resolution failed")]
    Resolve(#[from] std::io::Error),
    #[error("client metadata DNS deadline elapsed")]
    ResolutionTimeout(#[source] tokio::time::error::Elapsed),
    #[error("client metadata DNS task failed")]
    ResolutionTask(#[source] tokio::task::JoinError),
    #[error("client metadata capacity deadline elapsed")]
    CapacityTimeout(#[source] tokio::time::error::Elapsed),
    #[error("client metadata capacity closed")]
    CapacityClosed(#[source] tokio::sync::AcquireError),
}
impl CimdRetrievalError {
    pub fn is_capacity_failure(&self) -> bool {
        matches!(self, Self::CapacityTimeout(_) | Self::CapacityClosed(_))
    }
}
impl From<CimdRetrievalError> for McpOAuthError {
    fn from(source: CimdRetrievalError) -> Self {
        Self::MetadataRetrieval {
            source: std::sync::Arc::new(source),
        }
    }
}
impl From<crate::secret_store::SecretStoreError> for McpOAuthError {
    fn from(error: crate::secret_store::SecretStoreError) -> Self {
        Self::Internal {
            source: std::sync::Arc::new(OAuthFailure::Storage(error)),
        }
    }
}
impl From<serde_json::Error> for McpOAuthError {
    fn from(error: serde_json::Error) -> Self {
        Self::Internal {
            source: std::sync::Arc::new(OAuthFailure::Serialization(error)),
        }
    }
}
impl From<jsonwebtoken::errors::Error> for McpOAuthError {
    fn from(error: jsonwebtoken::errors::Error) -> Self {
        Self::Internal {
            source: std::sync::Arc::new(OAuthFailure::Jwt(error)),
        }
    }
}
impl From<reqwest::Error> for McpOAuthError {
    fn from(error: reqwest::Error) -> Self {
        Self::Internal {
            source: std::sync::Arc::new(OAuthFailure::Http(error)),
        }
    }
}
impl From<tokio::task::JoinError> for McpOAuthError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Internal {
            source: std::sync::Arc::new(OAuthFailure::DnsTask(error)),
        }
    }
}
impl std::fmt::Display for McpOAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.description())
    }
}
impl std::error::Error for McpOAuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataRetrieval { source } => Some(source.as_ref()),
            Self::Internal { source } => Some(source.as_ref()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    #[test]
    fn storage_fault_preserves_typed_cause_without_disclosing_it_on_wire() {
        let error = McpOAuthError::from(crate::secret_store::SecretStoreError::Authentication);
        let cause = error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .downcast_ref::<crate::secret_store::SecretStoreError>()
            .unwrap();
        assert!(matches!(
            cause,
            crate::secret_store::SecretStoreError::Authentication
        ));
        assert_eq!(error.oauth_error_code(), "server_error");
        assert_eq!(error.description(), "OAuth operation failed");
    }
}
