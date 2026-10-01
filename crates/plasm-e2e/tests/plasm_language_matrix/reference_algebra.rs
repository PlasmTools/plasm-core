//! Independent read/value semantics compared with Python -> reviewed DAG -> HTTP.
mod corpus;
mod model;
mod reduction_model;
mod reductions;
pub(super) use reductions::run as run_reductions;
mod tests;

use model::{Coverage, Expr, Row, Rows, Type};
use plasm_agent::{plasm_compile::compile_python_program, plasm_plan_run::run_plasm_comp};
use plasm_core::symbol_tuning::SymbolRender;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn source_rows(count: usize) -> Rows {
    let shape = BTreeMap::from([
        ("id".into(), Type::Text),
        ("title".into(), Type::Text),
        ("score".into(), Type::Nullable(Box::new(Type::Integer))),
    ]);
    let values = [
        json!({"id":"i1","title":"Alpha","score":10}),
        json!({"id":"i2","title":"Beta\n雪","score":null}),
        json!({"id":"i3","title":"Alpha","score":-7}),
    ]
    .into_iter()
    .take(count)
    .map(|row| serde_json::from_value(row).unwrap())
    .collect();
    Rows {
        observed: false,
        shape,
        values,
        coverage: Coverage::Complete,
        continuation: false,
    }
}

struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(rows: &Rows) -> Server {
    use axum::{
        extract::{Path, Query, State},
        routing::get,
        Json,
    };
    async fn list(
        State(rows): State<Vec<Row>>,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> Json<Value> {
        let offset = query
            .get("offset")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let limit = query
            .get("limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(rows.len());
        Json(
            serde_json::to_value(
                rows.into_iter()
                    .skip(offset)
                    .take(limit)
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        )
    }
    async fn one(
        State(rows): State<Vec<Row>>,
        Path(id): Path<String>,
    ) -> Result<Json<Value>, axum::http::StatusCode> {
        rows.into_iter()
            .find(|row| row["id"].as_str() == Some(&id))
            .map(|row| Json(serde_json::to_value(row).unwrap()))
            .ok_or(axum::http::StatusCode::NOT_FOUND)
    }
    let app = axum::Router::new()
        .route("/language/v1/items", get(list))
        .route("/language/v1/items/{id}", get(one))
        .with_state(rows.values.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { base, task }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    Admission,
    Dry,
    Runtime,
    Values,
    Coverage,
    Effects,
}
#[derive(Debug)]
struct Failure {
    kind: FailureKind,
    detail: String,
    python: String,
}

async fn compare(expr: &Expr, source: &Rows, base: &str) -> Result<(), Failure> {
    let expected = expr
        .eval(source)
        .expect("typed corpus evaluates in the independent model");
    compare_program(
        |entity| corpus::source(expr, &source.shape, entity),
        &expected.values,
        base,
    )
    .await
}

async fn compare_program(
    source: impl FnOnce(&str) -> String,
    expected: &[Row],
    base: &str,
) -> Result<(), Failure> {
    let dummy = super::python::Case {
        id: "reference_read_value",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, host) = super::python::parity_context(&dummy, base);
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(super::language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let python = source(&entity);
    let fail = |kind, detail| Failure {
        kind,
        detail,
        python: python.clone(),
    };
    let bundle = compile_python_program(&es, &python)
        .await
        .map_err(|e| fail(FailureKind::Admission, e))?;
    let dry = super::evaluate_plasm_comp_dry(&es, &bundle)
        .map_err(|e| fail(FailureKind::Dry, e.to_string()))?;
    super::assert_comp_witness(&dry).map_err(|e| fail(FailureKind::Dry, e))?;
    let run = Box::pin(run_plasm_comp(
        &es,
        &host,
        &es.prompt_hash,
        "reference-read-value",
        &bundle,
        true,
        None,
        None,
        Some(dry),
        None,
    ))
    .await
    .map_err(|e| fail(FailureKind::Runtime, e.diagnostic().to_owned()))?;
    if run.return_steps.len() != 1 {
        return Err(fail(
            FailureKind::Values,
            format!("expected one return, got {}", run.return_steps.len()),
        ));
    }
    let result = &run.return_steps[0].result;
    let actual: Vec<Row> = result
        .entities()
        .iter()
        .map(|row| {
            row.fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.to_string(),
                        serde_json::to_value(value.to_value()).unwrap(),
                    )
                })
                .collect()
        })
        .collect();
    if actual != expected {
        return Err(fail(
            FailureKind::Values,
            format!("expected {expected:#?}\nactual {actual:#?}"),
        ));
    }
    if result.coverage() != plasm_runtime::ResultCoverage::Complete
        || result.has_more
        || result.paging_handle.is_some()
        || result.pagination_resume.is_some()
    {
        return Err(fail(
            FailureKind::Coverage,
            format!(
                "expected complete/no continuation; got {:?}",
                result.coverage()
            ),
        ));
    }
    if !result.operations.is_empty() {
        return Err(fail(
            FailureKind::Effects,
            "read/value program produced effect acknowledgements".into(),
        ));
    }
    Ok(())
}

async fn shrink(mut expr: Expr, source: &Rows, base: &str, kind: FailureKind) -> Expr {
    // Every accepted step has a strictly smaller tree and the same output type.
    // Preserve failure class; a new admission error cannot shrink a value bug.
    for _ in 0..32 {
        let mut replacement = None;
        for candidate in corpus::smaller(&expr, &source.shape) {
            if compare(&candidate, source, base)
                .await
                .is_err_and(|e| e.kind == kind)
            {
                replacement = Some(candidate);
                break;
            }
        }
        let Some(candidate) = replacement else { break };
        expr = candidate;
    }
    expr
}

pub(super) async fn run() -> usize {
    let mut count = 0;
    for size in [0, 1, 3] {
        let source = source_rows(size);
        let backend = server(&source).await;
        for (index, expr) in corpus::cases(&source.shape).into_iter().enumerate() {
            if let Err(failure) = compare(&expr, &source, &backend.base).await {
                let minimal = shrink(expr.clone(), &source, &backend.base, failure.kind).await;
                let reduced = compare(&minimal, &source, &backend.base)
                    .await
                    .expect_err("reduced counterexample remains reproducible");
                assert_eq!(
                    reduced.kind, failure.kind,
                    "counterexample failure stage changed"
                );
                panic!("{} source-size={size} case={index} kind={:?}\noriginal failure: {}\noriginal={expr:?}\nshrunk={minimal:?}\nreduced failure: {}\n{}",
                    corpus::VERSION,failure.kind,failure.detail,reduced.detail,reduced.python);
            }
            count += 1;
        }
    }
    for size in [0, 1, 3] {
        let mut source = source_rows(size);
        source.observed = true;
        if let Some(first) = source.values.first_mut() {
            first.remove("score");
        }
        let backend = server(&source).await;
        for (index, expr) in corpus::observed_cases().into_iter().enumerate() {
            if let Err(failure) = compare(&expr, &source, &backend.base).await {
                let minimal = shrink(expr.clone(), &source, &backend.base, failure.kind).await;
                let reduced = compare(&minimal, &source, &backend.base)
                    .await
                    .expect_err("reduced counterexample remains reproducible");
                assert_eq!(reduced.kind, failure.kind);
                panic!("{} observed source-size={size} case={index} kind={:?}\noriginal={expr:?}\nshrunk={minimal:?}\n{}\n{}", corpus::VERSION, failure.kind, reduced.detail, reduced.python);
            }
            count += 1;
        }
    }
    assert_eq!(count, 141);
    println!("{}: {count} independent differential executions; 41 closed-value trees + 6 observed-value trees, each x 3 source cardinalities; deterministic exhaustive declared corpus, not all typed programs",corpus::VERSION);
    count
}
