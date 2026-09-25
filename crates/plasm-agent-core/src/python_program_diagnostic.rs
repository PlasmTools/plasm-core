//! Python source admission with the shared correction/replay envelope.
use crate::{
    execute_session::ExecuteSession, plasm_comp_bundle::PlasmCompBundle,
    program_diagnostic::ProgramStageError,
};

pub(crate) fn compile(
    session: &ExecuteSession,
    source: &str,
) -> Result<PlasmCompBundle, ProgramStageError> {
    crate::plasm_dag::compile_python_program_checked(session, source).map_err(|stage| match stage {
        ProgramStageError::Type { correction } => ProgramStageError::Type {
            correction: format!("{correction}\nRepair the Python Program using the current class and method declarations. Preserve selection criteria, domain types and completed-write evidence. Reuse the same logical session; obtain missing symbols through context extension."),
        },
        other => other,
    })
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

pub(crate) fn type_correction(session: &ExecuteSession, error: &plasm_core::TypeError) -> String {
    use plasm_core::TypeError;
    match error {
        TypeError::FieldNotFound { field, entity } => {
            let fields = session.contexts_by_entry.values().filter_map(|context| context.cgs.get_entity(entity)).flat_map(|entity| entity.fields.keys().map(ToString::to_string)).collect::<std::collections::BTreeSet<_>>();
            format!("Unknown field {field:?}. Use a declared row attribute or quoted projection name. Available fields: {}. Preserve the selection when repairing rows.where(lambda row: row.field == value).", fields.into_iter().collect::<Vec<_>>().join(", "))
        }
        TypeError::RelationNotFound { relation, .. } => format!("Unknown relation {relation:?}. Use a declared r# on a proven singleton; traverse plural rows with rows.flat_map(lambda row: row.rN). Obtain missing relation declarations by extending this session."),
        TypeError::EntityNotFound { .. } | TypeError::CapabilityNotFound { .. } => "Use an exposed e# class and a method declared on it. Extend this logical session for missing capabilities; do not invent names or open a replacement session.".into(),
        TypeError::RequiredParameterOmitted { parameter, .. } => format!("Supply the required keyword {parameter}=value on the declared Python method. The catalog has no default; use the intended literal or a typed singleton field and preserve its domain."),
        TypeError::InputRequired { capability } => format!("Capability {capability:?} requires input. Fill the named arguments from its Python declaration; optional omission does not permit None."),
        TypeError::RefKeyMismatch { message, .. } => format!("Get identity mismatch: {message}. Follow the declared eN.get signature: positional identity for a simple key, named arguments for a compound key."),
        TypeError::DomainPlaceholderLiteral { field, expected_type, .. } => format!("Replace the placeholder for {field:?} with an actual {expected_type} value or compatible typed binding."),
        TypeError::RecursiveError { relation, source } => format!("Relation {relation:?}: {}", type_correction(session, source)),
        TypeError::ChainTargetMissingGet { target_entity, .. } => format!("The relation target {target_entity:?} has no Get capability. Use its declared materialized relation/source; a reference value alone grants no read authority."),
        TypeError::IncompatibleOperator { .. } | TypeError::IncompatibleValue { .. } | TypeError::CrossCurrencyCompare { .. } => format!("{error}. Preserve the declared domain and use a compatible Python comparison; do not cast away constraints."),
        TypeError::RowsetNormalize { message } => format!("Source input contract: {message}. Use the exact Python source-method keywords and separate backend selection from rows.where predicates."),
    }
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
    #[test]
    fn python_cutover_rejects_native_source_and_admits_corrected_program() {
        let session = session();
        let rejected = crate::compile_program(
            &Default::default(),
            None,
            &session,
            "test",
            "items = e1\nitems",
        )
        .unwrap_err();
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
        crate::compile_program(&Default::default(), None, &session, "test", correct).unwrap();
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
        let session = session();
        let message = type_correction(
            &session,
            &plasm_core::TypeError::RequiredParameterOmitted {
                parameter: "item_id".into(),
                expression: "e2{item_id=$}".into(),
            },
        );
        assert!(message.contains("item_id=value"));
        assert!(!message.contains("e2{"));
    }
}
