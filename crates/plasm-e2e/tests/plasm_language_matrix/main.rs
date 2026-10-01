//! Python Program → typed IL → dry validation → live fixture execution.
//!
//! Conformance surface for user-visible Plasm syntax against `plasm_language_matrix` fixtures.
//! Coverage contract: see `features::REQUIRED_FEATURE_TAGS` and row `features` columns.

// Production async execution futures require deeper Send trait evaluation in host tests.
#![recursion_limit = "256"]
#![allow(dead_code)]

#[path = "../common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "../common/language_matrix.rs"]
mod language_matrix;

mod assert_live;
mod assert_planning;
mod callbacks;
mod catalog_registry;
mod collection_boundaries;
mod conformance_properties;
mod constructor_evidence;
mod constructor_registry;
mod datetime;
mod declaration_registry;
mod effect_boundaries;
mod effect_model;
mod features;
mod ir_helpers;
mod literate_contract;
mod python;
mod python_completion;
mod python_coverage;
mod python_expectations;
mod python_federated_parity;
mod python_host_contract;
mod python_render_parity;
mod recursive_values;
mod reference_algebra;
mod row;
mod rows;
mod scoped_composition;
mod semantic_contract;
mod teaching_ladder;

use plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry;
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

use assert_live::assert_comp_witness;
use assert_planning::assert_planning_ir;
use rows::{all_rows, find_row, row_count};

mod literal_registry;
mod relation_fanout;
mod relation_registry;
mod scalar_registry;

#[tokio::test]
async fn isolation_sidecar_serves_vault_and_lanes() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let client = reqwest::Client::new();
    let vault = client
        .get(format!("{base}/language/v1/vaults/venmo"))
        .send()
        .await
        .expect("vault get")
        .json::<serde_json::Value>()
        .await
        .expect("vault json");
    assert_eq!(
        vault.get("id").and_then(|v| v.as_str()),
        Some("venmo"),
        "sidecar must serve LangVault Get, got {vault}"
    );
    let lanes = client
        .get(format!("{base}/language/v1/lanes?shelf=alpha"))
        .send()
        .await
        .expect("lanes get")
        .json::<serde_json::Value>()
        .await
        .expect("lanes json");
    assert!(
        lanes.as_array().is_some_and(|a| a.len() == 2),
        "sidecar must serve LangLane multi-row, got {lanes}"
    );
}

#[tokio::test]
async fn plasm_language_matrix_cgs_templates_validate() {
    let cgs = language_matrix::load_language_matrix_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability CML templates");
}

#[test]
fn matrix_coverage_contract_all_rows_require_live_execution() {
    for row in all_rows() {
        assert!(
            row.min_node_results >= 1,
            "{} needs live node evidence",
            row.id
        );
        assert!(
            python::cases().any(|case| case.existing == Some(row.id)),
            "{} needs a Python program",
            row.id
        );
        assert!(
            !row.expect_markdown_substrings.is_empty()
                || row.features.contains(&"host_wait_cancel")
                || row.expect_live_error.is_some()
                || row.features.contains(&"iterate_bound_exhausted"),
            "{} needs live expectations",
            row.id
        );
    }
    assert_eq!(row_count(), 162, "semantic obligations must not disappear");
}

mod projection_registry;

mod predicate_registry;

mod aggregate_registry;

mod outer_values;
mod primitive_reductions;
mod typed_returns;
