//! Finite branch-type product plus arithmetic/effect boundaries, through Python admission.
use plasm_agent::plasm_compile::compile_python_program;
use serde_json::{json, Value};

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn run() -> usize {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let writes = Arc::new(AtomicUsize::new(0));
    let observed = writes.clone();
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(
            |axum::extract::Path(id): axum::extract::Path<String>| async move {
                let mut record = super::python::value_contract_matrix::record();
                record["id"] = json!(id);
                record["flag"] = json!(id == "000123");
                record["text"] = json!("[1]");
                record.as_object_mut().unwrap().remove("optional_count");
                axum::Json(record)
            },
        )
        .patch(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async { axum::Json(json!({"id":"000123", "count":1})) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let record = super::python::value_contract_matrix::record();
    let leaves = [
        ("row.count", json!(9007199254740993_i64)),
        ("row.ratio", json!(1.25)),
        ("row.text", json!("[1]")),
        ("row.numbers", json!([1, 2, 3])),
        ("row.document", record["document"].clone()),
        ("None", Value::Null),
    ];
    let mut count = 0;
    for (left, l) in &leaves {
        for (right, r) in &leaves {
            for (id, expected) in [("000123", l), ("000124", r)] {
                let (es, host, token) = super::recursive_values::fixture_context(base.clone());
                let source = format!("class Choice(Program):\n    @compute\n    def consume(self, row: Row) -> Row.value:\n        return row.value\n    def build(self):\n        root = {token}.get('{id}')\n        result = root.select(value=lambda row: {left} if row.flag == True else {right})\n        return self.consume(result)\n");
                let bundle = compile_python_program(&es, &source)
                    .await
                    .unwrap_or_else(|e| panic!("{source}\n{e}"));
                let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
                super::assert_comp_witness(&dry).unwrap();
                let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
                    &es,
                    &host,
                    &es.prompt_hash,
                    "outer-values",
                    &bundle,
                    true,
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .unwrap_or_else(|e| panic!("{source}\n{e}"));
                assert_eq!(
                    serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
                    json!({"value": expected}),
                    "{source}"
                );
                count += 1;
            }
        }
    }
    for (expression, expected) in [
        ("row.count + 1", json!(9007199254740994_i64)),
        (
            "row.count if row.flag == True else row.optional_count",
            json!(9007199254740993_i64),
        ),
        ("row.ratio * 2", json!(2.5)),
        ("row.flag / 2", json!(0.5)),
        ("row.count if 7 else row.ratio", json!(9007199254740993_i64)),
        ("row.count / 2", json!(4503599627370496.0)),
        ("row.text + row.text", json!("[1][1]")),
        (
            "money_mul(row.price, 2)",
            json!({"__plasm_money":"24691357802469135780.24691356","currency":"USD"}),
        ),
        (
            "row.count if row.flag == True else 1 / 0",
            json!(9007199254740993_i64),
        ),
    ] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class Arithmetic(Program):\n    @compute\n    def consume(self, row: Row) -> Row.value:\n        return row.value\n    def build(self):\n        root = {token}.get('000123')\n        result = root.select(value=lambda row: {expression})\n        return self.consume(result)\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "outer-math",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("{source}\n{e}"));
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"value":expected})
        );
        count += 1;
    }
    for expression in [
        "row.document + row.count",
        "row.price * row.price",
        "row.text + row.count",
    ] {
        let (es, _, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class Invalid(Program):\n    def build(self):\n        return {token}.get('000123').select(value=lambda row: {expression})\n");
        let error = compile_python_program(&es, &source)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported-operator"), "{source}\n{error}");
        count += 1;
    }
    for (expression, suffix, expected) in [
        (
            "row.count if row.flag == True else row.ratio",
            ".order_by('value')",
            json!([1.25, 9007199254740993_i64]),
        ),
        (
            "row.count if row.flag == True else row.ratio",
            ".where(lambda row: row.value > 2)",
            json!([9007199254740993_i64]),
        ),
        (
            "row.count if row.flag == True else row.text",
            ".where(lambda row: row.value == '[1]')",
            json!(["[1]"]),
        ),
        (
            "row.document if row.flag == True else row.text",
            ".select('value')",
            json!([record["document"], "[1]"]),
        ),
    ] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class Mixed(Program):\n    @compute\n    def consume(self, rows: list[Row]) -> list[Row.value]:\n        return [row.value for row in rows]\n    def build(self):\n        left = {token}.get('000123')\n        right = {token}.get('000124')\n        root = left.union(right)\n        result = root.select(value=lambda row: {expression}){suffix}\n        return self.consume(result)\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "outer-mixed",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("{source}\n{e}"));
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"value":expected}),
            "{source}"
        );
        count += 1;
    }
    for (a, b, expected) in [
        (
            "{'n': r.count}",
            "{'n': r.text}",
            json!([{"n":9007199254740993_i64},{"n":"[1]"}]),
        ),
        (
            "[r.count]",
            "[r.text]",
            json!([[9007199254740993_i64], ["[1]"]]),
        ),
    ] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class Structured(Program):\n    @compute\n    def consume(self, rows: list[Row]) -> list[Row.value]:\n        return [row.value for row in rows]\n    def build(self):\n        left = {token}.get('000123')\n        right = {token}.get('000124')\n        root = left.union(right)\n        shaped = root.map(lambda r: {{'a': {a}, 'b': {b}, 'flag': r.flag}}, max_parents=2)\n        result = shaped.select(value=lambda r: r.a if r.flag == True else r.b)\n        return self.consume(result)\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "outer-structured",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("{source}\n{e}"));
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"value":expected})
        );
        count += 1;
    }
    for expression in [
        "row.count / 0",
        "row.count + 9223372036854775807",
        "row.text + '!'",
    ] {
        for context in ["projection", "record", "argument"] {
            use plasm_core::symbol_tuning::SymbolRender;
            let (es, host, token) = super::recursive_values::fixture_context(base.clone());
            let method = es
                .teaching_exposure
                .as_ref()
                .unwrap()
                .to_symbol_map()
                .method_sym_for("types", "Sample", "sample_update");
            let direct = expression.replace("row.", "root.");
            let body = match context {
            "projection" => format!("result = root.select(value=lambda row: {expression})\n        return root.{method}(count=result.value)"),
            "record" => format!("result = {{'value': {direct}}}\n        return root.{method}(count=result.value)"),
            _ => format!("return root.{method}(count={direct})"),
        };
            let source = format!("class FailClosed(Program):\n    def build(self):\n        root = {token}.get('000123')\n        {body}\n");
            let bundle = compile_python_program(&es, &source)
                .await
                .unwrap_or_else(|e| panic!("{source}\n{e}"));
            let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
                &es,
                &host,
                &es.prompt_hash,
                "outer-fail",
                &bundle,
                true,
                None,
                None,
                None,
                None,
            )
            .await;
            let error = result.unwrap_err();
            assert!(
                error.diagnostic().contains("division by zero")
                    || error.diagnostic().contains("outside i64")
                    || error.diagnostic().contains("type")
                    || error.diagnostic().contains("integer"),
                "{error}"
            );
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            count += 1;
        }
    }
    count
}

#[tokio::test]
async fn outer_value_transfer_boundary() {
    assert_eq!(run().await, 99);
}
