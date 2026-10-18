//! Nominal output sums through the canonical matrix and production Python boundary.
use super::*;
use plasm_agent::plasm_compile::compile_python_program;
use plasm_agent::plasm_plan_run::run_plasm_comp_python;
use std::sync::{Arc, Mutex};

pub(super) const FEATURES: &[&str] = &[
    "entity_union_decode",
    "entity_union_narrow",
    "entity_union_receiver_authority",
    "entity_union_relation",
    "entity_union_reject_fabrication",
];

fn session(cgs: Arc<plasm_core::CGS>) -> plasm_agent::execute_session::ExecuteSession {
    let mut es = language_matrix::matrix_execute_session(cgs.clone());
    es.entities = [
        "LangUnionFeed",
        "HomographRowA",
        "HomographRowB",
        "LangDetail",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    es.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
        &cgs,
        language_matrix::MATRIX_ENTRY_ID,
        &[
            "LangUnionFeed",
            "HomographRowA",
            "HomographRowB",
            "LangDetail",
        ],
    ));
    es
}

#[tokio::test]
async fn entity_union_matrix_rejects_wrong_variant_and_fabricated_authority() {
    let es = session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let b = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowB");
    let mark = symbols.method_sym_for(
        language_matrix::MATRIX_ENTRY_ID,
        "HomographRowA",
        "homographrowa_mark",
    );
    for source in [
        format!("class Bad(Program):\n    def build(self):\n        return {feed}.query().of_type({feed})\n"),
        format!("class Bad(Program):\n    def build(self):\n        return {feed}.query().take(1).headline\n"),
        format!("class Bad(Program):\n    def build(self):\n        return {feed}.query().take(1).{mark}()\n"),
        format!("class Bad(Program):\n    def build(self):\n        return {feed}.query().of_type({b}).take(1).{mark}()\n"),
        format!("class Bad(Program):\n    @compute\n    def fabricated(self) -> list[dict[str, str]]:\n        return [{{'id': 'shared', 'headline': 'copied'}}]\n    def build(self):\n        return self.fabricated().of_type({a})\n"),
    ] {
        let error = compile_python_program(&es, &source).await.expect_err("must reject authority violation");
        assert!(matches!(error, plasm_agent::compilation_error::CompilationError::Program(_)), "must be an authored-program error: {error:?}; {source}");
    }
}

