use crate::plasm_plan_run::{evaluate_plasm_comp_dry, run_plasm_comp};
use crate::{execute_session::ExecuteSession, server_state::PlasmHostState, PlasmCompBundle};
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::plasm_monad::*;
use plasm_core::{
    discovery::CgsRegistry, symbol_tuning::SymbolRender, CgsContext, TeachingExposureSession,
};
use plasm_runtime::{
    auth::ResolvedAuth, ExecutionConfig, ExecutionEngine, ExecutionMode, HttpTransport,
    RuntimeError,
};
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::{
    collections::BTreeSet,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

mod python_lowering;
mod support;
use support::*;

#[test]
fn map_body_parent_composition_runs_as_one_comp() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let (root, body) = program(&es);
        let bundle = compose(root, body).unwrap();
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        assert_eq!(dry.topological_order, vec!["items", "result"]);
        assert_eq!(dry.node_results[1]["body"].as_array().unwrap().len(), 4); // input + three steps
        assert!(calls.lock().unwrap().is_empty());
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            rows(&run),
            vec![
                json!({"title":"Title 0","labels":""}),
                json!({"title":"Title 1","labels":"L0"}),
                json!({"title":"Title 2","labels":"L0|L1"})
            ]
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "/items",
                "/items/i0/tags",
                "/items/i1/tags",
                "/items/i2/tags"
            ]
        );
        assert_eq!(run.code_plan_run_artifacts.len(), 1);
        assert_eq!(run.graph_summary["scope_instances"][0]["completed"], 3);
        assert_eq!(
            run.graph_summary["scope_instances"][0]["instances"][2]["occurrence_path"],
            json!([2])
        );
        assert!(matches!(
            run.comp.as_ref().unwrap().comp.steps["result"],
            PlasmStepPayload::MapBody(_)
        ));
        assert_eq!(
            run.return_steps[0].result.coverage,
            plasm_runtime::ResultCoverage::Complete
        );
    });
}
#[test]
fn map_body_empty_parents_and_hard_budget() {
    on_runtime(async {
        for count in [0, 3] {
            let (es, host, calls) = fixture(count);
            let (root, mut body) = program(&es);
            body.max_parents = NonZeroU32::new(2).unwrap();
            let bundle = compose(root, body).unwrap();
            let run = execute(&es, &host, &bundle).await;
            if count == 0 {
                let run = run.unwrap();
                assert!(rows(&run).is_empty());
                assert_eq!(
                    run.graph_summary["scope_instances"][0]["phase"],
                    "not_invoked"
                );
            } else {
                assert!(run.unwrap_err().contains("budget exceeded"));
            }
            assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
        }
    });
}
#[test]
fn map_body_preflight_rejects_corruption_before_any_read() {
    on_runtime(async {
        let (es, _, calls) = fixture(3);
        for corruption in 0..5 {
            let (root, mut body) = program(&es);
            let PlasmStepPayload::Map(m) = body.body.steps.get_mut("reduced").unwrap() else {
                unreachable!()
            };
            let ComputeOp::Python {
                source,
                entity,
                catalog_hash,
                ..
            } = &mut m.compute.op
            else {
                unreachable!()
            };
            match corruption {
                0 => *entity = "Item".into(),
                1 => *source = source.replace("tag.label", "tag.absent"),
                2 => *catalog_hash = "wrong".into(),
                3 => body.parent.entity = owner("Tag"),
                _ => {
                    body.body
                        .bind
                        .deps
                        .get_mut(&id("output"))
                        .unwrap()
                        .remove(&id("parent"));
                }
            }
            let result = compose(root, body).and_then(|bundle| {
                evaluate_plasm_comp_dry(&es, &bundle)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
            assert!(result.is_err(), "corruption {corruption}");
        }
        assert!(calls.lock().unwrap().is_empty());
    });
}
#[test]
fn map_body_nested_identity_and_schedule_are_sealed() {
    on_runtime(async {
        let (es, _, _) = fixture(0);
        let (root, body) = program(&es);
        let bundle = compose(root.clone(), body.clone()).unwrap();
        let canonical = plasm_core::plasm_comp_commit_canonical(&bundle.artifact().comp);
        for change in 0..4 {
            let mut body = body.clone();
            match change {
                0 => body.max_parents = NonZeroU32::new(255).unwrap(),
                1 => {
                    let PlasmStepPayload::Map(m) = body.body.steps.get_mut("reduced").unwrap()
                    else {
                        unreachable!()
                    };
                    let ComputeOp::Python { source, .. } = &mut m.compute.op else {
                        unreachable!()
                    };
                    *source = source.replace("\"|\"", "\",\"");
                }
                2 => {
                    body.body
                        .bind
                        .deps
                        .get_mut(&id("reduced"))
                        .unwrap()
                        .insert(id("parent"));
                }
                _ => {
                    body.body.name = Some("display only".into());
                    body.body.metadata.insert("expanded".into(), json!(true));
                }
            }
            let changed = compose(root.clone(), body).unwrap();
            let actual = plasm_core::plasm_comp_commit_canonical(&changed.artifact().comp);
            assert_eq!(canonical == actual, change == 3);
            if change == 2 {
                let a = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
                let b = evaluate_plasm_comp_dry(&es, &changed).unwrap();
                assert_ne!(
                    crate::plasm_plan_run::ScheduleDigest::from_validated_plan(
                        a.validated_plan(),
                        &a.topological_order
                    ),
                    crate::plasm_plan_run::ScheduleDigest::from_validated_plan(
                        b.validated_plan(),
                        &b.topological_order
                    )
                );
            }
        }
    });
}
#[test]
fn map_body_partial_child_fails_and_preserves_empty_child() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let (root, mut body) = program(&es);
        let PlasmStepPayload::Invoke(child) = body.body.steps.get_mut("children").unwrap() else {
            unreachable!()
        };
        child.page_size = Some(1);
        let bundle = compose(root, body).unwrap();
        let error = execute(&es, &host, &bundle).await.unwrap_err();
        assert!(error.contains("complete collection coverage"), "{error}");
        assert!(error.contains("occurrence 2"), "{error}");
        assert!(calls.lock().unwrap().iter().any(|p| p == "/items/i2/tags"));
    });
}

