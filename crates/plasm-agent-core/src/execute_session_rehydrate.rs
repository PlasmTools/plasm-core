//! Rebuild an in-memory [`ExecuteSession`] from a Redis-persisted descriptor + catalog snapshot.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use plasm_core::discovery::CgsCatalog;
use plasm_core::discovery::DiscoveryError;
use plasm_core::InMemoryCgsRegistry;
use std::sync::Arc;

use crate::execute_session::ExecuteSession;
use crate::catalog_hash::RegistryCatalogHash;
use crate::execute_session_materialize::{
    materialize_entries_for_descriptor, MaterializeError,
};
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
    EntityCatalogPairingMismatch {
        entities: usize,
        catalog_ids: usize,
    },
    /// Embedded symbol ledger missing from durable descriptor (replay is not used).
    SymbolLedgerNotFound,
    /// Pinned catalog digests in the ledger no longer match live materialized CGS.
    SymbolSpaceResetRequired,
    SymbolLedgerDecode(String),
    Discovery(String),
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
            Self::EntityCatalogPairingMismatch {
                entities,
                catalog_ids,
            } => write!(
                f,
                "entity/catalog pairing mismatch ({entities} entities, {catalog_ids} catalog ids)"
            ),
            Self::SymbolLedgerNotFound => write!(
                f,
                "persisted execute session has no symbol ledger — call plasm_context with session_mode: \"new\""
            ),
            Self::SymbolSpaceResetRequired => write!(
                f,
                "symbol ledger catalog pins no longer match live catalogs — call plasm_context with session_mode: \"new\""
            ),
            Self::SymbolLedgerDecode(e) => write!(f, "symbol ledger decode failed: {e}"),
            Self::Discovery(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RehydrateError {}

impl From<MaterializeError> for RehydrateError {
    fn from(e: MaterializeError) -> Self {
        match e {
            MaterializeError::UnknownEntry(id) => Self::UnknownEntry(id),
            other => Self::Discovery(other.to_string()),
        }
    }
}

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
    pub(crate) registry_hash_by_entry: HashMap<String, RegistryCatalogHash>,
}

impl RegistryCatalogPins {
    pub(crate) fn from_execute_session(session: &ExecuteSession) -> Self {
        Self {
            entry_ids: session.contexts_by_entry.keys().cloned().collect(),
            registry_hash_by_entry: session
                .registry_catalog_hashes_by_entry
                .iter()
                .map(|(k, v)| (k.clone(), RegistryCatalogHash::from_hex(v.clone())))
                .collect(),
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
            registry_hash_by_entry: desc
                .registry_catalog_hashes_by_entry
                .iter()
                .map(|(k, v)| (k.clone(), RegistryCatalogHash::from_hex(v.clone())))
                .collect(),
        }
    }
}

pub(crate) fn registry_catalog_pins_from_registry(
    reg: &InMemoryCgsRegistry,
    entry_ids: &[String],
) -> Result<HashMap<String, String>, RehydrateError> {
    let pins = registry_catalog_pins_typed(reg, entry_ids)?;
    Ok(crate::catalog_hash::registry_hashes_to_strings(
        &pins.registry_hash_by_entry,
    ))
}

