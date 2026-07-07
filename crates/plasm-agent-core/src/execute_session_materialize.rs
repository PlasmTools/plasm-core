//! Symmetric tenant/backend/overlay materialization for execute session open and rehydrate.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::discovery::{CgsCatalog, DiscoveryError};
use plasm_core::{CgsContext, CGS};

use crate::catalog_hash::EffectiveCatalogHash;
use crate::execute_session::ExecuteSession;
use crate::http_backend::{CatalogHttpBackend, ResolvedHttpOrigin};
use crate::http_execute::{
    patch_cgs_context_outbound_hosted, patch_cgs_context_resolved_http_backend,
    resolve_http_backend_for_entry,
};
use crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError;
use crate::mcp_transport_store::execute_session_registry::PersistedExecuteSessionDescriptor;
use crate::server_state::PlasmHostState;

/// Rematerialized catalog pins + encoded symbol ledger for durable persist / rehydrate.
pub(crate) struct DurableExposureSnapshot {
    pub catalog_cgs_hashes_by_entry: IndexMap<String, EffectiveCatalogHash>,
    pub symbol_ledger_bytes: Vec<u8>,
}

/// Materialized registry row for one catalog `entry_id` (post tenant/http/overlay patches).
pub(crate) struct MaterializedEntryContext {
    pub ctx: Arc<CgsContext>,
    pub effective_cgs: Arc<CGS>,
    pub http_backend: ResolvedHttpOrigin,
}

/// Tenant outbound KV + per-entry bindings used to rematerialize effective CGS digests.
pub(crate) struct SessionMaterializationPins<'a> {
    pub outbound_hosted_kv_by_entry: &'a HashMap<String, String>,
    pub bindings_by_entry: &'a IndexMap<String, crate::binding_slots::SessionBindingMap>,
}

/// Typed failures from registry load + tenant/http/overlay materialization.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MaterializeError {
    #[error("unknown catalog entry `{0}`")]
    UnknownEntry(String),
    #[error("load context `{entry_id}`: {detail}")]
    LoadContext { entry_id: String, detail: String },
    #[error("http backend for `{entry_id}`: {detail}")]
    HttpBackend { entry_id: String, detail: String },
    #[error("schema overlay for `{entry_id}`: {detail}")]
    SchemaOverlay { entry_id: String, detail: String },
}

/// Outcome of comparing live rematerialized digests to pinned effective hashes.
pub(crate) enum CatalogPinVerifyOutcome {
    Ok(IndexMap<String, Arc<CGS>>),
    Mismatch,
}

/// Load registry CGS, apply tenant outbound hosted-KV / resolved backend / schema overlay.
pub(crate) async fn materialize_entry_context(
    st: &PlasmHostState,
    entry_id: &str,
    outbound_hosted_kv: Option<&str>,
    entry_bindings: Option<&crate::binding_slots::SessionBindingMap>,
) -> Result<MaterializedEntryContext, MaterializeError> {
    let reg = st.catalog.snapshot();
    let mut ctx = match reg.load_context(entry_id) {
        Ok(ctx) => ctx,
        Err(DiscoveryError::UnknownEntry(id)) => return Err(MaterializeError::UnknownEntry(id)),
        Err(e) => {
            return Err(MaterializeError::LoadContext {
                entry_id: entry_id.to_string(),
                detail: e.to_string(),
            });
        }
    };
    if let Some(kv) = outbound_hosted_kv {
        let already = outbound_hosted_kv_from_cgs(ctx.cgs.as_ref());
        if already.as_deref() != Some(kv) {
            ctx = patch_cgs_context_outbound_hosted(ctx, kv);
        }
    }
    let catalog_backend = CatalogHttpBackend::from_cgs_field(ctx.cgs.http_backend.as_str());
    let http_backend = resolve_http_backend_for_entry(
        st,
        entry_id,
        &catalog_backend,
        entry_bindings,
        outbound_hosted_kv,
    )
    .await
    .map_err(|detail| MaterializeError::HttpBackend {
        entry_id: entry_id.to_string(),
        detail,
    })?;
    if catalog_backend.needs_origin_resolution(entry_id) {
        ctx = patch_cgs_context_resolved_http_backend(ctx, &http_backend);
    }
    let effective_cgs =
        resolve_schema_overlay(st, ctx.cgs.clone(), http_backend.as_str(), entry_id)
            .await
            .map_err(|detail| MaterializeError::SchemaOverlay {
                entry_id: entry_id.to_string(),
                detail,
            })?;
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

/// Rematerialize every loaded registry row for a live session.
pub(crate) async fn materialize_entries_for_session(
    st: &PlasmHostState,
    session: &ExecuteSession,
) -> Result<IndexMap<String, MaterializedEntryContext>, MaterializeError> {
    let pins = session.materialization_pins();
    materialize_entries_for_pins(st, session.contexts_by_entry.keys(), &pins).await
}

/// Rematerialize catalog rows from a persisted descriptor's pin maps.
pub(crate) async fn materialize_entries_for_descriptor(
    st: &PlasmHostState,
    desc: &PersistedExecuteSessionDescriptor,
) -> Result<IndexMap<String, MaterializedEntryContext>, MaterializeError> {
    let pins = SessionMaterializationPins {
        outbound_hosted_kv_by_entry: &desc.outbound_hosted_kv_by_entry,
        bindings_by_entry: &desc.bindings_by_entry,
    };
    materialize_entries_for_pins(st, desc.context_entry_ids.iter(), &pins).await
}

async fn materialize_entries_for_pins<'a, I>(
    st: &PlasmHostState,
    entry_ids: I,
    pins: &SessionMaterializationPins<'_>,
) -> Result<IndexMap<String, MaterializedEntryContext>, MaterializeError>
where
    I: IntoIterator<Item = &'a String>,
{
    let ids: Vec<&String> = entry_ids.into_iter().collect();
    let mut out = IndexMap::with_capacity(ids.len());
    for entry_id in ids {
        let outbound = pins
            .outbound_hosted_kv_by_entry
            .get(entry_id.as_str())
            .map(String::as_str);
        let bindings = pins.bindings_by_entry.get(entry_id.as_str());
        let materialized =
            materialize_entry_context(st, entry_id.as_str(), outbound, bindings).await?;
        out.insert(entry_id.clone(), materialized);
    }
    Ok(out)
}

