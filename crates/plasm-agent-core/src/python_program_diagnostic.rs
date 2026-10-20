//! Python source admission with the shared correction/replay envelope.
use crate::{
    execute_session::ExecuteSession, plasm_comp_bundle::PlasmCompBundle,
    program_diagnostic::ProgramStageError,
};

pub(crate) fn compile(
    session: &ExecuteSession,
    source: &str,
) -> Result<PlasmCompBundle, ProgramStageError> {
    crate::plasm_dag::compile_python_program_checked(session, source)
}

pub(crate) fn admission_error(
    error: impl Into<crate::program_rejection::PythonComputeRejection>,
) -> ProgramStageError {
    ProgramStageError::PythonCompute {
        error: error.into(),
    }
}

/// Render the authoritative parser location, with a bounded Unicode-safe excerpt.
/// No delimiter blacklist: comments and string literals are owned by Python.
pub(crate) fn syntax_correction(source: &str, error: &ruff_python_parser::ParseError) -> String {
    let offset = usize::from(error.location.start()).min(source.len());
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..offset].chars().count();
    let window_start = column.saturating_sub(60);
    let excerpt: String = source[line_start..]
        .split('\n')
        .next()
        .unwrap_or("")
        .chars()
        .skip(window_start)
        .take(120)
        .collect();
    format!("Python syntax at line {line}, column {}:\n{excerpt}\n{}^\n{error}\nSubmit a complete valid Python program. No program prefix was executed.", column + 1, " ".repeat(column - window_start))
}

