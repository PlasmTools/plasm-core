//! Root/class rules are checked before lowering any executable program body.
use plasm_agent::plasm_compile::{compile_python_program, PythonDeclaration};
use plasm_core::symbol_tuning::SymbolRender;
#[tokio::test]
async fn declarations_require_live_witnesses_and_full_module_rejections() {
    super::constructor_evidence::assert_registry(
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
