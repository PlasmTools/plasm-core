use super::*;
use crate::credentials::{
    credential_error, CredentialReference, CredentialScope, SessionCredentialStore,
};
use crate::http_transport::HttpTransport;
use async_trait::async_trait;
use plasm_compile::CredentialSource;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct Store {
    records: std::sync::Mutex<Vec<(CredentialScope, CredentialSource)>>,
    expired: AtomicBool,
}

#[async_trait]
impl SessionCredentialStore for Store {
    async fn bind(
        &self,
        scope: CredentialScope,
        source: CredentialSource,
        _lifetime_seconds: u64,
    ) -> Result<CredentialReference, RuntimeError> {
        let mut records = self.records.lock().unwrap();
        let index = records
            .iter()
            .position(|entry| entry == &(scope.clone(), source))
            .unwrap_or_else(|| {
                records.push((scope, source));
                records.len() - 1
            });
        CredentialReference::parse(&format!("cr{index:032x}"))
    }
    async fn resolve(
        &self,
        reference: &CredentialReference,
        scope: &CredentialScope,
    ) -> Result<CredentialSource, RuntimeError> {
        if self.expired.load(Ordering::SeqCst) {
            return Err(credential_error("expired"));
        }
        let index = usize::from_str_radix(&reference.as_str()[2..], 16).unwrap();
        self.records
            .lock()
            .unwrap()
            .get(index)
            .filter(|entry| &entry.0 == scope)
            .map(|entry| entry.1)
            .ok_or_else(|| credential_error("scope mismatch"))
    }
}

#[derive(Default)]
struct InjectedSecrets(AtomicUsize);
impl crate::auth::SecretProvider for InjectedSecrets {
    fn get_secret<'a>(
        &'a self,
        key: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            (key == "test_injection").then(|| "synthetic-token".into())
        })
    }
}

#[derive(Default)]
struct Transport(AtomicUsize, bool);

#[async_trait]
impl HttpTransport for Transport {
    fn injects_host_auth(&self) -> bool {
        self.1
    }
    async fn send_compiled_http(
        &self,
        _base: &str,
        _request: &CompiledRequest,
        auth: Option<crate::auth::ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        assert_eq!(
            auth.unwrap().headers,
            vec![("Authorization".into(), "Bearer synthetic-token".into())]
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok((serde_json::json!({"value":"visible"}), None))
    }
    async fn get_json_absolute(
        &self,
        _url: &str,
        _auth: Option<crate::auth::ResolvedAuth>,
    ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
        panic!("unexpected absolute request")
    }
}

#[test]
fn credential_create_returns_receipt_and_cached_reads_revalidate_scope() {
    // Debug builds retain the full create/decoder future.
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(exercise_scoped_injection());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn exercise_scoped_injection() {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/credential_matrix"),
    )
    .unwrap();
    let store = Arc::new(Store::default());
    let secrets = Arc::new(InjectedSecrets::default());
    let transport = Arc::new(Transport::default());
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("https://example.test".into()),
            ..Default::default()
        },
        transport.clone(),
        Some(crate::auth::AuthResolver::new(
            plasm_core::AuthScheme::BearerToken {
                env: Some("test_injection".into()),
                hosted_kv: None,
                optional_env: false,
            },
            secrets.clone(),
        )),
    );
    let compiled_catalog = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    let material = Arc::new(ExecuteSessionMaterial {
        prompt_hash: "session".into(),
        session_id: "one".into(),
        catalog_revision: "revision".into(),
        compiled_catalog: compiled_catalog.clone(),
        credential_store: Some(store.clone()),
        transport_origin: None,
        ui_origin: None,
        catalog_bind: None,
    });
    ExecutionEngine::run_in_execute_task_scopes("https://example.test".into(), None, None, None, Some(material), compiled_catalog, None, None, async {
        let cap = cgs.get_capability("acquire").unwrap();
        let template = parse_capability_template(&cap.require_mapping().unwrap().template).unwrap();
        let env = serde_json::from_value(serde_json::json!({"record":"a"})).unwrap();
        let operation = compile_operation_dispatch(&template, &env).unwrap();
        assert!(store.records.lock().unwrap().is_empty(), "compilation cannot bind");
        assert!(engine.execute_with_replay(&operation, ExecutionMode::Replay, None).await.is_err());
        assert!(store.records.lock().unwrap().is_empty(), "replay cannot bind");
        let input: plasm_core::Value = serde_json::from_value(serde_json::json!({"record":"a"})).unwrap();
        let create = plasm_core::CreateExpr::new("acquire", "Receipt", input);
        let mut cache = SessionMaterialization::new();
        let result = engine.execute_create(&create, &cgs, &mut cache, ExecutionMode::Live).await.unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(store.records.lock().unwrap()[0].1, CredentialSource::Host {});
        assert!(!serde_json::to_string(&operation).unwrap().contains("synthetic-token"));
        assert_eq!(result.stats.network_requests, 0);
        assert_eq!(secrets.0.load(Ordering::SeqCst), 0, "binding must not resolve secrets");
        assert_eq!(transport.0.load(Ordering::SeqCst), 0);
        let reference = result.entities[0].fields["reference"].to_value();
        let request = serde_json::json!({"method":"GET","path":[{"type":"literal","value":"records"}],"auth":{"scheme":"credential","slot":"record_access","resource":{"type":"const","value":{"record":"a"}},"reference":{"type":"const","value":reference}}});
        let template = parse_capability_template(&request).unwrap();
        let mut operation = compile_operation_dispatch(&template, &CmlEnv::new()).unwrap();
        engine.execute_with_replay_full(&operation, ExecutionMode::Live, Some(&mut cache)).await.unwrap();
        engine.execute_with_replay_full(&operation, ExecutionMode::Live, Some(&mut cache)).await.unwrap();
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
        for resolver in [None, Some(crate::auth::AuthResolver::new(plasm_core::AuthScheme::None, secrets.clone()))] {
            let missing = ExecutionEngine::new_with_transport(
                ExecutionConfig { base_url: Some("https://example.test".into()), ..Default::default() },
                transport.clone(), resolver);
            assert!(missing.execute_with_replay_full(&operation, ExecutionMode::Live, Some(&mut cache)).await.is_err(),
                "missing injection must fail even with a cached response");
        }
        let delegated_transport = Arc::new(Transport(AtomicUsize::new(0), true));
        let delegated = ExecutionEngine::new_with_transport(
            ExecutionConfig { base_url: Some("https://example.test".into()), ..Default::default() },
            delegated_transport.clone(),
            Some(crate::auth::AuthResolver::new(plasm_core::AuthScheme::BearerToken {
                env: Some("test_injection".into()), hosted_kv: None, optional_env: false,
            }, secrets.clone())));
        delegated.execute_with_replay_full(&operation, ExecutionMode::Live, Some(&mut cache)).await.unwrap();
        assert_eq!(delegated_transport.0.load(Ordering::SeqCst), 1, "delegated injection must dispatch despite a cached response");
        assert!(delegated.execute_with_replay_full(&operation, ExecutionMode::Replay, Some(&mut cache)).await.is_err());
        store.expired.store(true, Ordering::SeqCst);
        assert!(engine.execute_with_replay_full(&operation, ExecutionMode::Live, Some(&mut cache)).await.is_err());
        store.expired.store(false, Ordering::SeqCst);
        let CompiledOperation::Http(request) = &mut operation else { panic!() };
        request.path = "/records/https://untrusted.test/collect".into();
        assert!(engine.execute_with_replay_full(&operation, ExecutionMode::Live, None).await.is_err());
        let CompiledOperation::Http(request) = &mut operation else { panic!() };
        request.path = "/records".into();
        request.credential.as_mut().unwrap().resource = serde_json::json!({"record":"other"});
        assert!(engine.execute_with_replay_full(&operation, ExecutionMode::Live, Some(&mut cache)).await.is_err());
        assert_eq!(transport.0.load(Ordering::SeqCst), 1);
    }).await;
}

