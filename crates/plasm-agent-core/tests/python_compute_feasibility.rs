//! Executable research probe. These tests do not claim outer DSL/DAG integration.
use boundary::{CheckedCompute, ValueContract};
use plasm_agent_core::python_compute as boundary;
use plasm_core::symbol_tuning::{SymbolRender, SymbolResolve};
use plasm_core::{TeachingExposureSession, CGS};
use plasm_runtime::ResultCoverage;
use serde_json::json;
use std::sync::Arc;

fn fixture() -> Arc<CGS> {
    Arc::new(
        plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_dag_slice"),
        )
        .expect("fixture"),
    )
}
fn source(symbol: &str, expression: &str) -> String {
    format!("@compute\ndef joined_labels(tags: list[Value[{symbol}]]) -> str:\n    return {expression}\n")
}
#[tokio::test]
async fn real_monty_materialized_records_empty_singleton_plural() {
    let cgs = fixture();
    let exposure = TeachingExposureSession::new(&cgs, "first", &["Item", "Tag"]);
    let symbols = exposure.to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    let compute = CheckedCompute::compile(
        &source(&symbol, r#""|".join(tag.label for tag in tags)"#),
        &cgs,
        "first",
        symbols.as_ref(),
    )
    .unwrap();
    let owner = compute.contract.owner.clone();
    for (rows, expected) in [
        (vec![], ""),
        (vec![json!({"id":"t1","label":"Solo"})], "Solo"),
        (
            vec![
                json!({"id":"t2","label":"First"}),
                json!({"id":"t3","label":"Second"}),
            ],
            "First|Second",
        ),
    ] {
        assert_eq!(
            compute
                .run(&pool, &owner, ResultCoverage::Complete, &rows)
                .await
                .unwrap(),
            expected
        );
    }
}
#[test]
fn static_rejection_precedes_execution() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Item", "Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    for expression in [
        "e1.query()",
        "tags[0].m1()",
        "open('secret')",
        r#""".join("".join(t.label for t in tags) for tag in tags)"#,
        r#""".join(t.label for t in tags).join(t.label for t in tags)"#,
        "42",
        r#""|".join(tag.unknown for tag in tags)"#,
        r#""|".join(tag.r1 for tag in tags)"#,
        r#""|".join(tag.__class__ for tag in tags)"#,
    ] {
        assert!(
            CheckedCompute::compile(
                &source(&symbol, expression),
                &cgs,
                "first",
                symbols.as_ref()
            )
            .is_err(),
            "{expression}"
        );
    }
    assert!(
        CheckedCompute::compile(&source("e9999", "'x'"), &cgs, "first", symbols.as_ref()).is_err()
    );
    let mutation = source(&symbol, "'x'").replace("    return", "    tags.clear()\n    return");
    assert!(CheckedCompute::compile(&mutation, &cgs, "first", symbols.as_ref()).is_err());
}
#[tokio::test]
async fn boundary_rejects_partial_unknown_missing_and_wrong_typed_values() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    let compute = CheckedCompute::compile(
        &source(&symbol, r#""|".join(tag.label for tag in tags)"#),
        &cgs,
        "first",
        symbols.as_ref(),
    )
    .unwrap();
    let owner = compute.contract.owner.clone();
    for coverage in [ResultCoverage::Partial, ResultCoverage::Unknown] {
        assert!(compute
            .run(&pool, &owner, coverage, &[])
            .await
            .unwrap_err()
            .contains("coverage"));
    }
    for row in [
        json!({"id":"t"}),
        json!({"id":"t","label":null}),
        json!({"id":"t","label":7}),
    ] {
        assert!(compute
            .run(&pool, &owner, ResultCoverage::Complete, &[row])
            .await
            .unwrap_err()
            .contains("materialized type"));
    }
    assert!(compute
        .run(
            &pool,
            &owner,
            ResultCoverage::Complete,
            &vec![json!({"id":"t","label":"x"}); 257]
        )
        .await
        .unwrap_err()
        .contains("budget"));
}
#[test]
fn incremental_symbols_do_not_erase_catalog_ownership() {
    let cgs = fixture();
    let mut exposure = TeachingExposureSession::new(&cgs, "first", &["Item", "Tag"]);
    let before = exposure.to_symbol_map();
    let first_symbol = before.entity_sym_for("first", "Tag");
    exposure.expose_entities(&[&cgs], cgs.clone(), "second", &["Item", "Tag"]);
    let symbols = exposure.to_symbol_map();
    assert_eq!(symbols.entity_sym_for("first", "Tag"), first_symbol);
    let second_symbol = symbols.entity_sym_for("second", "Tag");
    assert_ne!(first_symbol, second_symbol);
    let first = ValueContract::from_cgs(&cgs, "first", symbols.as_ref(), &first_symbol).unwrap();
    let second = symbols.resolve_session_entity(&second_symbol).unwrap();
    assert!(first
        .materialize(&second, ResultCoverage::Complete, &[])
        .unwrap_err()
        .contains("ownership"));
    assert!(ValueContract::from_cgs(&cgs, "first", symbols.as_ref(), &second_symbol).is_err());
    assert_eq!(
        first.fields["id"]
            .value_type
            .domain
            .as_ref()
            .unwrap()
            .value_ref
            .as_str(),
        "tag_id"
    );
}
#[test]
fn concrete_declaration_is_generated_from_the_checked_contract() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Item", "Tag"]).to_symbol_map();
    let token = symbols.entity_sym_for("first", "Tag");
    let contract = ValueContract::from_cgs(&cgs, "first", symbols.as_ref(), &token).unwrap();
    let declaration = contract.declaration(&symbols).unwrap();
    ruff_python_parser::parse_module(&declaration).expect("valid Python declaration");
    assert!(declaration.contains(&format!("class {token}Value:")));
    assert!(declaration.contains("# Human label."));
    assert!(!declaration.contains("TagId"));
    assert!(!declaration.contains("Any"));
    println!("{declaration}");
}

