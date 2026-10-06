//! Row witnesses inspect typed IL; spelling alone is not execution evidence.
use plasm_agent::plasm_compile::{compile_python_program, PythonRowOperation};
use plasm_core::symbol_tuning::SymbolRender;
use serde::Deserialize;
use std::collections::BTreeSet;

#[tokio::test]
async fn production_row_dispatch_requires_typed_rules_and_negative_admission() {
    super::constructor_evidence::assert_registry(
        include_str!("../../../../doc-site/docs/reference/python-row-constructors.md"),
        "plasm-constructors",
        &PythonRowOperation::ALL
            .iter()
            .map(|op| op.name())
            .collect::<Vec<_>>(),
        |comp, _| row_operations(comp),
    )
    .await;
}

fn row_operations(comp: &plasm_core::plasm_monad::PlasmComp) -> BTreeSet<String> {
    use plasm_core::plasm_monad::{ComputeOp, PlasmStepPayload, ScopedOutput};
    let mut found = BTreeSet::new();
    for step in comp.steps.values() {
        let operation = match step {
            PlasmStepPayload::Map(p) => match &p.compute.op {
                ComputeOp::Project { .. } => Some("select"),
                ComputeOp::Filter { .. } => Some("where"),
                ComputeOp::GroupBy { .. } => Some("group_by"),
                ComputeOp::Aggregate { .. } => Some("aggregate"),
                ComputeOp::Sort { .. } => Some("order_by"),
                ComputeOp::Limit { .. } => Some("take"),
                ComputeOp::DedupeBy { .. } => Some("distinct"),
                ComputeOp::Union { .. } => Some("union"),
                _ => None,
            },
            PlasmStepPayload::Invoke(p) if p.page_size.is_some_and(|size| size > 0) => {
                Some("page_size")
            }
            PlasmStepPayload::UnfoldUntil(_) => Some("iterate"),
            PlasmStepPayload::MapBody(body) => {
                found.extend(row_operations(&body.body));
                match body.output {
                    ScopedOutput::Record => Some("map"),
                    ScopedOutput::Filter => Some("where"),
                    ScopedOutput::Quantify { .. } => None,
                    ScopedOutput::Rows { .. } => Some("flat_map"),
                }
            }
            _ => None,
        };
        if let Some(operation) = operation {
            found.insert(operation.into());
        }
    }
    found
}

pub(super) mod build {
    use super::*;
    use plasm_agent::compilation_error::CompilationError;
    use plasm_agent::plasm_compile::PythonBuildStatement;
    use plasm_agent::program_diagnostic::ProgramStageError;
    use plasm_agent::program_rejection::{
        PythonLoweringError, PythonProgramError, PythonSourceError,
    };

    #[derive(Clone, Copy, Debug, Deserialize)]
    enum BuildRejection {
        BuildExpression,
        UnusedNonWriteExpression,
        ReservedBindingName,
        MutableLocalAssignment,
        BuildAssignmentTargets,
        BuildAugmentedAssignment,
        StaticIterationRequiresLiteralIds,
        StaticIterationReturn,
        CallbackDeclarationShape,
        BranchingReturnUnsupported,
        EmptyReturn,
    }

