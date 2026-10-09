//! View DAG conformance — preflight + catalog validation against `plasm_language_matrix_views`.
//!
//! Hermit is reserved for transport/decode regressions; view wiring lives here (fast, deterministic).

#[path = "common/hermit_lang_matrix.rs"]
#[allow(dead_code)]
mod hermit_lang_matrix;

#[path = "common/language_matrix_views.rs"]
mod language_matrix_views;

use std::sync::Arc;

#[path = "plasm_language_matrix_views/python.rs"]
mod python_programs;
use plasm_agent::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use plasm_compile::{validate_cgs_capability_templates, validate_cgs_views};
use plasm_core::QueryExpr;
use plasm_runtime::{
    preflight_view_query,
    view_test_support::{matrix_view_query, matrix_views_cgs, MATRIX_VIEW_PREFLIGHT_CASES},
    SessionMaterialization, ViewAmbientContext,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};
use python_programs::compile_views_program;

use language_matrix_views::{
    language_matrix_views_schema_dir, load_language_matrix_views_cgs, views_execute_session,
    views_matrix_host_state, VIEWS_MATRIX_ENTRY_ID,
};

fn block_on_views_live<Make, F>(make: Make)
where
    Make: Fn() -> F + Send + 'static,
    // The future is created and polled inside the test thread; only Make crosses threads.
    F: std::future::Future<Output = ()> + 'static,
{
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("views live runtime");
            rt.block_on(make());
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

#[tokio::test]
async fn matrix_views_all_preflight() {
    let cgs = matrix_views_cgs();
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).expect("compile CML");
    let ambient = ViewAmbientContext::default();
    let entities: Vec<_> = MATRIX_VIEW_PREFLIGHT_CASES
        .iter()
        .map(|(_, entity)| *entity)
        .collect();
    let es = language_matrix_views::execute_session_for_entities(Arc::new(cgs.clone()), &entities);
    let symbols = es.teaching_exposure.as_ref().unwrap().symbol_map_arc();
    for &(view_name, entity) in MATRIX_VIEW_PREFLIGHT_CASES {
        let query = matrix_view_query(entity);
        let symbol = symbols.entity_sym_for(VIEWS_MATRIX_ENTRY_ID, entity);
        // These are precisely matrix_view_query's scopes: item-1 for scoped views,
        // no scope for the two dashboard views. Exercise public Python admission
        // and planning before the same view-DAG preflight contract below.
        let args = if matches!(entity, "LangWorkSnapshot" | "LangWorkSnapshotEmpty") {
            ""
        } else {
            "item_id=\"item-1\""
        };
        let source = format!("class ViewPreflight(Program):\n    def build(self):\n        return {symbol}.query({args})\n");
        let bundle = plasm_agent::plasm_compile::compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|err| panic!("{view_name} Python admission: {err}"));
        evaluate_plasm_comp_dry(&es, &bundle)
            .unwrap_or_else(|err| panic!("{view_name} Python dry: {err}"));
        let lowered_query = bundle
            .artifact()
            .comp
            .steps
            .values()
            .find_map(|step| {
                let plasm_core::plasm_monad::PlasmStepPayload::Invoke(invoke) = step else {
                    return None;
                };
                let plasm_core::Expr::Query(query) = &invoke.ir.as_ref()?.expr else {
                    return None;
                };
                Some(query)
            })
            .expect("Python view query IR");
        assert_eq!(lowered_query.entity, query.entity, "{view_name} entity");
        assert_eq!(
            lowered_query.predicate, query.predicate,
            "{view_name} scope"
        );
        preflight_view_query(
            view_name,
            lowered_query,
            &cgs,
            &compiled,
            &ambient,
            &SessionMaterialization::new(),
        )
        .unwrap_or_else(|err| panic!("{view_name} preflight: {err}"));
    }
}

#[tokio::test]
async fn matrix_views_missing_scope_preflight_errors() {
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
    let es = language_matrix_views::execute_session_for_entities(Arc::new(cgs), &["LangDigest"]);
    let symbols = es.teaching_exposure.as_ref().unwrap().symbol_map_arc();
    let symbol = symbols.entity_sym_for(VIEWS_MATRIX_ENTRY_ID, "LangDigest");
    let source = format!(
        "class MissingScope(Program):\n    def build(self):\n        return {symbol}.query()\n"
    );
    let err = plasm_agent::plasm_compile::compile_python_program(&es, &source)
        .await
        .expect_err("Python must reject missing required view scope");
    assert!(err.to_string().contains("item_id"), "{err}");
}

