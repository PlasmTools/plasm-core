//! Finite, grammar-directed lowering properties. No Python evaluator is replicated.
use plasm_agent::plasm_compile::compile_python_program;
use plasm_core::symbol_tuning::SymbolRender;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Property {
    GetIdentitySpelling,
    InvalidGetBinding,
    ReadValueAlgebra,
    ReductionAlgebra,
    PrimitiveReductions,
    TypedReturns,
    OuterValues,
    MutationArrayEffects,
    CollectionBoundaries,
    RecursiveValueBoundary,
}

impl Property {
    pub fn role(self) -> super::literate_contract::Role {
        match self {
            Self::GetIdentitySpelling => super::literate_contract::Role::Metamorphic,
            Self::InvalidGetBinding => super::literate_contract::Role::NegativeAdmission,
            Self::RecursiveValueBoundary
            | Self::OuterValues
            | Self::TypedReturns
            | Self::PrimitiveReductions
            | Self::ReductionAlgebra
            | Self::ReadValueAlgebra
            | Self::MutationArrayEffects
            | Self::CollectionBoundaries => super::literate_contract::Role::RuntimeEvidence,
        }
    }
}

pub(super) const PROPERTIES: &[Property] = &[
    Property::GetIdentitySpelling,
    Property::InvalidGetBinding,
    Property::ReadValueAlgebra,
    Property::ReductionAlgebra,
    Property::PrimitiveReductions,
    Property::TypedReturns,
    Property::OuterValues,
    Property::MutationArrayEffects,
    Property::CollectionBoundaries,
    Property::RecursiveValueBoundary,
];

/// These contexts preserve the Get's entity authority and do not introduce effects.
fn contexts(call: &str) -> Vec<String> {
    vec![
        format!("return {call}"),
        format!("return {call}.select(\"id\")"),
        format!("row = {call}\nreturn E.get(row.id)"),
        format!("return E.query().take(1).flat_map(lambda parent: {call}, max_parents=1)"),
        format!("return E.query().take(1).flat_map(lambda parent: E.query().take(1).flat_map(lambda child: {call}, max_parents=1), max_parents=1)"),
    ]
}

fn program(body: &str, entity: &str) -> String {
    format!(
        "class ContractProgram(Program):\n    def build(self):\n{}\n",
        body.replace("E.", &format!("{entity}."))
            .lines()
            .map(|line| format!("        {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

#[tokio::test]
async fn registered_lowering_properties() {
    let es = super::language_matrix::matrix_execute_session(
        super::language_matrix::load_language_matrix_cgs(),
    );
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(super::language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for property in PROPERTIES {
        let mut count = 0;
        match property {
            Property::OuterValues => {
                count = super::outer_values::run().await;
                println!("{property:?}: {count} outer transfer cases");
                continue;
            }
            Property::TypedReturns => {
                count = super::typed_returns::run().await;
                println!("{property:?}: {count} typed output cases");
                continue;
            }
            Property::RecursiveValueBoundary => {
                count = super::recursive_values::run().await;
                println!("{property:?}: {count} recursive value/presence cases");
                continue;
            }
            Property::CollectionBoundaries => {
                count = super::collection_boundaries::run().await;
                println!("{property:?}: {count} pagination/embedding scenarios");
                continue;
            }
            Property::MutationArrayEffects => {
                count = super::effect_boundaries::run().await;
                println!("{property:?}: {count} independent effect scenarios");
                continue;
            }
            Property::PrimitiveReductions => {
                count = super::primitive_reductions::run().await;
                println!("{property:?}: {count} primitive endpoint/consumer cases");
                continue;
            }
            Property::ReductionAlgebra => {
                count = super::reference_algebra::run_reductions().await;
                println!("{property:?}: {count} independent reduction cases");
                continue;
            }
            Property::ReadValueAlgebra => {
                count = super::reference_algebra::run().await;
                println!("{property:?}: {count} independent differential cases");
                continue;
            }
            Property::GetIdentitySpelling => {
                for identity in [
                    "\"i1\"",
                    "True",
                    "False",
                    "0",
                    "-1",
                    "9223372036854775807",
                    "-9223372036854775808",
                ] {
                    for (left, right) in contexts(&format!("E.get({identity})"))
                        .into_iter()
                        .zip(contexts(&format!("E.get(identity={identity})")))
                    {
                        let a = compile_python_program(&es, &program(&left, &entity))
                            .await
                            .unwrap_or_else(|e| panic!("{property:?}: {left}: {e}"));
                        let b = compile_python_program(&es, &program(&right, &entity))
                            .await
                            .unwrap_or_else(|e| panic!("{property:?}: {right}: {e}"));
                        assert!(
                            plasm_core::plasm_monad::comp_semantic_eq(
                                &a.artifact().comp,
                                &b.artifact().comp
                            ),
                            "{property:?}: non-equivalent lowering\n{left}\n{right}"
                        );
                        count += 1;
                    }
                }
                assert_eq!(count, 35);
            }
            Property::InvalidGetBinding => {
                for call in [
                    "E.get()",
                    "E.get(\"i1\", identity=\"i2\")",
                    "E.get(identity=\"i1\", extra=\"x\")",
                    "E.get(id=\"i1\")",
                    "E.get(\"i1\", \"i2\")",
                    "E.get(identity=1.5)",
                ] {
                    for body in contexts(call) {
                        assert!(
                            compile_python_program(&es, &program(&body, &entity))
                                .await
                                .is_err(),
                            "{property:?}: invalid call admitted\n{body}"
                        );
                        count += 1;
                    }
                }
                assert_eq!(count, 30);
            }
        }
        println!("{property:?}: {count} generated checks; root/projection/bound-read/child/grandchild. This family is not general operation closure.");
    }
}
