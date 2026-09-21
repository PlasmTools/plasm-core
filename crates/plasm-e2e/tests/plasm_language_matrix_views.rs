//! View DAG conformance — preflight + catalog validation against `plasm_language_matrix_views`.
//!
//! Hermit is reserved for transport/decode regressions; view wiring lives here (fast, deterministic).

#[path = "common/hermit_lang_matrix.rs"]
#[allow(dead_code)]
mod hermit_lang_matrix;

#[path = "common/language_matrix_views.rs"]
mod language_matrix_views;

use std::sync::Arc;

use plasm_agent::plasm_compile::compile_plasm_program;
use plasm_agent::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use plasm_compile::{validate_cgs_capability_templates, validate_cgs_views};
use plasm_core::PromptPipelineConfig;
use plasm_core::QueryExpr;
use plasm_runtime::{
    preflight_view_query,
    view_test_support::{matrix_view_query, matrix_views_cgs, MATRIX_VIEW_PREFLIGHT_CASES},
    SessionMaterialization, ViewAmbientContext,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

use language_matrix_views::{
    language_matrix_views_schema_dir, load_language_matrix_views_cgs, views_execute_session,
    views_matrix_host_state, VIEWS_MATRIX_ENTRY_ID,
};

fn block_on_views_live<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("views live runtime");
            rt.block_on(fut)
        })
        .expect("spawn views live harness")
        .join()
        .expect("join views live harness")
}

#[test]
fn matrix_views_catalog_passes_static_validation() {
    let cgs = matrix_views_cgs();
    validate_cgs_capability_templates(&cgs).expect("CML templates");
    validate_cgs_views(&cgs).expect("views DAG");
}

#[test]
fn matrix_views_all_preflight() {
    let cgs = matrix_views_cgs();
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).expect("compile CML");
    let ambient = ViewAmbientContext::default();
    for &(view_name, entity) in MATRIX_VIEW_PREFLIGHT_CASES {
        let query = matrix_view_query(entity);
        preflight_view_query(
            view_name,
            &query,
            &cgs,
            &compiled,
            &ambient,
            &SessionMaterialization::new(),
        )
        .unwrap_or_else(|err| panic!("{view_name} preflight: {err}"));
    }
}

#[test]
fn matrix_views_missing_scope_preflight_errors() {
    let cgs = matrix_views_cgs();
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).expect("compile CML");
    let query = QueryExpr::all("LangDigest");
    let err = preflight_view_query(
        "lang_digest",
        &query,
        &cgs,
        &compiled,
        &ViewAmbientContext::default(),
        &SessionMaterialization::new(),
    )
    .expect_err("missing scope");
    assert!(err.to_string().contains("item_id"), "{err}");
}

/// Row-to-text render must persist wire-name column aliases in the comp wire. Fields bind by wire
/// name — the former `p#` field-symbol scheme was removed, so teaching tokens for fields ARE the
/// wire names and the alias keys must be those wire names.
#[test]
fn matrix_views_row_to_text_wire_column_aliases() {
    block_on_views_live(async {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
        let es = Arc::new(views_execute_session(cgs.clone()));
        let map = es
            .teaching_exposure
            .as_ref()
            .expect("views session exposure")
            .symbol_map_arc();
        // Field teaching tokens are wire names now (no `p#`).
        let f_id = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "id");
        let f_title = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "title");
        assert_eq!(f_id, "id", "LangItem.id teaching token is its wire name");
        assert_eq!(
            f_title, "title",
            "LangItem.title teaching token is its wire name"
        );
        let program = format!(
            "items = LangItem(\"i1\") | select {f_id}, {f_title}\nreport = items => <<PLASM_VIEWS_WIRE_BODY\n- {{{{ {f_id} }}}}: {{{{ {f_title} }}}}\nPLASM_VIEWS_WIRE_BODY\nreport"
        );
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            es.as_ref(),
            "matrix_views_wire_body_render",
            &program,
        )
        .expect("compile row-to-text wire body");
        evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry row-to-text wire body");
        let comp_wire = serde_json::to_string(&bundle.artifact().comp).expect("comp json");
        // Wire-name columns are carried on the render node (no `column_aliases` remap is emitted when the
        // projection tokens already equal the wire names — aliasing only existed under the `p#` scheme).
        assert!(
            comp_wire.contains(&f_id) && comp_wire.contains(&f_title),
            "comp wire must retain wire column tokens {f_id}/{f_title}"
        );
        let st = Arc::new(views_matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs,
        ));
        let live = run_plasm_comp(
            es.as_ref(),
            st.as_ref(),
            es.prompt_hash.as_str(),
            "matrix_views_sess",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("live row-to-text wire body");
        let md = live
            .run_markdown
            .as_deref()
            .expect("run markdown for render row");
        assert!(md.contains("i1"), "expected rendered id in markdown: {md}");
        let content = live
            .node_results
            .iter()
            .find_map(|nr| nr.get("rows").and_then(|r| r.as_array()))
            .and_then(|rows| rows.iter().find(|r| r.get("content").is_some()))
            .and_then(|r| r.get("content"))
            .and_then(|v| v.as_str());
        if let Some(text) = content {
            assert!(
                text.contains("i1"),
                "rendered content should include id via wire column: {text}"
            );
        }
        assert!(
            live.node_results.len() >= 2,
            "expected render + return nodes, got {}",
            live.node_results.len()
        );
    });
}

