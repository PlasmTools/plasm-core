//! Host continuations remain transport commands; executable DAG source is Python only.
use super::*;

/// Original non-row obligations exercised by the production-boundary tests below.
pub(super) const FEATURES: &[&str] = &["host_wait_cancel", "monadic_comp_witness"];
use plasm_agent::http_execute::try_dispatch_operation_program;
use plasm_agent::operation::{ExecutionScope, OpAcceptContext, OperationPhase};
use plasm_agent::plasm_compile::compile_python_program;
use plasm_agent::plasm_plan_run::run_plasm_comp_python;
use plasm_core::symbol_tuning::SymbolRender;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

fn run<F: Future<Output = ()> + Send + 'static>(future: F) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(future)
        })
        .unwrap()
        .join()
        .unwrap();
}

fn source(es: &plasm_agent::execute_session::ExecuteSession) -> String {
    let symbol = es
        .teaching_exposure
        .as_ref()
        .unwrap()
        .to_symbol_map()
        .entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    format!(
        "class HostContract(Program):\n    def build(self):\n        return {symbol}.get(\"i1\")\n"
    )
}

#[test]
fn python_monadic_comp_witness_uses_production_admission() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let bundle = compile_python_program(&es, &source(&es)).unwrap();
    assert_eq!(bundle.artifact().comp.metadata["source_language"], "python");
    let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    assert_comp_witness(&dry).unwrap();
    bundle.artifact().comp.validate().unwrap();
}

