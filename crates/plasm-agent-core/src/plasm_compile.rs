//! Canonical compile surface → [`PlasmCompBundle`] (monadic execution contract).

pub use crate::plasm_comp_bundle::PlasmCompBundle;

use crate::execute_session::ExecuteSession;
use crate::plasm_comp_wire::plasm_comp_from_validated;
use crate::plasm_dag::{
    compile_plasm_dag_to_plan_inner, compile_plasm_surface_line_to_plan, is_plasm_dag_source,
};
use crate::plasm_plan::validate_plan_artifact;
use crate::program_diagnostic::{diagnose_compile_failure, ProgramStageError};
use plasm_core::plasm_monad::PlasmCompArtifact;
use plasm_core::{PromptPipelineConfig, SymbolMapCrossRequestCache};

/// Lower surface/DAG source to a validated comp artifact (no plan wire exposure).
fn compile_source_to_artifact(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<PlasmCompArtifact, String> {
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
        Err(msg) => Err(diagnose_compile_failure(
            pipeline,
            symbol_map_cross_cache,
            session,
            source,
            msg,
        )),
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
pub fn compile_python_program(
    session: &ExecuteSession,
    source: &str,
) -> Result<PlasmCompBundle, String> {
    crate::plasm_dag::compile_python_program(session, source)
}

/// Sole production source-language entry point. Never retries the native parser.
pub fn compile_program(
    _pipeline: &PromptPipelineConfig,
    _symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    _name: &str,
    source: &str,
) -> Result<PlasmCompBundle, ProgramStageError> {
    crate::python_program_diagnostic::compile(session, source)
}