#[test]
fn matrix_views_row_to_text_source_alias_iteration() {
    block_on_views_live(async {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
        let es = Arc::new(views_execute_session(cgs.clone()));
        let map = es
            .teaching_exposure
            .as_ref()
            .expect("views session exposure")
            .symbol_map_arc();
        let p_id = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "id");
        let p_title = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "title");
        let p_score = map.ident_sym_entity_field_for(VIEWS_MATRIX_ENTRY_ID, "LangItem", "score");
        let program = format!(
            "items = LangItem(\"i1\") | select {p_id}, {p_title}, {p_score}\nreport = <<PLASM_VIEWS_ALIAS_BODY\n{{% for r in items %}}- {{{{ r.{p_id} }}}}: {{{{ r.{p_title} }}}} (score: {{{{ r.{p_score} or \"—\" }}}})\n{{% endfor %}}\nPLASM_VIEWS_ALIAS_BODY\nreport"
        );
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            es.as_ref(),
            "matrix_views_alias_render",
            &program,
        )
        .expect("compile plain-template collection render");
        evaluate_plasm_comp_dry(es.as_ref(), &bundle)
            .expect("dry plain-template collection render");
        let comp_wire = serde_json::to_string(&bundle.artifact().comp).expect("comp json");
        assert!(
            comp_wire.contains("\"items\""),
            "comp wire must retain items as a named-binding dependency"
        );
        let st = Arc::new(views_matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs,
        ));
        let live = run_plasm_comp(
            es.as_ref(),
            st.as_ref(),
            es.prompt_hash.as_str(),
            "matrix_views_alias_sess",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("live row-to-text source alias");
        let md = live
            .run_markdown
            .as_deref()
            .expect("run markdown for render row");
        assert!(md.contains("i1"), "expected rendered id in markdown: {md}");
        assert!(
            md.contains("avalanche fracture") || md.contains("score:"),
            "source-alias iteration should render item row: {md}"
        );
    });
}

/// A `{% for <cursor> in items %}` loop in a **plain** template accepts any cursor name.
/// Whole-collection text is one template evaluation (PLP-12); there is no implicit `rows` list.
#[test]
fn matrix_views_row_to_text_named_loop_cursor() {
    block_on_views_live(async {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
        let es = Arc::new(views_execute_session(cgs.clone()));
        let program = "items = LangItem(\"i1\") | select id, title\nreport = <<PLASM_VIEWS_NAMED_CURSOR\n{% for entry in items %}- {{ entry.id }}: {{ entry.title or \"—\" }}\n{% endfor %}\nPLASM_VIEWS_NAMED_CURSOR\nreport";
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            es.as_ref(),
            "matrix_views_named_cursor_render",
            program,
        )
        .expect("compile row-to-text named loop cursor");
        evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry row-to-text named loop cursor");
        let st = Arc::new(views_matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs,
        ));
        let live = run_plasm_comp(
            es.as_ref(),
            st.as_ref(),
            es.prompt_hash.as_str(),
            "matrix_views_named_cursor_sess",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("live row-to-text named loop cursor");
        let md = live
            .run_markdown
            .as_deref()
            .expect("run markdown for render row");
        assert!(
            md.contains("i1"),
            "named loop cursor `entry` should render item id: {md}"
        );
    });
}