#[tokio::test]
async fn typed_delete_payload_reaches_preflight_and_transport() {
    struct DeleteTransport;
    #[async_trait]
    impl HttpTransport for DeleteTransport {
        async fn send_compiled_http(
            &self,
            _: &str,
            request: &CompiledRequest,
            _: Option<crate::auth::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            assert_eq!(request.method, plasm_compile::HttpMethod::Delete);
            assert!(request.path.contains("/42/"));
            assert_eq!(
                request.headers.as_ref().unwrap().as_object().unwrap()["Authorization"].as_str(),
                Some("Bearer fixture-value")
            );
            Ok((serde_json::json!({}), None))
        }
        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<crate::auth::ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            panic!("unexpected GET")
        }
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let mut cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
    let touch = cgs.capabilities["langitem_secured_touch"].clone();
    let delete = cgs.capabilities.get_mut("langitem_delete").unwrap();
    delete.inputs = touch.inputs;
    delete.mapping = touch.mapping;
    delete.mapping.as_mut().unwrap().template.0["method"] = serde_json::json!("DELETE");
    let parsed =
        plasm_core::expr_parser::parse("LangItem(42).delete(access_token=\"fixture-value\")", &cgs)
            .unwrap();
    let Expr::Delete(delete) = parsed.expr else {
        panic!("delete")
    };
    let compiled = Arc::new(plasm_compile::compile_cgs_capability_templates(&cgs).unwrap());
    super::compile_preflight::preflight_compile_expr(
        &Expr::Delete(delete.clone()),
        &cgs,
        &compiled,
        &crate::view_plan::ViewAmbientContext::default(),
        &SessionMaterialization::new(),
    )
    .expect("preflight preserves payload");
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("https://example.test".into()),
            ..Default::default()
        },
        Arc::new(DeleteTransport),
        None,
    );
    ExecutionEngine::run_in_execute_task_scopes(
        "https://example.test".into(),
        None,
        None,
        None,
        None,
        compiled,
        None,
        None,
        async {
            engine
                .execute_delete(
                    &delete,
                    &cgs,
                    &mut SessionMaterialization::new(),
                    ExecutionMode::Live,
                )
                .await
                .expect("delete transport");
        },
    )
    .await;
}
