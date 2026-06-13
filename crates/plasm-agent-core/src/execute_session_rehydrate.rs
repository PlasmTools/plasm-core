//! Rebuild an in-memory [`ExecuteSession`] from a Redis-persisted descriptor + catalog snapshot.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use plasm_core::discovery::CgsCatalog;
use plasm_core::discovery::DiscoveryError;
use plasm_core::InMemoryCgsRegistry;

use crate::execute_session::ExecuteSession;
use crate::mcp_transport_store::execute_session_registry::PersistedExecuteSessionDescriptor;
use crate::server_state::PlasmHostState;

/// Why cross-pod rehydrate failed (distinct from "descriptor not in Redis").
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RehydrateError {
    UnknownEntry(String),
    CatalogHashMismatch {
        entry_id: String,
        expected: String,
        live: String,
    },
    DescriptorExpired,
    Discovery(String),
    PluginGenerationUnavailable {
        generation_id: u64,
    },
}

impl std::fmt::Display for RehydrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEntry(id) => write!(f, "unknown catalog entry `{id}`"),
            Self::CatalogHashMismatch {
                entry_id,
                expected,
                live,
            } => write!(
                f,
                "catalog hash mismatch for `{entry_id}` (session pinned {expected}, live {live})"
            ),
            Self::DescriptorExpired => write!(f, "persisted execute session descriptor expired"),
            Self::Discovery(e) => write!(f, "{e}"),
            Self::PluginGenerationUnavailable { generation_id } => write!(
                f,
                "pinned compile plugin generation `{generation_id}` is not available on this host"
            ),
        }
    }
}

impl std::error::Error for RehydrateError {}

pub fn descriptor_expired(desc: &PersistedExecuteSessionDescriptor) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now > desc.expires_at_unix
}

fn validate_catalog_hashes(
    desc: &PersistedExecuteSessionDescriptor,
    reg: &InMemoryCgsRegistry,
) -> Result<(), RehydrateError> {
    let entries: Vec<String> = if desc.catalog_cgs_hashes_by_entry.is_empty() {
        vec![desc.entry_id.clone()]
    } else {
        desc.context_entry_ids.clone()
    };
    for eid in entries {
        let expected = if desc.catalog_cgs_hashes_by_entry.is_empty() {
            desc.catalog_cgs_hash.clone()
        } else {
            desc.catalog_cgs_hashes_by_entry
                .get(&eid)
                .cloned()
                .unwrap_or_else(|| desc.catalog_cgs_hash.clone())
        };
        let live = match reg.load_context(&eid) {
            Ok(ctx) => ctx.cgs.effective_catalog_cgs_hash_hex(),
            Err(DiscoveryError::UnknownEntry(id)) => return Err(RehydrateError::UnknownEntry(id)),
            Err(e) => {
                return Err(RehydrateError::Discovery(format!(
                    "load context `{eid}`: {e}"
                )));
            }
        };
        if live != expected {
            return Err(RehydrateError::CatalogHashMismatch {
                entry_id: eid,
                expected,
                live,
            });
        }
    }
    Ok(())
}

