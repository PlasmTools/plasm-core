//! Symmetric tenant/backend/overlay materialization for execute session open and rehydrate.

use std::sync::Arc;

use plasm_core::discovery::CgsCatalog;
use plasm_core::{CgsContext, CGS};

use crate::http_backend::{CatalogHttpBackend, ResolvedHttpOrigin};
use crate::http_execute::{
    patch_cgs_context_outbound_hosted, patch_cgs_context_resolved_http_backend,
    resolve_http_backend_for_entry,
};
use crate::server_state::PlasmHostState;

/// Materialized registry row for one catalog `entry_id` (post tenant/http/overlay patches).
pub(crate) struct MaterializedEntryContext {
    pub ctx: Arc<CgsContext>,
    pub effective_cgs: Arc<CGS>,
    pub http_backend: ResolvedHttpOrigin,
}

/// Load registry CGS, apply tenant outbound hosted-KV / resolved backend / schema overlay.
pub(crate) async fn materialize_entry_context(
    st: &PlasmHostState,
    entry_id: &str,
    outbound_hosted_kv: Option<&str>,
    entry_bindings: Option<&crate::binding_slots::SessionBindingMap>,
) -> Result<MaterializedEntryContext, String> {
    let reg = st.catalog.snapshot();
    let mut ctx = reg
        .load_context(entry_id)
        .map_err(|e| format!("load context `{entry_id}`: {e}"))?;
    if let Some(kv) = outbound_hosted_kv {
        ctx = patch_cgs_context_outbound_hosted(ctx, kv);
    }
    let catalog_backend = CatalogHttpBackend::from_cgs_field(ctx.cgs.http_backend.as_str());
    let http_backend = resolve_http_backend_for_entry(
        st,
        entry_id,
        &catalog_backend,
        entry_bindings,
        outbound_hosted_kv,
    )
    .await?;
    if catalog_backend.needs_origin_resolution(entry_id) {
        ctx = patch_cgs_context_resolved_http_backend(ctx, &http_backend);
    }
    let effective_cgs =
        resolve_schema_overlay(st, ctx.cgs.clone(), http_backend.as_str(), entry_id).await?;
    let ctx = Arc::new(CgsContext::entry(
        entry_id.to_string(),
        effective_cgs.clone(),
    ));
    Ok(MaterializedEntryContext {
        ctx,
        effective_cgs,
        http_backend,
    })
}

async fn resolve_schema_overlay(
    st: &PlasmHostState,
    base: Arc<CGS>,
    http_base: &str,
    entry_id: &str,
) -> Result<Arc<CGS>, String> {
    crate::schema_overlay_session::resolve_schema_overlay_for_host(
        st.engine.as_ref(),
        st.mode,
        st.effective_outbound_secret_provider(),
        base,
        http_base,
        entry_id,
    )
    .await
}

/// Extract persisted tenant outbound hosted_kv key from a materialized CGS auth block.
pub(crate) fn outbound_hosted_kv_from_cgs(cgs: &CGS) -> Option<String> {
    use plasm_core::AuthScheme;
    let auth = cgs.auth.as_ref()?;
    match auth {
        AuthScheme::ApiKeyHeader { hosted_kv, .. }
        | AuthScheme::ApiKeyQuery { hosted_kv, .. }
        | AuthScheme::BearerToken { hosted_kv, .. }
        | AuthScheme::OauthBearer { hosted_kv, .. } => hosted_kv.clone(),
        AuthScheme::Oauth2ClientCredentials { .. } | AuthScheme::None => None,
    }
}
