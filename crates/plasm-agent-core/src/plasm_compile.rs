//! Canonical compile surface → [`PlasmCompBundle`] (monadic execution contract).

pub use crate::plasm_comp_bundle::PlasmCompBundle;
pub use crate::plasm_dag::{
    PythonAggregateDescriptor, PythonBuildStatement, PythonCatalogOperation, PythonDeclaration,
    PythonQuantifierOperation, PythonRelationOperation, PythonRowOperation,
};

use crate::execute_session::ExecuteSession;
use crate::plasm_comp_wire::plasm_comp_from_validated;
use crate::plasm_dag::{
    compile_plasm_dag_to_plan_inner, compile_plasm_surface_line_to_plan, is_plasm_dag_source,
};
use crate::plasm_plan::validate_plan_artifact;
use crate::program_diagnostic::ProgramStageError;
use plasm_core::plasm_monad::PlasmCompArtifact;
use plasm_core::{PromptPipelineConfig, SymbolMapCrossRequestCache};

#[derive(Debug, thiserror::Error)]
enum CompileSourceError {
    #[error(transparent)]
    Dag(#[from] crate::plasm_dag::error::DagCompilationError),
    #[error(transparent)]
    Plan(#[from] crate::plasm_plan::PlanValidationError),
    #[error(transparent)]
    SessionProvision(#[from] crate::plan_session_provisions::SessionProvisionError),
}

/// Lower surface/DAG source to a validated comp artifact (no plan wire exposure).
fn compile_source_to_artifact(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<PlasmCompArtifact, CompileSourceError> {
    let plan = if is_plasm_dag_source(source.trim()) {
        compile_plasm_dag_to_plan_inner(pipeline, symbol_map_cross_cache, session, name, source)?
    } else {
        compile_plasm_surface_line_to_plan(pipeline, symbol_map_cross_cache, session, name, source)?
    };
    let validated = validate_plan_artifact(&plan)?;
    let mut artifact = plasm_comp_from_validated(&validated);
    crate::plan_session_provisions::seal(session, validated.nodes(), &mut artifact.comp.bind)?;
    Ok(artifact)
}

fn compile_to_bundle(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<PlasmCompBundle, ProgramStageError> {
    match compile_source_to_artifact(pipeline, symbol_map_cross_cache, session, name, source) {
        Ok(artifact) => PlasmCompBundle::new(artifact).map_err(ProgramStageError::plan),
        Err(CompileSourceError::SessionProvision(error)) => {
            Err(ProgramStageError::SessionProvision { error })
        }
        Err(CompileSourceError::Dag(crate::error::DagCompilationError::SurfaceParse(
            crate::plasm_plan_run::ProgramSurfaceParseError::Parse(error),
        ))) => {
            let correction = crate::plasm_plan_run::format_session_symbolic_parse_error(
                session,
                symbol_map_cross_cache,
                pipeline,
                source,
                &error,
            );
            Err(ProgramStageError::Parse {
                correction,
                span_offset: Some(error.offset),
                error: std::sync::Arc::new(crate::program_diagnostic::ProgramParseError::Surface(
                    *error,
                )),
            })
        }
        Err(CompileSourceError::Dag(error)) => Err(error.into()),
        Err(CompileSourceError::Plan(error)) => Err(ProgramStageError::plan(error)),
    }
}

/// Compile a multi-line Plasm program to a runnable comp bundle.
pub fn compile_plasm_program(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<PlasmCompBundle, ProgramStageError> {
    compile_to_bundle(pipeline, symbol_map_cross_cache, session, name, source)
}

/// Compile one expression (DAG program or single surface line) to a runnable comp bundle.
pub fn compile_plasm_expression(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<PlasmCompBundle, ProgramStageError> {
    compile_to_bundle(pipeline, symbol_map_cross_cache, session, name, source)
}

/// One-line surface compile → runnable comp bundle.
pub fn compile_plasm_surface_line_to_comp(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<PlasmCompBundle, ProgramStageError> {
    compile_plasm_expression(pipeline, symbol_map_cross_cache, session, name, source)
}

/// Compile the statically admitted Python `Program.build` frontend.
/// This does not execute the module or infer effects by running Python.
pub async fn compile_python_program(
    session: &ExecuteSession,
    source: &str,
) -> Result<PlasmCompBundle, crate::compilation_error::CompilationError> {
    let preflight = session.preflight_snapshot().await;
    let bundle = crate::plasm_dag::compile_python_program_checked(&preflight, source)?;
    let admission = Box::pin(crate::python_compute::admit_bundle(&preflight, &bundle)).await;
    admission?;
    Ok(bundle)
}

/// Sole production source-language entry point. Never retries the native parser.
pub async fn compile_program(
    _pipeline: &PromptPipelineConfig,
    _symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    _name: &str,
    source: &str,
) -> Result<PlasmCompBundle, crate::compilation_error::CompilationError> {
    let preflight = session.preflight_snapshot().await;
    let bundle = crate::python_program_diagnostic::compile(&preflight, source)?;
    Box::pin(crate::python_compute::admit_bundle(&preflight, &bundle)).await?;
    Ok(bundle)
}

#[cfg(test)]
mod error_footprint_tests {
    #[test]
    fn repaired_owners_keep_compile_envelope_small_without_more_boxes() {
        let bytes = std::mem::size_of::<super::CompileSourceError>();
        assert!(
            bytes < 128,
            "CompileSourceError is {bytes} bytes; expected <128"
        );
    }
}
