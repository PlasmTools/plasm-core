//! PLP-8: observe, write, re-observe; echoes cannot satisfy the stop condition.
use super::*;
use plasm_core::symbol_tuning::SymbolRender;

struct IterationTransport {
    inner: Transport,
    empty_at: Option<usize>,
}
#[async_trait]
impl HttpTransport for IterationTransport {
    async fn send_compiled_http(
        &self,
        base: &str,
        req: &CompiledRequest,
        auth: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let (mut body, next) = self.inner.send_compiled_http(base, req, auth).await?;
        if req.path == "/advance" {
            body["value"] = json!(999);
        }
        if req.path == "/counter"
            && self
                .empty_at
                .is_some_and(|n| body["value"].as_u64() == Some(n as u64))
        {
            body = json!([]);
        }
        Ok((body, next))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("absolute GET")
    }
}
#[derive(Clone, Copy)]
struct Case {
    target: usize,
    bound: usize,
    fail_after: Option<usize>,
    cancel_after: Option<usize>,
    empty_at: Option<usize>,
    expected_error: Option<&'static str>,
}
impl Case {
    fn success(target: usize) -> Self {
        Self {
            target,
            bound: target.max(1),
            fail_after: None,
            cancel_after: None,
            empty_at: None,
            expected_error: None,
        }
    }
}
async fn check(case: Case, python_source: bool, python_host: bool) {
    let es = session();
    let events = Arc::new(Mutex::new(vec![]));
    let scope = crate::operation::ExecutionScope::new();
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            ..Default::default()
        },
        Arc::new(IterationTransport {
            inner: Transport(
                events.clone(),
                case.fail_after,
                case.cancel_after.map(|n| (n, scope.clone())),
            ),
            empty_at: case.empty_at,
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
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for("matrix", "Counter");
    let method = symbols.method_sym_for("matrix", "Counter", "advance");
    let bundle = if python_source {
        crate::plasm_compile::compile_python_program(&es,&format!("class Iterate(Program):\n    def build(self):\n        seed = {entity}.get(\"one\")\n        done = seed.iterate(lambda row: {entity}.{method}(id=row.id), until=lambda row: row.value >= {}, max_steps={})\n        return done\n",case.target,case.bound)).unwrap()
    } else {
        crate::compile_plasm_program(&Default::default(),None,&es,"iteration",&format!("seed = Counter(\"one\")\ndone = iterate seed step Counter.advance(id=_.id) until value >= {} take {}\ndone",case.target,case.bound)).unwrap()
    };
    assert!(events.lock().unwrap().is_empty());
    let comp =
        serde_json::from_slice(&serde_json::to_vec(&bundle.artifact().comp).unwrap()).unwrap();
    let bundle = crate::PlasmCompBundle::new(
        crate::plasm_comp_wire::plasm_comp_artifact_from_comp(comp).unwrap(),
    )
    .unwrap();
    let dry = super::super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let run = Box::pin(
        super::super::super::orchestrator::run_plasm_comp_with_dispatch(
            &es,
            &host,
            &es.prompt_hash,
            "iteration",
            &bundle,
            true,
            None,
            Some(&scope),
            Some(dry),
            None,
            python_host,
        ),
    )
    .await;
    let events = events.lock().unwrap();
    if let Some(expected) = case.expected_error {
        let error = run.unwrap_err();
        assert!(error.contains(expected), "{error}; events={events:?}");
    } else {
        let run = run.unwrap();
        let done = run
            .return_steps
            .iter()
            .find(|s| s.name.as_deref() == Some("done"))
            .unwrap();
        assert_eq!(
            done.result.entities[0].fields["value"].to_value(),
            Value::Integer(case.target as i64)
        );
        assert_eq!(
            done.result
                .operations
                .entries()
                .iter()
                .map(|a| a.completed)
                .sum::<usize>(),
            case.target
        );
        assert_eq!(
            done.result
                .operations
                .entries()
                .iter()
                .map(|a| a.logical_invocations)
                .sum::<usize>(),
            case.target
        );
        if case.target == 0 {
            assert!(done.result.operations.is_empty());
        }
    }
    let committed = case
        .fail_after
        .or(case.cancel_after)
        .or(case.empty_at)
        .unwrap_or(case.target.min(case.bound));
    let mut expected = vec![Event::Read(0)];
    for n in 1..=committed {
        expected.push(Event::Advance);
        if case.cancel_after != Some(n) {
            expected.push(Event::Read(n));
        }
    }
    if case.fail_after.is_some() {
        expected.push(Event::Rejected);
    }
    assert_eq!(
        *events, expected,
        "each iteration must re-observe; never stop on the forged mutator echo"
    );
}
#[test]
fn python_iteration_stateful_contract_native_and_async() {
    let cases = [
        Case::success(0),
        Case::success(1),
        Case::success(3),
        Case {
            target: 4,
            bound: 2,
            expected_error: Some("iterate_bound_exhausted"),
            ..Case::success(4)
        },
        Case {
            fail_after: Some(1),
            expected_error: Some("fixture write rejected"),
            ..Case::success(3)
        },
        Case {
            cancel_after: Some(2),
            expected_error: Some("cancel"),
            ..Case::success(3)
        },
        Case {
            empty_at: Some(0),
            expected_error: Some("No valid ID"),
            ..Case::success(3)
        },
        Case {
            empty_at: Some(1),
            expected_error: Some("No valid ID"),
            ..Case::success(3)
        },
    ];
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            for case in cases {
                for source in [false, true] {
                    for host in [false, true] {
                        rt.block_on(check(case, source, host));
                    }
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
