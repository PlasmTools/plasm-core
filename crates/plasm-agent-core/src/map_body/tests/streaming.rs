use super::*;

#[test]
fn map_body_streams_occurrences_before_host_read_completes() {
    on_runtime(async {
        use crate::occurrence_progress::{ExecutionStage, OccurrencePhase};
        let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        let (es, host, calls) = fixture_with_controls(1, None, Some(gate.clone()));
        let es = Arc::new(es);
        let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        let cancel = plasm_runtime::CancelSignal::new();
        es.try_begin_async_operation(handle.clone(), cancel.clone(), Default::default())
            .unwrap();
        let scope = crate::operation::ExecutionScope::for_async_operation(
            es.clone(),
            handle.clone(),
            cancel,
        );
        let mut rx = es.operation_progress_subscribe(&handle).unwrap();
        let (root, body) = program(&es);
        let bundle = compose(root, body).unwrap();
        let execution = crate::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "stream",
            &bundle,
            true,
            None,
            Some(&scope),
            None,
            None,
        );
        let inspect = async {
            gate.0.notified().await;
            let snapshot = crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle).unwrap();
            let child = snapshot
                .occurrences
                .iter()
                .find(|e| e.address.local_step == "children")
                .unwrap();
            assert_eq!(child.address.scope_path, vec!["result"]);
            assert_eq!(child.occurrence_path, vec![0]);
            assert_eq!(child.phase, OccurrencePhase::Running);
            assert_eq!(child.stage, Some(ExecutionStage::AwaitingHost));
            assert!(!snapshot.terminal);
            assert!(
                rx.try_recv().is_ok(),
                "events must arrive before run completion"
            );
            gate.1.notify_one();
        };
        let (run, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(execution, inspect)
        })
        .await
        .expect("live host interaction must complete or cancel");
        let run = run.unwrap();
        assert_eq!(rows(&run), vec![json!({"title":"Title 0", "labels":""})]);
        let mut seen_python = false;
        while let Ok(event) = rx.try_recv() {
            seen_python |= event.occurrences.iter().any(|e| {
                e.address.local_step == "reduced" && e.stage == Some(ExecutionStage::Executing)
            });
        }
        assert!(seen_python);
        let final_snapshot =
            crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle).unwrap();
        assert!(final_snapshot
            .occurrences
            .iter()
            .all(|e| e.phase == OccurrencePhase::Done));
        assert_eq!(*calls.lock().unwrap(), vec!["/items", "/items/i0/tags"]);
    });
}

#[test]
fn map_body_stream_retains_cancelled_occurrence_and_completed_parent() {
    on_runtime(async {
        use crate::occurrence_progress::OccurrencePhase;
        let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        let (es, host, calls) = fixture_with_controls(2, None, Some(gate.clone()));
        let es = Arc::new(es);
        let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        let cancel = plasm_runtime::CancelSignal::new();
        es.try_begin_async_operation(handle.clone(), cancel.clone(), Default::default())
            .unwrap();
        let scope = crate::operation::ExecutionScope::for_async_operation(
            es.clone(),
            handle.clone(),
            cancel,
        );
        let (root, body) = program(&es);
        let bundle = compose(root, body).unwrap();
        let execution = crate::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "cancel-stream",
            &bundle,
            true,
            None,
            Some(&scope),
            None,
            None,
        );
        let inspect = async {
            gate.0.notified().await;
            es.cancel_operation(&handle, None);
            gate.1.notify_one();
        };
        let (run, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(execution, inspect)
        })
        .await
        .expect("live host interaction must complete or cancel");
        assert!(run.unwrap_err().contains("cancelled"));
        let snapshot = crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle).unwrap();
        assert!(snapshot.terminal);
        assert!(snapshot
            .occurrences
            .iter()
            .any(|e| e.address.local_step == "items" && e.phase == OccurrencePhase::Done));
        assert!(snapshot
            .occurrences
            .iter()
            .any(|e| e.address.local_step == "children" && e.phase == OccurrencePhase::Cancelled));
        assert!(!snapshot
            .occurrences
            .iter()
            .any(|e| e.occurrence_path == vec![1]));
        assert_eq!(calls.lock().unwrap().len(), 2);
    });
}

