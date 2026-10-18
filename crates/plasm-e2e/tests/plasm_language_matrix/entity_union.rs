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
    "entity_union_relation_selection",
    "entity_asserted_get",
    "entity_union_reject_fabrication",
];

fn session(cgs: Arc<plasm_core::CGS>) -> plasm_agent::execute_session::ExecuteSession {
    let mut es = language_matrix::matrix_execute_session(cgs.clone());
    es.entities = [
        "LangUnionFeed",
        "LangUnionParent",
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
            "LangUnionParent",
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
        format!(
            "class Bad(Program):\n    def build(self):\n        return {feed}.query().of_type({feed})\n"
        ),
        format!(
            "class Bad(Program):\n    def build(self):\n        return {feed}.query().take(1).headline\n"
        ),
        format!(
            "class Bad(Program):\n    def build(self):\n        return {feed}.query().take(1).{mark}()\n"
        ),
        format!(
            "class Bad(Program):\n    def build(self):\n        return {feed}.query().of_type({b}).take(1).{mark}()\n"
        ),
        format!(
            "class Bad(Program):\n    @compute\n    def fabricated(self) -> list[dict[str, str]]:\n        return [{{'id': 'shared', 'headline': 'copied'}}]\n    def build(self):\n        return self.fabricated().of_type({a})\n"
        ),
    ] {
        let error = Box::pin(compile_python_program(&es, &source))
            .await
            .expect_err("must reject authority violation");
        assert!(
            matches!(
                error,
                plasm_agent::compilation_error::CompilationError::Program(_)
            ),
            "must be an authored-program error: {error:?}; {source}"
        );
    }
}

async fn union_fixture(
    next_url: bool,
) -> (
    plasm_agent::execute_session::ExecuteSession,
    plasm_agent::server_state::PlasmHostState,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let writes = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = writes.clone();
    let wrong_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_wrong_reads = wrong_reads.clone();
    let app = axum::Router::new()
        .route("/language/v1/union-parents", axum::routing::get(|| async { axum::Json(serde_json::json!({"rows":[{"id":"feed"}]})) }))
        .route("/language/v1/details/{id}", axum::routing::get(
            |axum::extract::Path(id): axum::extract::Path<String>| async move { axum::Json(serde_json::json!({"id":id,"body":"left detail"})) }))
        .route("/language/v1/union-feeds/{id}", axum::routing::get(
            |axum::extract::Path(id): axum::extract::Path<String>| async move {
                axum::Json(serde_json::json!({"id":id}))
            }))
        .route(
            "/language/v1/union",
            axum::routing::get(|headers: axum::http::HeaderMap, axum::extract::Query(query): axum::extract::Query<std::collections::BTreeMap<String,String>>| async move {
                let page = query.get("page").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                let parent = query.get("parent_id").map(String::as_str).unwrap_or("");
                if parent == "latefail" && page > 0 { return Err(axum::http::StatusCode::BAD_GATEWAY); }
                let data = if matches!(parent, "paged" | "latefail") {
                    match page {
                        0 => serde_json::json!([{"id":"r1","caption":"first","right":null},{"id":"r2","caption":"second","right":null}]),
                        1 => serde_json::json!([{"id":"late","headline":"later selected row"}]),
                        _ => serde_json::json!([]),
                    }
                } else if page == 0 {
                    serde_json::json!([
                        {"id":"shared","headline":"left","note":null, "detail":{"id":"d1","body":"left detail"}},
                        {"id":"shared","caption":"right","right":null}
                    ])
                } else { serde_json::json!([]) };
                let done = if parent == "empty" { false } else if matches!(parent, "paged" | "latefail") { page > 0 } else { page > 0 };
                let data = if parent == "empty" { serde_json::json!([]) } else if parent == "duplicate" && page == 0 {
                    serde_json::json!([{"id":"dup","headline":"first"},{"id":"dup","headline":"second"}])
                } else { data };
                let host = headers.get(axum::http::header::HOST).unwrap().to_str().unwrap();
                let next = (!done).then(|| format!("http://{host}/language/v1/union?page={}&parent_id={parent}", page + 1));
                Ok(axum::Json(serde_json::json!({"rows":data,"done":done,"next":next})))
            }),
        )
        .route("/language/v1/union/left/{id}", axum::routing::get(
            move |axum::extract::Path(id): axum::extract::Path<String>| {
                let wrong_reads = observed_wrong_reads.clone();
                async move {
                if id == "wrong" && wrong_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    axum::Json(serde_json::json!({"id":id,"right":null,"caption":"wrong nominal type"}))
                } else {
                    axum::Json(serde_json::json!({"id":id,"headline":"left","note":null,"detail":{"id":"d1","body":"left detail"}}))
                }
                }
            }))
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
    if next_url {
        cgs.capabilities
            .get_mut("lang_union_feed_query")
            .unwrap()
            .mapping
            .as_mut()
            .unwrap()
            .template
            .0["pagination"] = serde_json::json!({
            "strategy":"next_url", "location":"response_next_url",
            "response_next_url_field":"next", "params":{"limit":{"fixed":2,"role":"page_size"}}
        });
    }

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
    (es, st, server, writes, wrong_reads)
}

