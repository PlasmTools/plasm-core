//! Static checking is synchronous compiler work. Only server CPU scheduling is async.
use crate::compilation_error::CompilationError;
use monty_analysis::{AnalysisLimits, AnalysisOutcome, AnalysisRequest};
use tokio::sync::Semaphore;

pub(super) fn check_definition(source: &str, stubs: &str) -> Result<(), CompilationError> {
    let result = monty_analysis::analyze(&AnalysisRequest {
        source: source.into(),
        stubs: Some(stubs.into()),
        targets: Vec::new(),
        limits: AnalysisLimits::default(),
    })
    .map_err(host_error)?;
    match result.outcome {
        AnalysisOutcome::Inferred(_) => {
            // Upstream typing and runtime syntax support are distinct checks.
            // Constructing MontyRun compiles only; never call run/start here.
            monty::MontyRun::new(
                source.into(),
                "plasm_compute.py",
                Vec::new(),
                Default::default(),
            )
            .map(|_| ())
            .map_err(|error| {
                crate::python_program_diagnostic::admission_error(error.to_string()).into()
            })
        }
        AnalysisOutcome::Rejected(errors) => {
            Err(crate::python_program_diagnostic::admission_error(
                errors
                    .into_iter()
                    .map(|error| format_diagnostic(error, source, stubs))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .into())
        }
    }
}

pub(super) async fn check_definitions(
    definitions: Vec<(String, String)>,
) -> Result<(), CompilationError> {
    if definitions.is_empty() {
        return Ok(());
    }
    static CAPACITY: Semaphore = Semaphore::const_new(4);
    let permit = CAPACITY
        .acquire()
        .await
        .map_err(|e| host_error(e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        // Keep the permit until the CPU task finishes, even if its caller cancels.
        let _permit = permit;
        for (source, stubs) in definitions {
            check_definition(&source, &stubs)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| host_error(e.to_string()))?
}

fn format_diagnostic(
    error: monty_analysis::AnalysisDiagnostic,
    source: &str,
    stubs: &str,
) -> String {
    let mapped = match error.source.as_deref() {
        Some("/analysis.py") => Some(("plasm_compute.py", source)),
        Some("/analysis_stubs.pyi") => Some(("plasm_types.pyi", stubs)),
        _ => None,
    };
    let location = mapped
        .and_then(|(name, text)| {
            let span = error.span?;
            let prefix = text.get(..span.start as usize)?;
            let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
            let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            Some(format!("{name}:{line}:{column}: "))
        })
        .unwrap_or_else(|| {
            error
                .source
                .map(|name| format!("{name}: "))
                .unwrap_or_default()
        });
    format!("{location}{}: {}", error.code, error.message)
}

fn host_error(message: String) -> CompilationError {
    CompilationError::Host(plasm_runtime::ExecutionFailure::new(
        plasm_runtime::FailureCause::Runtime,
        "python_checker_failure",
        message,
    ))
}