pub async fn rehydrate_execute_session(
    st: &PlasmHostState,
    desc: &PersistedExecuteSessionDescriptor,
) -> Result<ExecuteSession, RehydrateError> {
    if descriptor_expired(desc) {
        return Err(RehydrateError::DescriptorExpired);
    }

    let reg = st.catalog.snapshot();
    validate_catalog_hashes(desc, reg.as_ref())?;

    let primary_ctx = match reg.load_context(&desc.entry_id) {
        Ok(c) => c,
        Err(DiscoveryError::UnknownEntry(id)) => {
            return Err(RehydrateError::UnknownEntry(id));
        }
        Err(e) => return Err(RehydrateError::Discovery(e.to_string())),
    };
    let cgs = primary_ctx.cgs.clone();

    let mut contexts_by_entry = IndexMap::new();
    for eid in &desc.context_entry_ids {
        let ctx = match reg.load_context(eid) {
            Ok(c) => c,
            Err(DiscoveryError::UnknownEntry(id)) => {
                return Err(RehydrateError::UnknownEntry(id));
            }
            Err(e) => {
                return Err(RehydrateError::Discovery(format!(
                    "load context `{eid}`: {e}"
                )))
            }
        };
        contexts_by_entry.insert(eid.clone(), Arc::new(ctx));
    }
    if !contexts_by_entry.contains_key(&desc.entry_id) {
        contexts_by_entry.insert(desc.entry_id.clone(), Arc::new(primary_ctx));
    }

    let refs: Vec<&str> = desc.entities.iter().map(String::as_str).collect();
    let teaching_exposure = match &desc.context_intent {
        Some(intent_s) => {
            let relation_keys =
                plasm_core::relation_endpoint_keys(desc.entry_id.as_str(), &desc.entities);
            let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
                cgs.as_ref(),
                desc.entry_id.as_str(),
                intent_s.as_str(),
                &relation_keys,
                &desc.entities,
                desc.ranked_capabilities.as_deref(),
                plasm_core::discovery::ExposureSurfaceOptions::default(),
            );
            plasm_core::TeachingExposureSession::new_with_intent_delta(
                cgs.as_ref(),
                desc.entry_id.as_str(),
                &refs,
                delta,
            )
        }
        None => {
            plasm_core::TeachingExposureSession::new(cgs.as_ref(), desc.entry_id.as_str(), &refs)
        }
    };

    let plugin_generation = match desc.plugin_generation_id {
        Some(id) => Some(
            st.plugin_manager
                .as_ref()
                .and_then(|pm| pm.generation(id))
                .ok_or(RehydrateError::PluginGenerationUnavailable { generation_id: id })?,
        ),
        None => None,
    };

    let mut session = ExecuteSession::new_with_bindings(
        desc.prompt_hash.clone(),
        desc.prompt_text.clone(),
        cgs,
        contexts_by_entry,
        desc.entry_id.clone(),
        desc.tenant_scope.clone(),
        desc.principal_subject.clone(),
        desc.http_backend.clone(),
        desc.entities.clone(),
        Some(teaching_exposure),
        desc.principal.clone(),
        plugin_generation,
        desc.catalog_cgs_hash.clone(),
        desc.context_intent.clone(),
        desc.ranked_capabilities.clone(),
        desc.bindings_by_entry.clone(),
    );
    session.domain_revision = desc.domain_revision;
    session.restore_persisted_plan_commits(&desc.plan_commits, desc.plan_commit_next);

    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_transport_store::execute_session_registry::{
        PersistedExecuteSessionDescriptor, PersistedSessionReuseKey,
    };

    #[test]
    fn descriptor_expired_rejects_past_timestamp() {
        let desc = PersistedExecuteSessionDescriptor {
            prompt_hash: "ph".into(),
            session_id: "sid".into(),
            prompt_text: String::new(),
            entry_id: "e".into(),
            context_entry_ids: vec!["e".into()],
            entities: vec!["x".into()],
            tenant_scope: "t".into(),
            principal_subject: String::new(),
            http_backend: None,
            principal: None,
            catalog_cgs_hash: "h".into(),
            context_intent: None,
            ranked_capabilities: None,
            plugin_generation_id: None,
            domain_revision: 0,
            reuse_key: PersistedSessionReuseKey {
                tenant_scope: "t".into(),
                entry_id: "e".into(),
                catalog_cgs_hash: "h".into(),
                entities: vec!["x".into()],
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
                plugin_generation_id: None,
                logical_session_id: None,
            },
            expires_at_unix: 1,
            catalog_cgs_hashes_by_entry: Default::default(),
            bindings_by_entry: Default::default(),
            plan_commits: Vec::new(),
            plan_commit_next: 0,
        };
        assert!(descriptor_expired(&desc));
    }
}
