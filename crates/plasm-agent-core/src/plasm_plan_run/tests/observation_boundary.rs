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
    Rejected,
}
struct Transport(
    Arc<Mutex<Vec<Event>>>,
    Option<usize>,
    Option<(usize, crate::operation::ExecutionScope)>,
);
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
                if self.1 == Some(value) {
                    events.push(Event::Rejected);
                    return Err(RuntimeError::RequestError {
                        source: plasm_runtime::RequestFailure::HttpStatus(
                            plasm_runtime::HttpStatusFailure::without_request(
                                422,
                                "fixture write rejected".into(),
                            ),
                        ),
                        attempts: 1,
                        status: Some(422),
                        body: None,
                    });
                }
                events.push(Event::Advance);
                if let Some((committed, scope)) = &self.2 {
                    if value + 1 == *committed {
                        scope.cancel();
                    }
                }
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

fn check_observations_with_driver(
    writes: usize,
    interleave: bool,
    direct: bool,
    python: bool,
    fail_after: Option<usize>,
    cancel_after: Option<usize>,
) {
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
                    let scope = crate::operation::ExecutionScope::new();
                    let engine = ExecutionEngine::new_with_transport(
                        ExecutionConfig {
                            base_url: Some("http://127.0.0.1:9".into()),
                            ..Default::default()
                        },
                        Arc::new(Transport(
                            events.clone(),
                            fail_after,
                            cancel_after.map(|n| (n, scope.clone())),
                        )),
                        None,
                    );
                    use plasm_core::symbol_tuning::SymbolRender;
                    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
                    let counter = symbols.entity_sym_for("matrix", "Counter");
                    let wire = symbols.entity_sym_for("matrix", "Wire");
                    let advance = symbols.method_sym_for("matrix", "Counter", "advance");
                    let entity = if direct { &wire } else { &counter };
                    let companion = if direct { &counter } else { &wire };
                    let mut body = Vec::new();
                    let mut expected = Vec::new();
                    if interleave {
                        body.push(format!("before = {entity}.get(\"one\")"));
                        expected.push(("before".to_string(), 0));
                    }
                    for n in 0..writes {
                        // Discarding a Python result must not discard the reviewed effect.
                        body.push(format!("{counter}.{advance}(id=\"one\")"));
                        if interleave {
                            body.push(format!("read_{n} = {entity}.get(\"one\")"));
                            expected.push((format!("read_{n}"), n + 1));
                        }
                    }
                    body.push(format!("current = {entity}.get(\"one\")"));
                    expected.push(("current".to_string(), writes));
                    body.push(format!("companion = {companion}.get(\"one\")"));
                    expected.push(("companion".to_string(), writes));
                    body.push(format!(
                        "return {}",
                        expected
                            .iter()
                            .map(|(name, _)| name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    let source = format!(
                        "class Writes(Program):\n    def build(self):\n{}\n",
                        body.iter()
                            .map(|line| format!("        {line}"))
                            .collect::<Vec<_>>()
                            .join("\n")
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
                        })
                        .expect("valid catalog fixture");
                    let bundle = crate::plasm_compile::compile_python_program(&es, &source)
                        .await
                        .expect("Python compile");
                    let dry = super::super::evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
                    let result =
                        Box::pin(super::super::orchestrator::run_plasm_comp_with_dispatch(
                            &es,
                            &host,
                            &es.prompt_hash,
                            "boundary",
                            &bundle,
                            true,
                            None,
                            Some(&scope),
                            Some(dry),
                            None,
                            python,
                        ))
                        .await;
                    if let Some(committed) = cancel_after {
                        let error = result.expect_err("cancelled write sequence");
                        assert!(error.diagnostic().contains("cancel"), "{error}");
                        let observed = events.lock().unwrap();
                        assert_eq!(
                            observed.iter().filter(|&&e| e == Event::Advance).count(),
                            committed
                        );
                        assert_eq!(
                            observed.last(),
                            Some(&Event::Advance),
                            "no post-cancellation reads or writes"
                        );
                        return;
                    }
                    if let Some(committed) = fail_after {
                        let error = result.expect_err("write must fail");
                        assert!(
                            error.diagnostic().contains("fixture write rejected"),
                            "{error}"
                        );
                        {
                            let observed = events.lock().unwrap();
                            assert_eq!(
                                observed.iter().filter(|&&e| e == Event::Advance).count(),
                                committed
                            );
                            assert_eq!(
                                observed.iter().filter(|&&e| e == Event::Rejected).count(),
                                1,
                                "failed writes are never retried"
                            );
                            assert_eq!(
                                observed.last(),
                                Some(&Event::Rejected),
                                "no later read or write may execute"
                            );
                        }
                        // A new execution reaches the driver in the same session.
                        // The fixture still rejects, proving admission is not the
                        // previous execution's stored failure.
                        let next_dry =
                            super::super::evaluate_plasm_comp_dry(&es, &bundle).expect("next dry");
                        let next =
                            Box::pin(super::super::orchestrator::run_plasm_comp_with_dispatch(
                                &es,
                                &host,
                                &es.prompt_hash,
                                "boundary",
                                &bundle,
                                true,
                                None,
                                Some(&scope),
                                Some(next_dry),
                                None,
                                python,
                            ))
                            .await;
                        assert!(next.is_err());
                        assert_eq!(
                            events
                                .lock()
                                .unwrap()
                                .iter()
                                .filter(|&&e| e == Event::Rejected)
                                .count(),
                            2,
                            "each new execution independently reaches the rejected write"
                        );
                        return;
                    }
                    let result = result.expect("live");
                    for (name, value) in expected {
                        let step = result
                            .return_steps
                            .iter()
                            .find(|s| s.name.as_deref() == Some(name.as_str()))
                            .expect("observation");
                        assert_eq!(
                            step.result.entities()[0]
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

fn check_observations(writes: usize, interleave: bool, direct: bool) {
    check_observations_with_driver(writes, interleave, direct, false, None, None)
}
#[test]
fn python_host_reads_and_writes_preserve_each_observation() {
    check_observations_with_driver(2, true, true, true, None, None);
}

#[test]
fn python_source_unreturned_writes_preserve_each_observation() {
    for writes in [1, 2, 5] {
        for direct in [false, true] {
            for interleave in [false, true] {
                check_observations_with_driver(writes, interleave, direct, false, None, None);
            }
        }
    }
}
#[test]
fn python_source_async_host_writes_preserve_each_observation() {
    check_observations_with_driver(3, true, true, true, None, None);
}

#[test]
fn python_source_failed_write_stops_later_effects_without_replay() {
    for python_host in [false, true] {
        for committed in [0, 2] {
            check_observations_with_driver(4, true, true, python_host, Some(committed), None);
        }
    }
}

#[test]
fn python_source_cancellation_keeps_committed_writes_and_stops_later_effects() {
    for python_host in [false, true] {
        check_observations_with_driver(4, true, true, python_host, None, Some(2));
    }
}

mod fanout;
mod iteration;
