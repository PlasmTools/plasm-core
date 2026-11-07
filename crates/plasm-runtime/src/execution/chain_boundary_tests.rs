use super::*;
use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::json;
use std::sync::Arc;

struct DelayedTransport;

#[async_trait]
impl crate::HttpTransport for DelayedTransport {
    async fn send_compiled_http(
        &self,
        _: &str,
        request: &plasm_compile::CompiledRequest,
        _: Option<crate::auth::ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        let id = request
            .path
            .strip_prefix("/notes/")
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let Some(Value::Object(headers)) = &request.headers else {
            panic!("missing headers")
        };
        assert_eq!(
            headers.get("Authorization"),
            Some(&Value::String(format!("token-{id}")))
        );
        tokio::time::sleep(std::time::Duration::from_millis((id % 3) as u64 * 3)).await;
        Ok((json!({"note_id":id,"title":"note"}), None))
    }
    async fn get_json_absolute(
        &self,
        _: &str,
        _: Option<crate::auth::ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute request")
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn scoped_hydration_preserves_order_and_row_scoped_parameters(ids in prop::collection::vec(1i64..12, 2..12)) {
        let ids = [vec![4, 9], ids].concat();
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let cgs = plasm_core::load_schema_dir(&std::path::Path::new(crate::test_manifest_dir()).join("../../fixtures/schemas/hydration_boundary_matrix")).unwrap();
            let engine = ExecutionEngine::new_with_transport(crate::ExecutionConfig::default(), Arc::new(DelayedTransport), None);
            let mut mat = SessionMaterialization::new();
            let entities: Vec<_> = ids.iter().map(|id| {
                let reference = Ref::new("SavedNote", id.to_string());
                mat.stamp_capability_params(&reference, IndexMap::from([("access_token".into(),Value::String(format!("token-{id}")))]));
                CachedEntity::from_decoded(reference, IndexMap::from([("note_id".into(), Value::Integer(*id))]), Default::default(), 0, EntityCompleteness::Complete)
            }).collect();
            let entities = serde_json::from_slice(&serde_json::to_vec(&entities).unwrap()).unwrap();
            let source = ExecutionResult {
collection: crate::execution::test_collection(entities, crate::ResultCoverage::Complete),
has_more: false,
pagination_resume: None,
paging_handle: None,
source: ExecutionSource::Cache,
stats: Default::default(),
request_fingerprints: vec![],
operations: OperationLedger::empty(),
};
            let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
            let result = super::super::EXECUTION_COMPILED_CATALOG.scope(compiled, engine.execute_chain_via_get_bindings(&source, cgs.get_entity("SavedNote").unwrap(), "Note".into(), &"note_get".into(), &IndexMap::from([("note_id".into(),"note_id".into())]), &cgs, &mut mat, ExecutionMode::Live)).await.unwrap();
            assert_eq!(result.entities().iter().map(|row| row.reference.primary_slot_str().parse::<i64>().unwrap()).collect::<Vec<_>>(), ids);
        });
    }
}

