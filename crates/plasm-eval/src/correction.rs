//! Plan validation and LLM-facing correction feedback for eval rounds.

use plasm_core::domain_lexicon::DomainLexicon;
use plasm_core::error_render::{
    format_recovery_hints, render_parse_error_with_feedback,
    render_query_resolve_error_for_feedback, render_type_error_with_feedback, FeedbackStyle,
};
use plasm_core::expr::{ChainStep, Expr, QueryExpr, Ref};
use plasm_core::expr_correction::parse_session_line_with_rewrite_recovery;
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::normalize_expr_query_capabilities;
use plasm_core::predicate::Predicate;
use plasm_core::step_semantics::{append_correction_lines, StepError};
use plasm_core::symbol_tuning::{strip_prompt_expression_annotations, symbol_map_for_prompt};
use plasm_core::type_checker::type_check_expr;
use plasm_core::PromptPipelineConfig;
use plasm_core::TypeError;
use plasm_core::Value;
use plasm_core::CGS;
use serde::{Deserialize, Deserializer, Serialize};

/// One failed step with structured error (parse or type).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepDiagnostic {
    pub step_index: usize,
    pub expression: String,
    pub error: StepErrorPayload,
}

/// LLM-facing diagnostic: **only** `correction` (plus category / optional span). Legacy JSON
/// with `message` + `hints` / `correction_hints` is merged into `correction` on deserialize.
#[derive(Debug, Clone, Serialize)]
pub struct StepErrorPayload {
    pub correction: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_offset: Option<usize>,
}

impl<'de> Deserialize<'de> for StepErrorPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(alias = "message")]
            correction: String,
            category: String,
            #[serde(default)]
            span_offset: Option<usize>,
            #[serde(default)]
            #[serde(alias = "hints")]
            correction_hints: Vec<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let correction = append_correction_lines(raw.correction, raw.correction_hints);
        Ok(StepErrorPayload {
            correction,
            category: raw.category,
            span_offset: raw.span_offset,
        })
    }
}

/// One row in eval JSON: mirrors BAML `PlasmPlan` (`text` + `reasoning`); eval uses a single step.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalPlanStep {
    pub text: String,
    /// BAML `PlasmPlan.reasoning` (short rationale).
    pub reasoning: String,
}

/// Deterministic parse recovery rewrote the step `text` before it type-checked.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepLexiconNote {
    pub step_index: usize,
    /// Trimmed model `text` from the round.
    pub emitted: String,
    /// Expression string that actually parsed (entity-case or lexicon correction).
    pub resolved_to: String,
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
    /// Steps where strict parse failed and deterministic recovery produced a parseable string.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lexicon_notes: Vec<StepLexiconNote>,
}

impl From<&StepError> for StepErrorPayload {
    fn from(e: &StepError) -> Self {
        use plasm_core::step_semantics::StepErrorCategory;
        let category = match e.category {
            StepErrorCategory::Parse => "parse",
            StepErrorCategory::Type => "type",
            StepErrorCategory::Runtime => "runtime",
            StepErrorCategory::Auth => "auth",
            StepErrorCategory::Network => "network",
            StepErrorCategory::Config => "config",
        };
        Self {
            correction: e.correction.clone(),
            category: category.to_string(),
            span_offset: e.span_offset,
        }
    }
}

