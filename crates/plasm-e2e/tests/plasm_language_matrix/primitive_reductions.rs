//! CGS type inventory -> value-preserving reductions -> real typed Monty consumer.
use plasm_agent::plasm_compile::compile_python_program;
use plasm_core::{value_contract::ValueShape, FieldType};
use serde_json::{json, Value};
use std::collections::BTreeSet;

// Exhaustive match makes a new CGS primitive require an explicit coverage decision.
fn kind(kind: &FieldType) -> &'static str {
    match kind {
        FieldType::Boolean => "boolean",
        FieldType::Number => "number",
        FieldType::Integer => "integer",
        FieldType::Uuid => "uuid",
        FieldType::DigitId => "digit_id",
        FieldType::Blob => "blob",
        FieldType::String => "string",
        FieldType::Select => "select",
        FieldType::MultiSelect => "multi_select",
        FieldType::Date => "date",
        FieldType::Array => "array",
        FieldType::Json => "json",
        FieldType::Money => "money",
        FieldType::EntityRef { .. } => "entity_ref",
    }
}
fn canonical(mut row: Value) -> Value {
    row["timestamp"] = json!("2026-09-25T12:34:56.123456+00:00");
    // Selection reductions retain native money and its wire metadata.
    row["price"]["format"] = json!({"encoding":"decimal_string"});
    if row["price"]["__plasm_money"] == "2.50" {
        row["price"]["__plasm_money"] = json!("2.5");
    }
    row
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) async fn run() -> usize {
    run_boundary(false).await
}

