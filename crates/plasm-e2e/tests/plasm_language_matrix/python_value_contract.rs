//! Type-boundary matrix: concrete CGS domains through the real checked Monty worker.
use plasm_agent::python_compute::{PreparedCompute, ValueContract};
use plasm_core::symbol_tuning::SymbolRender;
use plasm_core::{TeachingExposureSession, CGS};
use serde_json::{json, Value};

pub(in super::super) fn fixture() -> CGS {
    plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_value_contract"),
    )
    .unwrap()
}

fn membership(cgs: &CGS) -> plasm_core::collection_codec::RecordedCollection<plasm_core::Ref> {
    use plasm_core::collection_codec::{
        CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
    };
    RecordingCodec::new()
        .record(
            CollectionIdentity::for_expression(cgs, &"value contract fixture", 0).unwrap(),
            vec![plasm_core::Ref::new("Sample", "000123")],
            Observation::Literal,
        )
        .unwrap()
}

pub(in super::super) fn record() -> Value {
    json!({
        "id": "000123", "text": "plain", "uuid": "123e4567-e89b-12d3-a456-426614174000", "date": "2026-09-25", "flag": true, "count": 9007199254740993_i64, "ratio": 1.25,
        "address": "person@example.test", "state": "open", "states": ["open", "closed"],
        "timestamp": "2026-09-25T12:34:56.123456Z", "local_timestamp": "2026-09-25T12:34:56.123456", "epoch": 1720000000123_i64,
        "price": {"__plasm_money": "12345678901234567890.12345678", "currency": "USD"},
        "reference": "000456", "document": {"nested": [{"n": 18446744073709551615_u64}]},
        "attachment": {"bytes": "AAEC"}, "numbers": [1, 2, 3], "matrix": [[1, 2], []],
        "records": [{"label": "nested", "values": [true, null]}], "optional_count": null
    })
}

#[tokio::test]
async fn python_value_contract_matrix_projection_alias_and_review_seal() {
    use plasm_agent::execute_session::ExecuteSession;
    use plasm_agent::plasm_compile::compile_python_program;
    use std::sync::Arc;
    let cgs = Arc::new(fixture());
    let exposure = TeachingExposureSession::new(&cgs, "types", &["Sample"]);
    let token = exposure.to_symbol_map().entity_sym_for("types", "Sample");
    let contexts = indexmap::IndexMap::from([(
        "types".into(),
        Arc::new(plasm_core::CgsContext::entry("types", cgs.clone())),
    )]);
    let es = ExecuteSession::new(
        "types".into(),
        String::new(),
        cgs.clone(),
        contexts,
        "types".into(),
        String::new(),
        String::new(),
        None,
        vec!["Sample".into()],
        Some(exposure),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    );
    for field in cgs.get_entity("Sample").unwrap().fields.values() {
        let source = format!("class Typed(Program):\n    @compute\n    def render(self, row: Row) -> str:\n        return str(row.copied)\n    def build(self):\n        rows = {token}.get('000123').select(copied='{name}')\n        text = self.render(rows)\n        return text\n", name = field.name);
        let compiled = compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", field.name));
        let wire = serde_json::to_value(&compiled.artifact().comp).unwrap();
        let fields = wire["steps"]["text"]["compute"]["op"]["input_schema"]["fields"]
            .as_array()
            .unwrap();
        let copied = fields.iter().find(|f| f["name"] == "copied").unwrap();
        let mut expected = plasm_core::value_contract::ValueContract::from_domain(
            &cgs,
            "types",
            field.kind.registry_key(),
        )
        .unwrap();
        expected.nullable = !field.required;
        assert_eq!(
            copied["value_type"],
            serde_json::to_value(expected).unwrap(),
            "{}",
            field.name
        );
        if field.name.as_str() == "matrix" {
            let mut forged = wire.clone();
            // Corrupt both copies while keeping their coarse array summaries consistent.
            for (_, step) in forged["steps"].as_object_mut().unwrap().iter_mut() {
                if step.get("compute").is_none() {
                    continue;
                }
                for schema in ["schema", "op"] {
                    let fields = if schema == "op" {
                        &mut step["compute"][schema]["input_schema"]["fields"]
                    } else {
                        &mut step["compute"][schema]["fields"]
                    };
                    if let Some(fields) = fields.as_array_mut() {
                        for f in fields {
                            if f["name"] == "copied" {
                                f["value_type"]["domain"]["value_ref"] = json!("numbers");
                            }
                        }
                    }
                }
            }
            let mut artifact = compiled.artifact().clone();
            artifact.comp = serde_json::from_value(forged).unwrap();
            let result =
                plasm_agent::plasm_compile::PlasmCompBundle::new(artifact).and_then(|bundle| {
                    plasm_agent::plasm_plan_run::evaluate_plasm_comp_dry(&es, &bundle)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
            assert!(result.is_err(), "forged recursive domain accepted");
        }
    }
}

#[test]
fn python_value_contract_matrix_materialization_and_rejections() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "types", &["Sample"]).to_symbol_map();
    let token = symbols.entity_sym_for("types", "Sample");
    let contract = ValueContract::from_cgs(&cgs, "types", symbols.as_ref(), &token).unwrap();
    assert_eq!(
        contract.fields.len(),
        cgs.get_entity("Sample").unwrap().fields.len()
    );
    let input = record();
    let rows = contract
        .materialize(
            &contract.owner,
            &membership(&cgs),
            &[serde_json::from_value(input.clone()).unwrap()],
        )
        .unwrap();
    assert_eq!(serde_json::to_value(&rows[0]).unwrap(), input);
    for (field, invalid) in [
        ("date", json!("2026-02-31")),
        ("timestamp", json!("not-a-time")),
        ("uuid", json!("invented")),
        ("count", json!(1.5)),
        ("count", json!(true)),
        ("count", json!(null)),
        ("state", json!("invented")),
        ("states", json!(["invented"])),
        ("address", json!("not-an-email")),
        ("id", json!(123)),
        ("numbers", json!([1, "2"])),
        ("matrix", json!([[1], [false]])),
        ("price", json!({"__plasm_money": "1.25", "currency": "EUR"})),
        ("reference", json!([])),
    ] {
        let mut row = input.clone();
        row[field] = invalid;
        assert!(
            contract
                .materialize(
                    &contract.owner,
                    &membership(&cgs),
                    &[serde_json::from_value(row).unwrap()]
                )
                .is_err(),
            "accepted {field}"
        );
    }
    let mut empty = input.clone();
    empty["matrix"] = json!([]);
    empty["numbers"] = json!([]);
    contract
        .materialize(
            &contract.owner,
            &membership(&cgs),
            &[serde_json::from_value(empty).unwrap()],
        )
        .unwrap();
    let domain = contract.fields["matrix"]
        .value_type
        .domain
        .as_ref()
        .unwrap();
    assert_eq!(domain.value_ref.as_str(), "matrix");
    assert_eq!(domain.catalog_hash, cgs.catalog_cgs_hash_hex());
    assert!(contract.fields["matrix"]
        .value_type
        .validate(&plasm_core::Value::Array(vec![]), &cgs, "another", "matrix")
        .is_err());
}