fn expr_contains_domain_placeholder(expr: &Expr) -> bool {
    match expr {
        Expr::Query(QueryExpr {
            predicate,
            pagination,
            ..
        }) => {
            predicate
                .as_ref()
                .is_some_and(predicate_contains_domain_placeholder)
                || pagination
                    .as_ref()
                    .is_some_and(|p| p.cursor.as_deref() == Some("$"))
        }
        Expr::Get(g) => {
            ref_contains_domain_placeholder(&g.reference)
                || g.path_vars
                    .as_ref()
                    .is_some_and(|m| m.values().any(Value::contains_domain_placeholder_deep))
        }
        Expr::Create(c) => c.input.to_value().contains_domain_placeholder_deep(),
        Expr::Delete(d) => {
            ref_contains_domain_placeholder(&d.target)
                || d.path_vars
                    .as_ref()
                    .is_some_and(|m| m.values().any(Value::contains_domain_placeholder_deep))
        }
        Expr::Invoke(i) => {
            ref_contains_domain_placeholder(&i.target)
                || i.input
                    .as_ref()
                    .is_some_and(|inp| inp.to_value().contains_domain_placeholder_deep())
                || i.path_vars
                    .as_ref()
                    .is_some_and(|m| m.values().any(Value::contains_domain_placeholder_deep))
        }
        Expr::Chain(ch) => {
            expr_contains_domain_placeholder(&ch.source)
                || matches!(
                    &ch.step,
                    ChainStep::Explicit { expr } if expr_contains_domain_placeholder(expr)
                )
        }
        Expr::Page(p) => p.handle.as_str() == "$",
        Expr::Wait(_) | Expr::Cancel(_) => false,
        Expr::TeachingValue { value } => value.contains_domain_placeholder_deep(),
    }
}

fn ref_contains_domain_placeholder(reference: &Ref) -> bool {
    reference.contains_domain_placeholder()
}

fn predicate_contains_domain_placeholder(predicate: &Predicate) -> bool {
    match predicate {
        Predicate::True | Predicate::False => false,
        Predicate::Comparison { value, .. } => value.to_value().contains_domain_placeholder_deep(),
        Predicate::And { args } | Predicate::Or { args } => {
            args.iter().any(predicate_contains_domain_placeholder)
        }
        Predicate::Not { predicate } => predicate_contains_domain_placeholder(predicate),
        Predicate::ExistsRelation { predicate, .. } => predicate
            .as_ref()
            .is_some_and(|pred| predicate_contains_domain_placeholder(pred)),
    }
}

fn placeholder_literal_error() -> TypeError {
    TypeError::DomainPlaceholderLiteral {
        field: "expression".to_string(),
        expected_type:
            "concrete ids and parameter values — replace every `$` from prompt examples before execution"
                .to_string(),
        description: None,
    }
}

fn strip_contract_hint_tail(text: &str) -> String {
    strip_prompt_expression_annotations(text)
}

fn output_contract_violation(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let stripped = strip_contract_hint_tail(trimmed);
    if stripped == trimmed {
        return None;
    }
    Some(format!(
        "Emit only executable Plasm syntax in `text`: remove any teaching-row hint tail such as `;; ...` or `=> ...`. Keep only `{}`.",
        stripped
    ))
}

/// Validate every step: parse then type-check. Returns all diagnostics if any step fails.
///
/// Before emitting a diagnostic, applies deterministic correction via the domain lexicon:
/// - If correction resolves uniquely → accept corrected expression silently.
/// - If correction is ambiguous → keep original error but enrich hints with candidates.
/// - If no correction possible → emit original diagnostic unchanged.
pub fn validate_plan_steps(
    cgs: &CGS,
    step_texts: &[String],
) -> Result<Vec<ParsedExpr>, Vec<StepDiagnostic>> {
    validate_plan_steps_with_lexicon(cgs, step_texts, &DomainLexicon::from_cgs(cgs))
}

/// Variant that accepts a pre-built lexicon (avoids rebuilding per call in hot paths).
pub fn validate_plan_steps_with_lexicon(
    cgs: &CGS,
    step_texts: &[String],
    lexicon: &DomainLexicon,
) -> Result<Vec<ParsedExpr>, Vec<StepDiagnostic>> {
    validate_plan_steps_with_lexicon_detailed(
        cgs,
        step_texts,
        lexicon,
        &PromptPipelineConfig::for_canonical_no_symbols(),
        None,
    )
    .0
}