/// Parser recovery is evidence of syntax only, never evidence that a prefix ran.
pub(crate) fn understood_prefix(
    source: &str,
) -> Option<crate::program_diagnostic::UnderstoodSketch> {
    use ruff_python_ast::{Expr, PySourceType, Stmt};
    use ruff_text_size::Ranged;
    let parsed = ruff_python_parser::parse_unchecked_source(source, PySourceType::Python);
    let boundary = parsed.errors().first().map(|e| e.location.start());
    let class = parsed.suite().iter().find_map(|s| {
        if let Stmt::ClassDef(c) = s {
            Some(c)
        } else {
            None
        }
    })?;
    let build = class.body.iter().find_map(|s| {
        if let Stmt::FunctionDef(f) = s {
            (f.name.as_str() == "build").then_some(f)
        } else {
            None
        }
    })?;
    let mut bindings = Vec::new();
    let mut ok_count = 0;
    let mut failed_at = None;
    for statement in &build.body {
        if boundary.is_some_and(|end| statement.end() > end) {
            failed_at = Some(format!("byte {}", u32::from(statement.start())));
            break;
        }
        if let Stmt::Assign(assign) = statement {
            for target in &assign.targets {
                if let Expr::Name(name) = target {
                    bindings.push(name.id.to_string());
                }
            }
        }
        ok_count += 1;
    }
    Some(crate::program_diagnostic::UnderstoodSketch {
        bindings, failed_at, ok_count, total: build.body.len().max(1),
        sketch: Some(format!("Parsed {ok_count} build statements before the syntax error; none were executed or type-checked.")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program_diagnostic::{ProgramDiagnostic, ProgramErrorCategory};
    #[test]
    fn syntax_repair_uses_parser_location_without_rejecting_literal_markup() {
        let valid = "# </parameter>\ntext = '</parameter>'\nname = 'café'\n";
        assert!(ruff_python_parser::parse_module(valid).is_ok());
        let invalid = format!("{valid}</parameter\n");
        let error = ruff_python_parser::parse_module(&invalid).unwrap_err();
        let diagnostic = syntax_correction(&invalid, &error);
        assert!(diagnostic.contains("line 4, column 1"), "{diagnostic}");
        assert!(diagnostic.contains("</parameter\n^"), "{diagnostic}");
        let failure: plasm_runtime::ExecutionFailure = ProgramStageError::Parse {
            correction: diagnostic,
            span_offset: Some(u32::from(error.location.start()) as usize),
            error: std::sync::Arc::new(crate::program_diagnostic::ProgramParseError::Python(error)),
        }
        .into();
        assert_eq!(failure.code, "program_syntax");
    }
    fn session() -> ExecuteSession {
        let cgs = std::sync::Arc::new(
            plasm_core::load_schema(
                &std::path::Path::new(crate::test_manifest_dir())
                    .join("../../fixtures/schemas/python_dag_slice"),
            )
            .unwrap(),
        );
        let exposure = plasm_core::TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
        let mut session = crate::test_support::session_fixtures::ExecuteSessionFixture::new()
            .entry_id("fixture")
            .entities(vec!["Item".into(), "Tag".into()])
            .build(cgs);
        session.teaching_exposure = Some(exposure);
        session
    }

    #[tokio::test]
    async fn unmaterializable_compute_keeps_typed_evidence_in_tool_failure() {
        let session = session();
        let source = "from typing import Any, cast\nclass Read(Program):\n    @compute\n    def result(self, rows):\n        return cast(Any, len(rows))\n    def build(self):\n        return self.result(e1.query())\n";
        let error = crate::compile_program(&Default::default(), None, &session, "test", source)
            .await
            .unwrap_err();
        let failure: plasm_runtime::ExecutionFailure = error.into();
        let evidence = failure.program_rejection.as_ref().unwrap();
        let Some(plasm_runtime::ProgramRejectionDetail::ComputeInference { diagnostics }) =
            &evidence.detail
        else {
            panic!("{failure:?}");
        };
        assert!(diagnostics.iter().any(|entry| entry.compute_line.is_some()
            && entry
                .source_excerpt
                .as_ref()
                .is_some_and(|text| text.contains("cast(Any, len(rows))"))));
        let wire = serde_json::to_value(&failure).unwrap();
        let restored: plasm_runtime::ExecutionFailure = serde_json::from_value(wire).unwrap();
        assert_eq!(restored.program_rejection, failure.program_rejection);
    }
    #[tokio::test]
    async fn get_identity_does_not_satisfy_required_keyword_input() {
        let cgs = std::sync::Arc::new(
            plasm_core::load_schema(
                &std::path::Path::new(crate::test_manifest_dir())
                    .join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .unwrap(),
        );
        let exposure =
            plasm_core::TeachingExposureSession::new(&cgs, "fixture", &["LangSecuredNote"]);
        let symbol = exposure
            .to_symbol_map()
            .entity_sym_for("fixture", "LangSecuredNote");
        let mut session = crate::test_support::session_fixtures::ExecuteSessionFixture::new()
            .entry_id("fixture")
            .entities(vec!["LangSecuredNote".into()])
            .build(cgs);
        session.teaching_exposure = Some(exposure);
        let source =
            format!("class Read(Program):\n    def build(self):\n        return {symbol}.get(1)\n");
        let error = compile(&session, &source).unwrap_err();
        assert!(error.span_offset().is_some());
        let text = error.correction();
        assert!(text.contains("access_token=value"), "{text}");
        assert!(!text.contains("input object"), "{text}");
        compile(
            &session,
            &source.replace(".get(1)", ".get(1, access_token='token')"),
        )
        .unwrap();
    }
    #[tokio::test]
    async fn compute_mutator_reports_effect_boundary_before_value_typing() {
        let session = session();
        let symbols = session.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let mark = symbols.method_sym_for("fixture", "Item", "mark");
        let source = format!("class Mark(Program):\n    @compute\n    def render(self, rows: list[Row]) -> int:\n        for row in rows:\n            row.{mark}()\n        return len(rows)\n    def build(self):\n        return self.render(e1.query())\n");
        let error = crate::compile_program(&Default::default(), None, &session, "test", &source)
            .await
            .unwrap_err()
            .into_program()
            .expect("program-owned failure");
        assert_eq!(error.category(), ProgramErrorCategory::Type);
        assert!(
            error.correction().contains("@compute is pure"),
            "{}",
            error.correction()
        );
        assert!(
            error.correction().contains("flat_map"),
            "{}",
            error.correction()
        );
        assert!(!error.correction().contains("unresolved-attribute"));
    }
    #[tokio::test]
    async fn compute_relation_reports_materialization_boundary() {
        let session = session();
        let symbols = session.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let tags = symbols.ident_sym_relation_for("fixture", "Item", "tags");
        let source = format!("class Read(Program):\n    @compute\n    def collect(self, row: Row[e1]) -> int:\n        return len(row.{tags})\n    def build(self):\n        return self.collect(e1.get('i1'))\n");
        let error = crate::compile_program(&Default::default(), None, &session, "test", &source)
            .await
            .unwrap_err()
            .into_program()
            .expect("program-owned failure");
        assert_eq!(error.category(), ProgramErrorCategory::Type);
        let correction = error.correction();
        assert!(correction.contains("materialized"), "{correction}");
        assert!(correction.contains("build"), "{correction}");
        assert!(!correction.contains("unresolved-attribute"), "{correction}");
        assert!(
            !correction.contains("obtain missing symbols"),
            "{correction}"
        );
    }
    #[tokio::test]
    async fn compute_dag_handle_annotations_point_to_call_site_inference() {
        let session = session();
        for annotation in ["Rows[e1]", "Singleton[e1]", "list[e1]"] {
            let source = format!("class Read(Program):\n    @compute\n    def count(self, rows: {annotation}) -> int:\n        return len(rows)\n    def build(self):\n        return self.count(e1.query())\n");
            let error =
                crate::compile_program(&Default::default(), None, &session, "test", &source)
                    .await
                    .unwrap_err()
                    .into_program()
                    .expect("program-owned failure");
            assert!(matches!(&error, ProgramStageError::PythonLowering { .. }));
            assert!(error.span_offset().is_some());
            assert!(
                error.correction().contains("omit the annotation"),
                "{annotation}: {}",
                error.correction()
            );
            assert!(!error.correction().contains("exactly one value column"));
        }
    }
    #[tokio::test]
    async fn upstream_correction_preserves_original_body_line_without_unrelated_repair() {
        let session = session();
        let source = "class Read(Program):\n    @compute\n    def render(self, rows: list[Row]) -> str:\n        return rows[0].missing\n    def build(self):\n        rows = e1.query()\n        return self.render(rows)\n";
        let error = crate::compile_program(&Default::default(), None, &session, "test", source)
            .await
            .unwrap_err()
            .into_program()
            .expect("program-owned failure");
        assert_eq!(error.category(), ProgramErrorCategory::Type);
        assert!(
            matches!(
                &error,
                ProgramStageError::PythonLowering {
                    error: crate::program_rejection::PythonLoweringError::Compute(_)
                }
            ),
            "{error:?}"
        );
        let correction = error.correction();
        assert!(correction.contains("plasm_compute.py:4:"), "{correction}");
        assert!(correction.contains("missing"), "{correction}");
        assert!(
            !correction.contains("Reuse the same logical session"),
            "{correction}"
        );
        for prefix in [
            "import datetime as dt\n",
            "from datetime import date\nfrom datetime import timedelta\n",
        ] {
            let source = format!("{prefix}{source}");
            let error =
                crate::compile_program(&Default::default(), None, &session, "test", &source)
                    .await
                    .unwrap_err()
                    .into_program()
                    .expect("program-owned failure");
            let line = 4 + prefix.lines().count();
            assert!(
                error
                    .correction()
                    .contains(&format!("plasm_compute.py:{line}:")),
                "{}",
                error.correction()
            );
        }
    }
    #[tokio::test]
    async fn python_cutover_rejects_native_source_and_admits_corrected_program() {
        let session = session();
        let rejected = crate::compile_program(
            &Default::default(),
            None,
            &session,
            "test",
            "items = e1\nitems",
        )
        .await
        .unwrap_err()
        .into_program()
        .expect("program-owned failure");
        assert_eq!(rejected.category(), ProgramErrorCategory::Type);
        assert!(rejected.correction().contains("Program subclass"));
        assert!(!rejected.correction().contains("| where"));
        let first = ProgramDiagnostic::from_stage(
            &Default::default(),
            None,
            &session,
            "items = e1\nitems",
            rejected.clone(),
        );
        assert!(first.replay.is_none());
        let repeat = ProgramDiagnostic::from_stage(
            &Default::default(),
            None,
            &session,
            "items = e1\nitems",
            rejected,
        );
        assert!(repeat.replay.is_some());
        let correct = "class Read(Program):\n    def build(self):\n        rows = e1.query()\n        return rows\n";
        crate::compile_program(&Default::default(), None, &session, "test", correct)
            .await
            .unwrap();
    }
    #[test]
    fn python_cutover_syntax_error_has_position_and_recovered_bindings() {
        let session = session();
        let source = "class Read(Program):\n    def build(self):\n        rows = e1.query()\n        return (\n";
        let error = compile(&session, source).unwrap_err();
        assert_eq!(error.category(), ProgramErrorCategory::Parse);
        assert!(error.span_offset().is_some());
        let diagnostic =
            ProgramDiagnostic::from_stage(&Default::default(), None, &session, source, error);
        assert!(diagnostic
            .understood
            .unwrap()
            .bindings
            .contains(&"rows".into()));
    }
    #[test]
    fn python_cutover_parameter_correction_does_not_echo_native_expression() {
        let message = plasm_core::TypeError::RequiredParameterOmitted {
            parameter: "item_id".into(),
            expression: "e2{item_id=$}".into(),
        }
        .python_correction();
        assert!(message.contains("item_id=value"));
        assert!(!message.contains("e2{"));
    }
    #[tokio::test]
    async fn reserved_binding_has_structured_rejection_evidence() {
        let session = session();
        let source =
            "class P(Program):\n    def build(self):\n        e1 = e2.query()\n        return e1\n";
        let error = crate::compile_program(&Default::default(), None, &session, "test", source)
            .await
            .unwrap_err();
        let failure: plasm_runtime::ExecutionFailure = error.into();
        assert_eq!(
            failure.program_rejection.as_ref().unwrap().detail,
            Some(plasm_runtime::ProgramRejectionDetail::ReservedBinding { name: "e1".into() })
        );
    }

    #[tokio::test]
    async fn static_admission_does_not_require_a_worker() {
        let session = session();
        let source = "class Read(Program):\n    @compute\n    def count(self, rows: list[Row]) -> int:\n        return len(rows)\n    def build(self):\n        return self.count(e1.query())\n";
        crate::compile_program(&Default::default(), None, &session, "test", source)
            .await
            .unwrap();
    }
}
