//! Correctable program diagnostics for agent-facing `plasm` dry-run (`needs_fix` / `deny`).
//!
//! Category is carried by [`ProgramStageError`] at the failing stage — never re-sniffed from prose.

use plasm_core::error_render::{render_type_error_with_feedback, FeedbackStyle};
use plasm_core::expr_parser::{collect_program_statement_lines, split_assignment_for_binding};
use plasm_core::TypeError;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::execute_session::ExecuteSession;
use crate::mcp_agent_present::{AgentContent, PlanTokenRefs};
use crate::plan_dry_display::PlanDryVerdict;
use crate::plasm_plan_run::{
    format_session_symbolic_parse_error, parse_plasm_surface_line_program,
    symbol_map_for_plasm_surface_parse, typecheck_parsed_for_session, DryPlasmPlanEvaluation,
    PlasmPlanRunResult,
};
use plasm_trace::TraceCompWire;
use plasm_core::{PromptPipelineConfig, SymbolMapCrossRequestCache};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStageError {
    Parse {
        correction: String,
        span_offset: Option<usize>,
    },
    Type {
        correction: String,
    },
    Plan {
        correction: String,
    },
    Flow {
        message: String,
    },
}

impl ProgramStageError {
    pub fn plan(correction: impl Into<String>) -> Self {
        Self::Plan {
            correction: correction.into(),
        }
    }

    pub fn category(&self) -> ProgramErrorCategory {
        match self {
            Self::Parse { .. } => ProgramErrorCategory::Parse,
            Self::Type { .. } => ProgramErrorCategory::Type,
            Self::Plan { .. } => ProgramErrorCategory::Plan,
            Self::Flow { .. } => ProgramErrorCategory::Flow,
        }
    }

    pub fn correction(&self) -> &str {
        match self {
            Self::Parse { correction, .. }
            | Self::Type { correction }
            | Self::Plan { correction } => correction.as_str(),
            Self::Flow { message } => message.as_str(),
        }
    }

    pub fn into_correction(self) -> String {
        match self {
            Self::Parse { correction, .. }
            | Self::Type { correction }
            | Self::Plan { correction } => correction,
            Self::Flow { message } => message,
        }
    }