#[tokio::test]
async fn embedded_occurrences_keep_distinct_parent_scopes_and_duplicates() {
    #[derive(Clone)]
    struct ScopedTransport(Arc<std::sync::Mutex<Vec<String>>>);
    #[async_trait]
    impl crate::HttpTransport for ScopedTransport {
        async fn send_compiled_http(
            &self,
            _: &str,
            request: &plasm_compile::CompiledRequest,
            _: Option<crate::auth::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            let Some(Value::Object(headers)) = &request.headers else {
                panic!("headers")
            };
            let Some(Value::String(token)) = headers.get("Authorization") else {
                panic!("token")
            };
            self.0.lock().unwrap().push(token.clone());
            Ok((
                json!({"note_id":if token == "wrong" { 2 } else { 1 },"title":token}),
                None,
            ))
        }
        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<crate::auth::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            panic!("unexpected request")
        }
    }
    let cgs = plasm_core::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/hydration_boundary_matrix"),
    )
    .unwrap();
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig::default(),
        Arc::new(ScopedTransport(calls.clone())),
        None,
    );
    let mut mat = SessionMaterialization::new();
    let parents: Vec<_> = [("a", "scope-a"), ("b", "scope-b"), ("a", "scope-a")]
        .into_iter()
        .map(|(id, token)| {
            let reference = Ref::new("Folder", id);
            mat.stamp_capability_params(
                &reference,
                IndexMap::from([("access_token".into(), Value::String(token.into()))]),
            );
            let mut parent = CachedEntity::from_decoded(
                reference,
                IndexMap::from([("id".into(), Value::String(id.into()))]),
                Default::default(),
                0,
                EntityCompleteness::Complete,
            );
            parent.update_relations(
                "notes".into(),
                plasm_core::row_contract::RelationMembership::observe(
                    None,
                    &"fixture",
                    vec![Ref::new("Note", "1")],
                    None,
                )
                .unwrap(),
                0,
            );
            parent
        })
        .collect();
    let source = ExecutionResult {
        collection: crate::execution::test_collection(parents, crate::ResultCoverage::Complete),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: vec![],
        operations: OperationLedger::empty(),
    };
    let chain = plasm_core::ChainExpr::auto_get(Expr::Get(GetExpr::new("Folder", "a")), "notes");
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let result = super::super::EXECUTION_COMPILED_CATALOG
        .scope(
            compiled.clone(),
            engine.execute_chain_materialized(
                &chain,
                source,
                &cgs,
                &mut mat,
                ExecutionMode::Live,
                StreamConsumeOpts::default(),
                ExecuteOptions::default(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        result
            .entities()
            .iter()
            .map(|row| row.fields["title"].to_value())
            .collect::<Vec<_>>(),
        vec![
            Value::String("scope-a".into()),
            Value::String("scope-b".into()),
            Value::String("scope-a".into())
        ]
    );
    assert_eq!(*calls.lock().unwrap(), vec!["scope-a", "scope-b"]);

    calls.lock().unwrap().clear();
    let mut mat = SessionMaterialization::new();
    let parents = [("a", "scope-a"), ("b", "scope-b"), ("a", "scope-a")]
        .into_iter()
        .map(|(id, token)| {
            let reference = Ref::new("SavedNote", id);
            mat.stamp_capability_params(
                &reference,
                IndexMap::from([("access_token".into(), Value::String(token.into()))]),
            );
            CachedEntity::from_decoded(
                reference,
                IndexMap::from([("note_id".into(), Value::Integer(1))]),
                Default::default(),
                0,
                EntityCompleteness::Complete,
            )
        })
        .collect();
    let source = ExecutionResult {
        collection: crate::execution::test_collection(parents, crate::ResultCoverage::Complete),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: vec![],
        operations: OperationLedger::empty(),
    };
    let result = super::super::EXECUTION_COMPILED_CATALOG
        .scope(
            compiled.clone(),
            engine.execute_chain_via_get_bindings(
                &source,
                cgs.get_entity("SavedNote").unwrap(),
                "Note".into(),
                &"note_get".into(),
                &IndexMap::from([("note_id".into(), "note_id".into())]),
                &cgs,
                &mut mat,
                ExecutionMode::Live,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        result
            .entities()
            .iter()
            .map(|row| row.fields["title"].to_value())
            .collect::<Vec<_>>(),
        vec![
            Value::String("scope-a".into()),
            Value::String("scope-b".into()),
            Value::String("scope-a".into())
        ]
    );
    assert_eq!(*calls.lock().unwrap(), vec!["scope-a", "scope-b"]);

    let mut mat = SessionMaterialization::new();
    let reference = Ref::new("SavedNote", "a");
    mat.stamp_capability_params(
        &reference,
        IndexMap::from([("access_token".into(), Value::String("wrong".into()))]),
    );
    let parent = CachedEntity::from_decoded(
        reference,
        IndexMap::from([("note_id".into(), Value::Integer(1))]),
        Default::default(),
        0,
        EntityCompleteness::Complete,
    );
    let source = ExecutionResult {
        collection: crate::execution::test_collection(
            vec![parent],
            crate::ResultCoverage::Complete,
        ),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: Default::default(),
        request_fingerprints: vec![],
        operations: OperationLedger::empty(),
    };
    let error = super::super::EXECUTION_COMPILED_CATALOG
        .scope(
            compiled,
            engine.execute_chain_via_get_bindings(
                &source,
                cgs.get_entity("SavedNote").unwrap(),
                "Note".into(),
                &"note_get".into(),
                &IndexMap::from([("note_id".into(), "note_id".into())]),
                &cgs,
                &mut mat,
                ExecutionMode::Live,
            ),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, RuntimeError::HydrationGet { source, .. } if matches!(*source, RuntimeError::GetIdentityMismatch { .. }))
    );
}
