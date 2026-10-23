//! Authored historical output reuse must not replay producers or replace fresh reads.
use super::*;
use plasm_core::symbol_tuning::SymbolRender;

#[test]
fn recorded_output_restore_never_replays_effects_or_satisfies_fresh_reads() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    for direct in [false, true] {
                        for restores in [1, 3] {
                            check_recorded_output(direct, restores).await;
                        }
                    }
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn check_recorded_output(direct: bool, restores: usize) {
    let es = session();
    let events = Arc::new(Mutex::new(Vec::new()));
    let host = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
        engine: ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("http://127.0.0.1:9".into()),
                ..Default::default()
            },
            Arc::new(Transport(events.clone(), None, None)),
            None,
        ),
        mode: ExecutionMode::Live,
        registry: Arc::new(CgsRegistry::from_pairs(vec![(
            "matrix".into(),
            "Matrix".into(),
            vec![],
            es.cgs.clone(),
        )])),
        catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })
    .unwrap();
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let counter = symbols.entity_sym_for("matrix", "Counter");
    let reader = symbols.entity_sym_for("matrix", if direct { "Wire" } else { "Counter" });
    let advance = symbols.method_sym_for("matrix", "Counter", "advance");
    let source = format!("class Acquire(Program):\n    def build(self):\n        return {counter}.{advance}(id=\"one\")\n");
    let bundle = crate::plasm_compile::compile_python_program(&es, &source)
        .await
        .unwrap();
    let dry = super::super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    assert!(
        !dry.fuse_clean_read(),
        "the acquisition still requires review/run"
    );
    let produced = super::super::super::orchestrator::run_plasm_comp_with_dispatch(
        &es,
        &host,
        &es.prompt_hash,
        "reuse",
        &bundle,
        true,
        None,
        None,
        Some(dry),
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(*events.lock().unwrap(), [Event::Advance]);
    let artifact = produced.return_steps[0]
        .artifact
        .as_ref()
        .expect("recorded producer output");
    assert!(host
        .run_artifacts
        .get_payload(&es.prompt_hash, "other-session", artifact.run_id)
        .await
        .is_none());
    let captured_flow = es
        .core
        .get_run_artifact(artifact.run_id)
        .await
        .unwrap()
        .flow
        .clone();
    assert!(
        captured_flow.is_some(),
        "completed producer retains flow evidence"
    );
    let payload = host
        .run_artifacts
        .get_payload(&es.prompt_hash, "reuse", artifact.run_id)
        .await
        .unwrap();
    let doc = crate::run_artifacts::parse_run_artifact_document_bytes(&payload.bytes).unwrap();
    check_output_rejections(&es, &doc, artifact.run_id).await;
    let reuse_source = format!(
        "class Reuse(Program):\n    def build(self):\n        return self.recorded(\"{}\")\n",
        artifact.run_id.to_wire()
    );
    let reuse = crate::plasm_compile::compile_python_program(&es, &reuse_source)
        .await
        .unwrap();
    let reuse_dry = super::super::super::evaluate_plasm_comp_dry(&es, &reuse).unwrap();
    assert!(reuse_dry.fuse_clean_read());
    assert!(reuse_dry.flow.node_facts.values().any(|facts| facts
        .residual
        .provenance
        .contains(&artifact.run_id.to_wire())));
    assert!(reuse_dry.node_results.iter().any(|node| node["operation"]
        .as_str()
        .is_some_and(|text| text.contains("historical; no producer execution"))));
    let isolated = session();
    let unavailable = crate::plasm_compile::compile_python_program(&isolated, &reuse_source)
        .await
        .unwrap_err();
    assert!(unavailable
        .to_string()
        .contains("unavailable in this session"));
    assert_eq!(*events.lock().unwrap(), [Event::Advance]);
    let value = |collection: &plasm_runtime::execution::ExecutionCollection| {
        collection.resident_entities()[0]
            .fields
            .get("value")
            .map(|field| field.to_value())
    };
    for _ in 0..restores {
        let reused = super::super::super::orchestrator::run_plasm_comp_with_dispatch(
            &es,
            &host,
            &es.prompt_hash,
            "reuse",
            &reuse,
            true,
            None,
            None,
            Some(super::super::super::evaluate_plasm_comp_dry(&es, &reuse).unwrap()),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            value(&reused.return_steps[0].result.collection),
            Some(Value::Integer(1))
        );
        let recorded = host
            .run_artifacts
            .completed_output(&es.prompt_hash, "reuse", artifact.run_id)
            .await
            .unwrap();
        assert_eq!(recorded.id(), artifact.run_id);
        assert_eq!(recorded.entry_id(), "matrix");
        let restored = recorded.collection();
        assert_eq!(value(restored), Some(Value::Integer(1)));
        assert_eq!(
            restored.membership(),
            produced.return_steps[0].result.collection.membership()
        );
        assert_eq!(
            *events.lock().unwrap(),
            [Event::Advance],
            "restoration issues no requests"
        );
    }
    let source = format!("class ChangeAndObserve(Program):\n    def build(self):\n        old = self.recorded(\"{}\").take(1)\n        {counter}.{advance}(id=old[0].id)\n        return {reader}.get(\"one\")\n", artifact.run_id.to_wire());
    let bundle = crate::plasm_compile::compile_python_program(&es, &source)
        .await
        .unwrap();
    let dry = super::super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    assert!(
        !dry.fuse_clean_read(),
        "reuse does not authorize another mutation"
    );
    let current = super::super::super::orchestrator::run_plasm_comp_with_dispatch(
        &es,
        &host,
        &es.prompt_hash,
        "reuse",
        &bundle,
        true,
        None,
        None,
        Some(dry),
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        value(&current.return_steps[0].result.collection),
        Some(Value::Integer(2))
    );
    assert_eq!(
        *events.lock().unwrap(),
        [Event::Advance, Event::Advance, Event::Read(2)]
    );
    for _ in 0..restores {
        let reused = super::super::super::orchestrator::run_plasm_comp_with_dispatch(
            &es,
            &host,
            &es.prompt_hash,
            "reuse",
            &reuse,
            true,
            None,
            None,
            Some(super::super::super::evaluate_plasm_comp_dry(&es, &reuse).unwrap()),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            value(&reused.return_steps[0].result.collection),
            Some(Value::Integer(1)),
            "admitted output stays pinned after a subsequent write"
        );
        assert_eq!(
            value(&doc.recorded_collection().unwrap()),
            Some(Value::Integer(1))
        );
        assert_eq!(
            *events.lock().unwrap(),
            [Event::Advance, Event::Advance, Event::Read(2)]
        );
    }
    let fresh_source = format!(
        "class Fresh(Program):\n    def build(self):\n        return {reader}.get(\"one\")\n"
    );
    let fresh = crate::plasm_compile::compile_python_program(&es, &fresh_source)
        .await
        .unwrap();
    let fresh_dry = super::super::super::evaluate_plasm_comp_dry(&es, &fresh).unwrap();
    let observed = super::super::super::orchestrator::run_plasm_comp_with_dispatch(
        &es,
        &host,
        &es.prompt_hash,
        "reuse",
        &fresh,
        true,
        None,
        None,
        Some(fresh_dry),
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        value(&observed.return_steps[0].result.collection),
        Some(Value::Integer(2)),
        "importing historical values cannot replace a current read"
    );
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == Event::Advance)
            .count(),
        2
    );
    es.core
        .append_run_artifact(
            artifact.run_id,
            crate::execute_session::GraphEpoch(0),
            artifact.resource_index + 1000,
            payload,
        )
        .await;
    es.core
        .record_output_flow(artifact, captured_flow.unwrap())
        .await;
    let rejected = crate::plasm_compile::compile_python_program(&es, &reuse_source)
        .await
        .unwrap_err();
    assert!(
        rejected.to_string().contains("no retained flow evidence"),
        "an older observation cannot stamp its flow proof on a replacement"
    );
}

