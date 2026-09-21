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
            let cgs = plasm_core::load_schema_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/hydration_boundary_matrix")).unwrap();
            let engine = ExecutionEngine::new_with_transport(crate::ExecutionConfig::default(), Arc::new(DelayedTransport), None);
            let mut mat = SessionMaterialization::new();
            let entities: Vec<_> = ids.iter().map(|id| {
                let reference = Ref::new("SavedNote", id.to_string());
                mat.stamp_capability_params(&reference, IndexMap::from([("access_token".into(),Value::String(format!("token-{id}")))]));
                CachedEntity::from_decoded(reference, IndexMap::from([("note_id".into(), Value::Integer(*id))]), Default::default(), 0, EntityCompleteness::Complete)
            }).collect();
            let entities = serde_json::from_slice(&serde_json::to_vec(&entities).unwrap()).unwrap();
            let source = ExecutionResult {
                entities, count: ids.len(), has_more: false, coverage: crate::ResultCoverage::Complete,
                pagination_resume: None, paging_handle: None, source: ExecutionSource::Cache,
                stats: Default::default(), request_fingerprints: vec![], operations: OperationLedger::empty(),
            };
            let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
            let result = super::super::EXECUTION_COMPILED_CATALOG.scope(compiled, engine.execute_chain_via_get_bindings(&source, cgs.get_entity("SavedNote").unwrap(), "Note".into(), &"note_get".into(), &IndexMap::from([("note_id".into(),"note_id".into())]), &cgs, &mut mat, ExecutionMode::Live)).await.unwrap();
            assert_eq!(result.entities.iter().map(|row| row.reference.primary_slot_str().parse::<i64>().unwrap()).collect::<Vec<_>>(), ids);
        });
    }
}
