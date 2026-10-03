//! Source-owned Python compiler rejections carried intact to program diagnostics.

/// Rejection returned by the compute contract recognizer. The response layer
/// forwards its correction without selecting another repair from its prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonComputeRejection {
    correction: String,
}

impl PythonComputeRejection {
    pub fn correction(&self) -> &str {
        &self.correction
    }
    pub fn into_correction(self) -> String {
        self.correction
    }
}

impl From<String> for PythonComputeRejection {
    fn from(correction: String) -> Self {
        Self { correction }
    }
}

impl From<&str> for PythonComputeRejection {
    fn from(correction: &str) -> Self {
        Self {
            correction: correction.into(),
        }
    }
}

impl From<PythonComputeRejection> for String {
    fn from(error: PythonComputeRejection) -> Self {
        error.into_correction()
    }
}

/// A Python rejection is minted at the recognizing compiler boundary and is
/// carried unchanged through the agent response envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonLoweringError {
    Source {
        message: String,
        span: Option<(u32, u32)>,
    },
    Compute(PythonComputeRejection),
}

impl PythonLoweringError {
    pub fn message(&self) -> &str {
        match self {
            Self::Source { message, .. } => message,
            Self::Compute(error) => error.correction(),
        }
    }

    pub fn into_message(self) -> String {
        match self {
            Self::Source { message, .. } => message,
            Self::Compute(error) => error.into_correction(),
        }
    }

    pub fn span_offset(&self) -> Option<usize> {
        match self {
            Self::Source { span, .. } => span.map(|(start, _)| start as usize),
            Self::Compute(_) => None,
        }
    }
}

impl From<String> for PythonLoweringError {
    fn from(message: String) -> Self {
        Self::Source {
            message,
            span: None,
        }
    }
}

impl From<&str> for PythonLoweringError {
    fn from(message: &str) -> Self {
        Self::Source {
            message: message.into(),
            span: None,
        }
    }
}

impl From<PythonComputeRejection> for PythonLoweringError {
    fn from(error: PythonComputeRejection) -> Self {
        Self::Compute(error)
    }
}

impl std::fmt::Display for PythonLoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PythonLoweringError {}

impl std::ops::Deref for PythonLoweringError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.message()
    }
}

impl From<PythonLoweringError> for String {
    fn from(error: PythonLoweringError) -> Self {
        error.into_message()
    }
}
