//! Root/class rules are checked before lowering any executable program body.
use plasm_agent::compilation_error::CompilationError;
use plasm_agent::plasm_compile::{compile_python_program, PythonDeclaration};
use plasm_agent::program_diagnostic::ProgramStageError;
use plasm_agent::program_rejection::{
    AnalysisError, PythonComputeError, PythonComputeRejection, PythonLoweringError,
    PythonLoweringInvariantError, PythonProgramError, PythonSourceError,
};
use plasm_agent::python_compute::InferenceError;
use plasm_core::symbol_tuning::SymbolRender;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize)]
enum DeclarationRejection {
    ProgramDeclarationCount,
    ProgramBaseShape,
    ProgramBaseInvalid,
    ReservedRootName,
    RootShadowsEntity,
    ClassExecutableState,
    ProgramBuildMissing,
    DuplicateBuildMethod,
    MethodDeclarationShape,
    BuildInterfaceShape,
    BuildReceiverShape,
    VariadicBuildInputs,
    ArgumentBinding,
    ComputeDecoratorShape,
    ComputeRowsHandleAnnotation,
    InvalidReturnType,
    UnresolvedAttribute,
    DuplicateComputeMethod,
    RecursiveDagMethod,
}

impl DeclarationRejection {
    fn matches(self, error: &CompilationError) -> bool {
        let CompilationError::Program(stage) = error else {
            return false;
        };
        match stage.as_ref() {
            ProgramStageError::PythonLowering { error } => self.matches_lowering(error),
            ProgramStageError::PythonCompute { error } => self.matches_compute(error),
            _ => false,
        }
    }

    fn matches_lowering(self, error: &PythonLoweringError) -> bool {
        match error {
            PythonLoweringError::Located { error, .. } => self.matches_lowering(error),
            PythonLoweringError::Program(error) => matches!(
                (self, error),
                (
                    Self::ProgramDeclarationCount,
                    PythonProgramError::ProgramDeclarationCount
                ) | (
                    Self::ProgramBaseInvalid,
                    PythonProgramError::ProgramBaseInvalid
                )
            ),
            PythonLoweringError::Internal(error) => matches!(
                (self, error),
                (
                    Self::ProgramBuildMissing,
                    PythonLoweringInvariantError::ProgramBuildMissing
                )
            ),
            PythonLoweringError::Source { error, .. } => matches!(
                (self, error.as_ref()),
                (Self::ProgramBaseShape, PythonSourceError::ProgramBaseShape)
                    | (
                        Self::ReservedRootName,
                        PythonSourceError::ReservedRootName { .. }
                    )
                    | (
                        Self::RootShadowsEntity,
                        PythonSourceError::RootShadowsEntity { .. }
                    )
                    | (
                        Self::ClassExecutableState,
                        PythonSourceError::ClassExecutableState
                    )
                    | (
                        Self::DuplicateBuildMethod,
                        PythonSourceError::DuplicateBuildMethod
                    )
                    | (
                        Self::MethodDeclarationShape,
                        PythonSourceError::MethodDeclarationShape { .. }
                    )
                    | (
                        Self::BuildInterfaceShape,
                        PythonSourceError::BuildInterfaceShape
                    )
                    | (
                        Self::BuildReceiverShape,
                        PythonSourceError::BuildReceiverShape
                    )
                    | (
                        Self::VariadicBuildInputs,
                        PythonSourceError::VariadicBuildInputs
                    )
                    | (
                        Self::ComputeDecoratorShape,
                        PythonSourceError::ComputeDecoratorShape
                    )
                    | (
                        Self::ComputeRowsHandleAnnotation,
                        PythonSourceError::ComputeRowsHandleAnnotation
                    )
                    | (
                        Self::DuplicateComputeMethod,
                        PythonSourceError::DuplicateComputeMethod { .. }
                    )
                    | (
                        Self::RecursiveDagMethod,
                        PythonSourceError::RecursiveDagMethod { .. }
                    )
            ),
            PythonLoweringError::Compute(error) => self.matches_compute(error),
            PythonLoweringError::Analysis(error) => matches!(
                (self, error.as_ref()),
                (Self::ArgumentBinding, AnalysisError::ArgumentBinding { .. })
            ),
            PythonLoweringError::ComputeTyped(PythonComputeError::Inference(error)) => {
                self.matches_inference(error)
            }
            _ => false,
        }
    }

    fn matches_compute(self, error: &PythonComputeRejection) -> bool {
        match error {
            PythonComputeRejection::Lowering(error) => self.matches_lowering(error),
            PythonComputeRejection::Typed(PythonComputeError::Inference(error))
            | PythonComputeRejection::Inference(error) => self.matches_inference(error),
            _ => false,
        }
    }

    fn matches_inference(self, error: &InferenceError) -> bool {
        let code = match self {
            Self::InvalidReturnType => "invalid-return-type",
            Self::UnresolvedAttribute => "unresolved-attribute",
            _ => return false,
        };
        matches!(error, InferenceError::Diagnostics { diagnostics }
            if diagnostics.entries.iter().any(|entry| entry.diagnostic.code == code))
    }
}

#[test]
fn declaration_rejections_do_not_accept_unrelated_analysis_faults() {
    let error = CompilationError::from(ProgramStageError::PythonLowering {
        error: PythonLoweringError::Analysis(std::sync::Arc::new(
            AnalysisError::InvalidFunctionSpan,
        )),
    });
    assert!(!DeclarationRejection::ArgumentBinding.matches(&error));
    assert!(
        serde_json::from_value::<DeclarationRejection>(serde_json::json!("UnknownAnalysisFault"))
            .is_err()
    );
}

#[tokio::test]
async fn declarations_require_live_witnesses_and_full_module_rejections() {
    super::constructor_evidence::assert_registry_with_rejections(
        include_str!("../../../../doc-site/docs/reference/python-declaration-constructors.md"),
        "plasm-declaration-constructors",
        &PythonDeclaration::ALL
            .iter()
            .map(|kind| kind.name())
            .collect::<Vec<_>>(),
        |_, source| {
            PythonDeclaration::inventory(source)
                .unwrap()
                .iter()
                .map(|kind| kind.name().to_owned())
                .collect()
        },
        |expected, error| {
            let expected: DeclarationRejection =
                serde_json::from_value(serde_json::Value::String(expected.into()))
                    .expect("unknown declaration rejection label");
            expected.matches(error)
        },
    )
    .await;
}
#[tokio::test]
async fn class_documentation_erasure_preserves_the_live_plan() {
    use super::{language_matrix as matrix, python};
    let case = python::cases()
        .find(|case| case.id == "root_build_statements")
        .unwrap();
    let (es, _) = python::parity_context(case, "http://127.0.0.1:1");
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = python::program(
        &python::write_tokens(case.python, &symbols, matrix::MATRIX_ENTRY_ID),
        &entity,
    );
    let comment = "    \"class documentation\"\n";
    assert!(source.contains(comment));
    let documented = compile_python_program(&es, &source).await.unwrap();
    let plain = compile_python_program(&es, &source.replace(comment, ""))
        .await
        .unwrap();
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &documented.artifact().comp,
        &plain.artifact().comp
    ));
}
