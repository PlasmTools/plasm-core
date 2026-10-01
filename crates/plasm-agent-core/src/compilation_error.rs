//! Admission failures preserve the distinction between authored code and its host.
use crate::program_diagnostic::ProgramStageError;
pub use plasm_runtime::ExecutionFailure;

#[derive(Debug, Clone, thiserror::Error)]
pub enum CompilationError {
    #[error("{0}")]
    Program(ProgramStageError),
    #[error("{0}")]
    Host(ExecutionFailure),
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
        }
    }
}

impl CompilationError {
    pub fn into_program(self) -> Result<ProgramStageError, ExecutionFailure> {
        match self {
            Self::Program(error) => Ok(error),
            Self::Host(error) => Err(error),
        }
    }
}