/// HashMap bindings variant for logical-open restore (MCP outbound ref maps).
pub(crate) async fn verify_effective_catalog_pins_maps(
    st: &PlasmHostState,
    pinned_hashes: &IndexMap<String, String>,
    outbound: Option<&HashMap<String, String>>,
    bindings: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
) -> Result<CatalogPinVerifyOutcome, MaterializeError> {
    let mut catalog_cgs = IndexMap::with_capacity(pinned_hashes.len());
    for (entry_id, pinned) in pinned_hashes {
        let hosted_kv = outbound.and_then(|m| m.get(entry_id)).map(String::as_str);
        let entry_bindings = bindings.and_then(|m| m.get(entry_id));
        let materialized =
            materialize_entry_context(st, entry_id.as_str(), hosted_kv, entry_bindings).await?;
        let pinned_hash = EffectiveCatalogHash::from_hex(pinned.clone());
        let live_hash =
            EffectiveCatalogHash::from_effective_cgs(materialized.effective_cgs.as_ref());
        if live_hash != pinned_hash {
            return Ok(CatalogPinVerifyOutcome::Mismatch);
        }
        catalog_cgs.insert(entry_id.clone(), materialized.effective_cgs);
    }
    Ok(CatalogPinVerifyOutcome::Ok(catalog_cgs))
}

/// Rematerialize every loaded registry row and encode the append-only symbol ledger.
pub(crate) async fn build_durable_exposure_snapshot(
    st: &PlasmHostState,
    session: &ExecuteSession,
) -> Result<DurableExposureSnapshot, ExecuteSessionPersistError> {
    let materialized = materialize_entries_for_session(st, session)
        .await
        .map_err(ExecuteSessionPersistError::from)?;
    let catalog_cgs_hashes_by_entry: IndexMap<String, EffectiveCatalogHash> = materialized
        .iter()
        .map(|(eid, m)| {
            (
                eid.clone(),
                EffectiveCatalogHash::from_effective_cgs(m.effective_cgs.as_ref()),
            )
        })
        .collect();
    encode_durable_exposure_snapshot(session, catalog_cgs_hashes_by_entry).await
}

/// Encode ledger bytes using existing effective pins (no per-entry rematerialize).
pub(crate) async fn build_durable_exposure_snapshot_reusing_pins(
    session: &ExecuteSession,
    catalog_cgs_hashes_by_entry: IndexMap<String, EffectiveCatalogHash>,
) -> Result<DurableExposureSnapshot, ExecuteSessionPersistError> {
    encode_durable_exposure_snapshot(session, catalog_cgs_hashes_by_entry).await
}

async fn encode_durable_exposure_snapshot(
    session: &ExecuteSession,
    catalog_cgs_hashes_by_entry: IndexMap<String, EffectiveCatalogHash>,
) -> Result<DurableExposureSnapshot, ExecuteSessionPersistError> {
    let exp = session
        .teaching_exposure
        .as_ref()
        .ok_or(ExecuteSessionPersistError::MissingSymbolLedger)?;
    let ledger_pins: IndexMap<String, String> = catalog_cgs_hashes_by_entry
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
        .collect();
    let snap = plasm_core::PersistedSymbolLedger::from_session(exp, ledger_pins)
        .map_err(|e| ExecuteSessionPersistError::SymbolLedgerEncode(e.to_string()))?;
    let symbol_ledger_bytes = snap
        .encode()
        .map_err(|e| ExecuteSessionPersistError::SymbolLedgerEncode(e.to_string()))?;
    if symbol_ledger_bytes.is_empty() {
        return Err(ExecuteSessionPersistError::MissingSymbolLedger);
    }
    Ok(DurableExposureSnapshot {
        catalog_cgs_hashes_by_entry,
        symbol_ledger_bytes,
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

    use super::*;

    #[tokio::test]
    async fn materialize_is_idempotent_for_catalog_default_hosted_kv() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        if !dir.is_dir() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(&dir).expect("github"));
        let reg = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
            "github".into(),
            "GitHub".into(),
            vec!["github".into()],
            cgs,
        )]));
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        let st = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: reg,
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        });

        let baseline = materialize_entry_context(&st, "github", None, None)
            .await
            .expect("baseline");
        let kv = outbound_hosted_kv_from_cgs(baseline.effective_cgs.as_ref()).expect("hosted_kv");
        let with_kv = materialize_entry_context(&st, "github", Some(kv.as_str()), None)
            .await
            .expect("with_kv");
        assert_eq!(
            baseline.effective_cgs.effective_catalog_cgs_hash_hex(),
            with_kv.effective_cgs.effective_catalog_cgs_hash_hex(),
            "re-applying catalog-default hosted_kv must not change effective digest"
        );
    }
}
