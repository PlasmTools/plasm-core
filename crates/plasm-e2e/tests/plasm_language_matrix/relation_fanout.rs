//! Regression for parent references whose child rows leave the graph cache.

use super::*;

#[test]
fn relation_read_fanout_matches_unary_parent_gets() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("relation fanout runtime");
            rt.block_on(async {
                use std::collections::BTreeSet;
                use std::sync::Arc;
                use plasm_agent::plasm_plan_run::run_plasm_comp;

                use axum::{extract::Path, routing::get, Json, Router};
                use serde_json::json;
                let parent = |id: &str, child: &str| json!({
                    "id": id, "title": id, "score": 1, "owner": "alice",
                    "lines": [{"id": child}],
                });
                let router = Router::new()
                    .route("/language/v1/items/search", get(|| async {
                        Json(json!([
                            {"id": "i1", "title": "i1", "score": 1, "owner": "alice"},
                            {"id": "i2", "title": "i2", "score": 1, "owner": "alice"},
                        ]))
                    }))
                    .route("/language/v1/items", get(move || async move {
                        Json(json!([
                            {"id": "i1", "title": "i1", "score": 1, "owner": "alice"},
                            {"id": "i2", "title": "i2", "score": 1, "owner": "alice"},
                        ]))
                    }))
                    .route("/language/v1/items/{id}", get(move |Path(id): Path<String>| async move {
                        Json(parent(&id, if id == "i1" { "l1" } else { "l2" }))
                    }))
                    .route("/language/v1/lines/{id}", get(|Path(id): Path<String>| async move {
                        Json(json!({"item_id": if id == "l1" { "i1" } else { "i2" }, "id": id, "note": "observed"}))
                    }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture bind");
                let base = format!("http://{}", listener.local_addr().expect("fixture address"));
                let server = tokio::spawn(async move { axum::serve(listener, router).await });
                let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
                if let Some(plasm_core::RelationMaterialization::FromParentGet { path }) =
                    cgs.entities.get_mut("LangItem").expect("parent entity")
                        .relations.get_mut("lines").expect("child relation").materialize.as_mut()
                {
                    path.push(plasm_core::JsonPathSegment::Key { key: "id".into() });
                } else {
                    panic!("fixture must use FromParentGet identity extraction");
                }
                cgs.http_backend = base.clone();
                let cgs = Arc::new(cgs);
                let es = language_matrix::matrix_execute_session(cgs.clone());
                let st = language_matrix::matrix_host_state(
                    ExecutionEngine::new(ExecutionConfig {
                        base_url: Some(base),
                        ..Default::default()
                    }).expect("engine"),
                    cgs,
                );
                let mut sets = Vec::new();
                for (name, program) in [
                    ("unary", "first = LangItem(\"i1\").lines\nsecond = LangItem(\"i2\").lines\nfirst, second"),
                    ("fanout", "items = LangItem | take 2\nlines = items => _.lines\nlines"),
                    ("fanout_evicted", "items = LangItem | take 2\nlines = items => _.lines\nlines"),
                    ("unary_evicted", "first = LangItem(\"i1\").lines\nsecond = LangItem(\"i2\").lines\nfirst, second"),
                    ("get_relation_fanout", "items = LangItem | take 2\nlines = items => LangItem(_.id).lines\nlines"),
                    ("bound_get_relation_fanout", "items = LangItem | take 2\nparents = items => LangItem(_.id)\nlines = parents => _.lines\nlines"),
                    ("filtered_candidates", "candidates = LangItem~\"i1\"\nselected = candidates | where title = \"i1\" | where score > 0\nlines = selected => _.lines\nlines"),
                    ("filtered_application", "items = LangItem | take 2\nparents = items => LangItem(_.id)\nselected = parents | where title = \"i1\"\nlines = selected => _.lines\nlines"),
                    ("scalar_filter", "one = LangItem(\"i2\")\nselected = LangItem | where title = one.title | take 1\nlines = selected => _.lines\nlines"),
                    ("template_filter", "one = LangItem(\"i1\")\nselected = LangItem | where title = \"{{ one.title }}\"\nlines = selected => _.lines\nlines"),

                ] {
                    if name == "fanout_evicted" {
                        let mut graph = es.graph_cache.lock().await;
                        for (parent_id, child_id) in [("i1", "l1"), ("i2", "l2")] {
                            let parent = graph.get(&plasm_core::Ref::new("LangItem", parent_id))
                                .expect("parent remains cached");
                            assert_eq!(parent.relations.get("lines").expect("parent retains refs"),
                                &vec![plasm_core::Ref::new("LangLine", child_id)]);
                            assert!(graph.remove(&plasm_core::Ref::new("LangLine", child_id)).is_some(),
                                "eviction must remove a previously cached child");
                        }
                    }
                    let bundle = compile_plasm_program(
                        &PromptPipelineConfig::default(), None, &es, name, program,
                    ).expect("compile relation reads");
                    if matches!(name, "filtered_candidates" | "filtered_application") {
                        let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("candidate filter dry validation");
                        assert_comp_witness(&dry).expect("candidate filter serialized comp witness");
                    }
                    let wire = serde_json::to_vec(&bundle.artifact().comp).expect("serialize executable comp");
                    let bundle = plasm_agent::PlasmCompBundle::new(
                        plasm_agent::PlasmCompArtifact {
                            comp: serde_json::from_slice(&wire).expect("decode executable comp"),
                            approval_gates: bundle.artifact().approval_gates.clone(),
                        }
                    ).expect("admit serialized executable comp");
                    let out = Box::pin(run_plasm_comp(
                        &es, &st, &es.prompt_hash, name, &bundle, true,
                        None, None, None, None,
                    )).await.expect("execute relation reads");
                    eprintln!("{name}: {}", out.run_markdown.as_deref().unwrap_or(""));
                    if name == "fanout_evicted" {
                        assert!(out.return_steps.iter().map(|step| step.result.stats.network_requests).sum::<usize>() >= 2, "follow-up GETs must be counted");
                    }
                    for step in &out.return_steps {
                        for entity in &step.result.entities {
                            assert_eq!(entity.payload_to_json()["note"], "observed", "relation must return hydrated content");
                        }
                        let artifact = step.artifact.as_ref().expect("relation result has an artifact");
                        let bytes = st.run_artifacts.get(&es.prompt_hash, name, artifact.run_id).await.expect("stored relation artifact");
                        let document: serde_json::Value = serde_json::from_slice(&bytes).expect("artifact JSON");
                        let expected: Vec<_> = step.result.entities.iter().map(|entity| entity.payload_to_json()).collect();
                        assert_eq!(document["entities"], serde_json::json!(expected), "artifact must contain hydrated rows");
                    }

                    sets.push(out.return_steps.iter().flat_map(|step| {
                        step.result.entities.iter().map(|entity| entity.reference.to_string())
                    }).collect::<BTreeSet<_>>());
                }
                assert!(!sets[0].is_empty(), "unary fixture must provide child rows");
                assert_eq!(sets[0], BTreeSet::from(["LangLine:l1".into(), "LangLine:l2".into()]));
                assert_eq!(sets[1], sets[0], "fanout lost relation child rows");
                assert_eq!(sets[3], sets[0], "unary reads must rehydrate evicted children");
                assert_eq!(sets[2], sets[0], "fanout treated evicted child rows as an empty relation");
                assert_eq!(sets[4], sets[0], "Get followed by a relation must compose inside apply");
                assert_eq!(sets[5], sets[0], "intermediate Get results must retain entity identity");
                assert_eq!(sets[6], BTreeSet::from(["LangLine:l1".into()]), "nonmatching search candidate must not reach relation traversal");
                assert_eq!(sets[7], sets[6], "filter after a separately bound Get must constrain downstream reads");
                assert_eq!(sets[8], BTreeSet::from(["LangLine:l2".into()]), "scalar binding must constrain downstream traversal before take after serialization");
                assert_eq!(sets[9], sets[6], "template binding must constrain downstream traversal after serialization");
                server.abort();
            });
        })
        .expect("spawn relation fanout harness")
        .join()
        .expect("relation fanout harness");
}
