#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpOAuthError {
    OAuth { error: String, description: String },
    InvalidTarget { description: String },
    AccessDenied { description: String },
    Unavailable { description: String },
    Server { description: String },
    RateLimited { description: String },
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

    pub fn server(description: &str) -> Self {
        Self::Server {
            description: description.to_string(),
        }
    }

    pub fn oauth_error_code(&self) -> &str {
        match self {
            Self::OAuth { error, .. } => error,
            Self::InvalidTarget { .. } => "invalid_target",
            Self::AccessDenied { .. } => "access_denied",
            Self::Unavailable { .. } => "temporarily_unavailable",
            Self::Server { .. } => "server_error",
            Self::RateLimited { .. } => "invalid_client_metadata",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::OAuth { description, .. }
            | Self::InvalidTarget { description }
            | Self::AccessDenied { description }
            | Self::Unavailable { description }
            | Self::Server { description }
            | Self::RateLimited { description } => description,
        }
    }
}

pub fn map_auth_error(err: auth_framework::AuthError) -> McpOAuthError {
    let msg = err.to_string();
    if msg.to_ascii_lowercase().contains("not found") || msg.contains("Invalid client") {
        McpOAuthError::bad_request("invalid_client", &msg)
    } else if msg.contains("rate limit") {
        McpOAuthError::RateLimited { description: msg }
    } else {
        McpOAuthError::bad_request("invalid_client_metadata", &msg)
    }
}
