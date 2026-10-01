//! Typed output composition and effect rejection, over abstract CGS fixtures.
use plasm_agent::plasm_compile::compile_python_program;
use serde_json::{json, Value};
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(super) async fn run() -> usize {
    super::primitive_reductions::run_boundary(true).await
}

#[tokio::test]
async fn typed_return_roundtrips() {
    run().await;
}

#[tokio::test]
async fn typed_return_structures_and_effect_gate() {
    use plasm_core::symbol_tuning::SymbolRender;
    use std::sync::{Arc, Mutex};
    let writes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let observed = writes.clone();
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(
            |axum::extract::Path(id): axum::extract::Path<String>| async move {
                let mut record = super::python::value_contract_matrix::record();
                record["id"] = json!(id);
                if id == "000124" {
                    record.as_object_mut().unwrap().remove("optional_count");
                }
                axum::Json(record)
            },
        )
        .patch(move |axum::Json(body): axum::Json<Value>| {
            observed.lock().unwrap().push(body.clone());
            async move { axum::Json(json!({"id":"000123", "count": body["count"]})) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let mut full_record = super::python::value_contract_matrix::record();
    full_record["timestamp"] = json!("2026-09-25T12:34:56.123456+00:00");
    // Row selection retains the money's wire metadata as well as its amount.
    full_record["price"]["format"] = json!({"encoding":"decimal_string"});
    for (annotation, expression, setup, expected) in [
        ("Value[ENTITY]", "row", "root.take(1)", full_record),
        (
            "Row",
            "row",
            "root.aggregate(total=agg.sum('count'), mean=agg.avg('price'))",
            json!({"total":9007199254740993_i64,"mean":{"__plasm_money":"12345678901234567890.12345678","currency":"USD"}}),
        ),
        ("None", "None", "root.select('id')", Value::Null),
        (
            "float",
            "row.ratio / 2",
            "root.select('ratio')",
            json!(0.625),
        ),
        (
            "Row",
            "row",
            "root.select(\"id\", \"count\")",
            json!({"id":"000123", "count":9007199254740993_i64}),
        ),
        (
            "Row",
            "{'id': row.id, 'count': row.count + 1}",
            "root.select(\"id\", \"count\")",
            json!({"id":"000123", "count":9007199254740994_i64}),
        ),
        (
            "list[Row]",
            "[row, row]",
            "root.select(\"id\")",
            json!([{"id":"000123"},{"id":"000123"}]),
        ),
        (
            "int | str",
            "row.count if row.flag else 'no'",
            "root.select(\"count\", \"flag\")",
            json!(9007199254740993_i64),
        ),
        (
            "Row.boxed",
            "row.boxed",
            "root.map(lambda r: {'boxed': {'items': [r.count, None]}}, max_parents=1)",
            json!({"items":[9007199254740993_i64,null]}),
        ),
    ] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class Typed(Program):\n    @compute\n    def produce(self, row: Row) -> {annotation}:\n        return {expression}\n    @compute\n    def consume(self, row: Row) -> Row.value:\n        return row.value\n    def build(self):\n        root = {token}.get('000123')\n        source = {setup}\n        result = self.produce(source)\n        return self.consume(result)\n");
        let source = if expected.is_object() {
            source
                .replace("-> Row.value:", "-> Row:")
                .replace("return row.value", "return row")
        } else {
            source
        };
        let source = source.replace("ENTITY", &token);
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "structures",
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
            if expected.is_object() {
                expected
            } else {
                json!({"value": expected})
            }
        );
    }
    for (input, expression, build, expected) in [
        (
            "list[Row]",
            "rows",
            "result = self.produce(root.select('id'))\n        return result",
            json!({"value":[{"id":"000123"}]}),
        ),
        (
            "Row",
            "rows.count + 1",
            "return root.map(lambda r: {'number': self.produce(r)}, max_parents=1)",
            json!({"number":9007199254740994_i64}),
        ),
    ] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let annotation = if input == "Row" {
            "Row.count"
        } else {
            "list[Row]"
        };
        let source = format!("class Composed(Program):\n    @compute\n    def produce(self, rows: {input}) -> {annotation}:\n        return {expression}\n    def build(self):\n        root = {token}.get('000123')\n        {build}\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "composed",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            expected
        );
    }
    for id in ["000123", "000124"] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class Observe(Program):\n    @compute\n    def produce(self, row: Row) -> Row.boxed:\n        return row.boxed\n    @compute\n    def consume(self, row: Row) -> bool:\n        try:\n            return row.value[0].optional_count is None\n        except AttributeError:\n            return False\n    def build(self):\n        root = {token}.get('{id}')\n        boxed = root.map(lambda r: {{'boxed': root}}, max_parents=1)\n        result = self.produce(boxed)\n        return result, self.consume(result)\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "presence",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let returned =
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap();
        assert_eq!(
            returned["value"][0].get("optional_count"),
            if id == "000123" {
                Some(&Value::Null)
            } else {
                None
            }
        );
        assert_eq!(
            serde_json::to_value(&run.return_steps[1].result.entities()[0].fields).unwrap(),
            json!({"value": id == "000123"})
        );
    }
    for (number, accepted) in [(7, true), (20, false)] {
        let (mut es, host, token) = super::recursive_values::fixture_context(base.clone());
        let mut foreign = (*es.cgs).clone();
        foreign
            .values
            .get_mut("count")
            .unwrap()
            .domain
            .constraints
            .max = Some(10.0);
        foreign.bind_registry_entry_id("other");
        let foreign = Arc::new(foreign);
        es.contexts_by_entry.insert(
            "other".into(),
            Arc::new(plasm_core::CgsContext::entry("other", foreign.clone())),
        );
        es.teaching_exposure.as_mut().unwrap().expose_entities(
            &[es.cgs.as_ref(), foreign.as_ref()],
            foreign.clone(),
            "other",
            &["Sample"],
        );
        let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            es.teaching_exposure.as_ref().unwrap(),
            &es.python_teaching,
        )
        .unwrap();
        let symbol = wave
            .value_contracts
            .iter()
            .find(|(_, t)| {
                t.domain
                    .as_ref()
                    .is_some_and(|d| d.entry_id == "other" && d.value_ref.as_str() == "count")
            })
            .unwrap()
            .0;
        let source = format!("class Federated(Program):\n    @compute\n    def produce(self, row: Row) -> list[{symbol}]:\n        value: {symbol} = {number}\n        return [value]\n    @compute\n    def consume(self, row: Row) -> Row.value:\n        return row.value\n    def build(self):\n        root = {token}.get('000123').select('count')\n        result = self.produce(root)\n        return self.consume(result)\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "federated-return",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await;
        if accepted {
            assert_eq!(
                serde_json::to_value(&result.unwrap().return_steps[0].result.entities()[0].fields)
                    .unwrap(),
                json!({"value":[7]})
            );
        } else {
            assert!(result.unwrap_err().diagnostic().contains("compute return"));
        }
    }
    {
        let (es, _, token) = super::recursive_values::fixture_context(base.clone());
        let update = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .method_sym_for("types", "Sample", "sample_update");
        let source = format!("class NoAuthority(Program):\n    @compute\n    def copy(self, row: Row) -> Row:\n        return row\n    def build(self):\n        copied = self.copy({token}.get('000123'))\n        return copied.{update}(count=1)\n");
        assert!(
            compile_python_program(&es, &source).await.is_err(),
            "returned record acquired mutation authority"
        );
    }
    // Both static type errors and unknown boundary types fail before review.
    for (annotation, expression, diagnostic) in [
        ("int", "row.text", "Return type does not match"),
        ("Row.date", "'2026-02-31'", "Return type does not match"),
        (
            "Row.timestamp",
            "'not-a-time'",
            "Return type does not match",
        ),
        ("Row.state", "'invalid'", "Return type does not match"),
        ("Any", "row.text", "unknown Plasm return type"),
        ("tuple[int]", "(1,)", "unsupported Plasm return annotation"),
        ("v99999", "row.text", "unknown Plasm return type"),
    ] {
        let (es, _, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class Invalid(Program):\n    @compute\n    def produce(self, row: Row) -> {annotation}:\n        return {expression}\n    def build(self):\n        return self.produce({token}.get('000123'))\n");
        let error = compile_python_program(&es, &source).await.unwrap_err();
        assert!(error.to_string().contains(diagnostic), "{error}");
    }
    for (annotation, expression) in [
        ("Row.uuid", "row.text"),
        ("Row.address", "row.text"),
        ("Row.price", "{'__plasm_money': '1.25', 'currency': 'EUR'}"),
    ] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let source = format!("class InvalidValue(Program):\n    @compute\n    def produce(self, row: Row) -> {annotation}:\n        return {expression}\n    def build(self):\n        return self.produce({token}.get('000123'))\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let error = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "invalid-return",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.diagnostic().contains("compute return"), "{error}");
    }
    for (expression, accepted) in [("row.count + 1", true), ("2 ** 63", false)] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let update = symbols.method_sym_for("types", "Sample", "sample_update");
        let source = format!("class Store(Program):\n    @compute\n    def produce(self, row: Row) -> Row.count:\n        return {expression}\n    def build(self):\n        root = {token}.get('000123')\n        result = self.produce(root.select('count'))\n        return root.{update}(count=result)\n");
        let before = writes.lock().unwrap().len();
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "effects",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await;
        if accepted {
            result.unwrap();
            assert_eq!(
                writes.lock().unwrap()[before],
                json!({"count":9007199254740994_i64})
            );
        } else {
            assert!(result.unwrap_err().diagnostic().contains("compute return"));
            assert_eq!(
                writes.lock().unwrap().len(),
                before,
                "invalid return reached write IO"
            );
        }
    }
}