#[tokio::test]
async fn entity_union_matrix_preserves_variant_identity_relations_and_action_target() {
    let writes = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = writes.clone();
    let app = axum::Router::new()
        .route(
            "/language/v1/union",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!([
                    {"id":"shared","headline":"left", "detail":{"id":"d1","body":"left detail"}},
                    {"id":"shared","caption":"right","right":null}
                ]))
            }),
        )
        .route(
            "/language/v1/union/left/{id}/mark",
            axum::routing::post(
                move |axum::extract::Path(id): axum::extract::Path<String>| {
                    recorded.lock().unwrap().push(id);
                    async { axum::Json(serde_json::json!({"ok":true})) }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
    cgs.http_backend = base.clone();
    let cgs = Arc::new(cgs);
    let es = session(cgs.clone());
    let st = language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .unwrap(),
        cgs,
    );
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let b = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowB");
    let mark = symbols.method_sym_for(
        language_matrix::MATRIX_ENTRY_ID,
        "HomographRowA",
        "homographrowa_mark",
    );
    let program = format!("class Observe(Program):\n    def build(self):\n        mixed = {feed}.query()\n        left = mixed.of_type({a})\n        right = mixed.of_type({b})\n        return left, right\n");
    let bundle = compile_python_program(&es, &program).await.unwrap();
    evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let run = run_plasm_comp_python(
        &es,
        &st,
        &es.prompt_hash,
        "union_observe",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(run.return_steps.len(), 2);
    for (step, variant) in run
        .return_steps
        .iter()
        .zip(["HomographRowA", "HomographRowB"])
    {
        assert_eq!(step.result.entities().len(), 1);
        let row = &step.result.entities()[0];
        assert_eq!(row.reference.entity_type.as_str(), variant);
        assert_eq!(row.reference.primary_slot_str(), "shared");
        let (field, value) = if variant == "HomographRowA" {
            ("headline", "left")
        } else {
            ("caption", "right")
        };
        assert_eq!(
            row.fields.get(field).map(|v| v.to_value()),
            Some(plasm_core::Value::String(value.into()))
        );
    }
    let relation =
        symbols.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA", "detail");
    let program = format!("class Act(Program):\n    def build(self):\n        left = {feed}.query().of_type({a}).take(1)\n        detail = left.{relation}\n        changed = left.{mark}()\n        return detail, changed\n");
    let bundle = compile_python_program(&es, &program).await.unwrap();
    evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let run = run_plasm_comp_python(
        &es,
        &st,
        &es.prompt_hash,
        "union_act",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        run.return_steps[0].result.entities()[0]
            .reference
            .primary_slot_str(),
        "d1"
    );
    assert_eq!(*writes.lock().unwrap(), vec!["shared"]);
    server.abort();
}

pub(super) fn admission_witness() -> (plasm_agent::execute_session::ExecuteSession, String) {
    let es = session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let source = format!("class Narrow(Program):\n    def build(self):\n        return {feed}.query().of_type({a})\n");
    (es, source)
}

#[test]
fn entity_union_matrix_schema_and_decode_failures_remain_typed() {
    use plasm_compile::{
        decode_output_with_cgs, DecodeError, EntityDecoder, EntityUnionDecoder,
        EntityVariantDecoder, OutputDecoder, PathExpr,
    };
    use plasm_core::{
        CapabilityKind, EntityDiscriminator, OutputDecoderSpec, OutputType, SchemaError,
    };
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut invalid = (*cgs).clone();
    invalid
        .capabilities
        .get_mut("lang_union_feed_query")
        .unwrap()
        .kind = CapabilityKind::Get;
    assert!(matches!(
        invalid.validate(),
        Err(SchemaError::EntityUnionCapabilityKind { .. })
    ));
    let mut invalid = (*cgs).clone();
    let output = invalid
        .capabilities
        .get_mut("lang_union_feed_query")
        .unwrap()
        .output_schema
        .as_mut()
        .unwrap();
    if let OutputType::EntityUnion { variants, .. } = &mut output.output_type {
        variants.pop();
    }
    assert!(matches!(
        invalid.validate(),
        Err(SchemaError::EntityUnionVariants { .. })
    ));
    let output = cgs
        .get_capability("lang_union_feed_query")
        .unwrap()
        .output_schema
        .as_ref()
        .unwrap();
    let OutputDecoderSpec::EntityUnion { cases } = &output.decoder else {
        panic!("matrix sum")
    };
    let mut decoder = EntityUnionDecoder {
        source: PathExpr::empty().wildcard(),
        variants: cases
            .iter()
            .map(|case| EntityVariantDecoder {
                when: case.when.clone(),
                decoder: EntityDecoder::new(case.entity_type.as_str(), PathExpr::empty())
                    .with_id_field("id"),
            })
            .collect(),
    };
    // Both cases require the marker: absence matches none, presence matches both.
    decoder.variants[0].when = EntityDiscriminator::Present {
        path: vec!["right".into()],
    };
    let decoder = OutputDecoder::Union(decoder);
    assert!(matches!(
        decode_output_with_cgs(&decoder, &serde_json::json!([{"id":"x"}]), Some(&cgs)),
        Err(DecodeError::EntityUnionUnknownVariant { row: 0 })
    ));
    assert!(matches!(
        decode_output_with_cgs(
            &decoder,
            &serde_json::json!([{"id":"x","right":null}]),
            Some(&cgs)
        ),
        Err(DecodeError::EntityUnionAmbiguousVariant { row: 0, .. })
    ));
}