#[test]
fn map_body_uses_existing_commit_replay_and_rejects_tampering() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let (root, body) = program(&es);
        let bundle = compose(root.clone(), body.clone()).unwrap();
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        assert!(dry.probe_preflight_passed());
        let commit_ref = es.mint_plan_commit_ref();
        let record = crate::operation::PlanCommitRecord::from_dry_review(
            commit_ref.clone(),
            crate::operation::compute_plan_commit_id_from_dry(&dry),
            es.domain_revision,
            &dry,
            "typed parent composition".into(),
            crate::plan_dry_display::PlanDryVerdict::Review,
            std::time::Instant::now() + crate::operation::PLAN_COMMIT_TTL,
        )
        .unwrap();
        es.register_plan_commit(record);
        let committed = crate::plan_commit_store::resolve_committed_plan(&es, &commit_ref).unwrap();
        crate::plan_commit_store::verify_committed_plan_bundle(&bundle, &committed).unwrap();
        let cached =
            crate::plan_commit_store::dry_for_committed_plasm_run(&es, &bundle, &committed)
                .unwrap();
        let run = run_plasm_comp(
            &es,
            &host,
            &es.prompt_hash,
            "replayed",
            &bundle,
            true,
            None,
            None,
            Some(cached),
            None,
        )
        .await
        .unwrap();
        assert_eq!(rows(&run).len(), 3);
        let mut changed = body;
        changed.max_parents = NonZeroU32::new(1).unwrap();
        assert!(crate::plan_commit_store::verify_committed_plan_bundle(
            &compose(root, changed).unwrap(),
            &committed
        )
        .is_err());
    });
}
#[test]
fn map_body_cancellation_stops_remaining_occurrences() {
    on_runtime(async {
        let scope = crate::operation::ExecutionScope::new();
        let (es, host, calls) = fixture_with_cancel(3, Some(scope.clone()));
        let (root, body) = program(&es);
        let bundle = compose(root, body).unwrap();
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let error = run_plasm_comp(
            &es,
            &host,
            &es.prompt_hash,
            "cancelled",
            &bundle,
            true,
            None,
            Some(&scope),
            Some(dry),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        assert_eq!(*calls.lock().unwrap(), vec!["/items", "/items/i0/tags"]);
    });
}
#[test]
fn map_body_two_scopes_have_distinct_occurrence_addresses() {
    on_runtime(async {
        let (es, host, _) = fixture(2);
        let (root, body) = program(&es);
        let bundle = compose(root, body.clone()).unwrap();
        let mut comp = bundle.artifact().comp.clone();
        comp.steps
            .insert("second".into(), PlasmStepPayload::MapBody(Box::new(body)));
        comp.bind.topo.push(id("second"));
        comp.bind
            .deps
            .insert(id("second"), BTreeSet::from([id("items")]));
        comp.return_ = PlasmReturn::Parallel {
            steps: vec![id("result"), id("second")],
        };
        let bundle = PlasmCompBundle::new(
            crate::plasm_comp_wire::plasm_comp_artifact_from_comp(comp).unwrap(),
        )
        .unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(run.return_steps.len(), 2);
        let scopes = run.graph_summary["scope_instances"].as_array().unwrap();
        assert_eq!(scopes.len(), 2);
        assert_ne!(scopes[0]["scope_path"], scopes[1]["scope_path"]);
        assert_eq!(
            scopes[0]["instances"][0]["steps"][0]["address"]["local_step"],
            scopes[1]["instances"][0]["steps"][0]["address"]["local_step"]
        );
    });
}
#[test]
fn map_body_flow_keeps_child_read_provenance_on_parent_result() {
    on_runtime(async {
        let (es, _, _) = fixture(0);
        let (root, body) = program(&es);
        let bundle = compose(root, body).unwrap();
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let facts = dry.flow.node_facts["result"].row_join();
        assert!(
            facts.provenance.iter().any(|p| p.contains("item_query")),
            "{facts:?}"
        );
        assert!(
            facts.provenance.iter().any(|p| p.contains("tag_query")),
            "{facts:?}"
        );
        assert!(dry.review.has_full_collection_compute);
    });
}

#[test]
fn map_body_captured_relation_uses_real_parent_identity() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let (root, mut body) = program(&es);
        let mut get = plasm_core::GetExpr::from_ref(plasm_core::Ref::simple_binding(
            "Item",
            plasm_core::PlasmInputRef::node_output("parent", vec!["id".into()]),
        ));
        get.catalog_entry_id = plasm_core::CatalogEntryStamp::some("fixture".into());
        let expr = plasm_core::Expr::Chain(plasm_core::ChainExpr::auto_get(
            plasm_core::Expr::Get(get),
            "tags",
        ));
        body.body.steps.insert(
            "children".into(),
            PlasmStepPayload::FlatMapRelation(FlatMapRelationPayload {
                relation: PlanRelationTraversal {
                    source: "parent".into(),
                    relation: "tags".into(),
                    target: owner("Tag"),
                    cardinality: RelationCardinality::Many,
                    source_cardinality: RelationSourceCardinality::Single,
                    expr: String::new(),
                    ir: PlanExprIr {
                        expr,
                        projection: None,
                        display_expr: None,
                    },
                    binding_proofs: vec![],
                    materialize: es.cgs.get_entity("Item").unwrap().relations["tags"]
                        .materialize
                        .clone(),
                    view_embed_proof: None,
                },
                effect_class: EffectClass::Read,
                result_shape: ResultShape::List,
            }),
        );
        for corrupt_owner in [false, true] {
            let mut corrupted = body.clone();
            let PlasmStepPayload::FlatMapRelation(payload) =
                corrupted.body.steps.get_mut("children").unwrap()
            else {
                unreachable!()
            };
            if corrupt_owner {
                let plasm_core::Expr::Chain(chain) = &mut payload.relation.ir.expr else {
                    unreachable!()
                };
                let plasm_core::Expr::Get(get) = &mut *chain.source else {
                    unreachable!()
                };
                get.reference.entity_type = "Tag".into();
            } else {
                payload.relation.materialize = None;
            }
            let rejected = compose(root.clone(), corrupted);
            if let Ok(bundle) = rejected {
                assert!(evaluate_plasm_comp_dry(&es, &bundle).is_err());
            }
            assert!(calls.lock().unwrap().is_empty());
        }
        let bundle = compose(root, body).unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            rows(&run),
            vec![
                json!({"title":"Title 0","labels":""}),
                json!({"title":"Title 1","labels":"L0"}),
                json!({"title":"Title 2","labels":"L0|L1"})
            ]
        );
        if let Ok(path) = std::env::var("PLASM_MAP_BODY_FIXTURE_PATH") {
            std::fs::write(
                path,
                serde_json::to_string_pretty(&serde_json::json!({
                    "comp": bundle.artifact().comp,
                    "scope_instances":run.graph_summary["scope_instances"], "rows":rows(&run)
                }))
                .unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "/items",
                "/items/i0/tags",
                "/items/i1/tags",
                "/items/i2/tags"
            ]
        );
    });
}

mod live_demo;
mod streaming;