/// View-backed many-relations must execute via `view_embed` (not Unavailable cached-embed side door).
#[test]
fn matrix_views_view_embed_relation_traversal() {
    block_on_views_live(async {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
        let triage = cgs
            .get_entity("LangTriageContext")
            .expect("LangTriageContext");
        let tags_rel = triage.relations.get("tags").expect("tags relation");
        assert!(matches!(
            tags_rel.materialize,
            Some(plasm_core::RelationMaterialization::ViewEmbed { .. })
        ));
        let es = Arc::new(views_execute_session(cgs.clone()));
        let program = "LangTriageContext(\"i1\").tags";
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            es.as_ref(),
            "matrix_views_view_embed_tags",
            program,
        )
        .expect("compile view_embed relation traversal");
        evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry view_embed relation traversal");
        let st = Arc::new(views_matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs,
        ));
        let live = run_plasm_comp(
            es.as_ref(),
            st.as_ref(),
            es.prompt_hash.as_str(),
            "matrix_views_view_embed_sess",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("live view_embed relation traversal");
        let md = live.run_markdown.as_deref().unwrap_or("");
        assert!(
            live.node_results.len() >= 2,
            "expected view get + relation nodes, got {}",
            live.node_results.len()
        );
        assert!(
            md.contains("label") || md.contains("tag") || md.contains("i1"),
            "expected LangTag rows in markdown from view_embed hop: {md}"
        );
    });
}

#[test]
fn matrix_views_query_parent_fanout_preserves_embeds() {
    block_on_views_live(async {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        for program in [
            "parent = LangTriageContext{item_id=\"i1\"}\ntags = parent => _.tags\ntags",
            "parent = LangTriageContext{item_id=\"i1\"}\ntags = parent.tags\ntags",
            "parent = LangTriageContext{item_id=\"i1\"} | take 1\ntags = parent => _.tags\ntags",
        ] {
            let es = Arc::new(views_execute_session(cgs.clone()));
            let bundle = compile_plasm_program(
                &PromptPipelineConfig::default(),
                None,
                es.as_ref(),
                "view_parent_fanout",
                program,
            )
            .expect("compile view parent relation");
            evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("preflight view parent relation");
            let st = Arc::new(views_matrix_host_state(
                ExecutionEngine::new(ExecutionConfig {
                    base_url: Some(base.clone()),
                    ..Default::default()
                })
                .unwrap(),
                cgs.clone(),
            ));
            let live = run_plasm_comp(
                es.as_ref(),
                st.as_ref(),
                es.prompt_hash.as_str(),
                "view_parent_fanout",
                &bundle,
                true,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("live query-produced parent relation");
            assert!(
                live.run_markdown.as_deref().unwrap_or("").contains("label"),
                "child detail projection must survive: {:?}",
                live.run_markdown
            );
        }
    });
}

/// Parameterless dashboard view: nonempty assigned items via view_embed.
#[test]
fn matrix_views_parameterless_dashboard_view_embed_nonempty() {
    block_on_views_live(async {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        let es = Arc::new(views_execute_session(cgs.clone()));
        let map = es
            .teaching_exposure
            .as_ref()
            .expect("views session exposure")
            .symbol_map_arc();
        let esym = map.entity_sym_for(VIEWS_MATRIX_ENTRY_ID, "LangWorkSnapshot");
        let msym = map.method_sym_for(
            VIEWS_MATRIX_ENTRY_ID,
            "LangWorkSnapshot",
            "lang_work_snapshot_get",
        );
        let items_rel =
            map.ident_sym_relation_for(VIEWS_MATRIX_ENTRY_ID, "LangWorkSnapshot", "items");
        // Pathless dashboard Get: taught `e#.m#().r#` (receiver none), not kebab `e#.slug()`.
        let program = format!("{esym}.{msym}().{items_rel}");
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            es.as_ref(),
            "matrix_views_work_snapshot_items",
            &program,
        )
        .expect("compile parameterless dashboard relation");
        evaluate_plasm_comp_dry(es.as_ref(), &bundle)
            .expect("dry parameterless dashboard relation");
        let st = Arc::new(views_matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs,
        ));
        let live = run_plasm_comp(
            es.as_ref(),
            st.as_ref(),
            es.prompt_hash.as_str(),
            "matrix_views_work_snapshot_sess",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("live parameterless dashboard relation");
        assert!(
            live.node_results.len() >= 2,
            "expected view root + relation nodes, got {}",
            live.node_results.len()
        );
        let md = live.run_markdown.as_deref().unwrap_or("");
        assert!(
            md.contains("i1") || md.contains("Alpha"),
            "expected assigned LangItem rows: {md}"
        );
    });
}

