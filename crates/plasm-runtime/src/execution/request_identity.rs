//! Request-owned singleton observations have no invented response identity field.
use super::*;
use crate::auth::ResolvedAuth;

tokio::task_local! {
    static DISPATCH_AUTH: Option<ResolvedAuth>;
    static RESPONSE_FINGERPRINT: crate::RequestFingerprint;
}

pub(super) enum DispatchAuthScope {
    Unpinned,
    Anonymous,
    Authenticated(ResolvedAuth),
}

pub(super) fn pinned_auth() -> DispatchAuthScope {
    match DISPATCH_AUTH.try_with(Clone::clone) {
        Ok(Some(auth)) => DispatchAuthScope::Authenticated(auth),
        Ok(None) => DispatchAuthScope::Anonymous,
        Err(_) => DispatchAuthScope::Unpinned,
    }
}

pub(super) fn response_fingerprint() -> Option<crate::RequestFingerprint> {
    RESPONSE_FINGERPRINT.try_with(Clone::clone).ok()
}

pub(super) fn eligible(cgs: &CGS, entity: &str, template: &CapabilityTemplate) -> bool {
    cgs.get_entity(entity).is_some_and(|e| {
        e.implicit_request_identity
            && !e.fields.contains_key(&e.id_field)
            && e.key_vars.len() <= 1
            && e.id_from.as_ref().is_none_or(Vec::is_empty)
    }) && matches!(template, CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml)
        if cml.response_is_single_object() && cml.pagination.is_none())
}

pub(super) struct RequestIdentity {
    fingerprint: crate::RequestFingerprint,
    auth: Option<ResolvedAuth>,
}

impl RequestIdentity {
    pub(super) fn key(&self) -> String {
        self.fingerprint.to_hex()
    }

    pub(super) async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        DISPATCH_AUTH
            .scope(
                self.auth.clone(),
                RESPONSE_FINGERPRINT.scope(self.fingerprint.clone(), future),
            )
            .await
    }

    pub(super) fn validate_rows(&self, entity: &str, rows: usize) -> Result<(), RuntimeError> {
        if rows != 1 {
            return Err(RuntimeError::RequestIdentityCardinality {
                entity: entity.into(),
                rows,
            });
        }
        Ok(())
    }
}

impl ExecutionEngine {
    pub(super) async fn request_identity_for(
        &self,
        cgs: &CGS,
        entity: &str,
        template: &CapabilityTemplate,
        compiled: &CompiledOperation,
    ) -> Result<Option<RequestIdentity>, RuntimeError> {
        if !eligible(cgs, entity, template) {
            return Ok(None);
        }
        with_dispatch_entity(Some(entity), self.prepare_request_identity(compiled))
            .await
            .map(Some)
    }