#[tokio::test]
async fn scalar_compute_preserves_all_fixture_domains() {
    scalar_compute_fixture_domains(&[]).await;
}

#[tokio::test]
async fn scalar_blob_roundtrip_preserves_value_boundary() {
    scalar_compute_fixture_domains(&["attachment"]).await;
}

async fn scalar_compute_fixture_domains(fields: &[&str]) {
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(|| async { axum::Json(super::python::value_contract_matrix::record()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let mut expected = super::python::value_contract_matrix::record();
    expected["timestamp"] = json!("2026-09-25T12:34:56.123456+00:00");
    expected["price"]["format"] = json!({"encoding": "decimal_string"});
    for (field, value) in expected.as_object().unwrap() {
        if !fields.is_empty() && !fields.contains(&field.as_str()) {
            continue;
        }
        for projected in [true, false] {
            let (es, host, token) = super::recursive_values::fixture_context(base.clone());
            let domain = es.cgs.get_entity("Sample").unwrap().fields[field.as_str()]
                .kind
                .registry_key();
            let ty =
                plasm_core::value_contract::ValueContract::from_domain(&es.cgs, "types", domain)
                    .unwrap();
            let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
                es.teaching_exposure.as_ref().unwrap(),
                &es.python_teaching,
            )
            .unwrap();
            let symbol = wave
                .value_contracts
                .iter()
                .find(|(_, t)| **t == ty)
                .unwrap()
                .0;
            let annotation =
                if !es.cgs.get_entity("Sample").unwrap().fields[field.as_str()].required {
                    format!("{symbol} | None")
                } else {
                    symbol.clone()
                };
            let argument = if projected {
                format!("item.select(value='{field}')")
            } else {
                format!("item.{field}")
            };
            let source = format!("class Scalar(Program):\n    @compute\n    def consume(self, value: {annotation}) -> {annotation}:\n        return value\n    def build(self):\n        item = {token}.get('000123')\n        return self.consume({argument})\n");
            let bundle = compile_python_program(&es, &source)
                .await
                .unwrap_or_else(|e| panic!("{field}, projected={projected}: {e}"));
            // A serialized/reloaded plan must retain scalar-cell intent, even
            // when the cell has object-shaped storage (Blob/Json).
            let mut artifact = bundle.into_artifact();
            let wire = serde_json::to_vec(&artifact.comp).unwrap();
            artifact.comp = serde_json::from_slice(&wire).unwrap();
            let bundle = plasm_agent::PlasmCompBundle::new(artifact).unwrap();
            if !projected {
                assert!(bundle.artifact().comp.steps.values().any(|step| matches!(step,
                    plasm_core::PlasmStepPayload::Derive(d) if d.derive.kind == plasm_core::plasm_monad::DeriveKind::Cell
                )), "direct extraction retains its cell IL kind");
            }
            let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
                &es,
                &host,
                &es.prompt_hash,
                "scalar-domains",
                &bundle,
                true,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{field}, projected={projected}: {}", e.diagnostic()));
            assert_eq!(
                serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
                json!({"value": value}),
                "{field}, projected={projected}"
            );
        }
    }
}

#[tokio::test]
async fn compute_values_have_no_reserved_content_accessor() {
    let (es, host, _) = super::recursive_values::fixture_context("http://127.0.0.1:1".into());
    for (annotation, expression, use_value, expected) in [
        (
            "Row | None",
            "row if row.value > 0 else None",
            "{'whole': content}",
            json!({"whole":{"content":"ok", "value":7}}),
        ),
        (
            "Row | None",
            "None",
            "{'whole': content}",
            json!({"whole":null}),
        ),
        (
            "Row",
            "row",
            "{'field': content.content, 'number': content.value, 'whole': content}",
            json!({"field":"ok", "number":7, "whole":{"content":"ok", "value":7}}),
        ),
        (
            "str",
            "row.content",
            "{'text': content.upper()}",
            json!({"text":"OK"}),
        ),
        (
            "list[int]",
            "[row.value, 9]",
            "{'first': content[0], 'all': content}",
            json!({"first":7, "all":[7,9]}),
        ),
        (
            "None",
            "None",
            "{'empty': content is None}",
            json!({"empty":true}),
        ),
    ] {
        let source = format!("class Direct(Program):\n    @compute\n    def content(self, row: Row) -> {annotation}:\n        return {expression}\n    def build(self):\n        content = self.content({{'content': 'ok', 'value': 7}})\n        return {use_value}\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        super::assert_comp_witness(&dry).unwrap();
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "direct-compute-values",
            &bundle,
            true,
            None,
            None,
            Some(dry),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            expected
        );
        if annotation == "str" {
            let invalid = source.replace(use_value, "content.content");
            let error = compile_python_program(&es, &invalid).await.unwrap_err();
            assert!(
                error.contains("attribute") && error.contains("content"),
                "{error}"
            );
        }
    }
}

#[tokio::test]
async fn exact_money_library_outer_and_authored_compute() {
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(|| async { axum::Json(super::python::value_contract_matrix::record()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    for authored in [false, true] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let expression = "money_div(money_add(row.price, row.price), factor='2')";
        let definition = if authored {
            format!("    @compute\n    def calculate(self, row: Row) -> Row.price:\n        return {expression}\n")
        } else {
            String::new()
        };
        let result = if authored {
            "self.calculate(root.select('price'))".into()
        } else {
            format!("root.select(value=lambda row: {expression})")
        };
        let source = format!("class Exact(Program):\n{definition}    def build(self):\n        root = {token}.get('000123')\n        return {result}\n");
        let bundle = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "exact-money",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("{source}\n{}", e.diagnostic()));
        let fields = &run.return_steps[0].result.entities()[0].fields;
        let value = fields.get("value").expect("scalar money result");
        let plasm_core::TypedFieldValue::Money(money) = value else {
            panic!("lost Money type: {value:?}")
        };
        assert_eq!(money.amount().to_string(), "12345678901234567890.12345678");
        assert_eq!(money.currency(), Some("USD"));
    }
}
