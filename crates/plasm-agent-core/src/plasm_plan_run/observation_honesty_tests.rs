//! Properties through the production compiler and multi-line execution front door.
use crate::execute_pipeline::{ExecutePipeline, ExecutionIntent};
use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
use crate::http_execute::{apply_capability_seeds, CapabilitySeed};
use crate::server_state::CatalogBootstrap;
use async_trait::async_trait;
use plasm_compile::{CompiledRequest, HttpMethod};
use plasm_core::{discovery::CgsRegistry, loader::load_schema_dir, Value};
use plasm_runtime::{auth::ResolvedAuth, http_transport::HttpTransport};
use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode, RuntimeError};
use proptest::prelude::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Store {
    attempts: usize,
    rows: Vec<serde_json::Value>,
    source_rows: Vec<serde_json::Value>,
}
struct Transport {
    store: Arc<Mutex<Store>>,
    fail_at: usize,
}
#[async_trait]
impl HttpTransport for Transport {
    async fn send_compiled_http(
        &self,
        _: &str,
        req: &CompiledRequest,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        if matches!(req.method, HttpMethod::Post) {
            let mut store = self.store.lock().unwrap();
            let attempt = store.attempts;
            store.attempts += 1;
            if attempt == self.fail_at {
                return Err(RuntimeError::RequestError {
                    source: plasm_runtime::RequestFailure::HttpStatus(Box::new(
                        plasm_runtime::HttpStatusFailure::without_request(
                            422,
                            "fixture 422".into(),
                        ),
                    )),
                    attempts: 1,
                    status: Some(422),
                    body: None,
                });
            }
            let Value::Object(body) = req.body.as_ref().expect("POST body") else {
                panic!("object body")
            };
            let row = json!({"expense_id": format!("e{}", attempt+1), "group_id": "g1", "description": body.get("description").unwrap().as_str().unwrap()});
            store.rows.push(row.clone());
            return Ok((row, None));
        }
        if req.path.ends_with("/expenses") {
            if req.path.contains("/source/") {
                return Ok((json!(self.store.lock().unwrap().source_rows), None));
            }
            return Ok((json!(self.store.lock().unwrap().rows), None));
        }
        Ok((json!({"group_id":"g1", "name":"Group"}), None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute GET")
    }
}

async fn ordered_fanout_case(count: usize, fail_at: usize) {
    let cgs = Arc::new(
        load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/partial_write_relation_matrix"),
        )
        .unwrap(),
    );
    let registry = CgsRegistry::from_pairs(vec![("matrix".into(), "Matrix".into(), vec![], cgs)]);
    let store = Arc::new(Mutex::new(Store {
        source_rows: (0..count)
            .map(|index| {
                json!({
                    "expense_id": format!("source-{index}"),
                    "group_id": "source",
                    "description": format!("item-{index}"),
                })
            })
            .collect(),
        ..Store::default()
    }));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            hydrate: false,
            ..Default::default()
        },
        Arc::new(Transport {
            store: store.clone(),
            fail_at,
        }),
        None,
    );
    let host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(registry),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .expect("valid catalog fixture");
    let opened = Box::pin(apply_capability_seeds(
        &host,
        None,
        None,
        vec![
            CapabilitySeed {
                entry_id: "matrix".into(),
                entity: "PwExpense".into(),
            },
            CapabilitySeed {
                entry_id: "matrix".into(),
                entity: "PwGroup".into(),
            },
        ],
        None,
        None,
        None,
        "ordered effect fanout",
    ))
    .await
    .unwrap();
    let session = host
        .get_execute_session(&opened.prompt_hash, &opened.session_id)
        .await
        .unwrap();
    if count > crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE {
        // No fanout/aggregate consumer forces fetch-all here. The backend returns
        // one complete body larger than the presentation budget.
        let projection =
            "src = e1{group_id=\"source\"}\nselected = src | select description\nselected";
        let bundle = crate::plasm_compile::compile_plasm_expression(
            host.engine.prompt_pipeline(),
            Some(host.sessions.symbol_map_cross_cache()),
            &session,
            projection,
            projection,
        )
        .unwrap();
        let projected = Box::pin(ExecutePipeline::run_program(
            &session,
            &host,
            &opened.prompt_hash,
            &opened.session_id,
            &bundle,
            ExecutionIntent::Live,
            None,
            None,
            None,
        ))
        .await
        .unwrap();
        let delivered = crate::test_support::execution_fixtures::drain_presentation_pages(
            &session,
            &host,
            &opened.session_id,
            &projected.return_steps[0].result,
        )
        .await
        .unwrap();
        assert_eq!(
            delivered.entities().len(),
            count,
            "paging must preserve every acquired projected row"
        );
        assert!(!delivered.has_more);
    }
    let program = "src = e1{group_id=\"source\"}\nselected = src | select description\ncreated = selected => e1.m1(group_id=\"dest\", description=_.description)\ncreated";
    let bundle = crate::plasm_compile::compile_plasm_expression(
        host.engine.prompt_pipeline(),
        Some(host.sessions.symbol_map_cross_cache()),
        &session,
        program,
        program,
    )
    .unwrap();
    let result = Box::pin(ExecutePipeline::run_program(
        &session,
        &host,
        &opened.prompt_hash,
        &opened.session_id,
        &bundle,
        ExecutionIntent::Live,
        None,
        None,
        None,
    ))
    .await
    .expect_err("upstream rejection stops before the next effect occurrence");

    let (_, persisted, _) = Box::pin(crate::http_execute::execute_plasm_plasm_line(
        &host,
        &session,
        &opened.session_id,
        "e1{group_id=\"dest\"}",
        None,
        0,
    ))
    .await
    .unwrap();
    assert_eq!(
        persisted.entities().len(),
        fail_at,
        "same-session read must expose every successful fanout write"
    );

    let store = store.lock().unwrap();
    assert_eq!(
        store.attempts,
        fail_at + 1,
        "only the successful prefix and failed occurrence are dispatched"
    );
    assert_eq!(store.rows.len(), fail_at, "the suffix remains undispatched");
    assert_eq!(result.cause, plasm_runtime::FailureCause::Upstream);
    assert_eq!(
        result.recovery,
        plasm_runtime::RecoveryDisposition::ReconcileEffects
    );
    let receipts: Vec<_> = result
        .effects
        .iter()
        .filter(|ack| ack.capability == "pwexpense_create")
        .collect();
    assert_eq!(
        receipts.iter().map(|ack| ack.completed).sum::<usize>(),
        fail_at
    );
    assert_eq!(receipts.iter().map(|ack| ack.failed).sum::<usize>(), 1);
    assert!(result.effects_unresolved);
}