/// Parameterless dashboard view: zero assigned items still succeeds via present-empty provenance.
#[test]
fn matrix_views_parameterless_dashboard_view_embed_empty() {
    block_on_views_live(async {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        let es = Arc::new(views_execute_session(cgs.clone()));
        let map = es
            .teaching_exposure
            .as_ref()
            .expect("views session exposure")
            .symbol_map_arc();
        let esym = map.entity_sym_for(VIEWS_MATRIX_ENTRY_ID, "LangWorkSnapshotEmpty");
        let msym = map.method_sym_for(
            VIEWS_MATRIX_ENTRY_ID,
            "LangWorkSnapshotEmpty",
            "lang_work_snapshot_empty_get",
        );
        let items_rel =
            map.ident_sym_relation_for(VIEWS_MATRIX_ENTRY_ID, "LangWorkSnapshotEmpty", "items");
        // Pathless dashboard Get: taught `e#.m#().r#` (receiver none), not kebab `e#.slug()`.
        let program = format!("{esym}.{msym}().{items_rel}");
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            es.as_ref(),
            "matrix_views_work_snapshot_empty_items",
            &program,
        )
        .expect("compile empty dashboard relation");
        evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry empty dashboard relation");
        let st = Arc::new(views_matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .expect("ExecutionEngine"),
            cgs,
        ));
        let live = run_plasm_comp(
            es.as_ref(),
            st.as_ref(),
            es.prompt_hash.as_str(),
            "matrix_views_work_snapshot_empty_sess",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("live empty dashboard relation must succeed with zero rows");
        assert!(
            live.node_results.len() >= 2,
            "expected view root + relation nodes, got {}",
            live.node_results.len()
        );
    });
}

/// Scoped view with zero tag children: dry plan accepts present-empty provenance.
#[tokio::test]
async fn matrix_views_scoped_view_embed_empty_relation_dry() {
    let cgs = load_language_matrix_views_cgs();
    let es = Arc::new(views_execute_session(cgs.clone()));
    let program = "LangTriageContext(\"i2\").tags";
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        es.as_ref(),
        "matrix_views_empty_tags_dry",
        program,
    )
    .expect("compile empty scoped tags relation");
    evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry empty scoped tags");
}

#[test]
fn matrix_views_parse_rejects_unmaterialized_many_relation_before_normalize() {
    use plasm_core::expr_parser::{parse_with_cgs_layers, ParseErrorKind};
    use plasm_core::loader::load_schema_dir_unvalidated;
    use plasm_core::{CgsLayer, SymbolMap};
    use std::sync::Arc;

    let dir = language_matrix_views_schema_dir();
    let cgs = load_schema_dir_unvalidated(&dir).expect("load unvalidated matrix views");
    let tags = cgs
        .get_entity("LangTriageContext")
        .expect("LangTriageContext")
        .relations
        .get("tags")
        .expect("tags");
    assert!(tags.materialize.is_none());
    assert!(
        cgs.validate().is_err(),
        "validate must reject many-relation before normalize"
    );

    let (full, _) = plasm_core::entity_slices_for_render(&cgs, plasm_core::FocusSpec::All);
    let sym_map = Arc::new(SymbolMap::build(&cgs, &full));
    let stack = [CgsLayer::new("langmatrix_views", &cgs)];
    let err = parse_with_cgs_layers(r#"LangTriageContext("i1").tags"#, &stack, sym_map)
        .expect_err("parse must reject unmaterialized many-relation");
    assert!(
        matches!(err.kind, ParseErrorKind::ManyRelationUnmaterialized { .. }),
        "expected ManyRelationUnmaterialized, got {:?}",
        err.kind
    );
}

#[test]
fn matrix_views_rowsets_taught_navigation_compiles_and_preflights() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/view_rowsets");
    let cgs = Arc::new(plasm_core::loader::load_schema_dir(&dir).unwrap());
    let session = language_matrix_views::execute_session_for_entities(
        cgs,
        &["Library", "Item", "Collection"],
    );
    for program in [
        "items = Library{access_token=\"test-token\"}.items\nitems",
        "library = Library{access_token=\"test-token\"}\nitems = library.items\nitems",
    ] {
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &session,
            "rowset_view",
            program,
        )
        .expect("ordinary taught query and relation navigation");
        evaluate_plasm_comp_dry(&session, &bundle).expect("dry composed rowset view");
    }
}