pub(super) async fn run_boundary(typed_output: bool) -> usize {
    let mut first = super::python::value_contract_matrix::record();
    first["optional_count"] = json!(7);
    let mut last = first.clone();
    for (key, value) in [
        ("id", json!("000124")),
        ("text", json!("second")),
        ("uuid", json!("123e4567-e89b-12d3-a456-426614174001")),
        ("date", json!("2026-09-26")),
        ("flag", json!(false)),
        ("count", json!(-7)),
        ("ratio", json!(2.5)),
        ("address", json!("other@example.test")),
        ("state", json!("closed")),
        ("states", json!(["closed"])),
        ("epoch", json!(1720000000124_i64)),
        ("price", json!({"__plasm_money":"2.50","currency":"USD"})),
        ("reference", json!("000789")),
        ("document", json!({"other":[false]})),
        ("attachment", json!({"bytes":"AwQ="})),
        ("numbers", json!([4, 5])),
        ("matrix", json!([[3], []])),
        ("records", json!([{"label":"other"}])),
        ("optional_count", Value::Null),
    ] {
        last[key] = value;
    }
    let records = [first.clone(), last.clone()];
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(
            move |axum::extract::Path(id): axum::extract::Path<String>| {
                let row = records[usize::from(id == "000124")].clone();
                async move { axum::Json(row) }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let first = canonical(first);
    let last = canonical(last);
    let mut covered = BTreeSet::new();
    let mut count = 0;
    for field in first.as_object().unwrap().keys() {
        for scenario in 0..3 {
            let reverse = scenario == 1;
            let empty = scenario == 2;
            let (es, host, token) = super::recursive_values::fixture_context(base.clone());
            let domain = match field.as_str() {
                "id" => "identifier",
                "optional_count" => "count",
                x => x,
            };
            let input = plasm_core::value_contract::ValueContract::from_domain(
                &es.cgs,
                "types",
                &plasm_core::ValueDomainKey::new(domain).unwrap(),
            )
            .unwrap();
            let primitive = match &input.shape {
                ValueShape::Scalar { field_type } => kind(field_type),
                ValueShape::Array { .. } => "array",
                ValueShape::Temporal { .. } => "date",
                other => panic!("unclassified fixture shape: {other:?}"),
            };
            covered.insert(primitive);
            let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
                es.teaching_exposure.as_ref().unwrap(),
                &es.python_teaching,
            )
            .unwrap();
            let symbol = wave
                .value_contracts
                .iter()
                .find(|(_, t)| **t == input)
                .unwrap()
                .0;
            let (a, b) = if reverse {
                ("000124", "000123")
            } else {
                ("000123", "000124")
            };
            let filter = if empty {
                ".where(lambda row: row.id == '999999')"
            } else {
                ""
            };
            let source = format!("class Endpoints(Program):\n    @compute\n    def consume(self, row: Row) -> str:\n        return str(row.first_value == row.last_value)\n    def build(self):\n        rows = {token}.get('{a}').union({token}.get('{b}'))\n        result = rows{filter}.select(value='{field}').aggregate(first_value=agg.first('value'), last_value=agg.last('value'), n=agg.count())\n        rendered = self.consume(result)\n        return result, rendered\n");
            let source = if typed_output {
                format!("class Endpoints(Program):\n    @compute\n    def produce(self, row: Row) -> {symbol} | None:\n        return row.first_value\n    @compute\n    def consume(self, row: Row) -> Row.value:\n        return row.value\n    def build(self):\n        rows = {token}.get('{a}').union({token}.get('{b}'))\n        result = rows{filter}.select(value='{field}').aggregate(first_value=agg.first('value'), last_value=agg.last('value'), n=agg.count())\n        produced = self.produce(result)\n        rendered = self.consume(produced)\n        return produced, rendered\n")
            } else {
                source
            };
            let bundle = compile_python_program(&es, &source)
                .await
                .unwrap_or_else(|e| panic!("{field}: {e}"));
            let wire = serde_json::to_value(&bundle.artifact().comp).unwrap();
            if typed_output {
                for node in ["produced", "rendered"] {
                    let mut expected = serde_json::to_value(&input).unwrap();
                    expected["nullable"] = json!(true);
                    assert_eq!(
                        wire["steps"][node]["compute"]["schema"]["fields"][0]["value_type"],
                        expected,
                        "{field}: output contract"
                    );
                }
            } else {
                let fields = wire["steps"]["rendered"]["compute"]["op"]["input_schema"]["fields"]
                    .as_array()
                    .unwrap();
                for name in ["first_value", "last_value"] {
                    let actual = &fields.iter().find(|f| f["name"] == name).unwrap()["value_type"];
                    let mut expected = serde_json::to_value(&input).unwrap();
                    expected["nullable"] = json!(true);
                    assert_eq!(
                        actual, &expected,
                        "{field}: domain or recursive type erased"
                    );
                }
            }
            if typed_output && field == "count" && scenario == 0 {
                let mut forged = wire.clone();
                forged["steps"]["produced"]["compute"]["schema"]["fields"][0]["value_type"]
                    ["domain"] = Value::Null;
                let mut artifact = bundle.artifact().clone();
                artifact.comp = serde_json::from_value(forged).unwrap();
                let error =
                    plasm_agent::plasm_compile::PlasmCompBundle::new(artifact).and_then(|bundle| {
                        super::evaluate_plasm_comp_dry(&es, &bundle)
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    });
                assert!(error.is_err(), "forged return domain accepted");
            }
            let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            super::assert_comp_witness(&dry).unwrap();
            let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
                &es,
                &host,
                &es.prompt_hash,
                "primitive-reductions",
                &bundle,
                true,
                None,
                None,
                Some(dry),
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{field}/{reverse}: {e}"));
            let null = Value::Null;
            let (a, b) = if empty {
                (&null, &null)
            } else if reverse {
                (&last[field], &first[field])
            } else {
                (&first[field], &last[field])
            };
            assert_eq!(run.return_steps.len(), 2);
            assert_eq!(
                serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
                if typed_output {
                    json!({"value":a})
                } else {
                    json!({"first_value":a,"last_value":b,"n":if empty {0} else {2}})
                },
                "{field}/{reverse}"
            );
            assert_eq!(
                serde_json::to_value(&run.return_steps[1].result.entities()[0].fields).unwrap(),
                if typed_output {
                    json!({"value":a})
                } else {
                    json!({"value":if a==b {"True"} else {"False"}})
                },
                "{field}/{reverse}"
            );
            for step in &run.return_steps {
                assert_eq!(
                    step.result.coverage(),
                    plasm_runtime::ResultCoverage::Complete
                );
                assert!(step.result.operations.is_empty());
            }
            count += 1;
        }
    }
    assert_eq!(
        covered,
        BTreeSet::from([
            "boolean",
            "number",
            "integer",
            "uuid",
            "digit_id",
            "blob",
            "string",
            "select",
            "multi_select",
            "date",
            "array",
            "json",
            "money",
            "entity_ref"
        ])
    );
    assert_eq!(count, first.as_object().unwrap().len() * 3);
    count
}

/// Declared temporal ordering survives aliases, sort, reduction and a Python consumer.
#[tokio::test]
async fn temporal_ordering_contract_end_to_end() {
    let mut a = super::python::value_contract_matrix::record();
    a["timestamp"] = json!("2024-01-01T01:00:00+02:00");
    let mut b = a.clone();
    b["id"] = json!("000124");
    b["timestamp"] = json!("2024-01-01T00:00:00Z");
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(
            move |axum::extract::Path(id): axum::extract::Path<String>| {
                let row = if id == "000123" { a.clone() } else { b.clone() };
                async move { axum::Json(row) }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    for grouped in [false, true] {
        let (es, host, token) = super::recursive_values::fixture_context(base.clone());
        let reduction = if grouped {
            "group_by('flag',"
        } else {
            "aggregate("
        };
        let source = format!("class Ordered(Program):\n    @compute\n    def check(self, row: Row) -> bool:\n        return row.lo is not None and row.hi is not None and row.lo == row.first and row.hi == row.last and row.lo < row.hi\n    def build(self):\n        rows = {token}.get('000124').union({token}.get('000123')).select(value='timestamp', flag='flag').order_by('value')\n        result = rows.{reduction}lo=agg.min('value'), hi=agg.max('value'), first=agg.first('value'), last=agg.last('value'))\n        return self.check(result)\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "temporal-order",
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
            json!({"value":true})
        );
    }
}

#[tokio::test]
async fn unorderable_reductions_reject_before_execution() {
    let (es, _host, token) = super::recursive_values::fixture_context("http://127.0.0.1:1".into());
    for field in ["numbers", "document", "attachment", "reference"] {
        let program=format!("class Unsupported(Program):\n    def build(self):\n        return {token}.get('000123').aggregate(value=agg.min('{field}'))\n");
        let error = compile_python_program(&es, &program)
            .await
            .expect_err("unorderable contract admitted");
        assert!(error.to_string().contains("ordering"), "{field}: {error}");
    }
}

/// The declared instant, including inside a nested record, is the grouping key.
#[tokio::test]
async fn temporal_equivalence_contract_end_to_end() {
    let mut a = super::python::value_contract_matrix::record();
    a["timestamp"] = json!("2024-01-01T01:00:00+01:00");
    let mut b = a.clone();
    b["id"] = json!("000124");
    b["timestamp"] = json!("2024-01-01T00:00:00Z");
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(
            move |axum::extract::Path(id): axum::extract::Path<String>| {
                let row = if id == "000123" { a.clone() } else { b.clone() };
                async move { axum::Json(row) }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    for nested in [false, true] {
        for grouped in [false, true] {
            let (es, host, token) = super::recursive_values::fixture_context(base.clone());
            let project = if nested {
                "map(lambda row: {'key': {'when': row.timestamp}}, max_parents=2)"
            } else {
                "select(key='timestamp')"
            };
            let reduce = if grouped {
                "group_by('key', n=agg.count())"
            } else {
                "distinct('key')"
            };
            let source=format!("class Equivalent(Program):\n    def build(self):\n        rows = {token}.get('000123').union({token}.get('000124')).{project}\n        return rows.{reduce}.aggregate(n=agg.count())\n");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
                &es,
                &host,
                &es.prompt_hash,
                "temporal-equivalence",
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
                json!({"n":1}),
                "{source}"
            );
        }
    }
}
