use plasm_core::{Expr, InvokeExpr, Value, CGS};
use plasm_runtime::{preflight_compile_expr, ViewAmbientContext};

fn fixture() -> CGS {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/hydrate_invoke_target");
    plasm_core::load_schema(&dir).expect("load hydrate_invoke_target fixture")
}

fn run_expr(capability: &str) -> Expr {
    let mut input = indexmap::indexmap! {
        "expr".to_string() => Value::String("up".to_string()),
    };
    if capability == "datasource_run" {
        input.insert(
            "from".to_string(),
            Value::String("2026-08-03T03:50:00Z".to_string()),
        );
        input.insert(
            "to".to_string(),
            Value::String("2026-08-03T04:00:00Z".to_string()),
        );
    }
    Expr::Invoke(InvokeExpr::new(
        capability,
        "Datasource",
        "prometheus",
        Some(Value::Object(input)),
    ))
}

#[test]
fn static_compile_hydrates_declared_entity_fields_and_honors_prefix() {
    let cgs = fixture();
    let ambient = ViewAmbientContext::default();

    preflight_compile_expr(&run_expr("datasource_run"), &cgs, &ambient)
        .expect("ds_type comes from Datasource fields even though provides omits it");
    preflight_compile_expr(&run_expr("datasource_run_source_prefix"), &cgs, &ambient)
        .expect("source_type should honor the configured prefix");

    let error = preflight_compile_expr(&run_expr("datasource_run_typo"), &cgs, &ambient)
        .expect_err("ds_typo must remain an unknown CML variable");
    assert!(error.to_string().contains("ds_typo"), "{error}");
}
