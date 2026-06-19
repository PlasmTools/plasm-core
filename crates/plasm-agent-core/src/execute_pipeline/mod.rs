//! Unified execute front door: dry-run is live preflight with I/O stubbed, not a parallel simulation path.

mod dispatch;
mod preflight;
mod run_line_error;
mod scope;

pub(crate) use dispatch::preflight_line_compile_dispatch;

pub use plasm_core::PreflightToken;
pub use preflight::{PlasmPreflight, PreflightReport, SimulationBundle};
pub use run_line_error::{display_run_line_error, RunLineError};
pub use scope::{session_scope_for_node, SessionScope};

use crate::execute_session::ExecuteSession;
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::{PlanRunTraceHooks, PlasmPlanRunResult};
use crate::server_state::PlasmHostState;
use plasm_core::expr_parser::ParsedExpr;

/// What the caller wants — replaces scattered `plan_only` / `run: bool` flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionIntent {
    /// Preflight + simulate materialization (no HTTP).
    PlanOnly,
    /// Preflight + engine + artifacts.
    Live,
}

impl ExecutionIntent {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

/// Single front door for HTTP/MCP execute ingress.
pub struct ExecutePipeline;

impl ExecutePipeline {
    /// Multi-line Plasm program (MCP `plasm` / `plasm_run`, HTTP program body).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_program(
        es: &ExecuteSession,
        st: &PlasmHostState,
        prompt_hash: &str,
        session_id: &str,
        bundle: &PlasmCompBundle,
        intent: ExecutionIntent,
        mcp_tool_hooks: Option<PlanRunTraceHooks>,
        execution_scope: Option<&crate::operation::ExecutionScope>,
        dry: Option<crate::plasm_plan_run::DryPlasmPlanEvaluation>,
    ) -> Result<PlasmPlanRunResult, String> {
        crate::plasm_plan_run::run_plasm_comp(
            es,
            st,
            prompt_hash,
            session_id,
            bundle,
            intent.is_live(),
            mcp_tool_hooks,
            execution_scope,
            dry,
        )
        .await
    }

    /// Single parsed Plasm line — shared preflight, then live engine (paging follow-ups).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_expression(
        line: &str,
        sess: &ExecuteSession,
        st: &PlasmHostState,
        session_id: &str,
        parsed: ParsedExpr,
        trace: Option<&crate::trace_sink_emit::PlasmTraceContext>,
        line_index: i64,
    ) -> Result<
        (
            ParsedExpr,
            plasm_runtime::ExecutionResult,
            Option<crate::run_artifacts::RunArtifactHandle>,
        ),
        RunLineError,
    > {
        PlasmPreflight::preflight_parsed_line(sess, line, &parsed).map_err(RunLineError::Parse)?;
        crate::http_execute::run_parsed_plasm_line(
            line,
            sess,
            st,
            session_id,
            parsed,
            trace,
            line_index,
            None,
            None,
            None,
            Some(plasm_core::PreflightToken::VERIFIED),
            None,
        )
        .await
    }

    /// Parse + preflight dry preview (HTTP `plan_only`, MCP `plasm` single-line surface).
    pub fn dry_preview_line(
        session: &ExecuteSession,
        source: &str,
        parsed: &ParsedExpr,
    ) -> Result<(String, String, serde_json::Value), String> {
        PlasmPreflight::dry_preview_for_line(session, source, parsed)
    }
}