#[test]
fn map_body_failed_python_input_preserves_live_evidence() {
    on_runtime(async {
        use crate::occurrence_progress::OccurrencePhase;
        let (es, host, _) = fixture(3);
        let es = Arc::new(es);
        let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        let cancel = plasm_runtime::CancelSignal::new();
        es.try_begin_async_operation(handle.clone(), cancel.clone(), Default::default())
            .unwrap();
        let scope = crate::operation::ExecutionScope::for_async_operation(
            es.clone(),
            handle.clone(),
            cancel,
        );
        let (root, mut body) = program(&es);
        let PlasmStepPayload::Invoke(child) = body.body.steps.get_mut("children").unwrap() else {
            unreachable!()
        };
        child.page_size = Some(1);
        let bundle = compose(root, body).unwrap();
        let error = crate::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "failed-stream",
            &bundle,
            true,
            None,
            Some(&scope),
            None,
            None,
        )
        .await
        .unwrap_err();
        es.finalize_operation_failed(&handle, error, None);
        let snapshot = crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle).unwrap();
        assert!(snapshot.terminal);
        assert!(snapshot
            .occurrences
            .iter()
            .any(|e| e.address.local_step == "reduced"
                && e.occurrence_path == vec![2]
                && e.phase == OccurrencePhase::Failed));
        assert!(snapshot
            .occurrences
            .iter()
            .any(|e| e.address.local_step == "output"
                && e.occurrence_path == vec![0]
                && e.phase == OccurrencePhase::Done));
        let op = es.get_operation(&handle).unwrap();
        let persisted =
            crate::mcp_transport_store::persisted_operations::descriptor_from_operation_state(
                &handle, &op, 0,
            );
        let restored = serde_json::from_value::<
            crate::mcp_transport_store::persisted_operations::PersistedOperationDescriptor,
        >(serde_json::to_value(persisted).unwrap())
        .unwrap();
        assert_eq!(
            crate::op_ui_telemetry::OpUiTelemetry::from_persisted(&restored, &handle).occurrences,
            snapshot.occurrences
        );
    });
}

#[test]
fn occurrence_concurrent_deltas_preserve_sequence_and_queued_python_cancels() {
    on_runtime(async {
        let (es, _, _) = fixture(0);
        let es = Arc::new(es);
        let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        let cancel = plasm_runtime::CancelSignal::new();
        es.try_begin_async_operation(handle.clone(), cancel.clone(), Default::default())
            .unwrap();
        let scope = crate::operation::ExecutionScope::for_async_operation(
            es.clone(),
            handle.clone(),
            cancel,
        );
        let mut rx = es.operation_progress_subscribe(&handle).unwrap();
        std::thread::scope(|threads| {
            for ordinal in 0..4 {
                let scope = &scope;
                threads.spawn(move || {
                    for step in 0..8 {
                        scope.report_occurrence(
                            crate::occurrence_progress::OccurrenceProgress::running(
                                vec!["body".into()],
                                format!("step_{step}"),
                                vec![ordinal],
                            ),
                        );
                    }
                });
            }
        });
        for expected in 1..=32 {
            let event = rx.try_recv().unwrap();
            assert_eq!(event.seq, expected);
            assert!(!event.occurrence_snapshot);
            assert_eq!(event.occurrences.len(), 1);
        }
        let wait = crate::python_compute::await_checked(
            Some(&scope),
            std::future::pending::<Result<(), String>>(),
        );
        let cancel_wait = async {
            tokio::task::yield_now().await;
            es.cancel_operation(&handle, None);
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            tokio::join!(wait, cancel_wait)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("cancelled"));
    });
}