pub(crate) fn registry_catalog_pins_typed(
    reg: &InMemoryCgsRegistry,
    entry_ids: &[String],
) -> Result<RegistryCatalogPins, RehydrateError> {
    let mut registry_hash_by_entry = HashMap::new();
    for eid in entry_ids {
        let hash = match reg.load_context(eid) {
            Ok(ctx) => RegistryCatalogHash::from_registry_cgs(ctx.cgs.as_ref()),
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
            .map(RegistryCatalogHash::as_str)
            .unwrap_or_default()
            .to_string();
        let live = match reg.load_context(eid) {
            Ok(ctx) => RegistryCatalogHash::from_registry_cgs(ctx.cgs.as_ref()).as_str().to_string(),
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
#[cfg(test)]
mod legacy_descriptor_pins {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(super) struct PinnedCatalogHashes {
        pub entry_ids: Vec<String>,
        pub expected_by_entry: HashMap<String, String>,
    }

    impl PinnedCatalogHashes {
        pub(super) fn from_descriptor(desc: &PersistedExecuteSessionDescriptor) -> Self {
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
}

/// Whether a persisted descriptor / binding should be removed after rehydrate failure.
pub fn should_discard_persisted_execute_on_rehydrate_error(err: &RehydrateError) -> bool {
    matches!(
        err,
        RehydrateError::UnknownEntry(_)
            | RehydrateError::CatalogHashMismatch { .. }
            | RehydrateError::DescriptorExpired
            | RehydrateError::EntityCatalogPairingMismatch { .. }
            | RehydrateError::SymbolLedgerNotFound
            | RehydrateError::SymbolSpaceResetRequired
            | RehydrateError::SymbolLedgerDecode(_)
    )
}

fn hydrate_teaching_exposure_from_descriptor(
    desc: &PersistedExecuteSessionDescriptor,
    contexts_by_entry: &IndexMap<String, Arc<plasm_core::CgsContext>>,
) -> Result<plasm_core::TeachingExposureSession, RehydrateError> {
    if desc.symbol_ledger_bytes.is_empty() {
        return Err(RehydrateError::SymbolLedgerNotFound);
    }
    let snap = plasm_core::PersistedSymbolLedger::decode(desc.symbol_ledger_bytes.as_slice())
        .map_err(|e| RehydrateError::SymbolLedgerDecode(e.to_string()))?;
    let catalog_cgs: IndexMap<String, Arc<plasm_core::CGS>> = contexts_by_entry
        .iter()
        .map(|(eid, ctx)| (eid.clone(), ctx.cgs.clone()))
        .collect();
    if !plasm_core::catalog_pins_match(&snap.catalog_cgs_hashes, &catalog_cgs) {
        return Err(RehydrateError::SymbolSpaceResetRequired);
    }
    snap.hydrate(&catalog_cgs)
        .map_err(|e| RehydrateError::SymbolLedgerDecode(e.to_string()))
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

    let materialized =
        materialize_entries_for_descriptor(st, desc).await?;
    let primary_materialized = materialized
        .get(&desc.entry_id)
        .ok_or_else(|| MaterializeError::LoadContext {
            entry_id: desc.entry_id.clone(),
            detail: "primary entry missing after materialize".into(),
        })?;
    let cgs = primary_materialized.effective_cgs.clone();
    let http_backend = Some(primary_materialized.http_backend.as_str().to_string());

    let mut contexts_by_entry = IndexMap::new();
    for (eid, m) in &materialized {
        contexts_by_entry.insert(eid.clone(), m.ctx.clone());
    }

    if !desc.entity_catalog_entry_ids.is_empty()
        && desc.entity_catalog_entry_ids.len() != desc.entities.len()
    {
        return Err(RehydrateError::EntityCatalogPairingMismatch {
            entities: desc.entities.len(),
            catalog_ids: desc.entity_catalog_entry_ids.len(),
        });
    }
    let teaching_exposure =
        hydrate_teaching_exposure_from_descriptor(desc, &contexts_by_entry)?;

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
        desc.catalog_cgs_hash.clone(),
        desc.context_intent.clone(),
        desc.ranked_capabilities.clone(),
        desc.bindings_by_entry.clone(),
    );
    session.registry_catalog_hashes_by_entry = desc.registry_catalog_hashes_by_entry.clone();
    session.materialized_outbound_hosted_kv_by_entry =
        desc.outbound_hosted_kv_by_entry.clone();
    session.domain_revision = desc.domain_revision;
    session.restore_persisted_plan_commits(&desc.plan_commits, desc.plan_commit_next);
    session.restore_persisted_operations(&crate::mcp_transport_store::OperationPersistSnapshot {
        operations: desc.operations.clone(),
        operation_handle_next: desc.operation_handle_next,
    });
    session
        .restore_bind_credentials(&crate::execute_session::SessionBindCredentialsSnapshot {
            session_share_token: desc.session_share_token.clone(),
            session_proof_base_token: desc.session_proof_base_token.clone(),
        })
        .await;

    Ok(session)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;

    use super::*;
    use crate::execute_session::{ExecuteSession, SessionReuseKey};
    use crate::mcp_transport_store::execute_session_registry::{
        PersistedExecuteSessionDescriptor, PersistedSessionReuseKey,
    };
    use crate::test_support::exposure_replay_fixtures::{
        assert_github_langdetail_numbering_parity, interleaved_federated_matrix_fixture,
        matrix_federated_host,
    };
    use indexmap::IndexMap;

    async fn descriptor_from_live_session(
        st: &PlasmHostState,
        session: &ExecuteSession,
        session_id: &str,
        reuse_key: &SessionReuseKey,
    ) -> PersistedExecuteSessionDescriptor {
        let exposure =
            crate::execute_session_materialize::build_durable_exposure_snapshot(st, session)
                .await
                .expect("durable exposure snapshot");
        PersistedExecuteSessionDescriptor::from_session_and_durable_snapshot(
            session,
            session_id,
            reuse_key,
            session.snapshot_bind_credentials().await,
            &exposure,
        )
    }

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
            entity_catalog_entry_ids: vec!["e".into()],
            tenant_scope: "t".into(),
            principal_subject: String::new(),
            http_backend: None,
            principal: None,
            catalog_cgs_hash: "h".into(),
            context_intent: None,
            ranked_capabilities: None,
            domain_revision: 0,
            reuse_key: PersistedSessionReuseKey {
                tenant_scope: "t".into(),
                entry_id: "e".into(),
                catalog_cgs_hash: "h".into(),
                entities: vec!["x".into()],
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
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
            session_share_token: None,
            session_proof_base_token: None,
            symbol_ledger_bytes: Vec::new(),
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
    fn discard_on_entity_catalog_pairing_mismatch() {
        assert!(should_discard_persisted_execute_on_rehydrate_error(
            &RehydrateError::EntityCatalogPairingMismatch {
                entities: 2,
                catalog_ids: 1,
            }
        ));
    }

    #[tokio::test]
    async fn rehydrate_rejects_mismatched_entity_catalog_pairing() {
        let matrix_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = Arc::new(load_schema_dir(&matrix_dir).expect("matrix"));
        let reg = Arc::new(InMemoryCgsRegistry::from_pairs(vec![
            (
                "github".into(),
                "GitHub".into(),
                vec!["github".into()],
                cgs.clone(),
            ),
            (
                "linear".into(),
                "Linear".into(),
                vec!["linear".into()],
                cgs.clone(),
            ),
        ]));
        let engine = plasm_runtime::ExecutionEngine::new(plasm_runtime::ExecutionConfig::default())
            .expect("engine");
        let host = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
            engine,
            mode: plasm_runtime::ExecutionMode::Live,
            registry: reg,
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        });
        let desc = PersistedExecuteSessionDescriptor {
            prompt_hash: "ph".into(),
            session_id: "sid".into(),
            prompt_text: String::new(),
            entry_id: "github".into(),
            context_entry_ids: vec!["github".into(), "linear".into()],
            entities: vec!["LangItem".into(), "LangItem".into()],
            entity_catalog_entry_ids: vec!["github".into()],
            tenant_scope: String::new(),
            principal_subject: String::new(),
            http_backend: None,
            principal: None,
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
            context_intent: None,
            ranked_capabilities: None,
            domain_revision: 0,
            reuse_key: PersistedSessionReuseKey {
                tenant_scope: String::new(),
                entry_id: "github".into(),
                catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
                entities: vec!["LangItem".into(), "LangItem".into()],
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
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
            session_share_token: None,
            session_proof_base_token: None,
            symbol_ledger_bytes: Vec::new(),
        };
        let err = match rehydrate_execute_session(&host, &desc).await {
            Err(e) => e,
            Ok(_) => panic!("pairing mismatch should fail rehydrate"),
        };
        assert!(matches!(
            err,
            RehydrateError::EntityCatalogPairingMismatch { .. }
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
            entity_catalog_entry_ids: vec!["overshow".into()],
            tenant_scope: "t".into(),
            principal_subject: String::new(),
            http_backend: None,
            principal: None,
            catalog_cgs_hash: "deadbeef".into(),
            context_intent: None,
            ranked_capabilities: None,
            domain_revision: 0,
            reuse_key: PersistedSessionReuseKey {
                tenant_scope: "t".into(),
                entry_id: "overshow".into(),
                catalog_cgs_hash: "deadbeef".into(),
                entities: vec!["demo".into()],
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
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
            session_share_token: None,
            session_proof_base_token: None,
            symbol_ledger_bytes: Vec::new(),
        };
        let pins = legacy_descriptor_pins::PinnedCatalogHashes::from_descriptor(&desc);
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
            registry_hash_by_entry: HashMap::from([(
                "overshow".into(),
                RegistryCatalogHash::from_hex(live_hash.clone()),
            )]),
        };
        assert!(registry_pins_match_live(&reg, &ok_pins).is_ok());

        let bad_pins = RegistryCatalogPins {
            entry_ids: vec!["overshow".into()],
            registry_hash_by_entry: HashMap::from([(
                "overshow".into(),
                RegistryCatalogHash::from_hex("stale"),
            )]),
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

    #[test]
    fn replay_wave_structure_differs_from_incremental_live_path() {
        use crate::http_execute::replay_teaching_exposure_waves;

        let matrix_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = Arc::new(load_schema_dir(&matrix_dir).expect("matrix"));
        let mut contexts = IndexMap::new();
        contexts.insert(
            "github".into(),
            Arc::new(plasm_core::CgsContext::entry("github", cgs.clone())),
        );
        contexts.insert(
            "linear".into(),
            Arc::new(plasm_core::CgsContext::entry("linear", cgs.clone())),
        );
        let desc = PersistedExecuteSessionDescriptor {
            prompt_hash: "ph".into(),
            session_id: "sid".into(),
            prompt_text: String::new(),
            entry_id: "github".into(),
            context_entry_ids: vec!["github".into(), "linear".into()],
            entities: vec!["LangItem".into(), "LangItem".into()],
            entity_catalog_entry_ids: vec!["github".into(), "linear".into()],
            tenant_scope: String::new(),
            principal_subject: String::new(),
            http_backend: None,
            principal: None,
            catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
            context_intent: None,
            ranked_capabilities: None,
            domain_revision: 0,
            reuse_key: PersistedSessionReuseKey {
                tenant_scope: String::new(),
                entry_id: "github".into(),
                catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
                entities: vec!["LangItem".into(), "LangItem".into()],
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
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
            session_share_token: None,
            session_proof_base_token: None,
            symbol_ledger_bytes: Vec::new(),
        };
        let exp = replay_teaching_exposure_waves(
            &contexts,
            &desc.entities,
            &desc.entity_catalog_entry_ids,
            desc.context_intent.as_deref(),
            desc.ranked_capabilities.as_deref(),
        );
        assert_eq!(exp.entity_catalog_entry_ids, vec!["github", "linear"]);
        let (map, _): (Arc<plasm_core::SymbolMap>, _) = exp.symbol_map_arc_cross(None, None);
        assert!(map.resolve_session_entity_symbol("e2").is_some());
        assert_eq!(
            map.entry_id_for_entity_symbol("e2").as_deref(),
            Some("linear")
        );
    }

    #[tokio::test]
    async fn rehydrate_fails_without_embedded_symbol_ledger() {
        let fixture = interleaved_federated_matrix_fixture();
        let (host, reg) = matrix_federated_host(fixture.cgs.clone());
        let mut desc = descriptor_from_live_session(
            &host,
            &ExecuteSession::new_with_bindings(
                "ph".into(),
                String::new(),
                fixture.cgs.clone(),
                fixture.contexts.clone(),
                "linear".into(),
                String::new(),
                String::new(),
                None,
                fixture.live.entities.clone(),
                Some(fixture.live.clone()),
                None,
                fixture.cgs.catalog_cgs_hash_hex(),
                None,
                None,
                IndexMap::new(),
            ),
            "sid",
            &SessionReuseKey {
                tenant_scope: String::new(),
                entry_id: "linear".into(),
                catalog_cgs_hash: fixture.cgs.catalog_cgs_hash_hex(),
                entities: fixture.live.entities.clone(),
                context_intent: None,
                ranked_capabilities: None,
                principal: None,
                logical_session_id: None,
            },
        )
        .await;
        desc.expires_at_unix = u64::MAX;
        desc.registry_catalog_hashes_by_entry = HashMap::from([
            (
                "linear".into(),
                reg.load_context("linear")
                    .expect("linear")
                    .cgs
                    .catalog_cgs_hash_hex(),
            ),
            (
                "github".into(),
                reg.load_context("github")
                    .expect("github")
                    .cgs
                    .catalog_cgs_hash_hex(),
            ),
        ]);
        desc.symbol_ledger_bytes.clear();
        match rehydrate_execute_session(&host, &desc).await {
            Err(RehydrateError::SymbolLedgerNotFound) => {}
            Err(e) => panic!(
                "missing ledger must fail rehydrate with SymbolLedgerNotFound, got {e:?}"
            ),
            Ok(_) => panic!("missing ledger must fail rehydrate"),
        }
    }

    #[tokio::test]
    async fn rehydrate_ledger_hydrate_matches_live_numbering_end_to_end() {
        let fixture = interleaved_federated_matrix_fixture();
        let (host, reg) = matrix_federated_host(fixture.cgs.clone());

        let session = ExecuteSession::new_with_bindings(
            "ph".into(),
            String::new(),
            fixture.cgs.clone(),
            fixture.contexts.clone(),
            "linear".into(),
            String::new(),
            String::new(),
            None,
            fixture.live.entities.clone(),
            Some(fixture.live.clone()),
            None,
            fixture.cgs.catalog_cgs_hash_hex(),
            None,
            None,
            IndexMap::new(),
        );

        let reuse_key = SessionReuseKey {
            tenant_scope: String::new(),
            entry_id: "linear".into(),
            catalog_cgs_hash: session.catalog_cgs_hash.clone(),
            entities: session.entities.clone(),
            context_intent: None,
            ranked_capabilities: None,
            principal: None,
            logical_session_id: None,
        };

        let desc = descriptor_from_live_session(&host, &session, "sid", &reuse_key).await;
        let mut desc = desc;
        desc.expires_at_unix = u64::MAX;
        desc.registry_catalog_hashes_by_entry = HashMap::from([
            (
                "linear".into(),
                reg.load_context("linear")
                    .expect("linear")
                    .cgs
                    .catalog_cgs_hash_hex(),
            ),
            (
                "github".into(),
                reg.load_context("github")
                    .expect("github")
                    .cgs
                    .catalog_cgs_hash_hex(),
            ),
        ]);

        let rehydrated = rehydrate_execute_session(&host, &desc)
            .await
            .expect("rehydrate");

        let live_map = session
            .teaching_exposure
            .as_ref()
            .expect("live exposure")
            .symbol_map_arc();
        let re_map = rehydrated
            .teaching_exposure
            .as_ref()
            .expect("rehydrated exposure")
            .symbol_map_arc();
        assert_github_langdetail_numbering_parity(&live_map, &re_map);
        assert_eq!(rehydrated.entities, session.entities);
    }
}
