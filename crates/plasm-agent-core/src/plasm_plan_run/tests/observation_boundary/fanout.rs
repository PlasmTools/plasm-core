//! Stateful differential evidence: every source occurrence, then a read barrier.
use super::*;
use plasm_core::symbol_tuning::SymbolRender;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    List,
    Write(String),
    Rejected(String),
    Read(String, usize),
}
#[derive(Default)]
struct State {
    calls: Vec<Call>,
    values: BTreeMap<String, usize>,
}
struct FanoutTransport {
    rows: usize,
    state: Arc<Mutex<State>>,
    reject: Option<String>,
    cancel: Option<(usize, crate::operation::ExecutionScope)>,
}
#[async_trait]
impl HttpTransport for FanoutTransport {
    async fn send_compiled_http(
        &self,
        _: &str,
        req: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let mut state = self.state.lock().unwrap();
        let body = match req.path.as_str() {
            "/counters" => {
                state.calls.push(Call::List);
                json!((0..self.rows)
                    .map(|i| json!({"id":format!("r{i}"),"value":0}))
                    .collect::<Vec<_>>())
            }
            "/advance" => {
                let id = req.body.as_ref().unwrap().as_object().unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                if self.reject.as_ref() == Some(&id) {
                    state.calls.push(Call::Rejected(id));
                    return Err(RuntimeError::RequestError {
                        message: "rejected fanout row".into(),
                        attempts: 1,
                        status: Some(422),
                        body: None,
                    });
                }
                *state.values.entry(id.clone()).or_default() += 1;
                state.calls.push(Call::Write(id.clone()));
                if let Some((count, scope)) = &self.cancel {
                    if state
                        .calls
                        .iter()
                        .filter(|c| matches!(c, Call::Write(_)))
                        .count()
                        == *count
                    {
                        scope.cancel();
                    }
                }
                json!({"id":id,"value":state.values[&id]})
            }
            "/counter" => {
                let id = req.query.as_ref().unwrap().as_object().unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let value = state.values.get(&id).copied().unwrap_or(0);
                state.calls.push(Call::Read(id.clone(), value));
                json!({"id":id,"value":value})
            }
            other => panic!("unexpected path {other}"),
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

fn check(rows: usize, reject: Option<&str>, cancel_after: Option<usize>, python_host: bool) {
    let reject = reject.map(str::to_owned);
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            for python_source in [false, true] {
                runtime.block_on(check_one(
                    rows,
                    reject.clone(),
                    cancel_after,
                    python_host,
                    python_source,
                ));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn check_one(
    rows: usize,
    reject: Option<String>,
    cancel_after: Option<usize>,
    python_host: bool,
    python_source: bool,
) {
    let es = session();
    let state = Arc::new(Mutex::new(State::default()));
    let scope = crate::operation::ExecutionScope::new();
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            ..Default::default()
        },
        Arc::new(FanoutTransport {
            rows,
            state: state.clone(),
            reject: reject.clone(),
            cancel: cancel_after.map(|n| (n, scope.clone())),
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
    let wire = symbols.entity_sym_for("matrix", "Wire");
    let counter = symbols.entity_sym_for("matrix", "Counter");
    let advance = symbols.method_sym_for("matrix", "Counter", "advance");
    let bundle = if python_source {
        crate::plasm_compile::compile_python_program(&es,&format!("class Fanout(Program):\n    def build(self):\n        rows = {wire}.query()\n        before = rows.flat_map(lambda row: {wire}.get(row.id))\n        effects = rows.flat_map(lambda row: {counter}.{advance}(id=row.id))\n        after = rows.flat_map(lambda row: {wire}.get(row.id))\n        return before, effects, after\n")).unwrap()
    } else {
        crate::compile_plasm_program(&Default::default(),None,&es,"fanout","rows = Wire\nbefore = rows => Wire(_.id)\neffects = rows => Counter.advance(id=_.id)\nafter = rows => Wire(_.id)\nbefore, effects, after").unwrap()
    };
    assert!(state.lock().unwrap().calls.is_empty(), "compile is pure");
    let comp =
        serde_json::from_slice(&serde_json::to_vec(&bundle.artifact().comp).unwrap()).unwrap();
    let bundle = crate::PlasmCompBundle::new(
        crate::plasm_comp_wire::plasm_comp_artifact_from_comp(comp).unwrap(),
    )
    .unwrap();
    let dry = super::super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let result = Box::pin(
        super::super::super::orchestrator::run_plasm_comp_with_dispatch(
            &es,
            &host,
            &es.prompt_hash,
            "fanout",
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
    let state = state.lock().unwrap();
    if let Some(committed) = cancel_after {
        assert!(result.unwrap_err().contains("cancel"));
        assert_eq!(state.values.values().sum::<usize>(), committed);
        assert!(matches!(state.calls.last(), Some(Call::Write(_))));
        return;
    }
    let result = result.unwrap();
    let before = result
        .return_steps
        .iter()
        .find(|s| s.name.as_deref() == Some("before"))
        .unwrap();
    let effects = result
        .return_steps
        .iter()
        .find(|s| s.name.as_deref() == Some("effects"))
        .unwrap();
    let after = result
        .return_steps
        .iter()
        .find(|s| s.name.as_deref() == Some("after"))
        .unwrap();
    assert_eq!(before.result.entities.len(), rows);
    assert!(before
        .result
        .entities
        .iter()
        .all(|e| e.fields["value"].to_value() == Value::Integer(0)));
    assert_eq!(after.result.entities.len(), rows);
    for (i, row) in after.result.entities.iter().enumerate() {
        let id = format!("r{i}");
        assert_eq!(row.fields["id"].to_value(), Value::String(id.clone()));
        assert_eq!(
            row.fields["value"].to_value(),
            Value::Integer(i64::from(reject.as_ref() != Some(&id)))
        );
    }
    let acks = effects.result.operations.entries();
    if rows > 0 {
        assert_eq!(acks.len(), 1);
        let ack = &acks[0];
        assert_eq!(ack.logical_invocations, rows);
        assert_eq!(ack.failed, usize::from(reject.is_some()));
        assert_eq!(ack.completed, rows - ack.failed);
        assert_eq!(ack.outcomes.len(), rows);
        for (i, outcome) in ack.outcomes.iter().enumerate() {
            assert_eq!(outcome.source_index, i);
            assert!(outcome
                .source_identity
                .as_deref()
                .is_some_and(|s| s.contains(&format!("r{i}"))));
            assert_eq!(
                outcome.status,
                if reject.as_deref() == Some(format!("r{i}").as_str()) {
                    plasm_runtime::OperationInvocationStatus::Failed
                } else {
                    plasm_runtime::OperationInvocationStatus::Completed
                }
            );
        }
    } else {
        assert!(acks.is_empty() || acks.iter().all(|a| a.logical_invocations == 0));
    }
    if reject.is_some() {
        assert_eq!(
            effects.result.coverage,
            plasm_runtime::ResultCoverage::Partial
        );
    }
    let writes = state
        .calls
        .iter()
        .filter(|c| matches!(c, Call::Write(_) | Call::Rejected(_)))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        writes,
        (0..rows)
            .map(|i| {
                let id = format!("r{i}");
                if reject.as_ref() == Some(&id) {
                    Call::Rejected(id)
                } else {
                    Call::Write(id)
                }
            })
            .collect::<Vec<_>>()
    );
    if rows > 0 {
        let first = state
            .calls
            .iter()
            .position(|c| matches!(c, Call::Write(_) | Call::Rejected(_)))
            .unwrap();
        let last = state
            .calls
            .iter()
            .rposition(|c| matches!(c, Call::Write(_) | Call::Rejected(_)))
            .unwrap();
        // Pre-write Gets may lawfully reuse the fully materialized query rows.
        assert!(state.calls[..first]
            .iter()
            .all(|c| matches!(c, Call::List | Call::Read(_, 0))));
        assert_eq!(
            state.calls[last + 1..]
                .iter()
                .filter(|c| matches!(c, Call::Read(..)))
                .count(),
            rows,
            "post-write Gets must re-observe"
        );
        assert!(
            state.calls[first..=last]
                .iter()
                .all(|c| !matches!(c, Call::Read(..))),
            "no read crosses the write barrier"
        );
    }
}
#[test]
fn python_fanout_reads_observe_completed_write_phase() {
    for rows in [0, 1, 4] {
        for host in [false, true] {
            check(rows, None, None, host);
        }
    }
}
#[test]
fn python_fanout_partial_failure_keeps_row_outcomes_and_actual_state() {
    for host in [false, true] {
        check(4, Some("r1"), None, host);
    }
}
#[test]
fn python_fanout_cancellation_stops_new_writes_without_replay() {
    for host in [false, true] {
        check(4, None, Some(2), host);
    }
}
