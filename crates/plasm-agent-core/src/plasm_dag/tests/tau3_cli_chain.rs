//! τ³ banking: chained CLI submit → approve in one program (bind-ordered writes).

use super::super::*;
use crate::plan_flow::FlowVerdict;
use crate::plasm_plan_run::{evaluate_plasm_plan_dry, symbol_map_for_plasm_surface_parse};
use plasm_core::{load_schema, CgsContext, PromptPipelineConfig, TeachingExposureSession};
use std::path::PathBuf;
use std::sync::Arc;

fn tau3_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs =
        Arc::new(load_schema(&root.join("../../apis/tau3_banking")).expect("load tau3_banking"));
    let entities = [
        "CreditLimitRequest",
        "CreditLimitEligibility",
        "CreditCardDispute",
    ];
    let exp = TeachingExposureSession::new(cgs.as_ref(), "tau3_banking", &entities);
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "tau3_banking".into(),
        Arc::new(CgsContext::entry("tau3_banking", cgs.clone())),
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "tau3_banking".into(),
        String::new(),
        String::new(),
        None,
        entities.iter().map(|e| (*e).to_string()).collect(),
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

#[test]
fn tau3_cli_submit_then_approve_chained_in_one_program_compiles() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if !root.join("../../apis/tau3_banking/domain.yaml").exists() {
        return;
    }
    let session = tau3_session();
    let map = symbol_map_for_plasm_surface_parse(&session, None);
    let cli_e = map.entity_sym_for("tau3_banking", "CreditLimitRequest");
    let submit_m = map.method_sym_for(
        "tau3_banking",
        "CreditLimitRequest",
        "CreditLimitRequest_submit",
    );
    let approve_m = map.method_sym_for(
        "tau3_banking",
        "CreditLimitRequest",
        "CreditLimitRequest_approve",
    );
    let p_cc = map.ident_sym_cap_param_for(
        "tau3_banking",
        "CreditLimitRequest",
        "CreditLimitRequest_submit",
        "credit_card_account_id",
    );
    let p_user = map.ident_sym_cap_param_for(
        "tau3_banking",
        "CreditLimitRequest",
        "CreditLimitRequest_submit",
        "user_id",
    );
    let p_inc = map.ident_sym_cap_param_for(
        "tau3_banking",
        "CreditLimitRequest",
        "CreditLimitRequest_submit",
        "requested_increase_amount",
    );
    let p_rid = map.ident_sym_cap_param_for(
        "tau3_banking",
        "CreditLimitRequest",
        "CreditLimitRequest_approve",
        "request_id",
    );
    let p_new = map.ident_sym_cap_param_for(
        "tau3_banking",
        "CreditLimitRequest",
        "CreditLimitRequest_approve",
        "new_credit_limit",
    );
    let source = format!(
        r#"c1 = {cli_e}.{submit_m}({p_cc}="cc_test", {p_user}="u1", {p_inc}=7500)
c2 = {cli_e}(c1.request_id).{approve_m}({p_rid}=c1.request_id, {p_cc}="cc_test", {p_user}="u1", {p_new}=22500)
c1, c2"#,
        cli_e = cli_e,
        submit_m = submit_m,
        approve_m = approve_m,
        p_cc = p_cc,
        p_user = p_user,
        p_inc = p_inc,
        p_rid = p_rid,
        p_new = p_new,
    );
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "tau3-cli-chain",
        &source,
    )
    .unwrap_or_else(|e| panic!("chained CLI program must compile: {e}"));
    let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry-run");
    assert!(
        matches!(dry.flow.verdict, FlowVerdict::Clean),
        "dry verdict: {:?}",
        dry.flow.verdict
    );
    let steps: Vec<_> = plan
        .get("nodes")
        .and_then(|n| n.as_array())
        .into_iter()
        .flatten()
        .filter_map(|n| n.get("id").and_then(|id| id.as_str()))
        .collect();
    assert!(steps.contains(&"c1"), "plan nodes: {steps:?}");
    assert!(steps.contains(&"c2"), "plan nodes: {steps:?}");
}
