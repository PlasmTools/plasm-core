//! Matrix invoke IO: staged mutator with taught `p#` args executes through Hermit and materializes POST body.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
mod language_matrix;

use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp, PlasmPlanRunResult};
use plasm_compile::{compile_operation, parse_capability_template, CmlEnv};
use plasm_core::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions, MutatorAdmit};
use plasm_core::symbol_tuning::ExposureEntityKey;
use plasm_core::value::Value;
use plasm_core::{Expr, TeachingExposureSession};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use std::sync::Arc;

fn matrix_create_session() -> plasm_agent::execute_session::ExecuteSession {
    let cgs = language_matrix::load_language_matrix_cgs();
    let wave = ["LangItem"];
    let endpoints = [ExposureEntityKey {
        entry_id: language_matrix::MATRIX_ENTRY_ID.into(),
        entity: plasm_core::EntityName::from("LangItem"),
    }];
    let delta = derive_intent_exposure_surface_batch(
        cgs.as_ref(),
        language_matrix::MATRIX_ENTRY_ID,
        "create a lang item with title score and owner",
        &endpoints,
        &wave.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        Some(&["langitem_create".to_string()]),
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    let exp = TeachingExposureSession::new_with_intent_delta(
        cgs.as_ref(),
        language_matrix::MATRIX_ENTRY_ID,
        &wave,
        delta,
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        language_matrix::MATRIX_ENTRY_ID.into(),
        Arc::new(plasm_core::CgsContext::entry(
            language_matrix::MATRIX_ENTRY_ID,
            cgs.clone(),
        )),
    );
    plasm_agent::execute_session::ExecuteSession::new(
        "matrix_ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        language_matrix::MATRIX_ENTRY_ID.into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| s.to_string()).collect(),
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

#[tokio::test]
async fn langitem_create_p_symbols_execute_and_materialize_post_body() {
    let cgs = language_matrix::load_language_matrix_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
    let session = matrix_create_session();
    let map = plasm_agent::plasm_plan_run::symbol_map_for_plasm_surface_parse(&session, None);
    let item_e = map.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let cap = cgs
        .get_capability("langitem_create")
        .expect("langitem_create");
    let method_sym = map.method_sym_for("langmatrix", "LangItem", cap.name.as_str());
    let p_title = map.ident_sym_cap_param_for(
        language_matrix::MATRIX_ENTRY_ID,
        "LangItem",
        "langitem_create",
        "title",
    );
    let p_score = map.ident_sym_cap_param_for(
        language_matrix::MATRIX_ENTRY_ID,
        "LangItem",
        "langitem_create",
        "score",
    );
    let p_owner = map.ident_sym_cap_param_for(
        language_matrix::MATRIX_ENTRY_ID,
        "LangItem",
        "langitem_create",
        "owner",
    );
    let program = format!(
        "created = {item_e}.{method_sym}({p_title}=\"MatrixPSym\", {p_score}=9, {p_owner}=\"bot\")\ncreated"
    );

    let bundle = compile_plasm_program(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        &session,
        "matrix-create-p-sym",
        &program,
    )
    .expect("compile");
    let dry = evaluate_plasm_comp_dry(&session, &bundle).expect("dry");
    assert!(
        dry.node_results
            .iter()
            .any(|nr| nr.get("type_check") == Some(&serde_json::json!("ok"))),
        "staged create must pass dry preflight"
    );

    let base = hermit_lang_matrix::language_matrix_hermit_base_url()
        .await
        .clone();
    let st = Arc::new(language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .expect("engine"),
        cgs.clone(),
    ));
    let live: PlasmPlanRunResult = Box::pin(run_plasm_comp(
        &session,
        st.as_ref(),
        "matrix_ph",
        "matrix_sess",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await
    .expect("live run");
    let md = live.run_markdown.as_deref().unwrap_or("");
    assert!(
        md.contains("MatrixPSym"),
        "live create must return created row title, got:\n{md}"
    );

    let create_expr = dry
        .node_results
        .iter()
        .find_map(|nr| {
            let ev = nr.get("ir")?.get("expr")?;
            serde_json::from_value::<Expr>(ev.clone()).ok()
        })
        .and_then(|expr| match expr {
            Expr::Create(c) => Some(c),
            _ => None,
        })
        .expect("create surface IR");
    let Value::Object(input) = create_expr.input.to_value() else {
        panic!("create input must be object");
    };
    assert!(
        input.contains_key("title") && input.contains_key("score") && input.contains_key("owner")
    );

    let template = parse_capability_template(&cap.mapping.template.0).expect("template");
    let mut env = CmlEnv::new();
    for (k, v) in input {
        env.insert(k, v);
    }
    let compiled = compile_operation(&template, &env).expect("compile langitem_create");
    let plasm_compile::CompiledOperation::Http(req) = compiled else {
        panic!("expected HTTP");
    };
    assert_eq!(req.method_str(), "POST");
    let body = req.body.expect("body");
    let Value::Object(body_map) = body else {
        panic!("body object");
    };
    assert_eq!(
        body_map.get("title"),
        Some(&Value::String("MatrixPSym".to_string()))
    );
    assert_eq!(body_map.get("score"), Some(&Value::Integer(9)));
    assert_eq!(
        body_map.get("owner"),
        Some(&Value::String("bot".to_string()))
    );
}