#[tokio::test]
async fn cgs_enumeration_is_both_taught_and_enforced() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Item"]).to_symbol_map();
    let token = symbols.entity_sym_for("first", "Item");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    let compute = CheckedCompute::compile(
        &source(&token, "'|'.join(item.state for item in tags)"),
        &cgs,
        "first",
        symbols.as_ref(),
    )
    .unwrap();
    let owner = compute.contract.owner.clone();
    let declaration = compute.contract.declaration(&symbols).unwrap();
    assert!(declaration.contains("Literal[\"open\", \"closed\"]"));
    assert_eq!(
        compute
            .run(
                &pool,
                &owner,
                ResultCoverage::Complete,
                &[json!({"id":"i1", "title":"Alpha", "state":"open"})]
            )
            .await
            .unwrap(),
        "open"
    );
    assert!(compute
        .run(
            &pool,
            &owner,
            ResultCoverage::Complete,
            &[json!({"id":"i1", "title":"Alpha", "state":"invented"})]
        )
        .await
        .unwrap_err()
        .contains("not in enum"));
}

#[tokio::test]
async fn native_python_text_forms_preserve_exact_bytes() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    for (expression, expected) in [
        (
            r#""""line one
            twelve spaces
尾
""""#,
            "line one\n            twelve spaces\n尾\n",
        ),
        (
            r#"r"""literal \n
尾""""#,
            "literal \\n\n尾",
        ),
        (
            r#"f"""label={tags[0].label}
            kept
count={len(tags):02d}
""""#,
            "label=Ω\n            kept\ncount=01\n",
        ),
        (r#"rf"{{literal}}\n {tags[0].label}""#, "{literal}\\n Ω"),
        (r#"('a' 'b') + '\n' + tags[0].label"#, "ab\nΩ"),
        (
            r#"'{}\n{label}'.format('header', label=tags[0].label)"#,
            "header\nΩ",
        ),
        (r#"'%s\n%s' % ('header', tags[0].label)"#, "header\nΩ"),
        (r#"'\n'.join([f'{tag.label}' for tag in tags])"#, "Ω"),
        (r#"'prefix/suffix'.split('/')[1]"#, "suffix"),
        (r#"'  header  '.strip() + str(len(tags))"#, "header1"),
    ] {
        let compute = CheckedCompute::compile(
            &source(&symbol, expression),
            &cgs,
            "first",
            symbols.as_ref(),
        )
        .unwrap_or_else(|e| panic!("{expression}: {e}"));
        let rows = [json!({"label":"Ω"})];
        let text = compute
            .run(
                &pool,
                &compute.contract.owner,
                ResultCoverage::Complete,
                &rows,
            )
            .await
            .unwrap_or_else(|e| panic!("{expression}: {e}"));
        assert_eq!(text, expected, "{expression}");
    }
}

#[tokio::test]
async fn python_text_format_errors_are_execution_errors() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    for expression in ["'{}'.format()", "'a'.split('/')[9]", "f'{tags[0].label:d}'"] {
        let compute = CheckedCompute::compile(
            &source(&symbol, expression),
            &cgs,
            "first",
            symbols.as_ref(),
        )
        .unwrap();
        assert!(
            compute
                .run(
                    &pool,
                    &compute.contract.owner,
                    ResultCoverage::Complete,
                    &[json!({"label":"x"})]
                )
                .await
                .is_err(),
            "{expression}"
        );
    }
}
