//! Real HTTP mutation arrays compared with the independent effect model.
use super::effect_model::{nested_writes, Fault};
use plasm_agent::{
    operation::{ExecutionScope, OpAcceptContext},
    plasm_compile::compile_python_program,
};
use plasm_core::symbol_tuning::SymbolRender;
use plasm_runtime::{CancelSignal, MutationDispatchStatus};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Store {
    attempts: Vec<Value>,
    committed: Vec<Value>,
}

async fn case(count: usize, fault: Option<(usize, Fault)>) {
    use axum::{http::StatusCode, response::IntoResponse, routing::get, Json};
    let store = Arc::new(Mutex::new(Store::default()));
    let writes = store.clone();
    let app = axum::Router::new().route("/language/v1/items", get(move || async move {
        Json(json!((0..count).map(|n| json!({"id":format!("p{n}"),"title":format!("title-{n}"),"score":n})).collect::<Vec<_>>()))
    }).post(move |Json(body): Json<Value>| {
        let writes = writes.clone();
        async move {
            let mut store = writes.lock().unwrap();
            let index = store.attempts.len();
            store.attempts.push(body.clone());
            let fault = fault.filter(|(at, _)| *at == index).map(|(_, fault)| fault);
            if fault == Some(Fault::Rejected) {
                return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error":"rejected"}))).into_response();
            }
            let row = json!({"id":format!("w{index}"),"title":body["title"],"score":7,"owner":body["owner"]});
            store.committed.push(row.clone());
            match fault {
                // The write committed; the client receives no successful receipt.
                Some(Fault::ResponseLost) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"response lost after commit"}))).into_response(),
                Some(Fault::InvalidResponse) => Json(json!({"title":true})).into_response(),
                _ => Json(row).into_response(),
            }
        }
    }));
    let reads = store.clone();
    let app = app.route(
        "/language/v1/items/{id}",
        get(
            move |axum::extract::Path(id): axum::extract::Path<String>| {
                let reads = reads.clone();
                async move {
                    if let Some(n) = id
                        .strip_prefix('p')
                        .and_then(|n| n.parse::<usize>().ok())
                        .filter(|n| *n < count)
                    {
                        return Json(json!({"id":id,"title":format!("title-{n}"),"score":n}))
                            .into_response();
                    }
                    let found = reads
                        .lock()
                        .unwrap()
                        .committed
                        .iter()
                        .find(|row| row["id"] == id)
                        .cloned();
                    match found {
                        Some(row) => Json(row).into_response(),
                        None => StatusCode::NOT_FOUND.into_response(),
                    }
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dummy = super::python::Case {
        id: "effect-boundary",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, host) = super::python::parity_context(&dummy, &base);
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(super::language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let create = symbols.method_sym_for(
        super::language_matrix::MATRIX_ENTRY_ID,
        "LangItem",
        "create",
    );
    let source = format!("class MutationArrays(Program):\n    def build(self):\n        parents = {entity}.query().order_by(\"id\")\n        return parents.map(lambda parent: {{\"parent\": parent.id, \"created\": parents.flat_map(lambda child: {entity}.{create}(title=child.title, score=7, owner=parent.id), max_parents=3)}}, max_parents=3)\n");
    let bundle = compile_python_program(&es, &source)
        .await
        .unwrap_or_else(|e| panic!("{source}\n{e}"));
    let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    super::assert_comp_witness(&dry).unwrap();
    // Review/serialization must preserve recursive observed contracts.
    let bundle = plasm_agent::PlasmCompBundle::new(plasm_agent::PlasmCompArtifact {
        comp: serde_json::from_value(serde_json::to_value(&bundle.artifact().comp).unwrap())
            .unwrap(),
        approval_gates: bundle.artifact().approval_gates.clone(),
    })
    .unwrap();
    let es = Arc::new(es);
    let handle = es.mint_operation_handle_plain();
    let cancel = CancelSignal::new();
    es.try_begin_async_operation(handle.clone(), cancel.clone(), OpAcceptContext::default())
        .unwrap();
    let scope = ExecutionScope::for_async_operation(es.clone(), handle.clone(), cancel);
    let actual = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "effect-boundary",
        &bundle,
        true,
        None,
        Some(&scope),
        None,
        None,
    )
    .await;
    let expected = nested_writes(count, fault);
    let evidence = es.get_operation(&handle).unwrap();
    assert_eq!(
        actual.is_ok(),
        expected.succeeds,
        "count={count} fault={fault:?}: {actual:?}; occurrences={:?}",
        evidence.occurrences
    );
    if let Err(failure) = &actual {
        let cause = match fault.unwrap().1 {
            Fault::InvalidResponse => plasm_runtime::FailureCause::ResponseContract,
            Fault::Rejected | Fault::ResponseLost => plasm_runtime::FailureCause::Upstream,
        };
        assert_eq!(failure.cause, cause);
        assert_eq!(
            failure.recovery,
            plasm_runtime::RecoveryDisposition::ReconcileEffects
        );
        assert_eq!(
            failure
                .effects
                .iter()
                .map(|receipt| receipt.completed)
                .sum::<usize>(),
            expected.acknowledged
        );
        assert!(!failure.occurrence_path.is_empty());
    }
    let operation = es.get_operation(&handle).unwrap();
    let dispatches: Vec<_> = operation
        .occurrences
        .iter()
        .flat_map(|o| &o.mutation_dispatches)
        .collect();
    assert_eq!(
        dispatches.len(),
        expected.attempts,
        "no retry or duplicate container evidence: count={count} fault={fault:?}"
    );
    assert_eq!(
        dispatches
            .iter()
            .filter(|d| d.status == MutationDispatchStatus::Unresolved)
            .count(),
        expected.unresolved
    );
    assert!(dispatches
        .iter()
        .all(|d| d.operation.capability == "langitem_create" && !d.request_fingerprint.is_empty()));
    let acknowledged: usize = operation
        .occurrences
        .iter()
        .flat_map(|o| o.operations.entries())
        .map(|a| a.completed)
        .sum();
    assert_eq!(
        acknowledged, expected.acknowledged,
        "count={count} fault={fault:?}"
    );
    let stored = store.lock().unwrap();
    assert_eq!(stored.attempts.len(), expected.attempts);
    assert_eq!(stored.committed.len(), expected.committed);
    for (index, body) in stored.attempts.iter().enumerate() {
        assert_eq!(body["owner"], format!("p{}", index / count));
        assert_eq!(body["title"], format!("title-{}", index % count));
    }
    if let Ok(run) = actual {
        let rows: Vec<_> = run.return_steps[0]
            .result
            .entities()
            .iter()
            .map(|r| serde_json::to_value(&r.fields).unwrap())
            .collect();
        let expected_rows: Vec<_> = (0..count).map(|parent| json!({"parent":format!("p{parent}"),"created":stored.committed[parent*count..(parent+1)*count]})).collect();
        assert_eq!(
            rows, expected_rows,
            "nested mutation values must retain exact observed shape"
        );
    }
    drop(stored);
    let wire = serde_json::to_value(&operation.occurrences).unwrap();
    let restored: Vec<plasm_agent::occurrence_progress::OccurrenceProgress> =
        serde_json::from_value(wire).unwrap();
    assert_eq!(restored, operation.occurrences);
    server.abort();
}

pub(super) async fn run() -> usize {
    let mut checked = 0;
    for count in [0, 1, 3] {
        case(count, None).await;
        checked += 1;
        if count > 0 {
            for fault in [Fault::Rejected, Fault::ResponseLost, Fault::InvalidResponse] {
                for at in 0..count * count {
                    case(count, Some((at, fault))).await;
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 33);
    checked
}
