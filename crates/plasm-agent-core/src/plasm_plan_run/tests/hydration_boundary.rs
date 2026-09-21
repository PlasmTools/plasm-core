//! Compile/serde/live-transport regression boundary; no production catalog dependencies.
use crate::execute_session::ExecuteSession;
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::{discovery::CgsRegistry, CgsContext, TeachingExposureSession, Value};
use plasm_runtime::{
    auth::ResolvedAuth, ExecutionConfig, ExecutionEngine, ExecutionMode, HttpTransport,
    RuntimeError,
};
use proptest::prelude::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Transport {
    calls: Arc<Mutex<Vec<String>>>,
    token: String,
    ids: Vec<i64>,
    full_embed: bool,
    delayed: bool,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send_compiled_http(
        &self,
        _: &str,
        req: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        self.calls.lock().unwrap().push(req.path.clone());
        let parts: Vec<_> = req.path.trim_matches('/').split('/').collect();
        let body = match parts.as_slice() {
            ["login"] => json!({"access_token":self.token}),
            ["saved", id] => json!({"note_id":id.parse::<i64>().unwrap()}),
            ["saved"] => json!(self
                .ids
                .iter()
                .map(|id| json!({"note_id":id}))
                .collect::<Vec<_>>()),
            ["folders", _] => {
                json!({"id":"root", "notes":self.ids.iter().map(|id| json!({"note_id":id})).collect::<Vec<_>>()})
            }
            ["notes", id] => {
                let id: i64 = id.parse().expect("note identity must be a wire integer");
                {
                    if self.delayed {
                        tokio::time::sleep(std::time::Duration::from_millis((id % 3) as u64)).await;
                    }
                    let owner = if self.full_embed && id % 2 == 0 {
                        json!({"owner_id":id,"name":format!("owner-{id}"),"_tag":format!("tag-{id}"),"description":null})
                    } else {
                        json!({"owner_id":id})
                    };
                    json!({"note_id":id,"title":format!("note-{id}"),"owners":[owner]})
                }
            }
            ["owners", id] => {
                let id: i64 = id
                    .parse()
                    .expect("owner identity must be a wire integer, never a display Ref");
                json!({"owner_id":id,"name":format!("owner-{id}"),"_tag":format!("tag-{id}"),"description":null})
            }
            _ => panic!("unexpected path {}", req.path),
        };
        if matches!(
            parts.first(),
            Some(&"folders") | Some(&"notes") | Some(&"saved")
        ) {
            let Some(Value::Object(headers)) = &req.headers else {
                panic!("missing headers");
            };
            assert_eq!(
                headers.get("Authorization"),
                Some(&Value::String(self.token.clone()))
            );
        }
        Ok((body, None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute GET")
    }
}
fn session() -> ExecuteSession {
    let mut schema = plasm_core::load_schema(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/hydration_boundary_matrix"),
    )
    .unwrap();
    schema.bind_registry_entry_id("matrix");
    let cgs = Arc::new(
        serde_json::from_slice::<plasm_core::CGS>(&serde_json::to_vec(&schema).unwrap()).unwrap(),
    );
    let contexts = indexmap::IndexMap::from([(
        "matrix".into(),
        Arc::new(CgsContext::entry("matrix", cgs.clone())),
    )]);
    let wave = ["Session", "Folder", "Note", "Owner", "SavedNote"];
    let teaching = TeachingExposureSession::new(&cgs, "matrix", &wave);
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        contexts,
        "matrix".into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| s.to_string()).collect(),
        Some(teaching),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}
async fn run(program: &str, token: String) -> Result<super::super::PlasmPlanRunResult, String> {
    run_case(program, token, vec![1, 2], false, false).await
}
async fn run_case(
    program: &str,
    token: String,
    ids: Vec<i64>,
    full_embed: bool,
    delayed: bool,
) -> Result<super::super::PlasmPlanRunResult, String> {
    let es = session();
    let calls = Arc::new(Mutex::new(vec![]));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            ..Default::default()
        },
        Arc::new(Transport {
            calls: calls.clone(),
            token,
            ids,
            full_embed,
            delayed,
        }),
        None,
    );
    let host = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(CgsRegistry::from_pairs(vec![(
            "matrix".into(),
            "Matrix".into(),
            vec![],
            es.cgs.clone(),
        )])),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    });
    let bundle = crate::compile_plasm_program(&Default::default(), None, &es, "boundary", program)
        .expect("compile");
    let comp =
        serde_json::from_slice(&serde_json::to_vec(&bundle.artifact().comp).unwrap()).unwrap();
    let bundle = crate::PlasmCompBundle::new(
        crate::plasm_comp_wire::plasm_comp_artifact_from_comp(comp).unwrap(),
    )
    .unwrap();
    super::super::evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    Box::pin(super::super::run_plasm_comp(
        &es,
        &host,
        &es.prompt_hash,
        "boundary",
        &bundle,
        true,
        None,
        None,
        Some(super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap()),
        None,
    ))
    .await
    .map_err(|e| format!("cold: {e}; calls={:?}", calls.lock().unwrap()))?;
    let result = Box::pin(super::super::run_plasm_comp(
        &es,
        &host,
        &es.prompt_hash,
        "boundary",
        &bundle,
        true,
        None,
        None,
        Some(super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap()),
        None,
    ))
    .await;
    result.map_err(|e| format!("{e}; calls={:?}", calls.lock().unwrap()))
}
#[test]
fn boundary_relation_hydration_preserves_provision_and_wire_identity() {
    on_runtime(async {
        let r=run("auth = Session.login()\nfolder = Folder(\"root\")\nnotes = folder.notes\nowners = notes => _.owners\nowners", "opaque-token".into()).await.expect("live hydration");
        assert!(!r.return_steps.is_empty());
    });
}
#[test]
fn boundary_union_mixed_materialization_ignores_internal_metadata() {
    on_runtime(async {
        run("auth = Session.login()\na = Owner(1)\nb = Owner(2) | select owner_id, name, description, _tag\na | union b", "opaque-token".into()).await.expect("live union");
    });
}

