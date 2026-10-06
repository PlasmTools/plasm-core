//! Executable research probe. These tests do not claim outer DSL/DAG integration.
use boundary::{ComputeInputMode, PreparedCompute, ValueContract};
use plasm_agent_core::python_compute as boundary;
use plasm_core::symbol_tuning::{SymbolRender, SymbolResolve};
use plasm_core::{TeachingExposureSession, CGS};
use plasm_runtime::ResultCoverage;
macro_rules! json { ($($tt:tt)*) => { serde_json::from_value::<plasm_core::ValueRow>(serde_json::json!($($tt)*)).unwrap() }; }
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
    let compute = PreparedCompute::prepare(
        &source(&symbol, r#""|".join(tag.label for tag in tags)"#),
        &cgs,
        "first",
        symbols.as_ref(),
        ComputeInputMode::Collection,
    )
    .unwrap();
    let owner = compute.contract.as_ref().unwrap().owner.clone();
    for (rows, expected) in [
        (vec![], ""),
        (
            vec![json!({"item_id":"i1","id":"t1","label":"Solo"})],
            "Solo",
        ),
        (
            vec![
                json!({"item_id":"i1","id":"t2","label":"First"}),
                json!({"item_id":"i1","id":"t3","label":"Second"}),
            ],
            "First|Second",
        ),
    ] {
        assert_eq!(
            compute
                .run(
                    &pool,
                    &owner,
                    &membership(rows.len(), ResultCoverage::Complete),
                    &rows
                )
                .await
                .unwrap(),
            plasm_core::Value::String(expected.into())
        );
    }
}
#[tokio::test]
async fn static_rejection_precedes_execution() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Item", "Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    for expression in [
        "e1.query()",
        "tags[0].m1()",
        "open('secret')",
        "42",
        r#""|".join(tag.unknown for tag in tags)"#,
        r#""|".join(tag.r1 for tag in tags)"#,
        r#""|".join(tag.__class__ for tag in tags)"#,
    ] {
        let rejected = match PreparedCompute::prepare(
            &source(&symbol, expression),
            &cgs,
            "first",
            symbols.as_ref(),
            ComputeInputMode::Collection,
        ) {
            Ok(prepared) => prepared.admit().is_err(),
            Err(_) => true,
        };
        assert!(rejected, "{expression}");
    }
    assert!(PreparedCompute::prepare(
        &source("e9999", "'x'"),
        &cgs,
        "first",
        symbols.as_ref(),
        ComputeInputMode::Collection
    )
    .is_err());
}
#[tokio::test]
async fn boundary_rejects_partial_unknown_missing_and_wrong_typed_values() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    let compute = PreparedCompute::prepare(
        &source(&symbol, r#""|".join(tag.label for tag in tags)"#),
        &cgs,
        "first",
        symbols.as_ref(),
        ComputeInputMode::Collection,
    )
    .unwrap();
    let owner = compute.contract.as_ref().unwrap().owner.clone();
    for coverage in [ResultCoverage::Partial, ResultCoverage::Unknown] {
        assert!(
            compute
                .run(&pool, &owner, &membership(0, coverage), &[])
                .await
                .unwrap_err()
                .code
                == "collection_incomplete"
        );
    }
    for (row, expected, recovery) in [
        // An unobserved field survives materialization; accessing it fails in Python.
        (
            json!({"item_id":"i1","id":"t"}),
            "AttributeError",
            plasm_runtime::RecoveryDisposition::RepairProgram,
        ),
        (
            json!({"item_id":"i1","id":"t","label":null}),
            "materialized type",
            plasm_runtime::RecoveryDisposition::Stop,
        ),
        (
            json!({"item_id":"i1","id":"t","label":7}),
            "materialized type",
            plasm_runtime::RecoveryDisposition::Stop,
        ),
    ] {
        if recovery == plasm_runtime::RecoveryDisposition::Stop {
            let error = compute
                .contract
                .as_ref()
                .unwrap()
                .materialize(
                    &owner,
                    &membership(1, ResultCoverage::Complete),
                    std::slice::from_ref(&row),
                )
                .unwrap_err();
            assert!(matches!(
                error,
                plasm_agent_core::program_rejection::PythonComputeError::ComputeInputValueContract(
                    plasm_core::value_contract::ValueContractError::MaterializedValueMismatch { ref path }
                ) if path == "label"
            ));
        }
        let failure = compute
            .run(
                &pool,
                &owner,
                &membership(1, ResultCoverage::Complete),
                &[row],
            )
            .await
            .unwrap_err();
        assert!(failure.diagnostic().contains(expected), "{failure:?}");
        assert_eq!(failure.recovery, recovery);
        if recovery == plasm_runtime::RecoveryDisposition::Stop {
            assert_eq!(failure.cause, plasm_runtime::FailureCause::Runtime);
            assert_eq!(failure.code, "compute_input_value_contract_invalid");
        }
    }
    assert!(compute
        .run(
            &pool,
            &owner,
            &membership(257, ResultCoverage::Complete),
            &vec![json!({"item_id":"i1","id":"t","label":"x"}); 257]
        )
        .await
        .unwrap_err()
        .diagnostic()
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
        .materialize(&second, &membership(0, ResultCoverage::Complete), &[])
        .unwrap_err()
        .to_string()
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
    let compute = PreparedCompute::prepare(
        &source(&token, "'|'.join(item.state for item in tags)"),
        &cgs,
        "first",
        symbols.as_ref(),
        ComputeInputMode::Collection,
    )
    .unwrap();
    let owner = compute.contract.as_ref().unwrap().owner.clone();
    let declaration = compute
        .contract
        .as_ref()
        .unwrap()
        .declaration(&symbols)
        .unwrap();
    assert!(declaration.contains("Literal[\"open\", \"closed\"]"));
    assert_eq!(
        compute
            .run(
                &pool,
                &owner,
                &membership(1, ResultCoverage::Complete),
                &[json!({"id":"i1", "title":"Alpha", "state":"open"})]
            )
            .await
            .unwrap(),
        plasm_core::Value::String("open".into())
    );
    let invalid = json!({"id":"i1", "title":"Alpha", "state":"invented"});
    let error = compute
        .contract
        .as_ref()
        .unwrap()
        .materialize(
            &owner,
            &membership(1, ResultCoverage::Complete),
            std::slice::from_ref(&invalid),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        plasm_agent_core::program_rejection::PythonComputeError::ComputeInputValueContract(
            plasm_core::value_contract::ValueContractError::Domain {
                ref path,
                source: plasm_core::ValueDomainViolation::UnknownEnumMember,
            }
        ) if path == "state"
    ));
    let failure = compute
        .run(
            &pool,
            &owner,
            &membership(1, ResultCoverage::Complete),
            &[invalid],
        )
        .await
        .unwrap_err();
    assert_eq!(failure.cause, plasm_runtime::FailureCause::Runtime);
    assert_eq!(failure.recovery, plasm_runtime::RecoveryDisposition::Stop);
    assert_eq!(failure.code, "compute_input_value_contract_invalid");
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
        let compute = PreparedCompute::prepare(
            &source(&symbol, expression),
            &cgs,
            "first",
            symbols.as_ref(),
            ComputeInputMode::Collection,
        )
        .unwrap_or_else(|e| panic!("{expression}: {e}"));
        let rows = [json!({"item_id":"i1","id":"t1","label":"Ω"})];
        let text = compute
            .run(
                &pool,
                &compute.contract.as_ref().unwrap().owner,
                &membership(rows.len(), ResultCoverage::Complete),
                &rows,
            )
            .await
            .unwrap_or_else(|e| panic!("{expression}: {e}"));
        assert_eq!(text.as_str(), Some(expected), "{expression}");
    }
}