async fn check_output_rejections(
    es: &ExecuteSession,
    doc: &crate::run_artifacts::RunArtifactDocument,
    id: crate::run_artifacts::RunArtifactId,
) {
    use crate::run_artifacts::{CompletedOutputError, RunArtifactStore};
    use plasm_core::collection_codec::{CollectionCheckpoint, Observation, RecordingCodec};
    let store = RunArtifactStore::memory();
    let mut foreign = doc.clone();
    foreign.session_id = "different-session".into();
    store
        .insert(&es.prompt_hash, "reuse", id, &foreign)
        .await
        .unwrap();
    assert!(matches!(
        store.completed_output(&es.prompt_hash, "reuse", id).await,
        Err(CompletedOutputError::SessionMismatch)
    ));
    for observation in [Observation::MoreAvailable, Observation::UnprovenPage] {
        let collection = plasm_runtime::execution::ExecutionCollection::observe_for(
            &es.cgs,
            &"abstract-partial",
            0,
            doc.entities.iter().cloned().collect(),
            observation,
        )
        .unwrap();
        let mut partial = doc.clone();
        partial.collection =
            CollectionCheckpoint::capture(&RecordingCodec::new(), collection.membership()).unwrap();
        store
            .insert(&es.prompt_hash, "reuse", id, &partial)
            .await
            .unwrap();
        assert!(matches!(
            store.completed_output(&es.prompt_hash, "reuse", id).await,
            Err(CompletedOutputError::Incomplete)
        ));
    }
}
