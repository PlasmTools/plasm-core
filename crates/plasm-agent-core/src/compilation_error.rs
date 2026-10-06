//! Admission failures preserve the distinction between authored code and its host.
use crate::program_diagnostic::ProgramStageError;
pub use plasm_runtime::ExecutionFailure;

#[derive(Debug, Clone, thiserror::Error)]
pub enum CompilationError {
    #[error("{0}")]
    Program(#[source] ProgramStageError),
    #[error("{0}")]
    Host(#[source] ExecutionFailure),
    #[error("Python checker failed: {0}")]
    Checker(#[source] std::sync::Arc<PythonCheckerError>),
}

#[derive(Debug, thiserror::Error)]
pub enum PythonCheckerError {
    #[error(transparent)]
    Analysis(#[from] monty_analysis::AnalysisError),
    #[error("Python checker capacity is closed")]
    Capacity(#[source] tokio::sync::AcquireError),
    #[error("Python checker worker failed")]
    Worker(#[source] tokio::task::JoinError),
}

impl From<PythonCheckerError> for CompilationError {
    fn from(error: PythonCheckerError) -> Self {
        Self::Checker(std::sync::Arc::new(error))
    }
}
impl From<ProgramStageError> for CompilationError {
    fn from(error: ProgramStageError) -> Self {
        Self::Program(error)
    }
}
impl From<ExecutionFailure> for CompilationError {
    fn from(error: ExecutionFailure) -> Self {
        Self::Host(error)
    }
}
impl From<CompilationError> for ExecutionFailure {
    fn from(error: CompilationError) -> Self {
        match error {
            CompilationError::Program(error) => error.into(),
            CompilationError::Host(error) => error,
            CompilationError::Checker(error) => ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "python_checker_failure",
                error.to_string(),
            ),
        }
    }
}

impl CompilationError {
    pub fn into_program(self) -> Result<ProgramStageError, ExecutionFailure> {
        match self {
            Self::Program(error) => Ok(error),
            Self::Host(error) => Err(error),
            Self::Checker(error) => Err(CompilationError::Checker(error).into()),
        }
    }
}