    pub(super) async fn prepare_request_identity(
        &self,
        compiled: &CompiledOperation,
    ) -> Result<RequestIdentity, RuntimeError> {
        let request = match compiled {
            CompiledOperation::Http(r) | CompiledOperation::GraphQl(r) => r,
            _ => return Err(RuntimeError::HttpQueryTemplateRequired),
        };
        let auth = self.resolve_compiled_http_auth(request).await?;
        if matches!(
            self.transport.auth_scope(request, auth.as_ref())?,
            crate::http_transport::TransportAuthScope::Opaque
        ) {
            return Err(RuntimeError::RequestIdentityAuthOpaque);
        }
        let fingerprint = crate::RequestFingerprint::for_request_owned_identity(
            compiled,
            self.effective_http_base_for_request().as_ref(),
            auth.as_ref(),
        );
        Ok(RequestIdentity { fingerprint, auth })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> CGS {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/request_owned_observation");
        let cgs = plasm_core::load_schema(&path).expect("abstract request observation fixture");
        serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap()
    }

    fn operation(cgs: &CGS, cap: &str, selector: &str) -> (CapabilityTemplate, CompiledOperation) {
        let capability = cgs.get_capability(cap).unwrap();
        let template =
            parse_capability_template(&capability.require_mapping().unwrap().template).unwrap();
        let env = CmlEnv::from([("selector".into(), Value::String(selector.into()))]);
        let compiled = compile_operation(&template, &env).unwrap();
        (template, compiled)
    }

    #[test]
    fn query_and_receiver_free_quote_identity_is_request_owned_not_money_owned() {
        let cgs = fixture();
        for cap in ["observation_query", "observation_quote"] {
            let (template, compiled) = operation(&cgs, cap, "scope-a");
            assert!(eligible(&cgs, "Observation", &template));
            let key = crate::RequestFingerprint::for_request_owned_identity(
                &compiled,
                "https://fixture.test",
                None,
            )
            .to_hex();
            let decoder = create_entity_decoder_for_capability(
                "Observation",
                &cgs,
                Some(cap),
                None,
                Some(&key),
                None,
            );
            let first = decode_entities_with_cgs(
                &decoder,
                &serde_json::json!({"amount": "1.00"}),
                Some(&cgs),
            )
            .unwrap();
            let changed = decode_entities_with_cgs(
                &decoder,
                &serde_json::json!({"amount": "9.00"}),
                Some(&cgs),
            )
            .unwrap();
            assert_eq!(first.len(), 1);
            assert_eq!(first[0].reference, changed[0].reference);
            assert_ne!(first[0].fields["amount"], changed[0].fields["amount"]);
            assert!(!first[0].fields.contains_key("request_key"));
            let (_, distinct) = operation(&cgs, cap, "scope-b");
            assert_ne!(
                key,
                crate::RequestFingerprint::for_request_owned_identity(
                    &distinct,
                    "https://fixture.test",
                    None,
                )
                .to_hex(),
            );
        }
    }

    #[tokio::test]
    async fn identity_scope_pins_exact_credentials_and_response_cache_key() {
        let cgs = fixture();
        let (_, compiled) = operation(&cgs, "observation_query", "scope-a");
        let auth = |token: &str| ResolvedAuth {
            headers: vec![("Authorization".into(), token.into())],
            query_params: vec![],
        };
        let first = auth("account-a");
        let fingerprint = crate::RequestFingerprint::for_request_owned_identity(
            &compiled,
            "https://fixture.test",
            Some(&first),
        );
        assert_ne!(
            fingerprint,
            crate::RequestFingerprint::for_request_owned_identity(
                &compiled,
                "https://fixture.test",
                Some(&auth("account-b")),
            )
        );
        assert_ne!(
            fingerprint,
            crate::RequestFingerprint::for_request_owned_identity(
                &compiled,
                "https://other.test",
                Some(&first),
            )
        );
        let identity = RequestIdentity {
            fingerprint: fingerprint.clone(),
            auth: Some(first),
        };
        identity
            .scope(async {
                let DispatchAuthScope::Authenticated(auth) = pinned_auth() else {
                    panic!("expected pinned authenticated dispatch");
                };
                assert_eq!(auth.headers[0].1, "account-a");
                assert_eq!(response_fingerprint(), Some(fingerprint));
            })
            .await;
        assert!(matches!(pinned_auth(), DispatchAuthScope::Unpinned));
        assert!(response_fingerprint().is_none());
        assert!(matches!(
            identity.validate_rows("Observation", 2),
            Err(RuntimeError::RequestIdentityCardinality { rows: 2, .. })
        ));

        let anonymous = RequestIdentity {
            fingerprint: identity.fingerprint.clone(),
            auth: None,
        };
        anonymous
            .scope(async {
                assert!(matches!(pinned_auth(), DispatchAuthScope::Anonymous));
            })
            .await;
        assert!(matches!(pinned_auth(), DispatchAuthScope::Unpinned));
    }

    #[test]
    fn declared_identity_and_collection_responses_are_not_overridden() {
        let mut cgs = fixture();
        let (mut template, _) = operation(&cgs, "observation_query", "scope-a");
        if let CapabilityTemplate::Http(cml) = &mut template {
            cml.response.as_mut().unwrap().single = false;
        }
        assert!(!eligible(&cgs, "Observation", &template));
        if let CapabilityTemplate::Http(cml) = &mut template {
            cml.response.as_mut().unwrap().single = true;
        }
        cgs.entities.get_mut("Observation").unwrap().id_field = "amount".into();
        assert!(!eligible(&cgs, "Observation", &template));
    }

    struct Observations(std::sync::atomic::AtomicUsize);

    struct OpaqueTransport;

    #[async_trait::async_trait]
    impl crate::http_transport::HttpTransport for OpaqueTransport {
        fn injects_host_auth(&self) -> bool {
            true
        }

        async fn send_compiled_http(
            &self,
            _: &str,
            _: &CompiledRequest,
            _: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            panic!("opaque identity must fail before dispatch")
        }

        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            panic!("opaque identity must fail before dispatch")
        }
    }

