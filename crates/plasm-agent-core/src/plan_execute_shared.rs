//! Hoisted per-plan execute line setup for live [`run_plasm_comp`] (avoids repeated session locks).

use std::sync::{Arc, Mutex};

use plasm_core::PreflightToken;
use plasm_core::CGS;
use plasm_runtime::CancelSignal;
use plasm_runtime::{
    AuthResolver, ExecuteOptions, ExecuteSessionMaterial, GraphPageSpillHandle, RowsProgressFn,
    SecretProvider,
};

use crate::execute_session::ExecuteSession;
use crate::graph_page_spill_host::graph_page_spill_for_execute;
use crate::operation::plan_execute_cancel_signal;
use crate::server_state::PlasmHostState;

/// Session-stable fields reused for every surface line inside one live plan run.
pub struct PlanLineExecuteShared {
    secret_provider: Arc<dyn SecretProvider>,
    federation: Option<Arc<plasm_core::cgs_federation::FederationDispatch>>,
    graph_page_spill: Option<GraphPageSpillHandle>,
    cancel: Option<CancelSignal>,
    prompt_hash: String,
    session_id: String,
}

impl PlanLineExecuteShared {
    pub async fn prepare(es: &ExecuteSession, st: &PlasmHostState, session_id: &str) -> Self {
        let federation = es.federation_dispatch();
        let secret_provider = st.effective_outbound_secret_provider();
        let graph_page_spill = graph_page_spill_for_execute(
            st.session_graph_persistence.as_ref(),
            es.core.clone(),
            es.prompt_hash.as_str(),
            session_id,
        );
        let cancel = plan_execute_cancel_signal();
        Self {
            secret_provider,
            federation,
            graph_page_spill,
            cancel,
            prompt_hash: es.prompt_hash.clone(),
            session_id: session_id.to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn build_exec_opts(
        &self,
        sess: &ExecuteSession,
        st: &PlasmHostState,
        exec_cgs: &CGS,
        root_entity: &str,
        fp_sink: Arc<Mutex<Vec<String>>>,
        preflight: PreflightToken,
        rows_progress: Option<RowsProgressFn>,
    ) -> ExecuteOptions {
        let bound_share = sess.session_share_token.read().await.clone();
        let bound_proof_base_token = sess.session_proof_base_token.read().await.clone();
        let engine_override = st
            .engine
            .config()
            .base_url
            .as_deref()
            .and_then(|b| crate::http_backend::ReplHttpOverride::from_engine_base(b).ok());
        let catalog_backend = self
            .federation
            .as_ref()
            .and_then(|fed| fed.http_backend_for_entity(root_entity))
            .map(crate::http_backend::CatalogHttpBackend::from_cgs_field)
            .or_else(|| {
                sess.http_backend
                    .as_deref()
                    .map(crate::http_backend::CatalogHttpBackend::from_cgs_field)
            });
        let http_backend_for_root = crate::catalog_ownership::plan_http_origin(
            engine_override.as_ref(),
            catalog_backend.as_ref(),
        )
        .map(|origin| origin.as_str().to_string());
        let auth_for_exec = exec_cgs.auth.clone();
        let catalog_entry_for_bind = self
            .federation
            .as_ref()
            .and_then(|_| {
                sess.contexts_by_entry.keys().find(|eid| {
                    sess.contexts_by_entry
                        .get(*eid)
                        .and_then(|ctx| ctx.get_entity(root_entity))
                        .is_some()
                })
            })
            .cloned()
            .unwrap_or_else(|| sess.entry_id.clone());
        let catalog_bind = sess
            .session_bindings_for_entry(&catalog_entry_for_bind)
            .map(|m| m.cml_env_entries());
        ExecuteOptions {
            request_fingerprint_sink: Some(fp_sink),
            http_base_url_override: http_backend_for_root.clone(),
            auth_resolver_override: auth_for_exec.map(|scheme| {
                Arc::new(
                    AuthResolver::new(scheme, self.secret_provider.clone())
                        .with_session_bearer_override(bound_share.clone()),
                )
            }),
            federation: self.federation.clone(),
            preflight: Some(preflight),
            execute_session: Some(Arc::new(ExecuteSessionMaterial {
                prompt_hash: self.prompt_hash.clone(),
                session_id: self.session_id.clone(),
                share_token: bound_share,
                proof_base_token: bound_proof_base_token,
                transport_origin: http_backend_for_root.clone(),
                ui_origin: http_backend_for_root,
                catalog_bind,
            })),
            cancel: self.cancel.clone(),
            graph_page_spill: self.graph_page_spill.clone(),
            rows_progress,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::http_execute::try_proof_document_share_bind;
    use crate::test_support::proof_bind_fixtures::ProofBindFixture;

    use super::*;

    #[tokio::test]
    async fn build_exec_opts_reads_fresh_share_token_after_mid_plan_bind() {
        let fx = ProofBindFixture::open("plan_share_refresh");
        let st = fx.host_with_registry();
        let shared = PlanLineExecuteShared::prepare(&fx.session, &st, "sid_plan").await;
        let fp_sink = Arc::new(Mutex::new(Vec::<String>::new()));

        let before = shared
            .build_exec_opts(
                &fx.session,
                &st,
                fx.cgs.as_ref(),
                "Document",
                fp_sink.clone(),
                plasm_core::PreflightToken::VERIFIED,
                None,
            )
            .await;
        assert!(
            before
                .execute_session
                .as_ref()
                .and_then(|m| m.share_token.as_deref())
                .is_none(),
            "expected no share token before bind"
        );

        try_proof_document_share_bind(&fx.session, fx.cgs.as_ref(), &fx.token_only_bind_expr())
            .await
            .expect("bind")
            .expect("bind intercept");

        let after = shared
            .build_exec_opts(
                &fx.session,
                &st,
                fx.cgs.as_ref(),
                "Document",
                fp_sink,
                plasm_core::PreflightToken::VERIFIED,
                None,
            )
            .await;
        assert_eq!(
            after
                .execute_session
                .as_ref()
                .and_then(|m| m.share_token.as_deref()),
            Some("secret-tok"),
            "plan line after bind must see fresh session share token"
        );
    }

    #[tokio::test]
    async fn build_exec_opts_after_bind_matches_session_read() {
        let fx = ProofBindFixture::open("plan_share_read");
        let st = fx.host_with_registry();
        let shared = PlanLineExecuteShared::prepare(&fx.session, &st, "sid_read").await;

        try_proof_document_share_bind(&fx.session, fx.cgs.as_ref(), &fx.token_only_bind_expr())
            .await
            .expect("bind")
            .expect("bind intercept");

        let opts = shared
            .build_exec_opts(
                &fx.session,
                &st,
                fx.cgs.as_ref(),
                "Document",
                Arc::new(Mutex::new(Vec::<String>::new())),
                plasm_core::PreflightToken::VERIFIED,
                None,
            )
            .await;
        assert_eq!(
            opts.execute_session
                .as_ref()
                .and_then(|m| m.share_token.as_deref()),
            fx.session.session_share_token.read().await.as_deref(),
            "build_exec_opts share token must match session read lock"
        );
    }
}
