//! Python expression coverage is specified by live semantic witnesses.
use plasm_agent::compilation_error::CompilationError;
use plasm_agent::program_diagnostic::ProgramStageError;
use plasm_agent::program_rejection::{
    PythonComputeError, PythonComputeRejection, PythonLoweringContext, PythonLoweringError,
    PythonProgramError, PythonSourceError,
};
use plasm_agent::python_compute::{InferenceError, InferenceGraphError};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize)]
enum ProjectionRejection {
    ReservedProjectionParameter,
    DictionaryKeyNotString,
    UnsupportedOperator,
    InvalidArgumentType,
    UnresolvedReference,
}

#[test]
fn projection_rejections_reject_unrelated_causes_and_unknown_labels() {
    let error = CompilationError::from(ProgramStageError::PythonLowering {
        error: PythonLoweringError::Program(PythonProgramError::ReservedProjectionParameter),
    });
    assert!(ProjectionRejection::ReservedProjectionParameter.matches(&error));
    for rejection in [
        ProjectionRejection::DictionaryKeyNotString,
        ProjectionRejection::UnsupportedOperator,
        ProjectionRejection::InvalidArgumentType,
        ProjectionRejection::UnresolvedReference,
    ] {
        assert!(!rejection.matches(&error));
    }
    assert!(
        serde_json::from_value::<ProjectionRejection>(serde_json::json!("UnknownCompilationError"))
            .is_err()
    );
    let source = PythonLoweringError::Source {
        error: std::sync::Arc::new(PythonSourceError::ExpectedStringLiteral),
        span: Some((1, 2)),
    };
    let uncontextualized = CompilationError::from(ProgramStageError::PythonLowering {
        error: source.clone(),
    });
    assert!(!ProjectionRejection::DictionaryKeyNotString.matches(&uncontextualized));
    let dictionary = CompilationError::from(ProgramStageError::PythonLowering {
        error: source.with_context(PythonLoweringContext::DictionaryKeyRequiresString),
    });
    assert!(ProjectionRejection::DictionaryKeyNotString.matches(&dictionary));
    let unrelated_contextualized = CompilationError::from(ProgramStageError::PythonLowering {
        error: PythonLoweringError::Program(PythonProgramError::ReservedProjectionParameter)
            .with_context(PythonLoweringContext::DictionaryKeyRequiresString),
    });
    assert!(!ProjectionRejection::DictionaryKeyNotString.matches(&unrelated_contextualized));
    let graph = CompilationError::from(ProgramStageError::PythonCompute {
        error: PythonComputeRejection::Inference(Box::new(InferenceError::Graph(
            InferenceGraphError::DictionaryKeyNotString,
        ))),
    });
    assert!(ProjectionRejection::DictionaryKeyNotString.matches(&graph));
}

impl ProjectionRejection {
    fn matches(self, error: &CompilationError) -> bool {
        let CompilationError::Program(stage) = error else {
            return false;
        };
        match stage.as_ref() {
            ProgramStageError::PythonLowering { error } => self.lowering(error),
            ProgramStageError::PythonCompute { error } => self.compute(error),
            ProgramStageError::PythonAnalysis { diagnostics, .. } => diagnostics
                .iter()
                .any(|d| self.code() == Some(d.code.as_str())),
            _ => false,
        }
    }

    fn code(self) -> Option<&'static str> {
        match self {
            Self::UnsupportedOperator => Some("unsupported-operator"),
            Self::InvalidArgumentType => Some("invalid-argument-type"),
            Self::UnresolvedReference => Some("unresolved-reference"),
            _ => None,
        }
    }

    fn lowering(self, error: &PythonLoweringError) -> bool {
        match error {
            PythonLoweringError::Located {
                error,
                context: Some(PythonLoweringContext::DictionaryKeyRequiresString),
                ..
            } if matches!(self, Self::DictionaryKeyNotString)
                && matches!(error.as_ref(), PythonLoweringError::Source { error, .. }
                    if matches!(error.as_ref(), PythonSourceError::ExpectedStringLiteral)) =>
            {
                true
            }
            PythonLoweringError::Located { error, .. } => self.lowering(error),
            PythonLoweringError::Program(error) => self.program(error),
            PythonLoweringError::Compute(error) => self.compute(error),
            PythonLoweringError::ComputeTyped(PythonComputeError::Inference(error)) => {
                self.inference(error)
            }
            _ => false,
        }
    }

    fn program(self, error: &PythonProgramError) -> bool {
        matches!(
            (self, error),
            (
                Self::ReservedProjectionParameter,
                PythonProgramError::ReservedProjectionParameter
            )
        )
    }

    fn compute(self, error: &PythonComputeRejection) -> bool {
        match error {
            PythonComputeRejection::Lowering(error) => self.lowering(error),
            PythonComputeRejection::Program(error) => self.program(error),
            PythonComputeRejection::Typed(PythonComputeError::Inference(error))
            | PythonComputeRejection::Inference(error) => self.inference(error),
            _ => false,
        }
    }

    fn inference(self, error: &InferenceError) -> bool {
        match error {
            InferenceError::Graph(InferenceGraphError::DictionaryKeyNotString) => {
                matches!(self, Self::DictionaryKeyNotString)
            }
            InferenceError::Diagnostics { diagnostics } => diagnostics
                .entries
                .iter()
                .any(|entry| self.code() == Some(entry.diagnostic.code.as_str())),
            _ => false,
        }
    }
}
#[tokio::test]
async fn projection_contracts_require_live_witnesses_and_rejections() {
    super::constructor_evidence::assert_expression_contracts_with_rejections(
        include_str!("../../../../doc-site/docs/reference/python-projection-constructors.md"),
        "plasm-projection-constructors",
        &[
            ("field", &["render_parity_lang_with_mul"]),
            ("literal", &["render_parity_lang_with_mul"]),
            (
                "arithmetic",
                &[
                    "render_parity_lang_with_mul",
                    "render_parity_lang_with_div",
                    "render_parity_lang_with_concat",
                ],
            ),
            ("length", &["render_parity_lang_with_when_len"]),
            ("conditional", &["render_parity_lang_with_when_len"]),
        ],
        |expected, error| {
            let expected: ProjectionRejection =
                serde_json::from_value(serde_json::Value::String(expected.into()))
                    .expect("unknown projection rejection label");
            expected.matches(error)
        },
    )
    .await;
}
