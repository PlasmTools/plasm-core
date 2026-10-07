//! Correctable program diagnostics for agent-facing `plasm` dry-run (`needs_fix` / `deny`).
//!
//! Category is carried by [`ProgramStageError`] at the failing stage — never re-sniffed from prose.

use plasm_core::expr_parser::{collect_program_statement_lines, split_assignment_for_binding};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::borrow::Cow;

use crate::execute_session::ExecuteSession;
use crate::mcp_agent_present::{AgentContent, PlanTokenRefs};
use crate::plan_dry_display::PlanDryVerdict;
use crate::plasm_plan_run::{
    parse_plasm_surface_line_program, DryPlasmPlanEvaluation, PlasmPlanRunResult,
};
use crate::program_reject_memory::RejectReplay;
use plasm_core::{PromptPipelineConfig, SymbolMapCrossRequestCache};
use plasm_trace::TraceCompWire;

/// Stage where a correctable program failure was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramErrorCategory {
    Parse,
    Type,
    Plan,
    Flow,
}

impl ProgramErrorCategory {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Type => "type",
            Self::Plan => "plan",
            Self::Flow => "flow",
        }
    }
}

/// Typed failure from compile / dry-eval / flow gate — category is structural, not heuristic.
#[derive(Debug, Clone)]
pub enum ProgramStageError {
    Dispatch {
        error: std::sync::Arc<crate::execute_pipeline::DispatchError>,
    },
    SessionCatalog {
        error: crate::error::SessionCatalogNotLoaded,
    },
    Parse {
        correction: String,
        span_offset: Option<usize>,
        error: std::sync::Arc<ProgramParseError>,
    },
    PythonLowering {
        error: crate::program_rejection::PythonLoweringError,
    },
    CatalogOwnership {
        error: crate::catalog_ownership::CatalogOwnershipError,
    },
    SessionProvision {
        error: crate::plan_session_provisions::SessionProvisionError,
    },
    PythonCompute {
        error: crate::program_rejection::PythonComputeRejection,
    },
    PythonAnalysis {
        diagnostics: Vec<monty_analysis::AnalysisDiagnostic>,
        source: String,
        stubs: String,
    },
    PythonSyntax {
        error: monty_types::MontyException,
    },
    CoreType {
        error: plasm_core::TypeError,
    },
    RowCompute {
        error: plasm_core::row_plan::RowComputeError,
    },
    Plan {
        error: std::sync::Arc<PlanStageError>,
    },
    Flow {
        denial: crate::plan_flow::FlowDenial,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProgramParseError {
    #[error(transparent)]
    Dag(std::sync::Arc<crate::error::DagCompilationError>),
    #[error("Submit a Python Program subclass with build(self).")]
    EmptyProgram,
    #[error("Python program exceeds 32 KiB source budget")]
    SourceBudgetExceeded,
    #[error("Python syntax: {0}")]
    Python(#[source] ruff_python_parser::ParseError),
    #[error(transparent)]
    Surface(#[from] plasm_core::expr_parser::ParseError),
}

#[derive(Debug, thiserror::Error)]
pub enum PlanStageError {
    #[error(transparent)]
    Dag(#[from] crate::error::DagCompilationError),
    #[error(transparent)]
    DryValidation(#[from] crate::plasm_plan_run::DryPlanValidationError),
    #[error(transparent)]
    Bundle(#[from] crate::plasm_comp_bundle::PlasmCompBundleError),
    #[error(transparent)]
    Validation(#[from] crate::plasm_plan::PlanValidationError),
    #[error("evidence comp_committed: {0}")]
    Evidence(#[from] crate::evidence_chain::EvidenceEmitError),
    #[error(transparent)]
    Preparation(#[from] crate::plasm_step_convert::StepPayloadLiftError),
    #[error(transparent)]
    WireField(#[from] crate::plasm_plan_run::WireFieldTokenError),
    #[error(transparent)]
    Scope(#[from] crate::map_body::MapBodyValidationError),
    #[error("entity `{entity}` is not defined in the resolved catalog")]
    EntityMissing { entity: String },
    #[error("projection field `{field}` (wire `{wire}`) is not declared on entity `{entity}`")]
    ProjectionFieldMissing {
        field: String,
        wire: String,
        entity: String,
    },
    #[error("plan.nodes[{index}] requires ir or ir_template for executable surface")]
    ExecutableSurfaceMissing { index: usize },
    #[error("relation traversal requires federated session dispatch")]
    FederationRequired,
    #[error("plan dry-run preflight failed — fix errors before run_ref")]
    PreflightFailed,
    #[error("{0}")]
    DryStaging(#[source] plasm_runtime::ExecutionFailure),
}

impl ProgramStageError {
    pub fn plan(error: impl Into<PlanStageError>) -> Self {
        Self::Plan {
            error: std::sync::Arc::new(error.into()),
        }
    }

    pub fn category(&self) -> ProgramErrorCategory {
        match self {
            Self::Dispatch { .. } => ProgramErrorCategory::Plan,
            Self::SessionCatalog { .. } => ProgramErrorCategory::Plan,
            Self::Parse { .. } | Self::PythonSyntax { .. } => ProgramErrorCategory::Parse,
            Self::PythonLowering { .. }
            | Self::PythonCompute { .. }
            | Self::CatalogOwnership { .. }
            | Self::PythonAnalysis { .. }
            | Self::CoreType { .. }
            | Self::RowCompute { .. } => ProgramErrorCategory::Type,
            Self::Plan { .. } => ProgramErrorCategory::Plan,
            Self::SessionProvision { .. } => ProgramErrorCategory::Plan,
            Self::Flow { .. } => ProgramErrorCategory::Flow,
        }
    }

    pub fn correction(&self) -> Cow<'_, str> {
        match self {
            Self::Dispatch { error } => Cow::Owned(error.to_string()),
            Self::SessionCatalog { error } => Cow::Owned(error.to_string()),
            Self::Parse { correction, .. } => Cow::Borrowed(correction),
            Self::Plan { error } => Cow::Owned(error.to_string()),
            Self::PythonLowering { error } => Cow::Owned(error.to_string()),
            Self::CatalogOwnership { error } => Cow::Owned(error.to_string()),
            Self::SessionProvision { error } => Cow::Owned(error.to_string()),
            Self::PythonCompute { error } => Cow::Owned(error.correction().into_owned()),
            Self::PythonAnalysis {
                diagnostics,
                source,
                stubs,
            } => Cow::Owned(crate::python_compute::render_analysis_diagnostics(
                diagnostics,
                source,
                stubs,
            )),
            Self::PythonSyntax { error } => Cow::Owned(error.to_string()),
            Self::CoreType { error } => Cow::Owned(error.python_correction()),
            Self::RowCompute { error } => Cow::Owned(error.to_string()),
            Self::Flow { denial } => Cow::Owned(denial.agent_correction()),
        }
    }

    pub fn into_correction(self) -> String {
        match self {
            Self::Dispatch { error } => error.to_string(),
            Self::SessionCatalog { error } => error.to_string(),
            Self::Parse { correction, .. } => correction,
            Self::Plan { error } => error.to_string(),
            Self::PythonLowering { error } => error.into_message(),
            Self::CatalogOwnership { error } => error.to_string(),
            Self::SessionProvision { error } => error.to_string(),
            Self::PythonCompute { error } => error.into_correction(),
            Self::PythonAnalysis {
                diagnostics,
                source,
                stubs,
            } => crate::python_compute::render_analysis_diagnostics(&diagnostics, &source, &stubs),
            Self::PythonSyntax { error } => error.to_string(),
            Self::CoreType { error } => error.python_correction(),
            Self::RowCompute { error } => error.to_string(),
            Self::Flow { denial } => denial.agent_correction(),
        }
    }

    pub fn span_offset(&self) -> Option<usize> {
        match self {
            Self::Parse { span_offset, .. } => *span_offset,
            Self::PythonLowering { error } => error.span_offset(),
            _ => None,
        }
    }

    pub fn verdict(&self) -> PlanDryVerdict {
        match self {
            Self::Flow { .. } => PlanDryVerdict::Deny,
            _ => PlanDryVerdict::NeedsFix,
        }
    }
}

impl std::fmt::Display for ProgramStageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.correction())
    }
}

impl std::error::Error for ProgramStageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse { error, .. } => Some(error.as_ref()),
            Self::Plan { error } => Some(error.as_ref()),
            Self::Dispatch { error } => Some(error.as_ref()),
            Self::PythonLowering { error } => Some(error),
            Self::SessionCatalog { error } => Some(error),
            Self::CatalogOwnership { error } => Some(error),
            Self::SessionProvision { error } => Some(error),
            Self::PythonCompute { error } => Some(error),
            Self::CoreType { error } => Some(error),
            Self::RowCompute { error } => Some(error),
            _ => None,
        }
    }
}

impl From<crate::execute_pipeline::DispatchError> for ProgramStageError {
    fn from(error: crate::execute_pipeline::DispatchError) -> Self {
        Self::Dispatch {
            error: std::sync::Arc::new(error),
        }
    }
}

impl From<crate::error::DagCompilationError> for ProgramStageError {
    fn from(error: crate::error::DagCompilationError) -> Self {
        use crate::error::DagCompilationError as E;
        match error {
            E::Type(error) => Self::CoreType { error },
            error @ (E::SurfaceParse(_)
            | E::ExprNode(_)
            | E::SurfaceSyntax(_)
            | E::Pipe(_)
            | E::CollectMeta(_)
            | E::TemplateSyntax(_)
            | E::Iteration(_)
            | E::MembershipRhs(_)) => Self::Parse {
                correction: error.to_string(),
                span_offset: None,
                error: std::sync::Arc::new(ProgramParseError::Dag(std::sync::Arc::new(error))),
            },
            error => Self::plan(error),
        }
    }
}

impl From<ProgramStageError> for String {
    fn from(value: ProgramStageError) -> Self {
        value.into_correction()
    }
}

/// Whole-program correctness scores in \[0.0, 1.0\].
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ProgramScore {
    pub overall: f64,
    pub parse: f64,
    pub type_check: f64,
    pub plan: f64,
}

impl ProgramScore {
    pub fn wire_token(self) -> String {
        format!("{:.2}", self.overall)
    }

    pub fn to_json(self) -> Value {
        json!({
            "overall": self.overall,
            "parse": self.parse,
            "type": self.type_check,
            "plan": self.plan,
        })
    }

    fn from_stages(parse: f64, type_check: f64, plan: f64) -> Self {
        let overall = 0.4 * parse + 0.3 * type_check + 0.3 * plan;
        Self {
            overall: (overall * 100.0).round() / 100.0,
            parse,
            type_check,
            plan,
        }
    }

    fn for_category(category: ProgramErrorCategory, parse_ratio: f64) -> Self {
        let (parse, type_check, plan) = match category {
            ProgramErrorCategory::Parse => (parse_ratio, 0.0, 0.0),
            ProgramErrorCategory::Type => (1.0, 0.0, 0.0),
            ProgramErrorCategory::Plan | ProgramErrorCategory::Flow => (1.0, 1.0, 0.0),
        };
        Self::from_stages(parse, type_check, plan)
    }
}

/// Prefix the host understood before the failing locus (not an executable plan).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnderstoodSketch {
    pub bindings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sketch: Option<String>,
    pub ok_count: usize,
    pub total: usize,
}

impl UnderstoodSketch {
    pub fn parse_ratio(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.ok_count as f64 / self.total as f64).clamp(0.0, 1.0)
    }
}

/// Structured correctable program failure (MCP/HTTP `needs_fix` / `deny`).
///
/// Category and verdict are derived from [`ProgramStageError`] — never stored separately.
#[derive(Debug, Clone)]
pub struct ProgramDiagnostic {
    pub stage: ProgramStageError,
    pub score: ProgramScore,
    pub understood: Option<UnderstoodSketch>,
    /// Set when this program or reject text was already returned in-session.
    pub replay: Option<RejectReplay>,
}

/// Classify a compile `String` via typed parse/typecheck — bridge until DAG returns staged errors.
/// Best-effort prefix salvage for multi-statement programs.
pub fn salvage_understood_prefix(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    program: &str,
) -> Option<UnderstoodSketch> {
    let stmts = collect_program_statement_lines(program).ok()?;
    if stmts.len() <= 1 {
        return None;
    }
    let total = stmts.iter().filter(|s| !s.trim().is_empty()).count().max(1);
    let mut ok_bindings = Vec::new();
    let mut ok_lines = Vec::new();
    let mut failed_at = None;
    for stmt in &stmts {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        match parse_plasm_surface_line_program(
            session,
            symbol_map_cross_cache,
            pipeline,
            trimmed,
            None,
            false,
        ) {
            Ok(_) => {
                if let Some((label, _)) = split_assignment_for_binding(trimmed) {
                    ok_bindings.push(label.trim().to_string());
                }
                ok_lines.push(trimmed.to_string());
            }
            Err(_) => {
                failed_at = Some(
                    split_assignment_for_binding(trimmed)
                        .map(|(l, _)| l.trim().to_string())
                        .unwrap_or_else(|| trimmed.chars().take(48).collect()),
                );
                break;
            }
        }
    }
    if ok_lines.is_empty() && failed_at.is_none() {
        return None;
    }
    let ok_count = ok_lines.len();
    let sketch = if ok_lines.is_empty() {
        None
    } else {
        Some(format!(
            "parsed {}/{} statements · {}",
            ok_count,
            total,
            ok_lines.join("; ")
        ))
    };
    Some(UnderstoodSketch {
        bindings: ok_bindings,
        failed_at,
        sketch,
        ok_count,
        total,
    })
}

impl ProgramDiagnostic {
    pub fn category(&self) -> ProgramErrorCategory {
        self.stage.category()
    }

    pub fn verdict(&self) -> PlanDryVerdict {
        self.stage.verdict()
    }

    pub fn correction(&self) -> Cow<'_, str> {
        self.stage.correction()
    }

    pub fn span_offset(&self) -> Option<usize> {
        self.stage.span_offset()
    }

    pub fn from_stage(
        _pipeline: &PromptPipelineConfig,
        _symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
        session: &ExecuteSession,
        program: &str,
        stage: ProgramStageError,
    ) -> Self {
        let category = stage.category();
        let understood = match category {
            ProgramErrorCategory::Parse => {
                crate::python_program_diagnostic::understood_prefix(program)
            }
            _ => None,
        };
        let parse_ratio = understood
            .as_ref()
            .map(UnderstoodSketch::parse_ratio)
            .unwrap_or(0.0);
        let mut diag = Self {
            stage,
            score: ProgramScore::for_category(category, parse_ratio),
            understood,
            replay: None,
        };
        diag.replay =
            session.note_program_reject(program, category.as_wire(), diag.correction().as_ref());
        diag
    }

    pub fn flow_denied(denial: &crate::plan_flow::FlowDenial) -> Self {
        let stage = ProgramStageError::Flow {
            denial: denial.clone(),
        };
        Self {
            score: ProgramScore::for_category(ProgramErrorCategory::Flow, 1.0),
            stage,
            understood: None,
            replay: None,
        }
    }

    /// Quiet agent markdown: status · category, then correction (+ optional understood).
    pub fn agent_markdown(&self) -> String {
        let mut out = format!(
            "{} · {}\n\n{}",
            self.verdict().as_wire(),
            self.category().as_wire(),
            self.correction()
        );
        if let Some(u) = &self.understood {
            if let Some(sketch) = &u.sketch {
                out.push_str("\n\n");
                out.push_str(sketch);
            } else if !u.bindings.is_empty() {
                out.push_str("\n\nunderstood bindings: ");
                out.push_str(&u.bindings.join(", "));
            }
        }
        if let Some(replay) = self.replay {
            out.push_str("\n\n");
            out.push_str(&replay.markdown_line());
        }
        out
    }

    pub fn agent_meta(
        &self,
        logical_session_ref: &str,
        domain_revision: u32,
    ) -> Map<String, Value> {
        let mut plasm = Map::new();
        plasm.insert(
            "dry_verdict".into(),
            Value::String(self.verdict().as_wire().into()),
        );
        plasm.insert("dry_run".into(), Value::Bool(true));
        plasm.insert(
            "logical_session_ref".into(),
            Value::String(logical_session_ref.into()),
        );
        plasm.insert("domain_revision".into(), json!(domain_revision));
        plasm.insert(
            "error_category".into(),
            Value::String(self.category().as_wire().into()),
        );
        plasm.insert("program_score".into(), self.score.to_json());
        if let Some(off) = self.span_offset() {
            plasm.insert("span_offset".into(), json!(off));
        }
        if let Some(u) = &self.understood {
            plasm.insert(
                "understood".into(),
                serde_json::to_value(u).unwrap_or(Value::Null),
            );
        }
        if let Some(replay) = self.replay {
            plasm.insert("already_rejected".into(), Value::Bool(true));
            plasm.insert(
                "already_rejected_kind".into(),
                Value::String(replay.kind_wire().into()),
            );
            plasm.insert("already_rejected_prior".into(), json!(replay.prior_count));
        }
        plasm
    }

    pub fn into_plan_run_result(
        self,
        logical_session_ref: &str,
        domain_revision: u32,
    ) -> PlasmPlanRunResult {
        self.into_plan_run_result_inner(logical_session_ref, domain_revision, None)
    }

    /// Deny (and similar) after a successful dry shell — keep node/comp payload for UI.
    pub fn into_plan_run_result_with_dry(
        self,
        logical_session_ref: &str,
        domain_revision: u32,
        dry: &DryPlasmPlanEvaluation,
        comp: TraceCompWire,
    ) -> PlasmPlanRunResult {
        self.into_plan_run_result_inner(logical_session_ref, domain_revision, Some((dry, comp)))
    }

    fn into_plan_run_result_inner(
        self,
        logical_session_ref: &str,
        domain_revision: u32,
        dry_shell: Option<(&DryPlasmPlanEvaluation, TraceCompWire)>,
    ) -> PlasmPlanRunResult {
        use crate::plasm_plan_run::PlanAgentOutcome;

        let score_tok = self.score.wire_token();
        let cat = self.category().as_wire();
        let verdict = self.verdict();
        let verdict_wire = verdict.as_wire();
        let agent_outcome = PlanAgentOutcome::from_verdict(verdict);
        let markdown = AgentContent::plan_needs_fix(
            &PlanTokenRefs {
                run_ref: "",
                dry_verdict: verdict_wire,
                logical_session_ref,
                plan_uri: None,
                program_score: Some(score_tok.as_str()),
                error_category: Some(cat),
            },
            &self.agent_markdown(),
        )
        .render();
        let mut meta = Map::new();
        meta.insert(
            "plasm".into(),
            Value::Object(self.agent_meta(logical_session_ref, domain_revision)),
        );
        let (version, node_results, graph_summary, comp) = match dry_shell {
            Some((dry, comp)) => (
                dry.version.clone(),
                dry.node_results.clone(),
                dry.graph_summary.clone(),
                Some(comp),
            ),
            None => (json!(0), Vec::new(), json!({}), None),
        };
        PlasmPlanRunResult {
            version,
            agent_outcome,
            node_results,
            graph_summary,
            comp,
            code_plan_run_artifacts: Vec::new(),
            run_markdown: Some(markdown),
            run_plasm_meta: Some(meta),
            return_steps: Vec::new(),
            inline_plan_ui: None,
        }
    }
}

/// HTTP/JSON body for plan-mode `needs_fix` / `deny` (200).
pub fn needs_fix_http_payload(
    diagnostic: &ProgramDiagnostic,
    logical_session_ref: Option<&str>,
) -> Value {
    let mut obj = Map::new();
    obj.insert("plan".into(), Value::Bool(true));
    obj.insert("dry_run".into(), Value::Bool(true));
    obj.insert(
        "dry_verdict".into(),
        Value::String(diagnostic.verdict().as_wire().into()),
    );
    obj.insert(
        "error_category".into(),
        Value::String(diagnostic.category().as_wire().into()),
    );
    obj.insert("program_score".into(), diagnostic.score.to_json());
    obj.insert(
        "correction".into(),
        Value::String(diagnostic.correction().into()),
    );
    if let Some(u) = &diagnostic.understood {
        obj.insert(
            "understood".into(),
            serde_json::to_value(u).unwrap_or(Value::Null),
        );
    }
    if let Some(off) = diagnostic.span_offset() {
        obj.insert("span_offset".into(), json!(off));
    }
    if let Some(r) = logical_session_ref {
        obj.insert("logical_session_ref".into(), Value::String(r.into()));
    }
    if let Some(replay) = diagnostic.replay {
        obj.insert("already_rejected".into(), Value::Bool(true));
        obj.insert(
            "already_rejected_kind".into(),
            Value::String(replay.kind_wire().into()),
        );
        obj.insert("already_rejected_prior".into(), json!(replay.prior_count));
    }
    Value::Object(obj)
}

/// Shared MCP/HTTP builder: stage → diagnostic → plan run result.
pub fn plan_run_from_stage(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    program: &str,
    logical_session_ref: &str,
    stage: ProgramStageError,
) -> PlasmPlanRunResult {
    ProgramDiagnostic::from_stage(pipeline, symbol_map_cross_cache, session, program, stage)
        .into_plan_run_result(logical_session_ref, session.domain_revision)
}

impl From<ProgramStageError> for plasm_runtime::ExecutionFailure {
    fn from(error: ProgramStageError) -> Self {
        if let ProgramStageError::Plan { error: plan } = &error {
            if let PlanStageError::DryValidation(
                crate::plasm_plan_run::DryPlanValidationError::SurfacePolicy { source, .. },
            ) = plan.as_ref()
            {
                return source.clone().into();
            }
        }
        Self::new(
            plasm_runtime::FailureCause::Program,
            "program_admission",
            error.into_correction(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::PromptPipelineConfig;

    #[test]
    fn plan_stage_clone_preserves_semantic_cause() {
        use std::error::Error;
        let stage = ProgramStageError::plan(PlanStageError::ExecutableSurfaceMissing { index: 7 });
        let cloned = stage.clone();
        assert!(matches!(
            cloned.source().unwrap().downcast_ref::<PlanStageError>(),
            Some(PlanStageError::ExecutableSurfaceMissing { index: 7 })
        ));
        assert_eq!(cloned.category(), ProgramErrorCategory::Plan);
    }

    #[test]
    fn python_parse_stage_retains_original_parser_cause() {
        use std::error::Error;
        let error = ruff_python_parser::parse_module("def :").unwrap_err();
        let stage = ProgramStageError::Parse {
            correction: format!("Python syntax: {error}"),
            span_offset: Some(u32::from(error.location.start()) as usize),
            error: std::sync::Arc::new(ProgramParseError::Python(error)),
        };
        assert!(stage
            .source()
            .unwrap()
            .source()
            .unwrap()
            .is::<ruff_python_parser::ParseError>());
        assert_eq!(stage.category(), ProgramErrorCategory::Parse);
    }

    #[test]
    fn score_weights_parse_type_plan() {
        let s = ProgramScore::from_stages(1.0, 0.0, 0.0);
        assert!((s.overall - 0.4).abs() < f64::EPSILON);
        let s2 = ProgramScore::from_stages(1.0, 1.0, 0.0);
        assert!((s2.overall - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn agent_markdown_is_quiet() {
        let d = ProgramDiagnostic {
            stage: ProgramStageError::Parse {
                correction: "Change `e99` to a taught entity.".into(),
                span_offset: Some(0),
                error: std::sync::Arc::new(ProgramParseError::SourceBudgetExceeded),
            },
            score: ProgramScore::from_stages(0.0, 0.0, 0.0),
            understood: None,
            replay: None,
        };
        let md = d.agent_markdown();
        assert!(md.starts_with("needs_fix · parse"));
        assert!(!md.to_lowercase().contains("syntax/session are fine"));
        assert!(!md.to_lowercase().contains("not a disaster"));
    }

    #[test]
    fn into_plan_run_result_omits_run_ref_and_sets_needs_fix() {
        let d = ProgramDiagnostic {
            stage: ProgramStageError::Parse {
                correction: "Fix spelling of entity.".into(),
                span_offset: None,
                error: std::sync::Arc::new(ProgramParseError::SourceBudgetExceeded),
            },
            score: ProgramScore::from_stages(0.5, 0.0, 0.0),
            understood: Some(UnderstoodSketch {
                bindings: vec!["x0".into()],
                failed_at: Some("x1".into()),
                sketch: Some("parsed 1/2 statements · x0 = e1".into()),
                ok_count: 1,
                total: 2,
            }),
            replay: None,
        };
        let out = d.into_plan_run_result("l_ref", 3);
        let md = out.run_markdown.as_deref().unwrap_or("");
        assert!(md.contains("dry_verdict\tneeds_fix"));
        assert!(!md.contains("run_ref\t"));
        assert!(md.contains("program_score\t0.20") || md.contains("program_score\t0.2"));
        assert!(md.contains("error_category\tparse"));
        assert_eq!(
            out.agent_outcome,
            crate::plasm_plan_run::PlanAgentOutcome::NeedsFix
        );
        assert_eq!(out.metrics_label(), "needs_fix");
        assert!(!out.version.as_str().is_some_and(|s| s == "needs_fix"));
        let plasm = out
            .run_plasm_meta
            .as_ref()
            .and_then(|m| m.get("plasm"))
            .expect("plasm meta");
        assert_eq!(
            plasm.get("dry_verdict").and_then(|v| v.as_str()),
            Some("needs_fix")
        );
        assert!(plasm.get("run_ref").is_none());
        assert!(plasm.get("understood").is_some());
    }

    #[test]
    fn flow_denied_uses_deny_verdict_and_score() {
        let denial = crate::plan_flow::FlowDenial {
            verdict: crate::plan_flow::FlowVerdict::Denied,
            violations: vec![crate::plan_flow::FlowViolation {
                node: "n1".into(),
                kind: Some(crate::plan_flow::FlowViolationKind::ForbiddenFlow),
                sink_param: None,
                labels: Default::default(),
                reason: "restricted data cannot reach this sink".into(),
            }],
        };
        let d = ProgramDiagnostic::flow_denied(&denial);
        assert!(d
            .correction()
            .contains("n1: restricted data cannot reach this sink"));
        assert!(!d.correction().contains("violation(s)"));
        assert_eq!(d.verdict(), PlanDryVerdict::Deny);
        assert!((d.score.overall - 0.7).abs() < f64::EPSILON);
        let out = d.into_plan_run_result("l_ref", 1);
        assert_eq!(
            out.agent_outcome,
            crate::plasm_plan_run::PlanAgentOutcome::Deny
        );
        assert_eq!(out.metrics_label(), "deny");
        let md = out.run_markdown.as_deref().unwrap_or("");
        assert!(md.contains("dry_verdict\tdeny"));
        assert!(md.contains("deny · flow"));
    }

    #[test]
    fn stage_error_does_not_require_string_sniff() {
        let stages = [
            ProgramStageError::PythonLowering {
                error: crate::program_rejection::PythonLoweringError::Source {
                    error: std::sync::Arc::new(
                        crate::program_rejection::PythonSourceError::ExpectedFieldDependency,
                    ),
                    span: Some((5, 8)),
                },
            },
            ProgramStageError::PythonCompute {
                error: crate::program_rejection::PythonComputeError::AnnotationSourceMismatch
                    .into(),
            },
            ProgramStageError::CoreType {
                error: plasm_core::TypeError::RequiredParameterOmitted {
                    parameter: "item_id".into(),
                    expression: "e1{item_id=$}".into(),
                },
            },
            ProgramStageError::RowCompute {
                error: plasm_core::row_plan::RowComputeError::Fusion(
                    plasm_core::row_plan::FusionError::PythonInPipeline,
                ),
            },
        ];
        for stage in stages {
            assert_eq!(stage.category(), ProgramErrorCategory::Type);
            assert_eq!(stage.verdict(), PlanDryVerdict::NeedsFix);
            let expected = stage.correction().into_owned();
            let diagnostic = ProgramDiagnostic::from_stage(
                &PromptPipelineConfig::default(),
                None,
                &reject_memory_session(),
                "class P(Program): pass",
                stage,
            );
            assert_eq!(
                needs_fix_http_payload(&diagnostic, None)["correction"],
                expected
            );
            assert!(diagnostic.agent_markdown().contains(&expected));
            assert!(!expected.contains("session_mode"));
        }
    }

    fn reject_memory_session() -> ExecuteSession {
        use std::path::PathBuf;
        use std::sync::Arc;

        use plasm_core::TeachingExposureSession;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "langmatrix".into(),
            Arc::new(plasm_core::CgsContext::entry("langmatrix", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "langmatrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
        )
    }

    #[test]
    fn identical_program_reject_is_named_on_replay() {
        let session = reject_memory_session();
        let pipeline = PromptPipelineConfig::default();
        let program = "rows = e1\nout = rows | select dest = (id | split_part('/', 0))";
        let stage = ProgramStageError::Plan {
            error: std::sync::Arc::new(PlanStageError::PreflightFailed),
        };
        let first =
            ProgramDiagnostic::from_stage(&pipeline, None, &session, program, stage.clone());
        assert!(first.replay.is_none());
        let first_md = first.agent_markdown();
        assert!(!first_md.contains("revise this program and retry"));
        assert!(!first_md.contains("already rejected"));

        let second = ProgramDiagnostic::from_stage(&pipeline, None, &session, program, stage);
        assert!(second.replay.is_some());
        let second_md = second.agent_markdown();
        assert!(second_md.contains("This exact program was already rejected"));
        assert!(!second_md.contains("revise this program and retry"));
        assert!(!second_md.to_lowercase().contains("render"));
        let meta = second.agent_meta("l_ref", 1);
        assert_eq!(
            meta.get("already_rejected").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            meta.get("already_rejected_kind").and_then(|v| v.as_str()),
            Some("exact_program")
        );
        let http = needs_fix_http_payload(&second, Some("l_ref"));
        assert_eq!(
            http.get("already_rejected").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn get_suffix_diagnostic_preserves_selection_during_repair() {
        let session = reject_memory_session();
        let pipeline = PromptPipelineConfig::default();
        let invalid = r#"item = LangItem("i1"){title="Chosen"}
item"#;
        let error =
            crate::compile_plasm_expression(&pipeline, None, &session, "get-suffix", invalid)
                .expect_err("Get does not accept query braces");
        assert!(
            error.correction().contains("Get accepts identity only"),
            "{}",
            error.correction()
        );
        assert!(error.correction().contains("| where"));
        let repaired = r#"item = LangItem("i1") | where title = "Chosen"
item"#;
        crate::compile_plasm_expression(&pipeline, None, &session, "get-suffix-repair", repaired)
            .expect("repair retains both identity and selection");
    }

    #[test]
    fn compile_select_split_part_replay_names_closed_form() {
        let session = reject_memory_session();
        let pipeline = PromptPipelineConfig::default();
        let program = "rows = LangItem\nout = rows | select dest = (id | split_part('/', 0))\nout";
        let stage =
            crate::compile_plasm_expression(&pipeline, None, &session, "reject-loop", program)
                .expect_err("select split_part is not row algebra");
        let quoted = stage.correction().to_string();
        assert!(
            quoted.contains("split_part") || quoted.contains("pipe"),
            "compile reject must name the illegal construct: {quoted}"
        );
        let first =
            ProgramDiagnostic::from_stage(&pipeline, None, &session, program, stage.clone());
        assert!(first.replay.is_none(), "first compile reject is fresh");
        let second = ProgramDiagnostic::from_stage(&pipeline, None, &session, program, stage);
        let md = second.agent_markdown();
        assert!(
            md.contains("This exact program was already rejected"),
            "replay must name the closed program: {md}"
        );
        assert!(!md.contains("revise this program and retry"));
    }

    /// T184015 c77: NAPI `dry_run` used to throw `compile_plasm_expression` Display
    /// without [`ProgramDiagnostic::from_stage`]. Memory never fired; the agent
    /// resubmitted the same program ~28 times. Hosts must record through from_stage.
    #[test]
    fn compile_display_alone_does_not_record_reject_memory() {
        let session = reject_memory_session();
        let pipeline = PromptPipelineConfig::default();
        let program = "rows = LangItem\nbad = rows | select not_a_taught_field\nbad";
        let first_stage =
            crate::compile_plasm_expression(&pipeline, None, &session, "c77-replay", program)
                .expect_err("unknown field is a compile reject");
        let first_raw = first_stage.to_string();
        assert!(
            first_raw.contains("not a row field"),
            "compile reject names the field: {first_raw}"
        );
        assert!(
            !first_raw.contains("already rejected"),
            "compile Display must not name replay: {first_raw}"
        );

        let second_stage =
            crate::compile_plasm_expression(&pipeline, None, &session, "c77-replay", program)
                .expect_err("identical resubmit still compiles");
        let second_raw = second_stage.to_string();
        assert_eq!(
            first_raw, second_raw,
            "compile-only path repeats the same diagnostic with no memory"
        );
        assert!(!second_raw.contains("already rejected"));

        let first_diag =
            ProgramDiagnostic::from_stage(&pipeline, None, &session, program, first_stage);
        assert!(first_diag.replay.is_none(), "first from_stage is fresh");
        let second_diag =
            ProgramDiagnostic::from_stage(&pipeline, None, &session, program, second_stage);
        let md = second_diag.agent_markdown();
        assert!(
            md.contains("This exact program was already rejected"),
            "from_stage is the insert+lookup: {md}"
        );
    }
}
