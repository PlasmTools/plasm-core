use crate::execute_session::ExecuteSession;
use async_trait::async_trait;
use plasm_compile::CompiledRequest;
use plasm_core::{CgsContext, CgsRegistry, TeachingExposureSession, Value};
use plasm_runtime::{
    auth::ResolvedAuth, ExecutionConfig, ExecutionEngine, ExecutionMode, HttpTransport,
    RuntimeError,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Advance,
    Read(usize),
}
struct Transport(Arc<Mutex<Vec<Event>>>);
#[async_trait]
impl HttpTransport for Transport {
    async fn send_compiled_http(
        &self,
        _: &str,
        req: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let mut events = self.0.lock().unwrap();
        let value = events.iter().filter(|&&e| e == Event::Advance).count();
        let body = match req.path.as_str() {
            "/advance" => {
                events.push(Event::Advance);
                json!({"id":"one","value":value+1})
            }
            "/counter" => {
                events.push(Event::Read(value));
                json!({"id":"one","value":value})
            }
            p => panic!("unexpected {p}"),
        };
        Ok((body, None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("absolute GET")
    }
}
fn session() -> ExecuteSession {
    let mut schema = plasm_core::load_schema(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/observation_boundary_matrix"),
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
    let wave = ["Counter", "Wire"];
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

fn check_observations(writes: usize, interleave: bool, direct: bool) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let es = session();
                    let events = Arc::new(Mutex::new(vec![]));
                    let engine = ExecutionEngine::new_with_transport(
                        ExecutionConfig {
                            base_url: Some("http://127.0.0.1:9".into()),
                            ..Default::default()
                        },
                        Arc::new(Transport(events.clone())),
                        None,
                    );
                    let entity = if direct { "Wire" } else { "Counter" };
                    let mut program = String::new();
                    let mut expected = Vec::new();
                    if interleave {
                        program.push_str(&format!("before = {entity}{{id=\"one\"}}\n"));
                        expected.push(("before".to_string(), 0));
                    }
                    for n in 0..writes {
                        program.push_str(&format!("w{n} = Counter.advance(id=\"one\")\n"));
                        if interleave {
                            program.push_str(&format!("read_{n} = {entity}{{id=\"one\"}}\n"));
                            expected.push((format!("read_{n}"), n + 1));
                        }
                    }
                    program.push_str(&format!("current = {entity}{{id=\"one\"}}\n"));
                    expected.push(("current".to_string(), writes));
                    let companion = if direct { "Counter" } else { "Wire" };
                    program.push_str(&format!("companion = {companion}{{id=\"one\"}}\n"));
                    expected.push(("companion".to_string(), writes));
                    program.push_str(
                        &expected
                            .iter()
                            .map(|(name, _)| name.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                    let host =
                        crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
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
                            run_artifacts: Arc::new(
                                crate::run_artifacts::RunArtifactStore::memory(),
                            ),
                            session_graph_persistence: None,
                            oss_local_filesystem_defaults: false,
                        });
                    let bundle = crate::compile_plasm_program(
                        &Default::default(),
                        None,
                        &es,
                        "boundary",
                        &program,
                    )
                    .expect("compile");
                    let comp = serde_json::from_slice(
                        &serde_json::to_vec(&bundle.artifact().comp).unwrap(),
                    )
                    .unwrap();
                    let bundle = crate::PlasmCompBundle::new(
                        crate::plasm_comp_wire::plasm_comp_artifact_from_comp(comp).unwrap(),
                    )
                    .unwrap();

                    let current_id = plasm_core::plasm_monad::StepId::new("current").unwrap();
                    let companion_id = plasm_core::plasm_monad::StepId::new("companion").unwrap();
                    assert_eq!(
                        bundle.artifact().comp.bind.deps.get(&current_id),
                        bundle.artifact().comp.bind.deps.get(&companion_id),
                        "independent reads share the effect frontier"
                    );
                    let dry = super::super::evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
                    let result = Box::pin(super::super::run_plasm_comp(
                        &es,
                        &host,
                        &es.prompt_hash,
                        "boundary",
                        &bundle,
                        true,
                        None,
                        None,
                        Some(dry),
                        None,
                    ))
                    .await
                    .expect("live");
                    for (name, value) in expected {
                        let step = result
                            .return_steps
                            .iter()
                            .find(|s| s.name.as_deref() == Some(name.as_str()))
                            .expect("observation");
                        assert_eq!(
                            step.result.entities[0]
                                .fields
                                .get("value")
                                .map(|v| v.to_value()),
                            Some(Value::Integer(value as i64)),
                            "{name}: events={:?}",
                            events.lock().unwrap()
                        );
                    }
                    assert_eq!(
                        events
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|&&x| x == Event::Advance)
                            .count(),
                        writes,
                        "every mutation occurs exactly once"
                    );
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn sequential_writes_observe_final_view_state() {
    check_observations(4, false, false);
}
proptest::proptest! {
 #![proptest_config(proptest::test_runner::Config::with_cases(64))]
 #[test]
 fn serialized_effects_preserve_each_observation(writes in 1usize..7, interleave in proptest::bool::ANY, direct in proptest::bool::ANY) {
  check_observations(writes,interleave,direct);
 }
}
