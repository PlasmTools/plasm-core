//! Rebuild an in-memory [`ExecuteSession`] from a Redis-persisted descriptor + catalog snapshot.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use plasm_core::discovery::CgsCatalog;
use plasm_core::discovery::DiscoveryError;
use plasm_core::InMemoryCgsRegistry;

use crate::execute_session::ExecuteSession;
use crate::execute_session_materialize::materialize_entry_context;
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

/// Registry YAML digest at open time (before tenant/http/overlay patches).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryCatalogPins {
    pub(crate) entry_ids: Vec<String>,
    pub(crate) registry_hash_by_entry: HashMap<String, String>,
}

impl RegistryCatalogPins {
    pub(crate) fn from_execute_session(session: &ExecuteSession) -> Self {
        Self {
            entry_ids: session.contexts_by_entry.keys().cloned().collect(),
            registry_hash_by_entry: session.registry_catalog_hashes_by_entry.clone(),
        }
    }

    pub(crate) fn from_descriptor(desc: &PersistedExecuteSessionDescriptor) -> Self {
        let entry_ids = if desc.registry_catalog_hashes_by_entry.is_empty() {
            if desc.catalog_cgs_hashes_by_entry.is_empty() {
                vec![desc.entry_id.clone()]
            } else {
                desc.context_entry_ids.clone()
            }
        } else {
            desc.context_entry_ids.clone()
        };
        Self {
            entry_ids,
            registry_hash_by_entry: desc.registry_catalog_hashes_by_entry.clone(),
        }
    }
}

pub(crate) fn registry_catalog_pins_from_registry(
    reg: &InMemoryCgsRegistry,
    entry_ids: &[String],
) -> Result<RegistryCatalogPins, RehydrateError> {
    let mut registry_hash_by_entry = HashMap::new();
    for eid in entry_ids {
        let hash = match reg.load_context(eid) {
            Ok(ctx) => ctx.cgs.catalog_cgs_hash_hex(),
            Err(DiscoveryError::UnknownEntry(id)) => return Err(RehydrateError::UnknownEntry(id)),
            Err(e) => {
                return Err(RehydrateError::Discovery(format!(
                    "load context `{eid}`: {e}"
                )));
            }
        };
        registry_hash_by_entry.insert(eid.clone(), hash);
    }
    Ok(RegistryCatalogPins {
        entry_ids: entry_ids.to_vec(),
        registry_hash_by_entry,
    })
}

pub(crate) fn registry_pins_match_live(
    reg: &InMemoryCgsRegistry,
    pins: &RegistryCatalogPins,
) -> Result<(), RehydrateError> {
    if pins.registry_hash_by_entry.is_empty() {
        tracing::warn!(
            target: "plasm_agent::execute_session",
            "legacy execute session without registry catalog pins; skipping rotation check"
        );
        return Ok(());
    }
    for eid in &pins.entry_ids {
        let expected = pins
            .registry_hash_by_entry
            .get(eid)
            .cloned()
            .unwrap_or_default();
        let live = match reg.load_context(eid) {
            Ok(ctx) => ctx.cgs.catalog_cgs_hash_hex(),
            Err(DiscoveryError::UnknownEntry(id)) => return Err(RehydrateError::UnknownEntry(id)),
            Err(e) => {
                return Err(RehydrateError::Discovery(format!(
                    "load context `{eid}`: {e}"
                )));
            }
        };
        if live != expected {
            return Err(RehydrateError::CatalogHashMismatch {
                entry_id: eid.clone(),
                expected,
                live,
            });
        }
    }
    Ok(())
}

/// Pinned **effective** catalog digests (post-overlay) — legacy descriptor field only.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct PinnedCatalogHashes {
    pub(crate) entry_ids: Vec<String>,
    pub(crate) expected_by_entry: HashMap<String, String>,
}

impl PinnedCatalogHashes {
    #[allow(dead_code)]
    pub(crate) fn from_descriptor(desc: &PersistedExecuteSessionDescriptor) -> Self {
        let entry_ids = if desc.catalog_cgs_hashes_by_entry.is_empty() {
            vec![desc.entry_id.clone()]
        } else {
            desc.context_entry_ids.clone()
        };
        let mut expected_by_entry = HashMap::new();
        for eid in &entry_ids {
            let expected = if desc.catalog_cgs_hashes_by_entry.is_empty() {
                desc.catalog_cgs_hash.clone()
            } else {
                desc.catalog_cgs_hashes_by_entry
                    .get(eid)
                    .cloned()
                    .unwrap_or_else(|| desc.catalog_cgs_hash.clone())
            };
            expected_by_entry.insert(eid.clone(), expected);
        }
        Self {
            entry_ids,
            expected_by_entry,
        }
    }
}

/// Whether a persisted descriptor / binding should be removed after rehydrate failure.
pub fn should_discard_persisted_execute_on_rehydrate_error(err: &RehydrateError) -> bool {
    matches!(
        err,
        RehydrateError::UnknownEntry(_)
            | RehydrateError::CatalogHashMismatch { .. }
            | RehydrateError::DescriptorExpired
            | RehydrateError::PluginGenerationUnavailable { .. }
    )
}

