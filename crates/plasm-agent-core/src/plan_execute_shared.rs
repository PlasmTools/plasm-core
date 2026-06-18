//! Hoisted per-plan execute line setup for live [`run_plasm_comp`] (avoids repeated session locks).

use std::sync::{Arc, Mutex};

use plasm_core::PreflightToken;
use plasm_core::CGS;
use plasm_runtime::CancelSignal;
use plasm_runtime::{
    AuthResolver, CompileOperationFn, CompileQueryFn, ExecuteOptions, ExecuteSessionMaterial,
    GraphPageSpillHandle, RowsProgressFn, SecretProvider,
};

use crate::execute_session::ExecuteSession;
use crate::graph_page_spill_host::graph_page_spill_for_execute;
use crate::http_execute::plugin_execute_options_from_session;
use crate::operation::plan_execute_cancel_signal;
use crate::server_state::PlasmHostState;

/// Session-stable fields reused for every surface line inside one live plan run.
pub struct PlanLineExecuteShared {
    secret_provider: Arc<dyn SecretProvider>,
    federation: Option<Arc<plasm_core::cgs_federation::FederationDispatch>>,
    compile_operation_fn: Option<Arc<CompileOperationFn>>,
    compile_query_fn: Option<Arc<CompileQueryFn>>,
    plugin_generation_id: Option<u64>,
    bound_share: Option<String>,
    bound_proof_base_token: Option<String>,
    graph_page_spill: Option<GraphPageSpillHandle>,
    cancel: Option<CancelSignal>,
    prompt_hash: String,
    session_id: String,
}

impl PlanLineExecuteShared {
    pub async fn prepare(es: &ExecuteSession, st: &PlasmHostState, session_id: &str) -> Self {
        let bound_share = es.session_share_token.read().await.clone();
        let bound_proof_base_token = es.session_proof_base_token.read().await.clone();
        let (compile_operation_fn, compile_query_fn, plugin_generation_id) =
            plugin_execute_options_from_session(es);
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
            compile_operation_fn,
            compile_query_fn,
            plugin_generation_id,
            bound_share,
            bound_proof_base_token,
            graph_page_spill,
            cancel,
            prompt_hash: es.prompt_hash.clone(),
            session_id: session_id.to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_exec_opts(
        &self,
        sess: &ExecuteSession,
        st: &PlasmHostState,
        exec_cgs: &CGS,
        root_entity: &str,
        fp_sink: Arc<Mutex<Vec<String>>>,
        preflight: PreflightToken,
        rows_progress: Option<RowsProgressFn>,
    ) -> ExecuteOptions {
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
                        .with_session_bearer_override(self.bound_share.clone()),
                )
            }),
            compile_operation_fn: self.compile_operation_fn.clone(),
            compile_query_fn: self.compile_query_fn.clone(),
            plugin_generation_id: self.plugin_generation_id,
            federation: self.federation.clone(),
            preflight: Some(preflight),
            execute_session: Some(Arc::new(ExecuteSessionMaterial {
                prompt_hash: self.prompt_hash.clone(),
                session_id: self.session_id.clone(),
                share_token: self.bound_share.clone(),
                proof_base_token: self.bound_proof_base_token.clone(),
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
