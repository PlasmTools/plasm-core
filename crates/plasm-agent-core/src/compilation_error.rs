//! Admission failures preserve the distinction between authored code and its host.
use crate::program_diagnostic::ProgramStageError;
pub use plasm_runtime::ExecutionFailure;

#[derive(Debug, Clone, thiserror::Error)]
pub enum CompilationError {
    #[error("{0}")]
    Program(#[source] Box<ProgramStageError>),
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
        Self::Program(Box::new(error))
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
            CompilationError::Program(error) => (*error).into(),
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
            Self::Program(error) => Ok(*error),
            Self::Host(error) => Err(error),
            Self::Checker(error) => Err(CompilationError::Checker(error).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn boxed_program_stage_preserves_source_and_owned_conversion() {
        assert!(std::mem::size_of::<CompilationError>() < 128);
        let stage = ProgramStageError::PythonAnalysis {
            diagnostics: Vec::new(),
            source: "program".into(),
            stubs: "declarations".into(),
        };
        let expected_display = stage.to_string();
        let error = CompilationError::from(stage);
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<ProgramStageError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(source,
            ProgramStageError::PythonAnalysis { source, stubs, .. }
                if source == "program" && stubs == "declarations"
        ));
        let recovered = error.into_program().unwrap();
        assert_eq!(recovered.to_string(), expected_display);
        assert!(matches!(
            recovered,
            ProgramStageError::PythonAnalysis { source, stubs, .. }
                if source == "program" && stubs == "declarations"
        ));
    }
}