/// Parse + type-check all steps; returns diagnostics (if any) and any deterministic rewrite notes.
///
/// Each step is parsed with in-grammar session [`SymbolMap`] resolution when symbol tuning is active
/// (same as REPL / execute ingress — no string pre-expansion before parse).
///
/// Notes are collected even when validation fails (e.g. step 0 lexicon-fixed, step 1 invalid).
///
/// `repl_focus_override`: when `Some`, wins over [`PromptPipelineConfig::focus`] (REPL `:schema` / NL focus).
pub fn validate_plan_steps_with_lexicon_detailed(
    cgs: &CGS,
    step_texts: &[String],
    lexicon: &DomainLexicon,
    pipeline: &PromptPipelineConfig,
    repl_focus_override: Option<&str>,
) -> (
    Result<Vec<ParsedExpr>, Vec<StepDiagnostic>>,
    Vec<StepLexiconNote>,
) {
    if step_texts.is_empty() {
        return (
            Err(vec![StepDiagnostic {
                step_index: 0,
                expression: String::new(),
                error: StepErrorPayload {
                    correction: append_correction_lines(
                        "TranslatePlan returned no expression (empty `text`).".into(),
                        vec!["Emit one non-empty Plasm path expression in `text`.".into()],
                    ),
                    category: "parse".into(),
                    span_offset: None,
                },
            }]),
            Vec::new(),
        );
    }

    let mut diags = Vec::new();
    let mut parsed_ok = Vec::new();
    let mut lexicon_notes = Vec::new();

    let symbol_map = pipeline.with_focus_spec(repl_focus_override, |focus| {
        symbol_map_for_prompt(cgs, focus, pipeline.uses_symbols())
    });

    for (i, text) in step_texts.iter().enumerate() {
        let feedback = match symbol_map.as_deref() {
            Some(m) => FeedbackStyle::SymbolicLlm { map: m },
            None => FeedbackStyle::CanonicalDev,
        };
        let t = text.trim();
        let contract_violation = output_contract_violation(t);
        let parsed = parse_session_line_with_rewrite_recovery(t, cgs, lexicon, symbol_map.clone());
        match parsed {
            Ok((mut p, resolved)) => {
                if let Some(res) = resolved {
                    lexicon_notes.push(StepLexiconNote {
                        step_index: i,
                        emitted: t.to_string(),
                        resolved_to: res,
                    });
                }
                if let Err(e) = normalize_expr_query_capabilities(&mut p.expr, cgs) {
                    diags.push(StepDiagnostic {
                        step_index: i,
                        expression: t.to_string(),
                        error: StepErrorPayload {
                            correction: render_query_resolve_error_for_feedback(
                                &e,
                                feedback.clone(),
                            ),
                            category: "resolve".into(),
                            span_offset: None,
                        },
                    });
                    continue;
                }
                match type_check_expr(&p.expr, cgs) {
                    Err(te) => {
                        let se = render_type_error_with_feedback(&te, cgs, feedback.clone());
                        diags.push(StepDiagnostic {
                            step_index: i,
                            expression: t.to_string(),
                            error: StepErrorPayload::from(&se),
                        });
                    }
                    Ok(()) => {
                        if expr_contains_domain_placeholder(&p.expr) {
                            let se = render_type_error_with_feedback(
                                &placeholder_literal_error(),
                                cgs,
                                feedback.clone(),
                            );
                            diags.push(StepDiagnostic {
                                step_index: i,
                                expression: t.to_string(),
                                error: StepErrorPayload::from(&se),
                            });
                        } else if let Some(correction) = contract_violation {
                            diags.push(StepDiagnostic {
                                step_index: i,
                                expression: strip_prompt_expression_annotations(t),
                                error: StepErrorPayload {
                                    correction,
                                    category: "parse".into(),
                                    span_offset: None,
                                },
                            });
                        } else {
                            parsed_ok.push(p)
                        }
                    }
                }
            }
            Err((pe, work, recovery_lines)) => {
                let mut se = render_parse_error_with_feedback(&pe, &work, t, cgs, feedback.clone());
                if !recovery_lines.is_empty() {
                    let head = match &feedback {
                        FeedbackStyle::CanonicalDev => work.as_str(),
                        FeedbackStyle::SymbolicLlm { .. } => t,
                    };
                    let merged = format_recovery_hints(&recovery_lines, feedback.clone());
                    se.correction = format!("{head}\n\n{}", merged);
                }
                diags.push(StepDiagnostic {
                    step_index: i,
                    expression: t.to_string(),
                    error: StepErrorPayload::from(&se),
                });
            }
        }
    }

    if diags.is_empty() {
        (Ok(parsed_ok), lexicon_notes)
    } else {
        (Err(diags), lexicon_notes)
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
            text: strip_contract_hint_tail(t),
            reasoning: r,
        })
        .collect();
    let diagnostics: Vec<StepDiagnostic> = diagnostics
        .iter()
        .cloned()
        .map(|mut d| {
            d.expression = strip_contract_hint_tail(&d.expression);
            d
        })
        .collect();
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
    use plasm_core::domain_lexicon::DomainLexicon;
    use plasm_core::loader::load_schema_dir;

    #[test]
    fn rejects_bad_field() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        // Unknown predicate keys are often dropped as narrative noise by lexicon recovery; use a
        // known field with a value outside the select domain so validation fails at type-check.
        let steps = vec!["Pet{status=not_a_valid_status}".to_string()];
        let err = validate_plan_steps(&cgs, &steps).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn accepts_valid_query() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let steps = vec!["Pet{status=available}".to_string()];
        let ok = validate_plan_steps(&cgs, &steps).unwrap();
        assert_eq!(ok.len(), 1);
    }

    #[test]
    fn detailed_reports_entity_case_lexicon_note() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let lexicon = DomainLexicon::from_cgs(&cgs);
        let steps = vec!["pet{status=available}".to_string()];
        let (res, notes) = validate_plan_steps_with_lexicon_detailed(
            &cgs,
            &steps,
            &lexicon,
            &PromptPipelineConfig::for_canonical_no_symbols(),
            None,
        );
        assert!(res.is_ok());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].emitted, "pet{status=available}");
        assert!(notes[0].resolved_to.starts_with("Pet{"));
    }

    #[test]
    fn rejects_domain_placeholder_literal_in_query_value() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let steps = vec!["Pet{status=$}".to_string()];
        let err = validate_plan_steps(&cgs, &steps).unwrap_err();
        assert!(!err.is_empty());
        assert!(
            err[0].error.correction.contains("teaching placeholder"),
            "placeholder rejection should use the explicit correction copy"
        );
    }

    #[test]
    fn rejects_prompt_annotation_suffix_even_when_expression_parses() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let steps = vec!["Pet{status=available}  ;;  => [Pet]  List pets.".to_string()];
        let err = validate_plan_steps(&cgs, &steps).unwrap_err();
        assert!(!err.is_empty());
        assert!(
            err[0]
                .error
                .correction
                .contains("Emit only executable Plasm syntax"),
            "gloss-copy should be reported as an output-contract failure"
        );
        assert_eq!(err[0].expression, "Pet{status=available}");
    }

    #[test]
    fn correction_feedback_strips_prompt_annotation_tails() {
        let diagnostics = vec![StepDiagnostic {
            step_index: 0,
            expression: "e13{p87=e16(p69=\"plasm\", p63=\"plasm\"), p80=12}  ;;  => [e13]  List reviews on a pull request.".into(),
            error: StepErrorPayload {
                correction: "Remove the hint tail.".into(),
                category: "parse".into(),
                span_offset: None,
            },
        }];
        let json = build_correction_feedback(
            "List reviews on PR plasm/plasm#12 before merge",
            0,
            &[(
                "e13{p87=e16(p69=\"plasm\", p63=\"plasm\"), p80=12}  ;;  => [e13]  List reviews on a pull request.",
                "copied the whole teaching row",
            )],
            &diagnostics,
        );
        assert!(
            json.contains("e13{p87=e16(p69=\\\"plasm\\\", p63=\\\"plasm\\\"), p80=12}"),
            "correction replay should preserve only the executable expression"
        );
        assert!(
            !json.contains("List reviews on a pull request."),
            "correction replay should not echo trailing hint prose back to the model"
        );
        assert!(
            !json.contains(";;"),
            "correction replay should strip compact gloss markers from prior text"
        );
    }
}
