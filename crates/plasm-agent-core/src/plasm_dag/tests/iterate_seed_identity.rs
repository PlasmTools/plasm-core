//! PLP-8 + plan-time identity: bound Get is `ir_template`; iterate seed is Get kind
//! (template + binding). Dry/CML must not stuff Binding holes into HTTP bearer.

use super::*;
use crate::compile_plasm_program;
use crate::plasm_dag::compile_plasm_dag_to_plan;
use crate::plasm_plan_run::evaluate_plasm_plan_dry;
use indexmap::IndexMap;

fn session_token_get_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(&root.join("../../fixtures/schemas/session_token_get"))
            .expect("load session_token_get"),
    );
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "session_token_get".into(),
        Arc::new(CgsContext::entry("session_token_get", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(cgs.as_ref(), "session_token_get", &["Wallet"]);
    ExecuteSession::new(
        "ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        "session_token_get".into(),
        String::new(),
        String::new(),
        None,
        vec!["Wallet".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

fn compile_wallet(name: &str, source: &str) -> Result<crate::PlasmCompBundle, String> {
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &session_token_get_session(),
        name,
        source,
    )
    .map_err(|e| e.to_string())
}

fn wallet_plan(name: &str, source: &str) -> Result<(ExecuteSession, serde_json::Value), String> {
    let session = session_token_get_session();
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        name,
        source,
    )?;
    Ok((session, plan))
}

fn plan_node_named<'a>(plan: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    plan["nodes"]
        .as_array()
        .expect("plan.nodes")
        .iter()
        .find(|n| n["id"] == id)
        .unwrap_or_else(|| panic!("missing plan node `{id}`"))
}

#[test]
fn bound_token_identity_get_is_lawful_iterate_seed() {
    compile_wallet(
        "session_token_get_bound_brace_seed",
        r#"tok = "fixture-jwt"
cur = Wallet{access_token=tok}
done = iterate cur step Wallet(_.access_token).nudge() until amount = 0 take 3
done"#,
    )
    .expect("bound brace Get identity must compile as iterate seed");

    compile_wallet(
        "session_token_get_bound_paren_seed",
        r#"tok = "fixture-jwt"
cur = Wallet(tok)
done = iterate cur step Wallet(_.access_token).nudge() until amount = 0 take 3
done"#,
    )
    .expect("bound paren Get identity must compile as iterate seed");
}

#[test]
fn bound_token_get_is_ir_template_at_plan_time() {
    let (_session, plan) = wallet_plan(
        "session_token_get_bound_brace_template",
        r#"tok = "fixture-jwt"
cur = Wallet{access_token=tok}
cur"#,
    )
    .expect("bound brace Get must compile");
    let cur = plan_node_named(&plan, "cur");
    assert_eq!(cur["kind"], "get", "{cur}");
    assert!(
        cur["ir"].is_null(),
        "bound Get identity must not be concrete ir: {cur}"
    );
    assert!(
        cur["ir_template"].is_object(),
        "bound Get identity is ir_template: {cur}"
    );

    let (_session, plan) = wallet_plan(
        "session_token_get_bound_paren_template",
        r#"tok = "fixture-jwt"
cur = Wallet(tok)
cur"#,
    )
    .expect("bound paren Get must compile");
    let cur = plan_node_named(&plan, "cur");
    assert!(
        cur["ir"].is_null(),
        "bound paren Get must be template: {cur}"
    );
    assert!(
        cur["ir_template"].is_object(),
        "bound paren Get identity is ir_template: {cur}"
    );
}

#[test]
fn literal_token_get_is_ir_at_plan_time() {
    let (_session, plan) = wallet_plan(
        "session_token_get_literal_ir",
        r#"cur = Wallet{access_token="fixture-jwt"}
cur"#,
    )
    .expect("literal Get must compile");
    let cur = plan_node_named(&plan, "cur");
    assert_eq!(cur["kind"], "get", "{cur}");
    assert!(cur["ir"].is_object(), "literal Get identity is ir: {cur}");
    assert!(
        cur["ir_template"].is_null(),
        "literal Get must not be demoted to template: {cur}"
    );
}

#[test]
fn bound_token_get_dry_plan_does_not_stuff_bearer() {
    let (session, plan) = wallet_plan(
        "session_token_get_bound_dry",
        r#"tok = "fixture-jwt"
cur = Wallet{access_token=tok}
cur"#,
    )
    .expect("bound Get must compile");
    evaluate_plasm_plan_dry(&session, &plan).expect(
        "bound Get dry plan must typecheck only; CML bearer must not see @tok Binding holes",
    );
}

#[test]
fn bound_token_iterate_dry_plans() {
    let (session, plan) = wallet_plan(
        "session_token_get_bound_iterate_dry",
        r#"tok = "fixture-jwt"
cur = Wallet{access_token=tok}
done = iterate cur step Wallet(_.access_token).nudge() until amount = 0 take 3
done"#,
    )
    .expect("bound iterate seed must compile as template + binding");
    let cur = plan_node_named(&plan, "cur");
    assert!(
        cur["ir_template"].is_object(),
        "iterate seed Get stays ir_template: {cur}"
    );
    evaluate_plasm_plan_dry(&session, &plan)
        .expect("Wallet iterate + bound seed must dry-plan without bearer charset heresy");
}

#[test]
fn literal_token_get_dry_plans() {
    let (session, plan) = wallet_plan(
        "session_token_get_literal_dry",
        r#"cur = Wallet{access_token="fixture-jwt"}
cur"#,
    )
    .expect("literal Get must compile");
    evaluate_plasm_plan_dry(&session, &plan)
        .expect("literal token Get may CML-compile at dry time");
}

#[test]
fn non_get_iterate_seed_still_rejects() {
    let err = compile_wallet(
        "session_token_get_literal_seed_not_get",
        r#"cur = [{access_token: "fixture-jwt", amount: 1}]
done = iterate cur step Wallet(_.access_token).nudge() until amount = 0 take 3
done"#,
    )
    .expect_err("row-literal seed is not a Get identity");
    assert!(
        err.contains("e#(tok)")
            && err.contains("e#{id_field=tok}")
            && err.contains("iterate cur step"),
        "diagnostic must name taught Get family, got: {err}"
    );
}
