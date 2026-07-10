use thiserror::Error;

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Compilation error: {source}")]
    CompilationError {
        #[from]
        source: plasm_compile::CompileError,
    },

    #[error("Type error: {source}")]
    TypeError {
        #[from]
        source: plasm_core::TypeError,
    },

    #[error("Decode error: {source}")]
    DecodeError {
        #[from]
        source: plasm_compile::DecodeError,
    },

    #[error("CML error: {source}")]
    CmlError {
        #[from]
        source: plasm_compile::CmlError,
    },

    #[error("HTTP request failed: {message}")]
    RequestError {
        message: String,
        attempts: u32,
        status: Option<u16>,
        body: Option<serde_json::Value>,
    },

    #[error("Workflow conflict: {message}")]
    WorkflowConflict {
        conflict: plasm_core::WorkflowConflict,
        message: String,
        attempts: u32,
    },

    #[error("Upstream rate limited (HTTP {status}): {message}")]
    RateLimited {
        status: u16,
        host: String,
        retry_after: Option<std::time::Duration>,
        attempts: u32,
        message: String,
    },

    #[error("Cache error: {message}")]
    CacheError { message: String },

    #[error("Execution mode '{mode}' not supported")]
    UnsupportedExecutionMode { mode: String },

    #[error("Capability '{capability}' not found for entity '{entity}'")]
    CapabilityNotFound { capability: String, entity: String },

    #[error("No fingerprint found for request")]
    FingerprintNotFound,

    #[error("Replay entry not found for fingerprint: {fingerprint}")]
    ReplayEntryNotFound { fingerprint: String },

    #[error("Replay store error: {message}")]
    ReplayStoreError { message: String },

    #[error("Runtime configuration error: {message}")]
    ConfigurationError { message: String },

    #[error("Serialization error: {message}")]
    SerializationError { message: String },

    #[error("Authentication error: {message}")]
    AuthenticationError { message: String },

    #[error("Execution cancelled")]
    Cancelled,
}

impl RuntimeError {
    pub fn set_attempts(&mut self, attempts: u32) {
        match self {
            RuntimeError::RequestError { attempts: a, .. }
            | RuntimeError::WorkflowConflict { attempts: a, .. }
            | RuntimeError::RateLimited { attempts: a, .. } => *a = attempts,
            _ => {}
        }
    }

    pub fn request_failure(message: impl Into<String>, attempts: u32) -> Self {
        Self::RequestError {
            message: message.into(),
            attempts,
            status: None,
            body: None,
        }
    }
}

impl From<reqwest::Error> for RuntimeError {
    fn from(err: reqwest::Error) -> Self {
        let mut message = err.to_string();
        if let Some(url) = err.url() {
            message = format!("{message} (request URL: {url})");
        }
        RuntimeError::RequestError {
            message,
            attempts: 1,
            status: None,
            body: None,
        }
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(err: serde_json::Error) -> Self {
        RuntimeError::SerializationError {
            message: err.to_string(),
        }
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(err: std::io::Error) -> Self {
        RuntimeError::ReplayStoreError {
            message: err.to_string(),
        }
    }
}