    pub fn span_offset(&self) -> Option<usize> {
        match self {
            Self::Parse { span_offset, .. } => *span_offset,
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
        f.write_str(self.correction())
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
}

/// SymbolicLlm type-error correction for the active session map.
pub fn format_session_symbolic_type_error(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    err: &TypeError,
) -> String {
    let sym_map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let step = render_type_error_with_feedback(
        err,
        session.cgs.as_ref(),
        FeedbackStyle::SymbolicLlm {
            map: sym_map.as_ref(),
        },
    );
    if step.correction.is_empty() {
        err.to_string()
    } else {
        step.correction
    }
}

/// Classify a compile `String` via typed parse/typecheck — bridge until DAG returns staged errors.
/// Prefer [`crate::plasm_compile::compile_plasm_expression`] which already returns [`ProgramStageError`].
///
/// Multi-statement programs keep the original compile message as [`ProgramStageError::Plan`]:
/// re-walking lines mis-classifies binding refs (`rows`) as entity parse failures.
pub(crate) fn diagnose_compile_failure(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    program: &str,
    compile_msg: String,
) -> ProgramStageError {
    let Ok(stmts) = collect_program_statement_lines(program) else {
        return ProgramStageError::Parse {
            correction: compile_msg,
            span_offset: None,
        };
    };
    let nonempty: Vec<&str> = stmts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if nonempty.is_empty() {
        return ProgramStageError::Parse {
            correction: compile_msg,
            span_offset: None,
        };
    }
    if nonempty.len() > 1 {
        return ProgramStageError::Plan {
            correction: compile_msg,
        };
    }
    let trimmed = nonempty[0];
    match parse_plasm_surface_line_program(
        session,
        symbol_map_cross_cache,
        pipeline,
        trimmed,
        None,
        false,
    ) {
        Err(err) => ProgramStageError::Parse {
            correction: format_session_symbolic_parse_error(
                session,
                symbol_map_cross_cache,
                pipeline,
                trimmed,
                &err,
            ),
            span_offset: None,
        },
        Ok(parsed) => {
            if let Err(te) = typecheck_parsed_for_session(session, &parsed) {
                ProgramStageError::Type {
                    correction: format_session_symbolic_type_error(
                        session,
                        symbol_map_cross_cache,
                        &te,
                    ),
                }
            } else {
                ProgramStageError::Plan {
                    correction: compile_msg,
                }
            }
        }
    }
}

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

    pub fn correction(&self) -> &str {
        self.stage.correction()
    }

    pub fn span_offset(&self) -> Option<usize> {
        self.stage.span_offset()
    }

    pub fn from_stage(
        pipeline: &PromptPipelineConfig,
        symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
        session: &ExecuteSession,
        program: &str,
        stage: ProgramStageError,
    ) -> Self {
        let category = stage.category();
        let understood = match category {
            ProgramErrorCategory::Parse => {
                salvage_understood_prefix(pipeline, symbol_map_cross_cache, session, program)
            }
            _ => None,
        };
        let parse_ratio = understood
            .as_ref()
            .map(UnderstoodSketch::parse_ratio)
            .unwrap_or(0.0);
        Self {
            stage,
            score: ProgramScore::for_category(category, parse_ratio),
            understood,
        }
    }

    pub fn flow_denied(message: String) -> Self {
        let stage = ProgramStageError::Flow { message };
        Self {
            score: ProgramScore::for_category(ProgramErrorCategory::Flow, 1.0),
            stage,
            understood: None,
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
        out.push_str("\n\nrevise this program and retry");
        out
    }

    pub fn agent_meta(&self, logical_session_ref: &str, domain_revision: u32) -> Map<String, Value> {
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
        self.into_plan_run_result_inner(
            logical_session_ref,
            domain_revision,
            Some((dry, comp)),
        )
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
    ProgramDiagnostic::from_stage(
        pipeline,
        symbol_map_cross_cache,
        session,
        program,
        stage,
    )
    .into_plan_run_result(logical_session_ref, session.domain_revision)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            },
            score: ProgramScore::from_stages(0.0, 0.0, 0.0),
            understood: None,
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
            },
            score: ProgramScore::from_stages(0.5, 0.0, 0.0),
            understood: Some(UnderstoodSketch {
                bindings: vec!["x0".into()],
                failed_at: Some("x1".into()),
                sketch: Some("parsed 1/2 statements · x0 = e1".into()),
                ok_count: 1,
                total: 2,
            }),
        };
        let out = d.into_plan_run_result("l_ref", 3);
        let md = out.run_markdown.as_deref().unwrap_or("");
        assert!(md.contains("dry_verdict\tneeds_fix"));
        assert!(!md.contains("run_ref\t"));
        assert!(md.contains("program_score\t0.20") || md.contains("program_score\t0.2"));
        assert!(md.contains("error_category\tparse"));
        assert_eq!(out.agent_outcome, crate::plasm_plan_run::PlanAgentOutcome::NeedsFix);
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
        let d = ProgramDiagnostic::flow_denied("plan denied by flow policy".into());
        assert_eq!(d.verdict(), PlanDryVerdict::Deny);
        assert!((d.score.overall - 0.7).abs() < f64::EPSILON);
        let out = d.into_plan_run_result("l_ref", 1);
        assert_eq!(out.agent_outcome, crate::plasm_plan_run::PlanAgentOutcome::Deny);
        assert_eq!(out.metrics_label(), "deny");
        let md = out.run_markdown.as_deref().unwrap_or("");
        assert!(md.contains("dry_verdict\tdeny"));
        assert!(md.contains("deny · flow"));
    }

    #[test]
    fn stage_error_does_not_require_string_sniff() {
        let stage = ProgramStageError::Type {
            correction: "expected entity e1".into(),
        };
        assert_eq!(stage.category(), ProgramErrorCategory::Type);
        assert_eq!(stage.verdict(), PlanDryVerdict::NeedsFix);
    }
}
