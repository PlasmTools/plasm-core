//! OPH-3 / OPH-5 / OPH-6: multi-create partial commit → same-session relation/query reads.
//!
//! Proptest against the independent reference model is the correctness gate.

use super::*;
use crate::execution::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, ExecutionMode, ResultCoverage,
    SessionMaterialization, StreamConsumeOpts,
};
use plasm_core::loader::load_schema_dir;
use plasm_core::{ChainExpr, CreateExpr, Expr, GetExpr, Predicate, QueryExpr, Ref, Value};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use std::collections::BTreeSet;
use std::sync::Arc;

fn load_pw_cgs() -> plasm_core::CGS {
    load_schema_dir(
        &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/partial_write_relation_matrix"),
    )
    .expect("partial_write_relation_matrix CGS")
}

fn create_expense(group_id: &str, description: &str) -> CreateExpr {
    let input = Value::Object(
        [
            ("group_id".into(), Value::String(group_id.into())),
            ("description".into(), Value::String(description.into())),
        ]
        .into_iter()
        .collect(),
    );
    CreateExpr::new("pwexpense_create", "PwExpense", input)
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio rt")
        .block_on(f)
}

fn coverage_to_obs(c: ResultCoverage) -> ObservationCoverage {
    match c {
        ResultCoverage::Complete => ObservationCoverage::Complete,
        ResultCoverage::Partial => ObservationCoverage::Partial,
        ResultCoverage::Unknown => ObservationCoverage::Unknown,
    }
}

fn expense_ids_from_result(result: &crate::execution::ExecutionResult) -> BTreeSet<String> {
    result
        .entities
        .iter()
        .filter_map(|e| {
            e.fields
                .get("expense_id")
                .map(|f| f.to_value())
                .and_then(|v| match v {
                    Value::String(s) => Some(s),
                    _ => None,
                })
        })
        .collect()
}

/// Run a sequence of individual creates with a scripted fail index, then relation + query reads.
fn run_individual_create_sequence(
    descriptions: &[String],
    fail_at: Option<usize>,
    group_id: &str,
) -> Result<(), String> {
    let harness = HonestyHarness::new();
    let mut script: Vec<CreateScript> = descriptions.iter().map(|_| CreateScript::Ok).collect();
    if let Some(i) = fail_at {
        if i < script.len() {
            script[i] = CreateScript::Fail { status: 422 };
        }
    }
    harness.set_create_script(script);
    harness.backend_mut(|b| b.ensure_group(group_id, "Trip"));

    let cgs = load_pw_cgs();
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            hydrate: false,
            ..ExecutionConfig::default()
        },
        Arc::new(harness.clone()),
        None,
    );
    let mut mat = SessionMaterialization::new();
    let opts = ExecuteOptions::for_catalog(&cgs).map_err(|e| e.to_string())?;

    let mut plan_failed = false;
    let mut first_fail_seen = false;

    for (i, desc) in descriptions.iter().enumerate() {
        let should_fail = fail_at == Some(i);
        let res = block_on(engine.execute(
            &Expr::Create(create_expense(group_id, desc)),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            opts.clone(),
        ));
        match (should_fail, res) {
            (true, Err(_)) => {
                plan_failed = true;
                first_fail_seen = true;
                // Lone create Err carries no OperationAck; OPH-6 keys off prior completed acks.
            }
            (true, Ok(_)) => {
                return Err(format!("expected create[{i}] to fail, but it succeeded"));
            }
            (false, Ok(result)) => {
                if first_fail_seen {
                    return Err("create after failure should not run in this harness".into());
                }
                let completed = result
                    .operations
                    .entries()
                    .iter()
                    .filter(|a| a.capability == "pwexpense_create")
                    .map(|a| a.completed)
                    .sum::<usize>();
                let failed = result
                    .operations
                    .entries()
                    .iter()
                    .filter(|a| a.capability == "pwexpense_create")
                    .map(|a| a.failed)
                    .sum::<usize>();
                if completed != 1 || failed != 0 {
                    return Err(format!(
                        "successful create ack: completed={completed} failed={failed}"
                    ));
                }
                harness.with_model_mut(|m| {
                    m.record_reported("pwexpense_create", completed, failed);
                });
            }
            (false, Err(e)) => {
                return Err(format!("create[{i}] unexpectedly failed: {e}"));
            }
        }
        if should_fail {
            break; // sequence of individual creates stops at first failure
        }
    }

    // OPH-6 against the create phase ledger.
    harness.with_model(|m| m.check_acknowledgment_honesty("pwexpense_create", plan_failed))?;

    // Same-session relation read.
    let mut get = GetExpr::from_ref(Ref::new("PwGroup", group_id));
    get.capability_name = Some("pwgroup_get".into());
    let chain = ChainExpr::auto_get(Expr::Get(get), "expenses");
    let rel = block_on(engine.execute(
        &Expr::Chain(chain),
        &cgs,
        &mut mat,
        Some(ExecutionMode::Live),
        StreamConsumeOpts::default(),
        opts.clone(),
    ))
    .map_err(|e| format!("relation read after partial writes: {e}"))?;

    let rel_ids = expense_ids_from_result(&rel);
    harness.with_model(|m| {
        m.check_partial_write_visibility(group_id, &rel_ids)?;
        m.check_coverage_honesty(group_id, coverage_to_obs(rel.coverage), &rel_ids)
    })?;

    // Direct query path.
    let mut q = QueryExpr::filtered("PwExpense", Predicate::eq("group_id", group_id));
    q.capability_name = Some("pwexpense_query".into());
    let listed = block_on(engine.execute(
        &Expr::Query(q),
        &cgs,
        &mut mat,
        Some(ExecutionMode::Live),
        StreamConsumeOpts::default(),
        opts,
    ))
    .map_err(|e| format!("expense query after partial writes: {e}"))?;

    let q_ids = expense_ids_from_result(&listed);
    harness.with_model(|m| {
        m.check_partial_write_visibility(group_id, &q_ids)?;
        m.check_coverage_honesty(group_id, coverage_to_obs(listed.coverage), &q_ids)
    })?;

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// OPH-3 + OPH-6 (+ OPH-5 when coverage is Complete): individual creates with a scripted
    /// failure, then same-session relation and query reads.
    #[test]
    fn oph_partial_write_visibility_and_ack_honesty(
        descs in prop::collection::vec("[A-Za-z][A-Za-z0-9 ]{1,24}", 2..=5),
        fail_at in 0usize..5,
        group_suffix in 1u8..4,
    ) {
        let group_id = format!("g{group_suffix}");
        let fail = if fail_at < descs.len() {
            Some(fail_at)
        } else {
            // Occasionally all succeed.
            None
        };
        let err = run_individual_create_sequence(&descs, fail, &group_id);
        prop_assert!(err.is_ok(), "counterexample descs={descs:?} fail_at={fail:?} group={group_id}: {}", err.unwrap_err());
    }

    /// All-success plan still observes every commit (OPH-3 without failure).
    #[test]
    fn oph_all_success_relation_matches_backend(
        descs in prop::collection::vec("[A-Za-z]{2,12}", 1..=4),
    ) {
        let err = run_individual_create_sequence(&descs, None, "g1");
        prop_assert!(err.is_ok(), "{}", err.unwrap_err());
    }
}