    impl BuildRejection {
        fn matches(self, error: &CompilationError) -> bool {
            let CompilationError::Program(stage) = error else {
                return false;
            };
            let ProgramStageError::PythonLowering { error } = stage.as_ref() else {
                return false;
            };
            let mut cause = error;
            while let PythonLoweringError::Located { error, .. } = cause {
                cause = error.as_ref();
            }
            match cause {
                PythonLoweringError::Source { error, .. } => {
                    if let (
                        Self::ReservedBindingName,
                        PythonSourceError::ReservedBindingName { name },
                    ) = (self, error.as_ref())
                    {
                        return name == "Program";
                    }
                    matches!(
                        (self, error.as_ref()),
                        (Self::BuildExpression, PythonSourceError::BuildExpression)
                            | (
                                Self::UnusedNonWriteExpression,
                                PythonSourceError::UnusedNonWriteExpression
                            )
                            | (
                                Self::MutableLocalAssignment,
                                PythonSourceError::MutableLocalAssignment
                            )
                            | (
                                Self::BuildAssignmentTargets,
                                PythonSourceError::BuildAssignmentTargets { actual: 2 }
                            )
                            | (
                                Self::BuildAugmentedAssignment,
                                PythonSourceError::BuildAugmentedAssignment
                            )
                            | (
                                Self::StaticIterationRequiresLiteralIds,
                                PythonSourceError::StaticIterationRequiresLiteralIds
                            )
                            | (
                                Self::StaticIterationReturn,
                                PythonSourceError::StaticIterationReturn
                            )
                            | (
                                Self::CallbackDeclarationShape,
                                PythonSourceError::CallbackDeclarationShape {
                                    is_async: true,
                                    decorators: 0,
                                    has_type_parameters: false,
                                }
                            )
                    )
                }
                PythonLoweringError::Program(error) => matches!(
                    (self, error),
                    (
                        Self::BranchingReturnUnsupported,
                        PythonProgramError::BranchingReturnUnsupported
                    ) | (Self::EmptyReturn, PythonProgramError::EmptyReturn)
                ),
                _ => false,
            }
        }
    }
    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Rejection {
        body: String,
        error: BuildRejection,
    }
    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct BuildRule {
        operation: String,
        premise: String,
        transfer: String,
        law: String,
        witnesses: Vec<String>,
        invalid: Vec<Rejection>,
    }
    fn rules() -> Vec<BuildRule> {
        let doc = include_str!("../../../../doc-site/docs/reference/python-build-constructors.md");
        serde_json::from_str(
            doc.split("```plasm-build-constructors\n")
                .nth(1)
                .unwrap()
                .split("```")
                .next()
                .unwrap(),
        )
        .unwrap()
    }
    fn program(body: &str) -> String {
        if body.starts_with("class ") {
            return body.into();
        }
        format!(
            "class BuildCase(Program):\n    def build(self):\n{}\n",
            body.lines()
                .map(|line| format!("        {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
    #[derive(Debug)]
    pub(in super::super) enum RegistryError {
        DuplicateProduction,
        DuplicateRule { operation: String },
        MissingObligation { operation: String },
        DanglingWitness { id: String },
        NotPositiveEvidence { id: String },
        WitnessOmitsConstructor { id: String, operation: String },
        RelationWitness { id: String },
        InventoryMismatch,
        Contract(super::super::literate_contract::ContractError),
        Lowering(plasm_agent::program_rejection::PythonLoweringError),
    }
    impl std::fmt::Display for RegistryError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "constructor registry rejected: {self:?}")
        }
    }
    impl std::error::Error for RegistryError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                Self::Contract(source) => Some(source),
                Self::Lowering(source) => Some(source),
                _ => None,
            }
        }
    }
    impl From<super::super::literate_contract::ContractError> for RegistryError {
        fn from(source: super::super::literate_contract::ContractError) -> Self {
            Self::Contract(source)
        }
    }
    impl From<plasm_agent::program_rejection::PythonLoweringError> for RegistryError {
        fn from(source: plasm_agent::program_rejection::PythonLoweringError) -> Self {
            Self::Lowering(source)
        }
    }
    fn validate(rules: &[BuildRule]) -> Result<(), RegistryError> {
        let expected: BTreeSet<_> = PythonBuildStatement::ALL
            .iter()
            .map(|op| op.name())
            .collect();
        if expected.len() != PythonBuildStatement::ALL.len() {
            return Err(RegistryError::DuplicateProduction);
        }
        let laws = super::super::literate_contract::parse(include_str!(
            "../../../../doc-site/docs/reference/python-conformance.md"
        ))?;
        let mut actual = BTreeSet::new();
        for rule in rules {
            if !actual.insert(rule.operation.as_str()) {
                return Err(RegistryError::DuplicateRule {
                    operation: rule.operation.clone(),
                });
            }
            if rule.premise.trim().is_empty()
                || rule.transfer.trim().is_empty()
                || rule.witnesses.is_empty()
                || rule.invalid.is_empty()
                || rule.invalid.iter().any(|n| n.body.trim().is_empty())
                || !laws.contains_key(&rule.law)
            {
                return Err(RegistryError::MissingObligation {
                    operation: rule.operation.clone(),
                });
            }
            for id in &rule.witnesses {
                let case = super::super::python::cases()
                    .find(|case| case.id == id)
                    .ok_or_else(|| RegistryError::DanglingWitness { id: id.clone() })?;
                if case.expect_live_error.is_some() {
                    return Err(RegistryError::NotPositiveEvidence { id: id.clone() });
                }
                if !PythonBuildStatement::inventory(&program(case.python))?
                    .iter()
                    .any(|kind| kind.name() == rule.operation)
                {
                    return Err(RegistryError::WitnessOmitsConstructor {
                        id: id.clone(),
                        operation: rule.operation.clone(),
                    });
                }
            }
        }
        if actual != expected {
            return Err(RegistryError::InventoryMismatch);
        }
        Ok(())
    }
    #[tokio::test]
    async fn production_build_dispatch_requires_rules_and_negative_admission() {
        let rules = rules();
        validate(&rules).unwrap();
        let es = super::super::language_matrix::matrix_execute_session(
            super::super::language_matrix::load_language_matrix_cgs(),
        );
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity =
            symbols.entity_sym_for(super::super::language_matrix::MATRIX_ENTRY_ID, "LangItem");
        for rule in rules {
            for rejection in rule.invalid {
                let source = program(&rejection.body.replace("E.", &format!("{entity}.")));
                let error = compile_python_program(&es, &source)
                    .await
                    .expect_err("invalid build admitted");
                assert!(
                    rejection.error.matches(&error),
                    "{} expected {:?}, got {error:?}",
                    rule.operation,
                    rejection.error
                );
            }
        }
    }
    #[tokio::test]
    async fn documentation_erasure_preserves_the_semantic_plan() {
        let es = super::super::language_matrix::matrix_execute_session(
            super::super::language_matrix::load_language_matrix_cgs(),
        );
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity =
            symbols.entity_sym_for(super::super::language_matrix::MATRIX_ENTRY_ID, "LangItem");
        let body = format!("rows = {entity}.get('i1')\nreturn rows");
        let plain = compile_python_program(&es, &program(&body)).await.unwrap();
        let documented = compile_python_program(&es, &program(&format!("'documentation'\n{body}")))
            .await
            .unwrap();
        assert!(plasm_core::plasm_monad::comp_semantic_eq(
            &plain.artifact().comp,
            &documented.artifact().comp
        ));
    }

    #[test]
    fn build_inventory_rejects_missing_rules_and_false_witnesses() {
        let all = rules();
        validate(&all).unwrap();
        let mut without_finite_for = all.clone();
        without_finite_for.retain(|rule| rule.operation != "finite_for");
        assert!(matches!(
            validate(&without_finite_for),
            Err(RegistryError::InventoryMismatch)
        ));
        for index in 0..all.len() {
            let mut changed = all.clone();
            changed.remove(index);
            assert!(validate(&changed).is_err());
            let mut changed = all.clone();
            changed[index].witnesses = vec!["missing".into()];
            assert!(validate(&changed).is_err());
            let mut changed = all.clone();
            changed[index].invalid.clear();
            assert!(validate(&changed).is_err());
        }
        let mut changed = all.clone();
        changed.push(all[0].clone());
        assert!(validate(&changed).is_err());
        let mut changed = all.clone();
        changed[0].witnesses = vec!["get".into()];
        assert!(validate(&changed).is_err());
        let mut changed = all;
        changed[0].operation = "unknown".into();
        assert!(validate(&changed).is_err());
    }
}