#[test]
fn python_http_wait_retrieves_actual_completed_python_result() {
    run(async {
        let base = hermit_lang_matrix::fresh_language_matrix_hermit_base_url().await;
        let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
        cgs.http_backend = base.clone();
        let cgs = Arc::new(cgs);
        let es = language_matrix::matrix_execute_session(cgs.clone());
        let st = language_matrix::matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .unwrap(),
            cgs,
        );
        let program = source(&es);
        assert!(
            try_dispatch_operation_program(&es, Some(&st), None, &program, None)
                .await
                .is_none()
        );
        let bundle = compile_python_program(&es, &program).unwrap();
        let handle = es.mint_operation_handle_plain();
        es.try_begin_async_operation(
            handle.clone(),
            plasm_runtime::CancelSignal::new(),
            OpAcceptContext::default(),
        )
        .unwrap();
        let command = format!("wait({handle})");
        assert!(
            compile_python_program(&es, &command).is_err(),
            "host commands are not DAG Python"
        );
        let pending = try_dispatch_operation_program(&es, Some(&st), None, &command, None)
            .await
            .unwrap()
            .unwrap();
        assert!(pending.return_steps.is_empty());
        assert_eq!(
            es.get_operation(&handle).unwrap().phase,
            OperationPhase::Running
        );
        let completed = run_plasm_comp_python(
            &es,
            &st,
            es.prompt_hash.as_str(),
            "python_host_wait",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(!completed.return_steps.is_empty());
        let expected_rows = completed.node_results.clone();
        let expected_markdown = completed.run_markdown.clone();
        es.finalize_operation_succeeded(&handle, completed, None);
        let waited = try_dispatch_operation_program(&es, Some(&st), None, &command, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(waited.node_results, expected_rows);
        assert_eq!(waited.run_markdown, expected_markdown);
        for rejected in [
            "LangItem(\"i1\")",
            "e1 | take 1",
            "wait(o1)\nLangItem",
            "cancel(o1)\nreturn e1",
        ] {
            assert!(
                try_dispatch_operation_program(&es, Some(&st), None, rejected, None)
                    .await
                    .is_none(),
                "dispatched non-protocol source: {rejected}"
            );
            assert!(
                compile_python_program(&es, rejected).is_err(),
                "native source fallback: {rejected}"
            );
        }
        assert!(
            try_dispatch_operation_program(&es, None, None, "wait(o999)", None)
                .await
                .unwrap()
                .is_err()
        );
    });
}

#[test]
fn python_http_cancel_interrupts_inflight_reviewed_read() {
    run(async {
        let entered = Arc::new(tokio::sync::Notify::new());
        let notify = entered.clone();
        let app = axum::Router::new().fallback(axum::routing::get(move || {
            let notify = notify.clone();
            async move {
                notify.notify_one();
                std::future::pending::<axum::Json<serde_json::Value>>().await
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
        cgs.http_backend = base.clone();
        let cgs = Arc::new(cgs);
        let es = Arc::new(language_matrix::matrix_execute_session(cgs.clone()));
        let st = Arc::new(language_matrix::matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .unwrap(),
            cgs,
        ));
        let bundle = compile_python_program(&es, &source(&es)).unwrap();
        let handle = es.mint_operation_handle_plain();
        let cancel = plasm_runtime::CancelSignal::new();
        es.try_begin_async_operation(handle.clone(), cancel.clone(), OpAcceptContext::default())
            .unwrap();
        let scope = ExecutionScope::for_async_operation(es.clone(), handle.clone(), cancel.clone());
        let worker_es = es.clone();
        let worker_st = st.clone();
        let worker = tokio::spawn(async move {
            run_plasm_comp_python(
                &worker_es,
                &worker_st,
                worker_es.prompt_hash.as_str(),
                "python_host_cancel",
                &bundle,
                true,
                None,
                Some(&scope),
                None,
                None,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("reviewed read started");
        let cancelled = try_dispatch_operation_program(
            &es,
            Some(&st),
            None,
            &format!("cancel({handle})"),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(cancel.is_cancelled());
        assert!(cancelled.return_steps.is_empty());
        assert_eq!(
            es.get_operation(&handle).unwrap().phase,
            OperationPhase::Cancelled
        );
        let error = tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .expect("cancel must interrupt pending HTTP")
            .unwrap()
            .expect_err("cancelled run must fail");
        assert!(error.contains("cancel"), "{error}");
        let waited =
            try_dispatch_operation_program(&es, Some(&st), None, &format!("wait({handle})"), None)
                .await
                .unwrap()
                .unwrap();
        assert!(waited.return_steps.is_empty());
        assert_eq!(
            es.get_operation(&handle).unwrap().phase,
            OperationPhase::Cancelled
        );
        server.abort();
    });
}

#[test]
fn python_http_cancel_drains_inflight_write_without_replay() {
    run(async {
        let entered = Arc::new(tokio::sync::Notify::new());
        let released = Arc::new(tokio::sync::Notify::new());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let acknowledged = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notify = entered.clone();
        let release = released.clone();
        let count = requests.clone();
        let ack = acknowledged.clone();
        let app = axum::Router::new().fallback(axum::routing::post(
            move |axum::Json(mut body): axum::Json<serde_json::Value>| {
                let (notify, release, count, ack) =
                    (notify.clone(), release.clone(), count.clone(), ack.clone());
                async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    notify.notify_one();
                    release.notified().await;
                    body["id"] = serde_json::json!("created-once");
                    ack.store(true, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(body)
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut cgs = (*language_matrix::load_language_matrix_cgs()).clone();
        cgs.http_backend = base.clone();
        let cgs = Arc::new(cgs);
        let es = Arc::new(language_matrix::matrix_execute_session(cgs.clone()));
        let st = Arc::new(language_matrix::matrix_host_state(
            ExecutionEngine::new(ExecutionConfig {
                base_url: Some(base),
                ..Default::default()
            })
            .unwrap(),
            cgs,
        ));
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
        let create = symbols.method_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "create");
        let program = format!("class WriteContract(Program):\n    def build(self):\n        return {entity}.{create}(title=\"once\", score=1, owner=\"alice\")\n");
        let bundle = compile_python_program(&es, &program).unwrap();
        let handle = es.mint_operation_handle_plain();
        let cancel = plasm_runtime::CancelSignal::new();
        es.try_begin_async_operation(handle.clone(), cancel.clone(), OpAcceptContext::default())
            .unwrap();
        let scope = ExecutionScope::for_async_operation(es.clone(), handle.clone(), cancel.clone());
        let worker_es = es.clone();
        let worker_st = st.clone();
        let mut worker = tokio::spawn(async move {
            run_plasm_comp_python(
                &worker_es,
                &worker_st,
                worker_es.prompt_hash.as_str(),
                "python_host_cancel",
                &bundle,
                true,
                None,
                Some(&scope),
                None,
                None,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), entered.notified())
            .await
            .expect("reviewed write started");
        let cancelled = try_dispatch_operation_program(
            &es,
            Some(&st),
            None,
            &format!("cancel({handle})"),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(cancel.is_cancelled());
        assert!(cancelled.return_steps.is_empty());
        assert_eq!(
            es.get_operation(&handle).unwrap().phase,
            OperationPhase::Cancelled
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut worker)
                .await
                .is_err(),
            "cancellation must not drop an unacknowledged write"
        );
        assert!(!acknowledged.load(std::sync::atomic::Ordering::SeqCst));
        released.notify_one();
        let error = tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .expect("cancel must complete after write acknowledgement")
            .unwrap()
            .expect_err("cancelled run must fail");
        assert!(error.contains("cancel"), "{error}");
        let waited =
            try_dispatch_operation_program(&es, Some(&st), None, &format!("wait({handle})"), None)
                .await
                .unwrap()
                .unwrap();
        assert!(waited.return_steps.is_empty());
        assert_eq!(
            es.get_operation(&handle).unwrap().phase,
            OperationPhase::Cancelled
        );
        assert!(acknowledged.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cancel/wait never replays the write"
        );
        server.abort();
    });
}
