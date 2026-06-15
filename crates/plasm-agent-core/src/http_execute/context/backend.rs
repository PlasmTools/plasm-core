//! HTTP backend resolution for federated execute sessions.

use super::super::*;

pub(crate) fn cgs_entity_names_sample(names: &[String], max_list: usize) -> String {
    if names.is_empty() {
        return "()".to_string();
    }
    let mut sorted = names.to_vec();
    sorted.sort();
    let show = sorted.len().min(max_list);
    let head = sorted[..show].join(", ");
    if sorted.len() > max_list {
        format!("{head}, … (+{} more)", sorted.len() - max_list)
    } else {
        head
    }
}

/// For tenant MCP: resolve `plasm:outbound:*` keys bound to each catalog `entry_id` via Phoenix tables.
pub(super) async fn tenant_outbound_hosted_kv_for_entries(
    st: &PlasmHostState,
    cfg: &crate::mcp_runtime_config::McpRuntimeConfig,
    principal_incoming: Option<&crate::incoming_auth::TenantPrincipal>,
    entry_ids: &[String],
) -> HashMap<String, String> {
    let Some(repo) = st.mcp_config_repository() else {
        return HashMap::new();
    };
    let subject_lookup = crate::mcp_config_repository::effective_owner_subject_for_hosted_kv(
        cfg.id,
        cfg.owner_subject.as_deref(),
        principal_incoming.map(|p| p.subject.as_str()),
    );
    let mut out = HashMap::new();
    for eid in entry_ids {
        if !cfg.auth_config_by_entry.contains_key(eid) {
            continue;
        }
        match repo
            .fetch_hosted_kv_for_graph_binding(cfg.id, eid, subject_lookup)
            .await
        {
            Ok(Some(kv)) if !kv.trim().is_empty() => {
                crate::metrics::record_tenant_outbound_hosted_kv_lookup("hit");
                out.insert(eid.clone(), kv);
            }
            Ok(_) => {
                crate::metrics::record_tenant_outbound_hosted_kv_lookup("miss");
                tracing::warn!(
                    target: "plasm_agent::tenant_outbound",
                    config_id = %cfg.id,
                    entry_id = %eid,
                    "MCP auth binding exists but no active connected account with hosted_kv_key (link OAuth/API in the web app)"
                );
            }
            Err(e) => {
                crate::metrics::record_tenant_outbound_hosted_kv_lookup("error");
                tracing::warn!(
                    target: "plasm_agent::tenant_outbound",
                    config_id = %cfg.id,
                    entry_id = %eid,
                    error = %e,
                    "hosted_kv lookup for MCP auth binding failed"
                );
            }
        }
    }
    out
}

/// Migration: read legacy `http_backend` from outbound credential envelope.
pub(super) async fn migration_legacy_http_backend_from_outbound_key(
    st: &PlasmHostState,
    hosted_kv_key: &str,
) -> Option<crate::http_backend::BindingOriginValue> {
    let storage = st.auth_storage()?;
    let bytes = storage.get_kv(hosted_kv_key.trim()).await.ok()??;
    let raw = String::from_utf8_lossy(&bytes);
    plasm_runtime::hosted_oauth_kv::hosted_outbound_http_backend_from_kv(raw.trim())
        .map(|s| crate::http_backend::BindingOriginValue::from_legacy_outbound_kv(&s))
}

pub(crate) async fn resolve_http_backend_for_entry(
    st: &PlasmHostState,
    entry_id: &str,
    catalog_backend: &crate::http_backend::CatalogHttpBackend,
    bindings: Option<&crate::binding_slots::SessionBindingMap>,
    outbound_hosted_kv_key: Option<&str>,
) -> Result<crate::http_backend::ResolvedHttpOrigin, String> {
    let legacy = if let Some(key) = outbound_hosted_kv_key {
        migration_legacy_http_backend_from_outbound_key(st, key).await
    } else {
        None
    };
    crate::binding_slots::resolve_catalog_http_backend(
        entry_id,
        catalog_backend,
        bindings,
        legacy.as_ref(),
    )
}

pub(crate) fn patch_cgs_context_resolved_http_backend(
    ctx: CgsContext,
    resolved: &crate::http_backend::ResolvedHttpOrigin,
) -> CgsContext {
    let mut cgs = (*ctx.cgs).clone();
    cgs.http_backend = resolved.as_str().to_string();
    CgsContext::new(ctx.prefix.clone(), Arc::new(cgs))
}

fn patch_auth_scheme_for_tenant_hosted(
    auth: Option<&AuthScheme>,
    hosted_kv_key: &str,
) -> AuthScheme {
    let kv = Some(hosted_kv_key.to_string());
    match auth {
        Some(AuthScheme::ApiKeyHeader { header, .. }) => AuthScheme::ApiKeyHeader {
            header: header.clone(),
            env: None,
            hosted_kv: kv,
        },
        Some(AuthScheme::ApiKeyQuery { param, .. }) => AuthScheme::ApiKeyQuery {
            param: param.clone(),
            env: None,
            hosted_kv: kv,
        },
        Some(AuthScheme::BearerToken { optional_env, .. }) => AuthScheme::BearerToken {
            env: None,
            hosted_kv: kv,
            optional_env: *optional_env,
        },
        Some(AuthScheme::OauthBearer { optional_env, .. }) => AuthScheme::OauthBearer {
            env: None,
            hosted_kv: kv,
            optional_env: *optional_env,
        },
        None => AuthScheme::BearerToken {
            env: None,
            hosted_kv: kv,
            optional_env: false,
        },
        Some(AuthScheme::Oauth2ClientCredentials { .. }) => AuthScheme::BearerToken {
            env: None,
            hosted_kv: kv,
            optional_env: false,
        },
        Some(AuthScheme::None) => AuthScheme::BearerToken {
            env: None,
            hosted_kv: kv,
            optional_env: false,
        },
    }
}

fn patch_cgs_outbound_auth(cgs: &Arc<CGS>, hosted_kv_key: &str) -> Arc<CGS> {
    let mut c = (**cgs).clone();
    c.auth = Some(patch_auth_scheme_for_tenant_hosted(
        c.auth.as_ref(),
        hosted_kv_key,
    ));
    Arc::new(c)
}

pub(crate) fn patch_cgs_context_outbound_hosted(
    ctx: CgsContext,
    hosted_kv_key: &str,
) -> CgsContext {
    let CgsContext { prefix, cgs } = ctx;
    CgsContext::new(prefix, patch_cgs_outbound_auth(&cgs, hosted_kv_key))
}
