//! Python DAG validation and lossless correction feedback.
use crate::ProgramSession;
use plasm_agent_core::PlasmCompBundle;
use serde::{Deserialize, Serialize};

/// One failed step with structured error (parse or type).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepDiagnostic {
    pub step_index: usize,
    pub expression: String,
    pub error: StepErrorPayload,
}

/// Production diagnostic fields plus the host repair/replay envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepErrorPayload {
    pub correction: String,
    pub category: String,
    pub span_offset: Option<usize>,
    pub details: serde_json::Value,
}

/// One row in eval JSON: mirrors BAML `PlasmPlan` (`text` + `reasoning`); eval uses a single step.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalPlanStep {
    pub text: String,
    /// BAML `PlasmPlan.reasoning` (short rationale).
    pub reasoning: String,
}

/// One LLM attempt: emitted plan steps and whether static validation passed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalAttemptReport {
    /// 1-based attempt index.
    pub attempt: u32,
    pub steps: Vec<EvalPlanStep>,
    pub validation_ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<StepDiagnostic>>,
    /// JSON [`build_correction_feedback`] passed into this attempt's `TranslatePlan` (set from round ≥ 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_context_in: Option<String>,
}

/// Compile complete programs in one stable session. No source rewrites or native parser.
pub fn validate_programs(
    session: &ProgramSession,
    sources: &[String],
) -> Result<Vec<PlasmCompBundle>, Vec<StepDiagnostic>> {
    if sources.is_empty() {
        return Err(vec![StepDiagnostic {
            step_index: 0,
            expression: String::new(),
            error: StepErrorPayload {
                correction:
                    "Submit one Python Program subclass with build(self) and an explicit return."
                        .into(),
                category: "parse".into(),
                span_offset: None,
                details: serde_json::json!({}),
            },
        }]);
    }
    let mut bundles = Vec::new();
    let mut errors = Vec::new();
    for (step_index, source) in sources.iter().enumerate() {
        match session.compile(source) {
            Ok(bundle) => bundles.push(bundle),
            Err(error) => errors.push(StepDiagnostic {
                step_index,
                expression: source.clone(),
                error: StepErrorPayload {
                    correction: error.agent_markdown(),
                    category: error.category().as_wire().into(),
                    span_offset: error.span_offset(),
                    details: serde_json::Value::Object(
                        error.agent_meta("local", session.execute.domain_revision),
                    ),
                },
            }),
        }
    }
    if errors.is_empty() {
        Ok(bundles)
    } else {
        Err(errors)
    }
}

/// JSON payload for the next `TranslatePlan` correction round.
#[derive(Debug, Serialize)]
struct CorrectionFeedback<'a> {
    round: usize,
    goal: &'a str,
    previous_steps: Vec<StepLine<'a>>,
    diagnostics: Vec<StepDiagnostic>,
}

#[derive(Debug, Serialize)]
struct StepLine<'a> {
    text: String,
    reasoning: &'a str,
}

/// Build `correction_context` string for BAML (JSON). Pass empty reasoning as `""`.
pub fn build_correction_feedback(
    goal: &str,
    round: usize,
    previous_steps: &[(&str, &str)],
    diagnostics: &[StepDiagnostic],
) -> String {
    let previous_steps: Vec<StepLine<'_>> = previous_steps
        .iter()
        .map(|(t, r)| StepLine {
            text: (*t).to_string(),
            reasoning: r,
        })
        .collect();
    let diagnostics = diagnostics.to_vec();
    let payload = CorrectionFeedback {
        round,
        goal,
        previous_steps,
        diagnostics,
    };
    serde_json::to_string_pretty(&payload).expect("serialize correction feedback")
}

/// Pipeline quality: first-shot success vs recovery vs failure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorrectionMetrics {
    pub rounds_used: u32,
    pub first_attempt_full_ok: bool,
    /// First attempt had any parse or type error.
    pub first_attempt_had_errors: bool,
    /// Final attempt passed parse + typecheck for all steps.
    pub final_full_ok: bool,
    /// First failed but a later round succeeded.
    pub recovered: bool,
    /// 1.0 first-shot ok, 0.85 recovered, 0.0 never ok.
    pub pipeline_score: f32,
    /// Diagnostics from the last failed round (if final_full_ok is false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<serde_json::Value>,
}

pub fn compute_pipeline_score(first_ok: bool, final_ok: bool) -> f32 {
    match (first_ok, final_ok) {
        (true, true) => 1.0,
        (false, true) => 0.85,
        (_, false) => 0.0,
    }
}

/// Summarize rounds for JSON report (after validation loop).
pub fn build_correction_metrics(
    rounds_used: u32,
    first_success_at: Option<u32>,
    final_ok: bool,
    last_failure: Option<serde_json::Value>,
) -> CorrectionMetrics {
    let first_attempt_full_ok = first_success_at == Some(0);
    let first_attempt_had_errors = first_success_at != Some(0);
    let recovered = final_ok && first_attempt_had_errors;
    let pipeline_score = compute_pipeline_score(first_attempt_full_ok, final_ok);
    CorrectionMetrics {
        rounds_used,
        first_attempt_full_ok,
        first_attempt_had_errors,
        final_full_ok: final_ok,
        recovered,
        pipeline_score,
        last_failure: if final_ok { None } else { last_failure },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correction_feedback_preserves_python_layout_and_literal_contents() {
        let source = "class Report(Program):\n    def build(self):\n        text = \"\"\";; => [not a teaching annotation]\n  keep indentation\n\"\"\"\n        return text\n";
        let diagnostics = vec![StepDiagnostic {
            step_index: 0,
            expression: source.into(),
            error: StepErrorPayload {
                correction: "Use a supported return root.".into(),
                category: "type".into(),
                details: serde_json::json!({}),
                span_offset: None,
            },
        }];
        let feedback = build_correction_feedback(
            "Render a report",
            1,
            &[(source, "literal content")],
            &diagnostics,
        );
        let value: serde_json::Value = serde_json::from_str(&feedback).unwrap();
        assert_eq!(value["previous_steps"][0]["text"], source);
        assert_eq!(value["diagnostics"][0]["expression"], source);
    }
}