async fn case(count: usize, fail_at: usize) {
    let cgs = Arc::new(
        load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/partial_write_relation_matrix"),
        )
        .unwrap(),
    );
    let registry = CgsRegistry::from_pairs(vec![("matrix".into(), "Matrix".into(), vec![], cgs)]);
    let store = Arc::new(Mutex::new(Store::default()));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("http://127.0.0.1:9".into()),
            hydrate: false,
            ..Default::default()
        },
        Arc::new(Transport {
            store: store.clone(),
            fail_at,
        }),
        None,
    );
    let host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(registry),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .expect("valid catalog fixture");
    let opened = Box::pin(apply_capability_seeds(
        &host,
        None,
        None,
        vec![
            CapabilitySeed {
                entry_id: "matrix".into(),
                entity: "PwExpense".into(),
            },
            CapabilitySeed {
                entry_id: "matrix".into(),
                entity: "PwGroup".into(),
            },
        ],
        None,
        None,
        None,
        "observation honesty",
    ))
    .await
    .unwrap();
    let session = host
        .get_execute_session(&opened.prompt_hash, &opened.session_id)
        .await
        .unwrap();
    let program = (0..count)
        .map(|i| format!("x{i} = e1.m1(group_id=\"g1\", description=\"item{i}\")"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\nx0";
    let bundle = crate::plasm_compile::compile_plasm_expression(
        host.engine.prompt_pipeline(),
        Some(host.sessions.symbol_map_cross_cache()),
        &session,
        &program,
        &program,
    )
    .unwrap();
    let result = Box::pin(ExecutePipeline::run_program(
        &session,
        &host,
        &opened.prompt_hash,
        &opened.session_id,
        &bundle,
        ExecutionIntent::Live,
        None,
        None,
        None,
    ))
    .await;
    if fail_at >= count {
        let result = result.expect("all writes succeed");
        let completed: usize = result
            .return_steps
            .iter()
            .flat_map(|step| step.result.operations.entries())
            .map(|ack| ack.completed)
            .sum();
        assert_eq!(
            completed, count,
            "unreturned writes must have receipts exactly once"
        );
        assert_eq!(store.lock().unwrap().rows.len(), count);
        return;
    }
    let error = result.expect_err("plan should abort at injected failure");
    assert_eq!(
        error.effects.iter().map(|ack| ack.completed).sum::<usize>(),
        fail_at,
        "typed failure must retain acknowledged prefix exactly once"
    );
    assert_ne!(
        error.recovery,
        plasm_runtime::RecoveryDisposition::RepairProgram
    );
    assert_eq!(
        store.lock().unwrap().attempts,
        fail_at + 1,
        "production plan must stop at failure"
    );
    let expected = store.lock().unwrap().rows.clone();
    assert_eq!(expected.len(), fail_at);
    for expression in ["e2(\"g1\").r1", "e1{group_id=\"g1\"}"] {
        let (_, result, _) = Box::pin(crate::http_execute::execute_plasm_plasm_line(
            &host,
            &session,
            &opened.session_id,
            expression,
            None,
            0,
        ))
        .await
        .unwrap();
        let mut got: Vec<_> = result
            .entities()
            .iter()
            .map(|e| e.fields.get("expense_id").unwrap().to_value())
            .collect();
        let mut want: Vec<_> = (1..=fail_at)
            .map(|i| Value::String(format!("e{i}")))
            .collect();
        got.sort_by_key(|v| format!("{v:?}"));
        want.sort_by_key(|v| format!("{v:?}"));
        assert_eq!(
            got,
            want,
            "same-session read lost durable prefix (coverage={:?})",
            result.coverage()
        );
    }
    let attempts_before_next = store.lock().unwrap().attempts;
    let next = Box::pin(ExecutePipeline::run_program(
        &session,
        &host,
        &opened.prompt_hash,
        &opened.session_id,
        &bundle,
        ExecutionIntent::Live,
        None,
        None,
        None,
    ))
    .await
    .expect("a subsequent execution dispatches independently");
    assert_eq!(store.lock().unwrap().attempts, attempts_before_next + count);
    assert_eq!(store.lock().unwrap().rows.len(), fail_at + count);
    assert_eq!(
        next.return_steps
            .iter()
            .flat_map(|step| step.result.operations.entries())
            .map(|ack| ack.completed)
            .sum::<usize>(),
        count,
        "the subsequent execution owns its own receipts"
    );
}
proptest! {
 #![proptest_config(proptest::test_runner::Config::with_cases(12))]
 #[test]
 fn oph_real_plan_abort_preserves_backend_prefix(count in 1usize..6, fail_seed in 0usize..6) {
  let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
  rt.block_on(Box::pin(case(count, fail_seed % count)));
 }
}

proptest! {
 #![proptest_config(proptest::test_runner::Config::with_cases(8))]
 #[test]
 fn oph_mutating_fanout_preserves_effects_on_system_failure(count in 1usize..6, fail_seed in 0usize..6) {
  let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
  rt.block_on(Box::pin(ordered_fanout_case(count, fail_seed % count)));
 }
}

#[tokio::test]
async fn successful_plan_publishes_unreturned_write_receipts() {
    Box::pin(case(3, usize::MAX)).await;
}

#[tokio::test]
async fn materialized_collection_over_default_page_retains_all_writes() {
    Box::pin(ordered_fanout_case(31, 30)).await;
}