#[tokio::test]
async fn entity_union_matrix_preserves_variant_identity() {
    let (es, st, server, _writes, _wrong_reads) = Box::pin(union_fixture(false)).await;
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let b = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowB");
    let _mark = symbols.method_sym_for(
        language_matrix::MATRIX_ENTRY_ID,
        "HomographRowA",
        "homographrowa_mark",
    );
    let program = format!(
        "class Observe(Program):\n    def build(self):\n        mixed = {feed}.query()\n        left = mixed.of_type({a})\n        right = mixed.of_type({b})\n        return left, right\n"
    );
    let bundle = Box::pin(compile_python_program(&es, &program))
        .await
        .unwrap();
    evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let run = Box::pin(run_plasm_comp_python(
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
    ))
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
    server.abort();
}

#[tokio::test]
async fn entity_union_matrix_selects_relation_variants() {
    let (es, st, server, _writes, _wrong_reads) = Box::pin(union_fixture(false)).await;
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionParent");
    let _a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let _b = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowB");
    let _mark = symbols.method_sym_for(
        language_matrix::MATRIX_ENTRY_ID,
        "HomographRowA",
        "homographrowa_mark",
    );
    let selected_left =
        symbols.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionParent", "left");
    let selected_right = symbols.ident_sym_relation_for(
        language_matrix::MATRIX_ENTRY_ID,
        "LangUnionParent",
        "right",
    );
    let projected = format!(
        "class Project(Program):\n    def build(self):\n        parent = {feed}.query().take(1)\n        return parent.{selected_left}, parent.{selected_right}\n"
    );
    let projected_bundle = Box::pin(compile_python_program(&es, &projected))
        .await
        .unwrap();
    evaluate_plasm_comp_dry(&es, &projected_bundle).unwrap();
    let projected_run = Box::pin(run_plasm_comp_python(
        &es,
        &st,
        &es.prompt_hash,
        "union_relation_selection",
        &projected_bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await
    .unwrap();
    for (step, variant) in projected_run
        .return_steps
        .iter()
        .zip(["HomographRowA", "HomographRowB"])
    {
        assert_eq!(step.result.entities().len(), 1);
        assert_eq!(
            step.result.entities()[0].reference.entity_type.as_str(),
            variant
        );
        assert_eq!(
            step.result.entities()[0].reference.primary_slot_str(),
            "shared"
        );
    }
    server.abort();
}

#[tokio::test]
async fn entity_union_matrix_preserves_relation_and_action_target() {
    let (es, st, server, writes, _wrong_reads) = Box::pin(union_fixture(false)).await;
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let _b = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowB");
    let mark = symbols.method_sym_for(
        language_matrix::MATRIX_ENTRY_ID,
        "HomographRowA",
        "homographrowa_mark",
    );
    let relation =
        symbols.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA", "detail");
    let program = format!(
        "class Act(Program):\n    def build(self):\n        left = {feed}.query().of_type({a}).take(1)\n        detail = left.{relation}\n        changed = left.{mark}()\n        return detail, changed\n"
    );
    let bundle = Box::pin(compile_python_program(&es, &program))
        .await
        .unwrap();
    evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let run = Box::pin(run_plasm_comp_python(
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
    ))
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

#[tokio::test]
async fn entity_union_matrix_rejects_get_without_poisoning_cache() {
    let (es, st, server, _writes, wrong_reads) = Box::pin(union_fixture(false)).await;
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let _feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let _b = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowB");
    let _mark = symbols.method_sym_for(
        language_matrix::MATRIX_ENTRY_ID,
        "HomographRowA",
        "homographrowa_mark",
    );
    let wrong =
        format!("class Wrong(Program):\n    def build(self):\n        return {a}.get(\"wrong\")\n");
    let wrong_bundle = Box::pin(compile_python_program(&es, &wrong)).await.unwrap();
    let wrong_run = Box::pin(run_plasm_comp_python(
        &es,
        &st,
        &es.prompt_hash,
        "union_wrong_get",
        &wrong_bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await;
    let wrong_failure = wrong_run.expect_err("wrong nominal Get must reject before caching");
    assert_eq!(
        wrong_failure.cause,
        plasm_runtime::execution_failure::FailureCause::ResponseContract
    );
    assert_eq!(wrong_failure.code, "entity_assertion_mismatch");
    {
        let mut materialization = es.graph_cache.lock().await;
        assert!(materialization
            .graph
            .get(&plasm_core::Ref::new("HomographRowA", "wrong"))
            .is_none());
        assert!(materialization
            .graph
            .get(&plasm_core::Ref::new("HomographRowB", "wrong"))
            .is_none());
        // Isolate the corrected provider observation from raw response reuse while
        // retaining the nominal graph whose publication boundary is under test.
        materialization.responses = Default::default();
    }
    let corrected = Box::pin(run_plasm_comp_python(
        &es,
        &st,
        &es.prompt_hash,
        "union_wrong_get",
        &wrong_bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await
    .unwrap();
    assert_eq!(
        corrected.return_steps[0].result.entities()[0]
            .reference
            .entity_type
            .as_str(),
        "HomographRowA"
    );
    assert_eq!(
        wrong_reads.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "corrected observation must reach the provider after raw response isolation"
    );
    server.abort();
}

#[tokio::test]
async fn entity_union_matrix_preserves_selected_pagination_failures() {
    for (parent, succeeds) in [
        ("paged", true),
        ("latefail", false),
        ("empty", false),
        ("duplicate", false),
    ] {
        let (es, st, server, _writes, _wrong_reads) =
            Box::pin(union_fixture(parent == "empty")).await;
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
        let _a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
        let _b = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowB");
        let _mark = symbols.method_sym_for(
            language_matrix::MATRIX_ENTRY_ID,
            "HomographRowA",
            "homographrowa_mark",
        );
        let selected_left = symbols.ident_sym_relation_for(
            language_matrix::MATRIX_ENTRY_ID,
            "LangUnionFeed",
            "left",
        );

        let source = format!(
            "class Pages(Program):\n    def build(self):\n        return {feed}.get(\"{parent}\").{selected_left}\n"
        );
        let bundle = Box::pin(compile_python_program(&es, &source))
            .await
            .unwrap();
        let result = Box::pin(run_plasm_comp_python(
            &es,
            &st,
            &es.prompt_hash,
            parent,
            &bundle,
            true,
            None,
            None,
            None,
            None,
        ))
        .await;
        if succeeds {
            let run = result.unwrap();
            assert_eq!(run.return_steps[0].result.entities().len(), 1);
            assert_eq!(
                run.return_steps[0].result.entities()[0]
                    .reference
                    .primary_slot_str(),
                "late"
            );
        } else {
            let failure = result
                .expect_err("incomplete or failed acquisition must not publish selected relation");
            let expected = match parent {
                "latefail" => "upstream_rejection",
                "empty" => "selected_relation_incomplete",
                "duplicate" => "pagination_progress_violation",
                _ => unreachable!(),
            };
            assert_eq!(failure.code, expected, "{parent}: {failure:?}");
        }
        server.abort();
    }
}

pub(super) fn admission_witness() -> (plasm_agent::execute_session::ExecuteSession, String) {
    let es = session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let source = format!(
        "class Narrow(Program):\n    def build(self):\n        return {feed}.query().of_type({a})\n"
    );
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
    // Nominal selection is an occurrence filter, not evidence of source exhaustion.
    let partial = plasm_runtime::execution::ExecutionCollection::observe_for(
        &cgs,
        &"matrix_partial_union",
        0,
        Vec::new(),
        plasm_core::collection_codec::Observation::UnprovenPage,
    )
    .unwrap();
    let empty_selected = partial
        .filter(&("relation_select_entity", "HomographRowA"), &[], &[])
        .unwrap();
    assert_eq!(empty_selected.coverage(), partial.coverage());
    assert_ne!(
        empty_selected.coverage(),
        plasm_core::collection_codec::ResultCoverage::Complete
    );
    let mut unselected = (*cgs).clone();
    unselected.capabilities.shift_remove("homographrowa_query");
    unselected
        .capabilities
        .get_mut("lang_union_feed_query")
        .unwrap()
        .domain = "HomographRowA".into();
    let relation = unselected
        .entities
        .get_mut("LangUnionFeed")
        .unwrap()
        .relations
        .get_mut("left")
        .unwrap();
    if let Some(plasm_core::RelationMaterialization::QueryScopedBindings {
        select_entity, ..
    }) = &mut relation.materialize
    {
        *select_entity = None;
    }
    let missing_selection = unselected
        .validate()
        .expect_err("union materialization requires selector");
    assert!(
        matches!(
            missing_selection,
            SchemaError::RelationMaterializeCapabilityInvalid {
                source: plasm_core::error::RelationMaterializeError::EntitySelectionRequired { .. },
                ..
            }
        ),
        "{missing_selection:?}"
    );
    for mode in ["target_mismatch", "nonmember", "nonunion"] {
        let mut invalid_selection = (*cgs).clone();
        let relation = invalid_selection
            .entities
            .get_mut("LangUnionFeed")
            .unwrap()
            .relations
            .get_mut("left")
            .unwrap();
        if let Some(plasm_core::RelationMaterialization::QueryScopedBindings {
            select_entity,
            ..
        }) = &mut relation.materialize
        {
            *select_entity = Some(
                if mode == "target_mismatch" {
                    "HomographRowB"
                } else if mode == "nonmember" {
                    "LangDetail"
                } else {
                    "HomographRowA"
                }
                .into(),
            );
        }
        if mode == "nonmember" {
            relation.target_resource = "LangDetail".into();
        }
        if mode == "nonunion" {
            invalid_selection
                .capabilities
                .get_mut("lang_union_feed_query")
                .unwrap()
                .output_schema = None;
        }
        assert!(
            matches!(
                invalid_selection.validate(),
                Err(SchemaError::RelationMaterializeCapabilityInvalid {
                    source: plasm_core::error::RelationMaterializeError::InvalidEntitySelection { .. },
                    ..
                })
            ),
            "{mode}"
        );
    }
    let mut invalid = (*cgs).clone();
    invalid
        .entities
        .get_mut("LangUnionFeed")
        .unwrap()
        .relations
        .clear();
    invalid
        .entities
        .get_mut("LangUnionParent")
        .unwrap()
        .relations
        .clear();
    invalid.capabilities.shift_remove("lang_union_feed_get");
    invalid
        .capabilities
        .get_mut("lang_union_feed_query")
        .unwrap()
        .kind = CapabilityKind::Get;
    let unsupported = invalid.validate().expect_err("Get union is unsupported");
    assert!(
        matches!(unsupported, SchemaError::EntityUnionCapabilityKind { .. }),
        "{unsupported:?}"
    );
    let mut invalid = (*cgs).clone();
    invalid
        .entities
        .get_mut("LangUnionFeed")
        .unwrap()
        .relations
        .clear();
    invalid
        .entities
        .get_mut("LangUnionParent")
        .unwrap()
        .relations
        .clear();
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

#[tokio::test]
async fn entity_union_matrix_narrow_filter_nullable_field_and_effect() {
    let (es, st, server, writes, _) = Box::pin(union_fixture(false)).await;
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let mark = symbols.method_sym_for(
        language_matrix::MATRIX_ENTRY_ID,
        "HomographRowA",
        "homographrowa_mark",
    );
    let program = format!(
        "class Act(Program):\n    def build(self):\n        left = {feed}.query().of_type({a})\n        chosen = left.where(lambda row: row.note is None)\n        return chosen.flat_map(lambda row: row.{mark}(), max_parents=4)\n"
    );
    let bundle = Box::pin(compile_python_program(&es, &program))
        .await
        .unwrap();
    evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    Box::pin(run_plasm_comp_python(
        &es,
        &st,
        &es.prompt_hash,
        "union_nullable_effect",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await
    .unwrap();
    assert_eq!(*writes.lock().unwrap(), vec!["shared".to_owned()]);
    server.abort();
}

#[tokio::test]
async fn entity_union_matrix_narrow_typed_compute_uses_variant_owner() {
    let (es, st, server, _, _) = Box::pin(union_fixture(false)).await;
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let feed = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangUnionFeed");
    let a = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "HomographRowA");
    let program = format!(
        "class Read(Program):\n    @compute\n    def headlines(self, rows: list[Row[{a}]]) -> list[str]:\n        return [row.headline for row in rows]\n    def build(self):\n        left = {feed}.query().of_type({a})\n        return self.headlines(left)\n"
    );
    let bundle = Box::pin(compile_python_program(&es, &program))
        .await
        .unwrap();
    evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let run = Box::pin(run_plasm_comp_python(
        &es,
        &st,
        &es.prompt_hash,
        "union_typed_compute",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    ))
    .await
    .unwrap();
    assert_eq!(run.return_steps.len(), 1);
    assert_eq!(run.return_steps[0].result.count(), 1);
    server.abort();
}