/// Row-to-text render must persist wire-name column aliases in the comp wire. Fields bind by wire
/// name — the former `p#` field-symbol scheme was removed, so teaching tokens for fields ARE the
/// wire names and the alias keys must be those wire names.
#[test]
fn matrix_views_row_to_text_wire_column_aliases() {
    block_on_views_live(|| async move {
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
        let bundle = python_programs::render(
            es.as_ref(),
            false,
            "row",
            "id, title",
            "f\"- {row.id}: {row.title}\\n\"",
        )
        .await
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
    block_on_views_live(|| async move {
        // The program filters for i1; its backend must preserve fixture identities.
        let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
        let cgs = load_language_matrix_views_cgs();
        plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
        let es = Arc::new(views_execute_session(cgs.clone()));
        let bundle = python_programs::render(
            es.as_ref(),
            true,
            "r",
            "id, title, score",
            "f\"- {r.id}: {r.title} (score: {r.score or '—'})\\n\"",
        )
        .await
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
    block_on_views_live(|| async move {
        // The program filters for i1; its backend must preserve fixture identities.
        let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
        let cgs = load_language_matrix_views_cgs();
        plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");
        let es = Arc::new(views_execute_session(cgs.clone()));
        let bundle = python_programs::render(
            es.as_ref(),
            true,
            "entry",
            "id, title",
            "f\"- {entry.id}: {entry.title or '—'}\\n\"",
        )
        .await
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
    block_on_views_live(|| async move {
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
        let program = "return LangTriageContext.get(\"i1\").tags";
        let bundle = compile_views_program(es.as_ref(), program)
            .await
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
    block_on_views_live(|| async move {
        let base = hermit_lang_matrix::language_matrix_hermit_base_url()
            .await
            .clone();
        let cgs = load_language_matrix_views_cgs();
        for program in [
            "parent = LangTriageContext.query(item_id=\"i1\")\ntags = parent.flat_map(lambda row: row.tags)\nreturn tags",
            "parent = LangTriageContext.query(item_id=\"i1\")\ntags = parent.tags\nreturn tags",
            "parent = LangTriageContext.query(item_id=\"i1\").take(1)\ntags = parent.flat_map(lambda row: row.tags)\nreturn tags",
        ] {
            let es = Arc::new(views_execute_session(cgs.clone()));
            let bundle = compile_views_program(es.as_ref(), program)
                .await
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
    block_on_views_live(|| async move {
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
        let items_rel =
            map.ident_sym_relation_for(VIEWS_MATRIX_ENTRY_ID, "LangWorkSnapshot", "items");
        // Pathless dashboard Get: taught `e#.m#().r#` (receiver none), not kebab `e#.slug()`.
        let program = format!("return {esym}.get().{items_rel}");
        let bundle = compile_views_program(es.as_ref(), &program)
            .await
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
    block_on_views_live(|| async move {
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
        let items_rel =
            map.ident_sym_relation_for(VIEWS_MATRIX_ENTRY_ID, "LangWorkSnapshotEmpty", "items");
        // Pathless dashboard Get: taught `e#.m#().r#` (receiver none), not kebab `e#.slug()`.
        let program = format!("return {esym}.get().{items_rel}");
        let bundle = compile_views_program(es.as_ref(), &program)
            .await
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
    let program = "return LangTriageContext.get(\"i2\").tags";
    {
        let bundle = compile_views_program(es.as_ref(), program)
            .await
            .expect("compile empty scoped tags relation");
        evaluate_plasm_comp_dry(es.as_ref(), &bundle).expect("dry empty scoped tags");
    }
}

#[tokio::test]
async fn matrix_views_python_rejects_unmaterialized_many_relation_before_normalize() {
    use plasm_core::loader::load_schema_dir_unvalidated;
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

    let es = views_execute_session(Arc::new(cgs));
    let error = compile_views_program(&es, "return LangTriageContext.get(\"i1\").tags")
        .await
        .expect_err("Python must also reject an unmaterialized many relation");
    assert!(
        error.contains("materializ"),
        "unexpected rejection: {error}"
    );
}

#[tokio::test]
async fn matrix_views_rowsets_taught_navigation_compiles_and_preflights() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/view_rowsets");
    let cgs = Arc::new(plasm_core::loader::load_schema_dir(&dir).unwrap());
    let session = language_matrix_views::execute_session_for_entities(
        cgs,
        &["Library", "Item", "Collection"],
    );
    {
        for program in [
            "items = Library.query(access_token=\"test-token\").items\nreturn items",
            "library = Library.query(access_token=\"test-token\")\nitems = library.items\nreturn items",
        ] {
            let bundle = compile_views_program(&session, program)
                .await
                .expect("ordinary taught query and relation navigation");
            evaluate_plasm_comp_dry(&session, &bundle).expect("dry composed rowset view");
        }
    }
}

#[test]
fn matrix_views_query_only_parent_relations_live() {
    block_on_views_live(|| async move {
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
            "library = Library.query(access_token=\"test-token\")\nitems = library.flat_map(lambda row: row.items)\nreturn items",
            "library = Library.query(access_token=\"test-token\")\nitems = library.items\nreturn items",
            "library = Library.query(access_token=\"test-token\").take(1)\nitems = library.flat_map(lambda row: row.items)\nreturn items",
            "library = Library.query(access_token=\"test-token\").take(1)\nitems = library.items\nreturn items",
            "library = Library.query(access_token=\"test-token\")\ncopy = library.select(\"access_token\")\nitems = copy.flat_map(lambda row: row.items)\nreturn items",
            "library = Library.query(access_token=\"test-token\").select(\"access_token\")\nitems = library.flat_map(lambda row: row.items)\nreturn items",
        ] {
            let es = language_matrix_views::execute_session_for_entities(cgs.clone(), &["Library", "Item", "Collection"]);
            let bundle = compile_views_program(&es, program).await.unwrap_or_else(|e| panic!("compile {program}: {e}"));
            evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            let st = views_matrix_host_state(ExecutionEngine::new(ExecutionConfig { base_url: Some(base.clone()), ..Default::default() }).unwrap(), cgs.clone());
            let live = run_plasm_comp(&es, &st, es.prompt_hash.as_str(), "query_only_view", &bundle, true, None, None, None, None).await
                .expect("query-only view relation must execute");
            let children = live.return_steps.iter().find(|s| s.node_id.as_deref() == Some("items")).expect("returned child relation");
            let titles: std::collections::BTreeSet<_> = children.result.entities().iter().map(|e| e.fields.get("title").unwrap().to_value().as_str().unwrap().to_string()).collect();
            assert_eq!(titles, std::collections::BTreeSet::from(["one".into(), "two".into(), "three".into()]));
            assert_eq!(children.result.entities().len(), 3, "overlapping child IDs must deduplicate");
        }
        for program in [
            "library = Library.query(access_token=\"test-token\").where(lambda row: row.access_token == \"absent\")\nitems = library.flat_map(lambda row: row.items)\nreturn items",
        ] {
            let es = language_matrix_views::execute_session_for_entities(cgs.clone(), &["Library", "Item", "Collection"]);
            let bundle = compile_views_program(&es, program).await.unwrap();
            evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            let st = views_matrix_host_state(ExecutionEngine::new(ExecutionConfig { base_url: Some(base.clone()), ..Default::default() }).unwrap(), cgs.clone());
            let live = run_plasm_comp(&es, &st, es.prompt_hash.as_str(), "empty_query_view", &bundle, true, None, None, None, None).await
                .expect("empty view fanout must succeed");
            let children = live.return_steps.iter().find(|s| s.node_id.as_deref() == Some("items")).expect("returned empty relation");
            assert_eq!(children.result.count(), 0, "empty parent set must not read all cached children");
            assert!(children.result.entities().is_empty());
        }
        let es = language_matrix_views::execute_session_for_entities(
            cgs.clone(),
            &["Library", "Item", "Collection"],
        );
        let program = "library = Library.query(access_token=\"test-token\").where(lambda row: row.access_token == \"absent\").take(1)\nitems = library.items\nreturn items";
        let bundle = compile_views_program(&es, program).await.unwrap();
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
            error.diagnostic().contains("zero rows") || error.diagnostic().contains("0 rows"),
            "unexpected singleton error: {error}"
        );
        server.abort();
    });
}

/// Whole-collection compute must retain proven coverage through view relations.
#[test]
fn matrix_views_relation_collection_compute_coverage() {
    block_on_views_live(|| async move {
        use axum::{
            extract::{Path, Query},
            routing::get,
            Json,
        };
        use serde_json::json;
        let item =
            |id: &str, score: i64| json!({"id":id,"title":id,"score":score,"owner":"viewer"});
        let app = axum::Router::new()
            .route(
                "/language/v1/viewer",
                get(|| async { Json(json!({"id":"v1","display_name":"viewer"})) }),
            )
            .route(
                "/language/v1/viewer/nobody",
                get(|| async { Json(json!({"id":"v0","display_name":"nobody"})) }),
            )
            .route(
                "/language/v1/items",
                get(
                    move |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                        Json(if q.get("owner").is_some_and(|s| s == "nobody") {
                            json!([])
                        } else {
                            json!([item("i1", 3), item("i2", 1), item("i3", 2)])
                        })
                    },
                ),
            )
            .route(
                "/language/v1/items/{id}",
                get(move |Path(id): Path<String>| async move {
                    Json(item(
                        &id,
                        match id.as_str() {
                            "i1" => 3,
                            "i2" => 1,
                            _ => 2,
                        },
                    ))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let transforms = [
            ("LangWorkSnapshot", "", "i1|i2|i3"),
            (
                "LangWorkSnapshot",
                concat!(
                    ".where(lambda r: r.score is not None and r.score >= 2)",
                    ".order_by(\"score\", descending=True).take(2).select(\"title\")",
                ),
                "i1|i3",
            ),
            (
                "LangWorkSnapshot",
                ".where(lambda r: r.score is not None and r.score > 99).select(\"title\")",
                "",
            ),
            ("LangWorkSnapshotEmpty", "", ""),
        ];
        for acquisition in ["get", "query"] {
            for (root, transform, expected) in transforms {
                let cgs = load_language_matrix_views_cgs();
                let es = views_execute_session(cgs.clone());
                let map = es.teaching_exposure.as_ref().unwrap().symbol_map_arc();
                let entity = map.entity_sym_for(VIEWS_MATRIX_ENTRY_ID, root);
                let relation = map.ident_sym_relation_for(VIEWS_MATRIX_ENTRY_ID, root, "items");
                let source = format!(
                    r#"class Render(Program):
    @compute
    def render(self, rows: list[Row]) -> str:
        return "|".join(row.title for row in rows)
    def build(self):
        rows = {entity}.{acquisition}().{relation}{transform}
        return self.render(rows)
"#
                );
                let bundle = plasm_agent::plasm_compile::compile_python_program(&es, &source)
                    .await
                    .unwrap();
                evaluate_plasm_comp_dry(&es, &bundle).unwrap();
                let host = views_matrix_host_state(
                    ExecutionEngine::new(ExecutionConfig {
                        base_url: Some(base.clone()),
                        ..Default::default()
                    })
                    .unwrap(),
                    cgs,
                );
                let live = plasm_agent::plasm_plan_run::run_plasm_comp_python(
                    &es,
                    &host,
                    &es.prompt_hash,
                    "view_collection_coverage",
                    &bundle,
                    true,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .unwrap_or_else(|error| panic!("{root}/{acquisition}/{transform}: {error}"));
                let result = &live.return_steps[0].result;
                assert_eq!(result.coverage(), plasm_runtime::ResultCoverage::Complete);
                assert!(
                    !result.has_more
                        && result.paging_handle.is_none()
                        && result.pagination_resume.is_none()
                );
                assert_eq!(result.count(), 1);
                assert_eq!(
                    result.entities()[0].fields["value"].to_value(),
                    plasm_core::Value::String(expected.into())
                );
            }
        }
        server.abort();
    });
}
