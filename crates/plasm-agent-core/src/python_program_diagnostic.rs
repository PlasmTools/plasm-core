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
    fn session() -> ExecuteSession {
        let cgs = std::sync::Arc::new(
            plasm_core::load_schema(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
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
    async fn static_admission_does_not_require_a_worker() {
        let session = session();
        let source = "class Read(Program):\n    @compute\n    def count(self, rows: list[Row]) -> int:\n        return len(rows)\n    def build(self):\n        return self.count(e1.query())\n";
        crate::compile_program(&Default::default(), None, &session, "test", source)
            .await
            .unwrap();
    }
}