#[tokio::test]
async fn python_text_format_errors_are_execution_errors() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    for expression in ["'{}'.format()", "'a'.split('/')[9]", "f'{tags[0].label:d}'"] {
        let compute = PreparedCompute::prepare(
            &source(&symbol, expression),
            &cgs,
            "first",
            symbols.as_ref(),
            ComputeInputMode::Collection,
        )
        .unwrap();
        assert!(
            compute
                .run(
                    &pool,
                    &compute.contract.as_ref().unwrap().owner,
                    &membership(1, ResultCoverage::Complete),
                    &[json!({"item_id":"i1","id":"t1","label":"x"})]
                )
                .await
                .is_err(),
            "{expression}"
        );
    }
}

#[tokio::test]
async fn local_mutation_and_nested_comprehensions_do_not_change_input_rows() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "first", &["Tag"]).to_symbol_map();
    let symbol = symbols.entity_sym_for("first", "Tag");
    let pool = plasm_agent_core::python_pool::PythonPool::default();
    let rows = vec![json!({"item_id":"i1", "id":"t1", "label":"a"})];
    let original = rows.clone();
    for (body, expected) in [
        ("tags.clear()\n    return str(len(tags))", "0"),
        (
            "sep = '|'\n    return sep.join(tag.label for group in [tags] for tag in group)",
            "a",
        ),
    ] {
        let code =
            format!("@compute\ndef render(tags: list[Value[{symbol}]]) -> str:\n    {body}\n");
        let compute = PreparedCompute::prepare(
            &code,
            &cgs,
            "first",
            symbols.as_ref(),
            ComputeInputMode::Collection,
        )
        .unwrap();
        assert_eq!(
            compute
                .run(
                    &pool,
                    &compute.contract.as_ref().unwrap().owner,
                    &membership(rows.len(), ResultCoverage::Complete),
                    &rows
                )
                .await
                .unwrap(),
            plasm_core::Value::String(expected.into())
        );
        assert_eq!(rows, original);
    }
    pool.close().await;
}

fn membership(
    count: usize,
    coverage: ResultCoverage,
) -> plasm_core::collection_codec::RecordedCollection<plasm_core::Ref> {
    use plasm_core::collection_codec::{
        CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
    };
    RecordingCodec::new()
        .record(
            CollectionIdentity::for_untyped_observation(&"compute_fixture").unwrap(),
            (0..count)
                .map(|i| plasm_core::Ref::new("Row", i.to_string()))
                .collect(),
            match coverage {
                ResultCoverage::Complete => Observation::ExactOutput { decoded: count },
                ResultCoverage::Partial => Observation::MoreAvailable,
                ResultCoverage::Unknown => Observation::UnprovenPage,
            },
        )
        .unwrap()
}
