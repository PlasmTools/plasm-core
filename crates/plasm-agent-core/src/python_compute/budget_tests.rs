use super::*;
use plasm_runtime::{InputBudgetUnit, RecoveryDisposition};

#[test]
fn compute_source_budget_measures_utf8_and_keeps_the_existing_boundary() {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/python_value_contract"),
    )
    .unwrap();
    let symbols =
        plasm_core::TeachingExposureSession::new(&cgs, "types", &["Sample"]).to_symbol_map();
    for source in ["x".repeat(4097), "é".repeat(2049), "\n".repeat(4097)] {
        let Err(PythonComputeRejection::Typed(PythonComputeError::SourceBudgetExceeded {
            actual_bytes,
            limit_bytes,
        })) = PreparedCompute::prepare(
            &source,
            &cgs,
            "types",
            symbols.as_ref(),
            ComputeInputMode::Singleton,
        )
        else {
            panic!("source budget rejection")
        };
        assert_eq!(actual_bytes, source.len());
        assert_eq!(limit_bytes, 4096);
    }
    // Exactly 4096 bytes passes the source-size gate (syntax can still be rejected).
    assert!(!matches!(
        PreparedCompute::prepare(
            &"x".repeat(4096),
            &cgs,
            "types",
            symbols.as_ref(),
            ComputeInputMode::Singleton
        ),
        Err(PythonComputeRejection::Typed(
            PythonComputeError::SourceBudgetExceeded { .. }
        ))
    ));
}

#[test]
fn named_input_budget_reports_accounted_bytes_without_payloads() {
    let rows = vec![crate::fixture_row!({"secret": "x".repeat(MAX_INPUT_BYTES)})];
    let error = validate_named_input_budget(&rows, "source").unwrap_err();
    let evidence = error.input_budget.as_ref().unwrap();
    assert_eq!(evidence.unit, InputBudgetUnit::AccountedBytes);
    assert_eq!(evidence.limit, MAX_INPUT_BYTES);
    assert_eq!(evidence.observed_at_least, MAX_INPUT_BYTES + 1);
    assert_eq!(evidence.input, "source");
    assert_eq!(error.recovery, RecoveryDisposition::RepairProgram);
    let encoded = serde_json::to_value(&error).unwrap();
    let decoded: plasm_runtime::ExecutionFailure = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(decoded.input_budget, error.input_budget);
    assert!(!encoded.to_string().contains("secret"));
    assert!(validate_named_input_budget(&[], "empty").is_ok());
}

#[test]
fn materialized_input_budget_accumulates_across_named_inputs() {
    let first = "first".to_owned();
    let second = "second".to_owned();
    let values = Value::Array(vec![Value::Integer(1); 128]);
    assert!(validate_materialized_input_budget(
        [(&first, &values), (&second, &values)].into_iter()
    )
    .is_ok());
    let extra = Value::Array(vec![Value::Integer(1); 129]);
    let error =
        validate_materialized_input_budget([(&first, &values), (&second, &extra)].into_iter())
            .unwrap_err();
    let evidence = error.input_budget.as_ref().unwrap();
    assert_eq!(evidence.unit, InputBudgetUnit::Rows);
    assert_eq!(evidence.limit, 256);
    assert_eq!(evidence.observed_at_least, 257);
    // The stack visits the second input first; this identifies where the
    // aggregate crosses the limit, not a claim that one input alone is too big.
    assert_eq!(evidence.input, first);
}