fn on_runtime(future: impl std::future::Future<Output = ()> + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(future);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn boundary_scoped_relation_preserves_provision() {
    on_runtime(async {
        run("auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => _.note\nnotes", "opaque-token".into()).await.expect("scoped hydration");
    });
}
#[test]
fn boundary_applied_get_relation_preserves_wire_identity() {
    on_runtime(async {
        run("auth = Session.login()\nfolder = Folder(\"root\")\nnotes = folder.notes\nowners = notes => Note(_.note_id).owners\nowners", "opaque-token".into()).await.expect("applied relation hydration");
    });
}

#[test]
fn boundary_fanout_get_then_relation_preserves_wire_identity() {
    on_runtime(async {
        run("auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => Note(_.note_id)\nowners = notes => _.owners\nowners", "opaque-token".into()).await.expect("fanout relation hydration");
    });
}

#[test]
fn boundary_scoped_get_then_relation_preserves_wire_identity() {
    on_runtime(async {
        run("auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => _.note\nowners = notes => _.owners\nowners", "opaque-token".into()).await.expect("scoped then embedded relation hydration");
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn boundary_hydration_roundtrip_preserves_rows(
        ids in prop::collection::vec(1i64..1000, 1..22),
        token in "[a-zA-Z0-9:_-]{1,30}",
        full_embed in any::<bool>(),
        delayed in any::<bool>(),
        scoped in any::<bool>(),
    ) {
        on_runtime(async move {
            let get = if scoped {"_.note"} else {"Note(_.note_id)"};
            let program = format!("auth = Session.login()\nsaved = SavedNote{{access_token=auth.access_token}}\nnotes = saved => {get}\nowners = notes => _.owners\nowners");
            let result = run_case(&program, token, ids.clone(), full_embed, delayed).await.expect("serialized live hydration");
            let rows = &result.return_steps.iter().find(|step|step.name.as_deref()==Some("owners")).expect("owners return").result.entities;
            let cgs = session().cgs;
            let actual: Vec<_> = rows.iter().map(|row| {
                let value = plasm_runtime::entity_to_agent_row_json(row, Some(&cgs));
                value["owner_id"].as_i64().expect("integer identity")
            }).collect();
            assert_eq!(actual, ids, "identity, order and multiplicity survive the boundary");
        });
    }
}

#[test]
fn boundary_two_branches_retain_embedded_identities() {
    on_runtime(async {
        run_case("auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nother = SavedNote{access_token=auth.access_token}\nnotes = saved => Note(_.note_id)\nowners = notes => _.owners\nother_owners = other => Note(_.note_id).owners\nowners, other_owners", "token".into(), (1..=18).collect(), true, true).await.expect("two fanout branches");
    });
}

#[test]
fn boundary_cached_relation_row_roundtrip_retains_typed_identity() {
    on_runtime(async {
        let result = run("auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => Note(_.note_id)\nnotes", "token".into()).await.unwrap();
        let cgs = session().cgs;
        let notes = &result
            .return_steps
            .iter()
            .find(|step| step.name.as_deref() == Some("notes"))
            .unwrap()
            .result
            .entities;
        for note in notes {
            let wire = plasm_runtime::entity_to_row_json(note, Some(&cgs));
            let wire: serde_json::Value =
                serde_json::from_slice(&serde_json::to_vec(&wire).unwrap()).unwrap();
            let restored = plasm_runtime::CachedEntity::from_row_json("Note", &wire, &cgs).unwrap();
            assert_eq!(restored.relations, note.relations);
            let children = wire["owners"].as_array().unwrap();
            let decoded =
                super::super::json_rows_to_entities_with_refs("Owner", children, Some(&cgs))
                    .unwrap();
            for child in &decoded {
                let program = format!(
                    "owner = Owner({})\nowner",
                    child.reference.primary_slot_str()
                );
                let hydrated = run(&program, "unused".into())
                    .await
                    .expect("decoded identity reaches integer transport");
                assert_eq!(
                    hydrated.return_steps[0].result.entities[0].reference,
                    child.reference
                );
            }
            assert_eq!(
                decoded
                    .iter()
                    .map(|row| row.reference.clone())
                    .collect::<Vec<_>>(),
                note.relations["owners"],
                "internal relation JSON must be executable wire identity, not display text"
            );
        }
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn boundary_union_uses_public_values_for_stable_distinct(ids in prop::collection::vec(1i64..12, 1..12)) {
        on_runtime(async move {
            let result = run_case("auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => Note(_.note_id)\nowners = notes => _.owners\nprojected = owners | select owner_id, name, description, _tag\nmerged = owners | union projected\nmerged", "token".into(), ids.clone(), false, true).await.expect("public union");
            let actual: Vec<_> = result.return_steps.iter().find(|step| step.name.as_deref()==Some("merged")).unwrap().result.entities.iter().map(|row| {
                let value = plasm_runtime::entity_to_agent_row_json(row, None);
                let id = value["owner_id"].as_i64().unwrap();
                assert_eq!(value["_tag"], format!("tag-{id}"), "declared underscore columns are public values");
                id
            }).collect();
            let mut expected = Vec::new();
            for id in ids { if !expected.contains(&id) { expected.push(id); } }
            assert_eq!(actual, expected);
        });
    }
}

#[test]
fn boundary_union_rejects_different_public_schemas() {
    let es = session();
    assert!(crate::compile_plasm_program(
        &Default::default(),
        None,
        &es,
        "bad-union",
        "a = Owner(1) | select name\nb = Owner(2) | select owner_id\na | union b"
    )
    .is_err());
}

#[test]
fn boundary_union_retains_identity_for_following_relation() {
    on_runtime(async {
        let result = run("auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => Note(_.note_id)\ncopy = notes | select note_id, title, body, owners\nmerged = notes | union copy\nowners = merged => Note(_.note_id).owners\nowners", "token".into()).await.expect("relation after union");
        let rows = &result
            .return_steps
            .iter()
            .find(|step| step.name.as_deref() == Some("owners"))
            .unwrap()
            .result
            .entities;
        assert_eq!(
            rows.iter()
                .map(|row| row.reference.primary_slot_str())
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
    });
}

proptest::proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn sibling_sample_preserves_source_rows_across_serialized_execution(
        ids in prop::collection::vec(1i64..20, 2..30), cap in 1usize..8, bounded_source in any::<bool>(),
    ) {
        on_runtime(async move {
            let boundary=if bounded_source {format!(" | take {}",cap+2)} else {String::new()};
            let program=format!("auth = Session.login()\nsaved = SavedNote{{access_token=auth.access_token}}{boundary}\nsample = saved | take {cap}\nhigh = saved | order by note_id desc | take {cap}\nsaved, sample, high");
            let result=run_case(&program,"sample-token".into(),ids.clone(),false,false).await.expect("shared read");
            let cgs=session().cgs;
            let source=if bounded_source {&ids[..ids.len().min(cap+2)]} else {ids.as_slice()};
            let mut high=source.to_vec();high.sort_by(|a,b|b.cmp(a));high.truncate(cap);
            for (name, expected) in [("saved",source),("sample",&source[..source.len().min(cap)]),("high",high.as_slice())] {
                let step=result.return_steps.iter().find(|s|s.name.as_deref()==Some(name)).expect("returned binding");
                let actual:Vec<_>=step.result.entities.iter().map(|row|plasm_runtime::entity_to_agent_row_json(row,Some(&cgs))["note_id"].as_i64().unwrap()).collect();
                assert_eq!(actual,expected,"{name}: identity, order, multiplicity and integer type");
            }
        });
    }
}

#[test]
fn boundary_fanout_above_one_thousand_survives_compile_serde_and_execution() {
    on_runtime(async {
        let result = run_case(
            "auth = Session.login()\nsaved = SavedNote{access_token=auth.access_token}\nnotes = saved => Note(_.note_id)\nnotes",
            "opaque-token".into(),
            (1..=1002).collect(), false, false,
        ).await.expect("large fanout must execute without a trace-index panic");
        let step = result
            .return_steps
            .iter()
            .find(|step| step.name.as_deref() == Some("notes"))
            .unwrap();
        let cgs = session().cgs;
        let actual: Vec<_> = step
            .result
            .entities
            .iter()
            .map(|row| {
                plasm_runtime::entity_to_agent_row_json(row, Some(&cgs))["note_id"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(actual, (1..=1002).collect::<Vec<_>>());
    });
}