#[test]
fn matrix_views_query_only_parent_relations_live() {
    block_on_views_live(async {
        let app =
            axum::Router::new().fallback(axum::routing::get(|uri: axum::http::Uri| async move {
                let body = match uri.path() {
                    "/items" => serde_json::json!([{"id":"i1","title":"one"}]),
                    "/collections" if uri.query().unwrap_or("").contains("page_index=1") => {
                        serde_json::json!([])
                    }
                    "/collections" => serde_json::json!([{"id":"c1"},{"id":"c2"}]),
                    "/collections/c1" => {
                        serde_json::json!({"id":"c1","items":[{"id":"i1"},{"id":"i2"}]})
                    }
                    "/collections/c2" => {
                        serde_json::json!({"id":"c2","items":[{"id":"i2"},{"id":"i3"}]})
                    }
                    "/items/i1" => serde_json::json!({"id":"i1","title":"one"}),
                    "/items/i2" => serde_json::json!({"id":"i2","title":"two"}),
                    "/items/i3" => serde_json::json!({"id":"i3","title":"three"}),
                    other => panic!("unexpected fixture request {other}"),
                };
                axum::Json(body)
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/view_rowsets");
        let mut cgs = plasm_core::loader::load_schema_dir(&dir).unwrap();
        cgs.http_backend = base.clone();
        let cgs = Arc::new(cgs);
        for program in [
            "library = Library{access_token=\"test-token\"}\nitems = library => _.items\nitems",
            "library = Library{access_token=\"test-token\"}\nitems = library.items\nitems",
            "library = Library{access_token=\"test-token\"} | take 1\nitems = library => _.items\nitems",
            "library = Library{access_token=\"test-token\"} | take 1\nitems = library.items\nitems",
            "library = Library{access_token=\"test-token\"}\ncopy = library | select access_token\nitems = copy => _.items\nitems",
            "library = Library{access_token=\"test-token\"} | select access_token\nitems = library => _.items\nitems",
        ] {
            let es = language_matrix_views::execute_session_for_entities(cgs.clone(), &["Library", "Item", "Collection"]);
            let bundle = compile_plasm_program(&PromptPipelineConfig::default(), None, &es, "query_only_view", program).unwrap_or_else(|e| panic!("compile {program}: {e}"));
            evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            let st = views_matrix_host_state(ExecutionEngine::new(ExecutionConfig { base_url: Some(base.clone()), ..Default::default() }).unwrap(), cgs.clone());
            let live = run_plasm_comp(&es, &st, es.prompt_hash.as_str(), "query_only_view", &bundle, true, None, None, None, None).await
                .expect("query-only view relation must execute");
            let children = live.return_steps.iter().find(|s| s.node_id.as_deref() == Some("items")).expect("returned child relation");
            let titles: std::collections::BTreeSet<_> = children.result.entities.iter().map(|e| e.fields.get("title").unwrap().to_value().as_str().unwrap().to_string()).collect();
            assert_eq!(titles, std::collections::BTreeSet::from(["one".into(), "two".into(), "three".into()]));
            assert_eq!(children.result.entities.len(), 3, "overlapping child IDs must deduplicate");
        }
        for program in [
            "library = Library{access_token=\"test-token\"} | where access_token = \"absent\"\nitems = library => _.items\nitems",
        ] {
            let es = language_matrix_views::execute_session_for_entities(cgs.clone(), &["Library", "Item", "Collection"]);
            let bundle = compile_plasm_program(&PromptPipelineConfig::default(), None, &es, "empty_query_view", program).unwrap();
            evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            let st = views_matrix_host_state(ExecutionEngine::new(ExecutionConfig { base_url: Some(base.clone()), ..Default::default() }).unwrap(), cgs.clone());
            let live = run_plasm_comp(&es, &st, es.prompt_hash.as_str(), "empty_query_view", &bundle, true, None, None, None, None).await
                .expect("empty view fanout must succeed");
            let children = live.return_steps.iter().find(|s| s.node_id.as_deref() == Some("items")).expect("returned empty relation");
            assert_eq!(children.result.count, 0, "empty parent set must not read all cached children");
            assert!(children.result.entities.is_empty());
        }
        let es = language_matrix_views::execute_session_for_entities(
            cgs.clone(),
            &["Library", "Item", "Collection"],
        );
        let program = "library = Library{access_token=\"test-token\"} | where access_token = \"absent\" | take 1\nitems = library.items\nitems";
        let bundle = compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            &es,
            "empty_singleton_view",
            program,
        )
        .unwrap();
        evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let st = views_matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .unwrap(),
            cgs,
        );
        let error = run_plasm_comp(
            &es,
            &st,
            es.prompt_hash.as_str(),
            "empty_singleton_view",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("empty bounded receiver must fail rather than read cached parents");
        assert!(
            error.contains("zero rows") || error.contains("0 rows"),
            "unexpected singleton error: {error}"
        );
        server.abort();
    });
}