#[tokio::test]
async fn python_value_contract_matrix_real_monty() {
    let cgs = fixture();
    let symbols = TeachingExposureSession::new(&cgs, "types", &["Sample"]).to_symbol_map();
    let token = symbols.entity_sym_for("types", "Sample");
    let pool = plasm_agent::python_pool::PythonPool::default();
    for (expression, expected) in [
        ("rows[0].id", "000123"),
        ("rows[0].text", "plain"),
        ("rows[0].uuid", "123e4567-e89b-12d3-a456-426614174000"),
        ("str(rows[0].date)", "2026-09-25"),
        ("str(rows[0].count)", "9007199254740993"),
        ("str(rows[0].flag)", "True"),
        ("str(rows[0].ratio)", "1.25"),
        ("rows[0].address", "person@example.test"),
        ("rows[0].state", "open"),
        ("'|'.join(s for s in rows[0].states)", "open|closed"),
        ("rows[0].timestamp.isoformat()", "2026-09-25T12:34:56.123456+00:00"),
        ("rows[0].epoch.isoformat()", "2024-07-03T09:46:40.123000+00:00"),
        (
            "str(rows[0].price['__plasm_money'])",
            "12345678901234567890.12345678",
        ),
        ("str(rows[0].reference)", "000456"),
        (
            "str(obj['n']) if isinstance(doc := rows[0].document, dict) and isinstance(xs := doc.get('nested'), list) and xs and isinstance(obj := xs[0], dict) else ''",
            "18446744073709551615",
        ),
        ("str(blob['bytes']) if isinstance(blob := rows[0].attachment, dict) else ''", "AAEC"),
        ("'|'.join(str(n) for n in rows[0].numbers)", "1|2|3"),
        ("str(rows[0].matrix[0][1])", "2"),
        ("str(record['label']) if isinstance(record := rows[0].records[0], dict) else ''", "nested"),
        ("str(rows[0].optional_count)", "None"),
    ] {
        let source = format!(
            "@compute\ndef render(rows: list[Value[{token}]]) -> str:\n    return {expression}\n"
        );
        let checked = PreparedCompute::prepare(&source, &cgs, "types", symbols.as_ref())
            .unwrap_or_else(|e| panic!("{expression}: {e}"));
        let result = checked
            .run(
                &pool,
                &checked.contract.as_ref().unwrap().owner,
                &membership(&cgs),
                &[serde_json::from_value(record()).unwrap()],
            )
            .await
            .unwrap_or_else(|e| panic!("{expression}: {e}"));
        assert_eq!(result.as_str(), Some(expected), "{expression}");
    }
}

#[tokio::test]
async fn python_outer_record_preserves_every_fixture_value_domain() {
    let (es, _, token) =
        super::super::recursive_values::fixture_context("http://127.0.0.1:1".into());
    let cgs = &es.cgs;
    for field in cgs.get_entity("Sample").unwrap().fields.values() {
        let source = format!("class Typed(Program):\n    @compute\n    def consume(self, row: Row) -> Row.copied:\n        return row.copied\n    def build(self):\n        item = {token}.get('000123')\n        record = {{'copied': item.{name}}}\n        result = self.consume(record)\n        return result\n", name = field.name);
        let compiled = plasm_agent::plasm_compile::compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", field.name));
        let wire = serde_json::to_value(&compiled.artifact().comp).unwrap();
        let copied = &wire["steps"]["result"]["compute"]["op"]["input_schema"]["fields"][0];
        let mut expected = plasm_core::value_contract::ValueContract::from_domain(
            cgs,
            "types",
            field.kind.registry_key(),
        )
        .unwrap();
        expected.nullable = !field.required;
        assert_eq!(
            copied["value_type"],
            serde_json::to_value(expected).unwrap(),
            "{}",
            field.name
        );
    }
}