    #[tokio::test]
    async fn default_transport_is_visible_and_delegated_transport_stays_opaque() {
        let cgs = fixture();
        let (_, compiled) = operation(&cgs, "observation_query", "scope-a");
        let config = ExecutionConfig {
            base_url: Some("https://fixture.test".into()),
            ..Default::default()
        };
        let default = ExecutionEngine::new(config.clone()).unwrap();
        assert!(default.prepare_request_identity(&compiled).await.is_ok());
        let opaque = ExecutionEngine::new_with_transport(config, Arc::new(OpaqueTransport), None);
        assert!(matches!(
            opaque.prepare_request_identity(&compiled).await,
            Err(RuntimeError::RequestIdentityAuthOpaque)
        ));
    }

    #[async_trait::async_trait]
    impl crate::http_transport::HttpTransport for Observations {
        async fn send_compiled_http(
            &self,
            _: &str,
            _: &CompiledRequest,
            _: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            let amount = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Ok((serde_json::json!({"amount": amount.to_string()}), None))
        }

        async fn get_json_absolute(
            &self,
            _: &str,
            _: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            panic!("singleton fixture has no continuation")
        }
    }

    #[tokio::test]
    async fn live_query_and_quote_use_request_identity_through_engine_decode() {
        let cgs = fixture();
        let transport = std::sync::Arc::new(Observations(std::sync::atomic::AtomicUsize::new(0)));
        let engine = ExecutionEngine::new_with_transport(
            ExecutionConfig {
                base_url: Some("https://fixture.test".into()),
                ..Default::default()
            },
            transport,
            None,
        );
        for quote in [false, true] {
            let mut rows = Vec::new();
            for selector in ["scope-a", "scope-a", "scope-b"] {
                let expr = if quote {
                    Expr::Invoke(InvokeExpr {
                        capability: "observation_quote".into(),
                        target: plasm_core::GetExpr::pathless_nullary("Observation").reference,
                        input: Some(
                            Value::Object(IndexMap::from([(
                                "selector".into(),
                                Value::String(selector.into()),
                            )]))
                            .into(),
                        ),
                        catalog_entry_id: Default::default(),
                    })
                } else {
                    let mut query = QueryExpr::all("Observation");
                    query.predicate = Some(plasm_core::Predicate::eq("selector", selector));
                    Expr::Query(query)
                };
                if let Expr::Invoke(invoke) = &expr {
                    assert!(invoke.target.is_pathless_nullary());
                    assert!(!cgs
                        .get_capability(&invoke.capability)
                        .unwrap()
                        .requires_receiver());
                }
                plasm_core::type_check_expr(&expr, &cgs).unwrap();
                let result = engine
                    .execute(
                        &expr,
                        &cgs,
                        &mut SessionMaterialization::new(),
                        Some(ExecutionMode::Live),
                        StreamConsumeOpts::default(),
                        ExecuteOptions::for_catalog(&cgs).unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(result.count(), 1);
                rows.push(result.entities()[0].clone());
            }
            assert_eq!(rows[0].reference, rows[1].reference);
            assert_ne!(rows[0].fields["amount"], rows[1].fields["amount"]);
            assert_ne!(rows[0].reference, rows[2].reference);
            assert!(!rows[0].fields.contains_key("request_key"));
        }
    }
}
