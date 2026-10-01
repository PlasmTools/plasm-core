//! Independent fixture observations and expected Python values through recursive DAG inputs.
use plasm_agent::{execute_session::ExecuteSession, plasm_compile::compile_python_program};
use plasm_core::{symbol_tuning::SymbolRender, CgsContext, TeachingExposureSession};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ResultCoverage};
use serde_json::{json, Value};
use std::sync::Arc;

// Expectations are literal fixture semantics, never derived from production stubs/codecs.
const LEAVES: &[(&str, &str, &str)] = &[
    ("id", "VALUE", "000123"),
    ("text", "VALUE", "plain"),
    ("uuid", "VALUE", "123e4567-e89b-12d3-a456-426614174000"),
    ("date", "str(VALUE)", "2026-09-25"),
    ("flag", "str(VALUE)", "True"),
    ("count", "str(VALUE)", "9007199254740993"),
    ("ratio", "str(VALUE)", "1.25"),
    ("address", "VALUE", "person@example.test"),
    ("state", "VALUE", "open"),
    ("states", "'|'.join(s for s in VALUE)", "open|closed"),
    ("timestamp", "VALUE.isoformat()", "2026-09-25T12:34:56.123456+00:00"),
    ("epoch", "VALUE.isoformat()", "2024-07-03T09:46:40.123000+00:00"),
    ("price", "str(VALUE['__plasm_money'])", "12345678901234567890.12345678"),
    ("reference", "str(VALUE)", "000456"),
    ("document", "str(obj['n']) if isinstance(doc := VALUE, dict) and isinstance(xs := doc.get('nested'), list) and xs and isinstance(obj := xs[0], dict) else ''", "18446744073709551615"),
    ("attachment", "str(blob['bytes']) if isinstance(blob := VALUE, dict) else ''", "AAEC"),
    ("numbers", "'|'.join(str(n) for n in VALUE)", "1|2|3"),
    ("matrix", "str(VALUE[0][1])", "2"),
    ("records", "str(record['label']) if isinstance(record := VALUE[0], dict) else ''", "nested"),
    ("optional_count", "str(VALUE)", "7"),
];
#[derive(Debug, Clone, Copy)]
enum Presence {
    Present,
    Absent,
    Null,
}

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn case(field: &str, expression: &str, expected: &str, presence: Presence, observed: bool) {
    let mut record = super::python::value_contract_matrix::record();
    record["optional_count"] = json!(7);
    match presence {
        Presence::Present => {}
        Presence::Absent => {
            record.as_object_mut().unwrap().remove(field);
        }
        Presence::Null => record[field] = Value::Null,
    }
    let served = record.clone();
    // Catalog temporal/money domains define canonical materialized encodings.
    // These expectations are explicit rather than copied from runtime output.
    if record.get("timestamp").is_some() {
        record["timestamp"] = json!("2026-09-25T12:34:56.123456+00:00");
    }
    if let Some(price) = record.get_mut("price").and_then(Value::as_object_mut) {
        price.insert("format".into(), json!({"encoding":"decimal_string"}));
    }

    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(move || {
            let served = served.clone();
            async move { axum::Json(served) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let (es, host, token) = fixture_context(base);
    let value = if observed {
        "root".into()
    } else {
        format!("[row.{field}]")
    };
    let access = if observed {
        format!("row.boxed.items[0].{field}")
    } else {
        "row.boxed.items[0]".into()
    };
    let expression = expression.replace("VALUE", &access);
    let source = format!("class Recursive(Program):\n    @compute\n    def render(self, row: Row) -> str:\n        try:\n            return {expression}\n        except AttributeError:\n            return 'absent'\n    def build(self):\n        root = {token}.get('000123')\n        boxed = root.map(lambda row: {{'boxed': {{'items': {value}}}}}, max_parents=1)\n        rendered = self.render(boxed)\n        return boxed, rendered\n");
    let bundle = compile_python_program(&es, &source)
        .await
        .unwrap_or_else(|e| panic!("{field}/{presence:?}/{observed}: {e}\n{source}"));
    let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    super::assert_comp_witness(&dry).unwrap();
    // A copied scalar must keep its named domain even under record/array wrappers.
    // The expected domain is fixture-authored, not inferred from the resulting IL.
    let domain = match field {
        "id" => "identifier",
        "optional_count" => "count",
        other => other,
    };
    let wire = serde_json::to_value(&bundle.artifact().comp).unwrap();
    let fields = wire["steps"]["rendered"]["compute"]["op"]["input_schema"]["fields"]
        .as_array()
        .expect("render input schema");
    let boxed = fields
        .iter()
        .find(|f| f["name"] == "boxed")
        .expect("boxed input");
    let items = &boxed["value_type"]["shape"]["fields"]["items"];
    assert_eq!(items["shape"]["shape"], "array", "collection contract");
    let element = &items["shape"]["element"];
    let leaf = if observed {
        assert_eq!(element["shape"]["shape"], "observed_record");
        assert!(
            element["shape"]["optional_fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == field)
                || field == "id"
        );
        &element["shape"]["fields"][field]
    } else {
        element
    };
    assert_eq!(
        leaf["domain"],
        json!({
            "entry_id": "types", "catalog_hash": es.cgs.catalog_cgs_hash_hex(),
            "value_ref": domain
        }),
        "nested domain erased: {field}"
    );
    let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "recursive",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await;
    if matches!(presence, Presence::Absent) && !observed {
        let error = run.expect_err("a constructed field cannot fabricate an absent value");
        assert!(
            error.diagnostic().contains("unobserved")
                || error.diagnostic().contains("missing")
                || error.diagnostic().contains("has no field"),
            "{field}: {error}"
        );
        return;
    }
    let run = run.unwrap_or_else(|e| panic!("{field}/{presence:?}/{observed}: {e}\n{source}"));
    let expected_value = if observed {
        json!([record])
    } else {
        json!([record[field]])
    };
    let boxed = &run.return_steps[0].result;
    assert_eq!(
        serde_json::to_value(&boxed.entities()[0].fields).unwrap(),
        json!({"boxed":{"items":expected_value}}),
        "{field}/{presence:?}/{observed}"
    );
    assert_eq!(boxed.coverage(), ResultCoverage::Complete);
    assert!(boxed.operations.is_empty());
    let text = match presence {
        Presence::Present => expected,
        Presence::Absent => "absent",
        Presence::Null => "None",
    };
    assert_eq!(
        serde_json::to_value(&run.return_steps[1].result.entities()[0].fields).unwrap(),
        json!({"value":text}),
        "{field}/{presence:?}/{observed}"
    );
}

pub(super) async fn run() -> usize {
    let mut count = 0;
    for &(field, expression, expected) in LEAVES {
        for observed in [false, true] {
            case(field, expression, expected, Presence::Present, observed).await;
            count += 1;
            // Get identity is authoritative even if omitted from the response.
            if field != "id" {
                case(field, expression, expected, Presence::Absent, observed).await;
                count += 1;
            }
            if field == "optional_count" {
                case(field, expression, expected, Presence::Null, observed).await;
                count += 1;
            }
        }
    }
    assert_eq!(count, 80);
    count
}

pub(super) fn fixture_context(
    base: String,
) -> (
    ExecuteSession,
    plasm_agent::server_state::PlasmHostState,
    String,
) {
    let mut cgs = super::python::value_contract_matrix::fixture();
    cgs.http_backend = base.clone();
    cgs.bind_registry_entry_id("types");
    let cgs = Arc::new(cgs);
    let exposure = TeachingExposureSession::new(&cgs, "types", &["Sample"]);
    let token = exposure.to_symbol_map().entity_sym_for("types", "Sample");
    let es = ExecuteSession::new(
        "recursive-values".into(),
        String::new(),
        cgs.clone(),
        indexmap::IndexMap::from([(
            "types".into(),
            Arc::new(CgsContext::entry("types", cgs.clone())),
        )]),
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
    let host = super::language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base),
            ..Default::default()
        })
        .unwrap(),
        cgs,
    );
    (es, host, token)
}