pub async fn rehydrate_execute_session(
    st: &PlasmHostState,
    desc: &PersistedExecuteSessionDescriptor,
) -> Result<ExecuteSession, RehydrateError> {
    if descriptor_expired(desc) {
        return Err(RehydrateError::DescriptorExpired);
    }

    let reg = st.catalog.snapshot();
    registry_pins_match_live(reg.as_ref(), &RegistryCatalogPins::from_descriptor(desc))?;

    let primary_materialized = materialize_entry_context(
        st,
        desc.entry_id.as_str(),
        desc.outbound_hosted_kv_by_entry
            .get(&desc.entry_id)
            .map(String::as_str),
        desc.bindings_by_entry.get(&desc.entry_id),
    )
    .await
    .map_err(RehydrateError::Discovery)?;

    let cgs = primary_materialized.effective_cgs;
    let http_backend = Some(primary_materialized.http_backend.as_str().to_string());

    let mut contexts_by_entry = IndexMap::new();
    contexts_by_entry.insert(desc.entry_id.clone(), primary_materialized.ctx);
    for eid in &desc.context_entry_ids {
        if eid == &desc.entry_id {
            continue;
        }
        let materialized = materialize_entry_context(
            st,
            eid.as_str(),
            desc.outbound_hosted_kv_by_entry
                .get(eid)
                .map(String::as_str),
            desc.bindings_by_entry.get(eid),
        )
        .await
        .map_err(RehydrateError::Discovery)?;
        contexts_by_entry.insert(eid.clone(), materialized.ctx);
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
        desc.http_backend.clone().or(http_backend),
        desc.entities.clone(),
        Some(teaching_exposure),
        desc.principal.clone(),
        plugin_generation,
        desc.catalog_cgs_hash.clone(),
        desc.context_intent.clone(),
        desc.ranked_capabilities.clone(),
        desc.bindings_by_entry.clone(),
    );
    session.registry_catalog_hashes_by_entry = desc.registry_catalog_hashes_by_entry.clone();
    session.domain_revision = desc.domain_revision;
    session.restore_persisted_plan_commits(&desc.plan_commits, desc.plan_commit_next);
    session.restore_persisted_operations(&crate::mcp_transport_store::OperationPersistSnapshot {
        operations: desc.operations.clone(),
        operation_handle_next: desc.operation_handle_next,
    });

    Ok(session)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;

    use super::*;
    use crate::mcp_transport_store::execute_session_registry::{
        PersistedExecuteSessionDescriptor, PersistedSessionReuseKey,
    };

    fn overshow_registry() -> InMemoryCgsRegistry {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs,
        )])
    }

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
            registry_catalog_hashes_by_entry: Default::default(),
            outbound_hosted_kv_by_entry: Default::default(),
            bindings_by_entry: Default::default(),
            plan_commits: Vec::new(),
            plan_commit_next: 0,
            operations: Vec::new(),
            operation_handle_next: 0,
        };
        assert!(descriptor_expired(&desc));
    }

    #[test]
    fn discard_on_catalog_mismatch_not_discovery_transient() {
        use RehydrateError::*;
        assert!(should_discard_persisted_execute_on_rehydrate_error(
            &CatalogHashMismatch {
                entry_id: "github".into(),
                expected: "a".into(),
                live: "b".into(),
            }
        ));
        assert!(!should_discard_persisted_execute_on_rehydrate_error(
            &Discovery("network".into())
        ));
    }

    #[test]
    fn pinned_hashes_from_descriptor_legacy_single_entry() {
        let desc = PersistedExecuteSessionDescriptor {
            prompt_hash: "ph".into(),
            session_id: "sid".into(),
            prompt_text: String::new(),
            entry_id: "overshow".into(),
            context_entry_ids: vec!["overshow".into()],
            entities: vec!["demo".into()],
            tenant_scope: "t".into(),
            principal_subject: String::new(),
            http_backend: None,
            principal: None,
            catalog_cgs_hash: "deadbeef".into(),
            context_intent: None,
            ranked_capabilities: None,
            plugin_generation_id: None,
            domain_revision: 0,
            reuse_key: PersistedSessionReuseKey {
                tenant_scope: "t".into(),
                entry_id: "overshow".into(),
                catalog_cgs_hash: "deadbeef".into(),
                entities: vec!["demo".into()],
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
                plugin_generation_id: None,
                logical_session_id: None,
            },
            expires_at_unix: u64::MAX,
            catalog_cgs_hashes_by_entry: Default::default(),
            registry_catalog_hashes_by_entry: Default::default(),
            outbound_hosted_kv_by_entry: Default::default(),
            bindings_by_entry: Default::default(),
            plan_commits: Vec::new(),
            plan_commit_next: 0,
            operations: Vec::new(),
            operation_handle_next: 0,
        };
        let pins = PinnedCatalogHashes::from_descriptor(&desc);
        assert_eq!(pins.entry_ids, vec!["overshow".to_string()]);
        assert_eq!(
            pins.expected_by_entry.get("overshow"),
            Some(&"deadbeef".to_string())
        );
    }

    #[test]
    fn registry_pins_match_live_ok_and_mismatch() {
        let reg = overshow_registry();
        let live_hash = reg
            .load_context("overshow")
            .expect("overshow")
            .cgs
            .catalog_cgs_hash_hex();
        let ok_pins = RegistryCatalogPins {
            entry_ids: vec!["overshow".into()],
            registry_hash_by_entry: HashMap::from([("overshow".into(), live_hash.clone())]),
        };
        assert!(registry_pins_match_live(&reg, &ok_pins).is_ok());

        let bad_pins = RegistryCatalogPins {
            entry_ids: vec!["overshow".into()],
            registry_hash_by_entry: HashMap::from([("overshow".into(), "stale".into())]),
        };
        assert_eq!(
            registry_pins_match_live(&reg, &bad_pins),
            Err(RehydrateError::CatalogHashMismatch {
                entry_id: "overshow".into(),
                expected: "stale".into(),
                live: live_hash,
            })
        );
    }
}
