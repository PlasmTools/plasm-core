//! Catalog call witnesses are checked against typed IL and resolved capability kinds.
use plasm_agent::plasm_compile::{compile_python_program, PythonCatalogOperation};
use plasm_core::{
    plasm_monad::{PlasmComp, PlasmStepPayload},
    symbol_tuning::SymbolRender,
    Expr,
};
use std::collections::BTreeSet;
#[tokio::test]
async fn catalog_dispatch_requires_typed_live_witnesses_and_rejections() {
    super::constructor_evidence::assert_registry(
        include_str!("../../../../doc-site/docs/reference/python-catalog-constructors.md"),
        "plasm-catalog-constructors",
        &PythonCatalogOperation::ALL
            .iter()
            .map(|op| op.name())
            .collect::<Vec<_>>(),
        |comp, _| catalog_nodes(comp),
    )
    .await;
}
fn catalog_nodes(comp: &PlasmComp) -> BTreeSet<String> {
    use super::language_matrix as matrix;
    let cgs = matrix::load_language_matrix_cgs();
    let mut kinds = BTreeSet::new();
    for payload in comp.steps.values() {
        let PlasmStepPayload::Invoke(invoke) = payload else {
            continue;
        };
        let expr = invoke
            .ir
            .as_ref()
            .map(|ir| &ir.expr)
            .or_else(|| invoke.ir_template.as_ref().map(|ir| &ir.expr))
            .expect("invoke IL");
        assert_eq!(
            expr.session_catalog_entry_id().unwrap().as_str(),
            matrix::MATRIX_ENTRY_ID
        );
        let cap = match expr {
            Expr::Get(g) => g
                .capability_name
                .as_ref()
                .and_then(|n| cgs.get_capability(n.as_str()))
                .or_else(|| {
                    assert!(g.capability_name.is_none(), "unknown explicit Get");
                    cgs.primary_get_capability(g.reference.entity_type.as_str())
                }),
            Expr::Query(q) => q
                .capability_name
                .as_ref()
                .and_then(|n| cgs.get_capability(n.as_str()))
                .or_else(|| {
                    assert!(q.capability_name.is_none(), "unknown explicit query");
                    cgs.primary_query_capability(q.entity.as_str())
                }),
            Expr::Create(c) => cgs.get_capability(c.capability.as_str()),
            Expr::Invoke(i) => cgs.get_capability(i.capability.as_str()),
            Expr::Delete(d) => cgs.get_capability(d.capability.as_str()),
            _ => panic!("unexpected witness IL"),
        }
        .expect("resolved fixture capability");
        kinds.insert(
            PythonCatalogOperation::from_kind(cap.kind)
                .name()
                .to_owned(),
        );
    }

    kinds
}
#[tokio::test]
async fn primary_and_symbolic_reads_preserve_the_resolved_plan() {
    use super::{language_matrix as matrix, python};
    for (id, method) in [
        ("get", "get"),
        ("query", "query"),
        ("complete_search", "search"),
    ] {
        let case = python::cases().find(|case| case.id == id).unwrap();
        let (es, _) = python::parity_context(case, "http://127.0.0.1:1");
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity = symbols.entity_sym_for(matrix::MATRIX_ENTRY_ID, "LangItem");
        let token = symbols.method_sym_for(matrix::MATRIX_ENTRY_ID, "LangItem", method);
        let source = python::program(case.python, &entity);
        let symbolic_source = source.replace(&format!(".{method}("), &format!(".{token}("));
        let primary = compile_python_program(&es, &source).await.unwrap();
        let symbolic = compile_python_program(&es, &symbolic_source).await.unwrap();
        // An implicit primary capability and an explicit method pin differ in IL.
        // Resolve only absent pins against this fixture before comparing the full plan;
        // never rewrite an explicit pin or erase any other contract component.
        let mut resolved = primary.artifact().comp.clone();
        let cgs = matrix::load_language_matrix_cgs();
        for step in resolved.steps.values_mut() {
            let PlasmStepPayload::Invoke(invoke) = step else {
                continue;
            };
            let expr = &mut invoke.ir.as_mut().expect("literal read IL").expr;
            match expr {
                Expr::Get(g) if g.capability_name.is_none() => {
                    g.capability_name = Some(
                        cgs.primary_get_capability(g.reference.entity_type.as_str())
                            .unwrap()
                            .name
                            .clone(),
                    );
                }
                Expr::Query(q) if q.capability_name.is_none() => {
                    q.capability_name = Some(
                        cgs.primary_query_capability(q.entity.as_str())
                            .unwrap()
                            .name
                            .clone(),
                    );
                }
                _ => {}
            }
        }
        assert!(
            plasm_core::plasm_monad::comp_semantic_eq(&resolved, &symbolic.artifact().comp),
            "{method} spelling changed semantic plan"
        );
    }
}
