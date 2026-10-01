//! Finite pagination/embedding products through the Python compiler and HTTP.
use plasm_agent::{execute_session::ExecuteSession, plasm_compile::compile_python_program};
use plasm_core::{symbol_tuning::SymbolRender, CgsContext, TeachingExposureSession};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ResultCoverage};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug)]
enum Context {
    Page,
    Compute,
    Embed,
    FanoutEmbed,
    NestedFanoutEmbed,
    BoundedFanoutEmbed,
    CapturedFanoutEmbed,
}

fn row(n: usize) -> Value {
    json!({"id":format!("i{n}"),"n":n})
}

async fn page_case(kind: &str, count: usize, context: Context) {
    use axum::{
        extract::{Path, Query},
        routing::get,
        Json,
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let app = axum::Router::new()
        .route("/items/indexed/anchor", get(|| async { Json(row(999)) }))
        .route(
            "/items/{kind}",
            get(
                move |Path(kind): Path<String>, Query(query): Query<BTreeMap<String, String>>| {
                    let observed = observed.clone();
                    async move {
                        let number = |key: &str, default: usize| {
                            query
                                .get(key)
                                .and_then(|n| n.parse().ok())
                                .unwrap_or(default)
                        };
                        let limit = number(
                            if kind == "indexed" {
                                "page_limit"
                            } else {
                                "limit"
                            },
                            20,
                        );
                        let offset = match kind.as_str() {
                            "indexed" => number("page_index", 0) * limit,
                            "offset" => number("offset", 0),
                            "cursor" => number("cursor", 0),
                            _ => panic!("unexpected strategy"),
                        };
                        observed.lock().unwrap().push((offset, limit));
                        let end = count.min(offset + limit);
                        let mut result =
                            json!({"results":(offset..end).map(row).collect::<Vec<_>>()});
                        if kind == "cursor" && end < count {
                            result["next_cursor"] = json!(end.to_string());
                        }
                        Json(result)
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_pagination_matrix"),
    )
    .unwrap();
    cgs.http_backend = base.clone();
    let entry = super::language_matrix::MATRIX_ENTRY_ID;
    cgs.bind_registry_entry_id(entry);
    let cgs = Arc::new(cgs);
    let wave = ["Item", "ItemOffset", "ItemCursor"];
    let es = ExecuteSession::new(
        "collection-ph".into(),
        String::new(),
        cgs.clone(),
        indexmap::IndexMap::from([(
            entry.into(),
            Arc::new(CgsContext::entry(entry, cgs.clone())),
        )]),
        entry.into(),
        String::new(),
        String::new(),
        None,
        wave.iter().map(|s| s.to_string()).collect(),
        Some(TeachingExposureSession::new(&cgs, entry, &wave)),
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
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(
        entry,
        match kind {
            "indexed" => "Item",
            "offset" => "ItemOffset",
            _ => "ItemCursor",
        },
    );
    let anchor = symbols.entity_sym_for(entry, "Item");
    let query = format!("{entity}.query().page_size(20)");
    let source = match context {
        Context::BoundedFanoutEmbed => format!("class Capture(Program):\n    def build(self):\n        roots = {anchor}.get('anchor')\n        children = roots.flat_map(lambda parent: {query}.take(3), max_parents=1)\n        return roots.map(lambda parent: {{'items': children}}, max_parents=1)\n"),
        Context::CapturedFanoutEmbed => format!("class Capture(Program):\n    def build(self):\n        roots = {anchor}.get('anchor')\n        items = {query}\n        children = roots.flat_map(lambda parent: items, max_parents=1)\n        return roots.map(lambda parent: {{'items': children}}, max_parents=1)\n"),
        Context::FanoutEmbed => format!("class Capture(Program):\n    def build(self):\n        roots = {anchor}.get('anchor')\n        children = roots.flat_map(lambda parent: {query}, max_parents=1)\n        return roots.map(lambda parent: {{'items': children}}, max_parents=1)\n"),
        Context::NestedFanoutEmbed => format!("class Capture(Program):\n    def build(self):\n        roots = {anchor}.get('anchor')\n        children = roots.flat_map(lambda parent: {anchor}.get('anchor').flat_map(lambda inner: {query}, max_parents=1), max_parents=1)\n        return roots.map(lambda parent: {{'items': children}}, max_parents=1)\n"),
        Context::Page => format!("class Page(Program):\n    def build(self):\n        return {query}\n"),
        Context::Compute => format!("class Count(Program):\n    @compute\n    def count(self, rows: list[Row]) -> str:\n        return str(len(rows))\n    def build(self):\n        return self.count({query})\n"),
        Context::Embed => format!("class Embed(Program):\n    def build(self):\n        return {anchor}.get(\"anchor\").map(lambda parent: {{\"items\": {query}}}, max_parents=1)\n"),
    };
    let bundle = compile_python_program(&es, &source)
        .await
        .unwrap_or_else(|e| panic!("{source}\n{e}"));
    let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    super::assert_comp_witness(&dry).unwrap();
    let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "collection",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("{kind}/{count}/{context:?}: {}", e.diagnostic()));
    let result = &result.return_steps[0].result;
    let rows: Vec<Value> = result
        .entities()
        .iter()
        .map(|entity| {
            json!(entity
                .fields
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::to_value(v.to_value()).unwrap()))
                .collect::<BTreeMap<_, _>>())
        })
        .collect();
    let partial = matches!(context, Context::Page)
        && if kind == "cursor" {
            count > 20
        } else {
            count >= 20
        };
    assert_eq!(
        result.coverage(),
        if partial {
            ResultCoverage::Partial
        } else {
            ResultCoverage::Complete
        },
        "{kind}/{count}/{context:?}"
    );
    match context {
        Context::Page => {
            assert_eq!(rows, (0..count.min(20)).map(row).collect::<Vec<_>>());
            assert_eq!(result.has_more, partial);
            assert_eq!(
                result.paging_handle.is_some() || result.pagination_resume.is_some(),
                partial
            );
            assert_eq!(requests.lock().unwrap().as_slice(), [(0, 20)]);
        }
        Context::Compute
        | Context::Embed
        | Context::FanoutEmbed
        | Context::NestedFanoutEmbed
        | Context::BoundedFanoutEmbed
        | Context::CapturedFanoutEmbed => {
            assert!(
                !result.has_more
                    && result.paging_handle.is_none()
                    && result.pagination_resume.is_none()
            );
            let expected = if matches!(context, Context::Compute) {
                json!([{"value":count.to_string()}])
            } else {
                json!([{"items":(0..if matches!(context, Context::BoundedFanoutEmbed) {count.min(3)} else {count}).map(row).collect::<Vec<_>>() }])
            };
            assert_eq!(json!(rows), expected, "{kind}/{count}/{context:?}");
            let expected_pages = if matches!(context, Context::BoundedFanoutEmbed) {
                1
            } else if kind == "cursor" {
                count.div_ceil(20).max(1)
            } else {
                count / 20 + 1
            };
            assert_eq!(
                *requests.lock().unwrap(),
                (0..expected_pages)
                    .map(|page| (page * 20, 20))
                    .collect::<Vec<_>>(),
                "no skipped or duplicated pages: {kind}/{count}/{context:?}"
            );
        }
    }
    assert!(result.operations.is_empty());
    server.abort();
}

pub(super) async fn run() -> usize {
    let mut checked = 0;
    for kind in ["indexed", "offset", "cursor"] {
        for count in [0, 1, 20, 21] {
            for context in [Context::Page, Context::Compute, Context::Embed] {
                page_case(kind, count, context).await;
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 36);
    checked
}

#[tokio::test]
async fn complete_collection_demand_crosses_scoped_outputs() {
    for kind in ["indexed", "offset", "cursor"] {
        for count in [0, 21] {
            for context in [
                Context::FanoutEmbed,
                Context::NestedFanoutEmbed,
                Context::BoundedFanoutEmbed,
                Context::CapturedFanoutEmbed,
            ] {
                page_case(kind, count, context).await;
            }
        }
    }
}
