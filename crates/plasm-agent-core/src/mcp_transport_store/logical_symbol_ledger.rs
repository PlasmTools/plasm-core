//! Append-only symbol ledger per logical MCP session (`logical_session_ref`).
//!
//! Numbering authority for `e#` / `m#` / `p#` / `r#` is scoped to the logical session, not
//! [`crate::execute_session::SessionReuseKey`] or transport `(prompt_hash, session_id)` rows.
//!
//! Durable wire format: `b"PLSL" + u8 version + postcard(PersistedSymbolLedger)` in Redis hot cache
//! and optional object-store archive (`symbol_ledgers/{uuid}.psl`).

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::{
    PersistedSymbolLedger, PersistedSymbolLedgerDecodeError, TeachingExposureSession,
};
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use super::symbol_ledger_archive::SymbolLedgerArchive;

const LEDGER_KEY_PREFIX: &str = "mcp:execute:symbol_ledger:";

#[derive(Clone, Debug)]
pub struct LogicalSymbolLedgerEntry {
    /// Pinned digest per registry `entry_id` (mismatch on any row ⇒ symbol reset).
    pub catalog_cgs_hashes: IndexMap<String, String>,
    pub exposure: Arc<TeachingExposureSession>,
}

/// Result of loading a durable blob (Redis → archive), without hydrating live exposure.
#[derive(Debug)]
pub enum DurableSymbolLedgerLoad {
    Found(Box<PersistedSymbolLedger>),
    NotFound,
    UnsupportedVersion(u8),
    Decode(PersistedSymbolLedgerDecodeError),
}

fn ledger_key(uuid: &Uuid) -> String {
    format!("{LEDGER_KEY_PREFIX}{uuid}")
}

fn decode_durable_bytes(
    bytes: &[u8],
) -> Result<PersistedSymbolLedger, PersistedSymbolLedgerDecodeError> {
    PersistedSymbolLedger::decode(bytes)
}

fn classify_decode_error(err: PersistedSymbolLedgerDecodeError) -> DurableSymbolLedgerLoad {
    match err {
        PersistedSymbolLedgerDecodeError::UnsupportedVersion(v) => {
            DurableSymbolLedgerLoad::UnsupportedVersion(v)
        }
        other => DurableSymbolLedgerLoad::Decode(other),
    }
}

/// In-memory hot cache + optional Redis + object-store durable blobs.
#[derive(Clone, Default)]
pub struct LogicalSymbolLedgerRegistry {
    local: Arc<RwLock<HashMap<Uuid, LogicalSymbolLedgerEntry>>>,
    redis: Arc<RwLock<Option<Arc<super::redis_backend::RedisBackend>>>>,
    archive: Arc<RwLock<Option<Arc<SymbolLedgerArchive>>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SymbolLedgerUpsertError {
    #[error("symbol ledger encode failed: {0}")]
    Encode(#[from] plasm_core::PersistedSymbolLedgerEncodeError),
    #[error("redis durable write failed for logical session {logical_id}")]
    RedisWriteFailed { logical_id: Uuid },
}

impl LogicalSymbolLedgerRegistry {
    pub fn new_in_memory() -> Self {
        Self::default()
    }

    pub async fn attach_redis(&self, backend: Arc<super::redis_backend::RedisBackend>) {
        *self.redis.write().await = Some(backend);
    }

    pub async fn attach_archive(&self, archive: Arc<SymbolLedgerArchive>) {
        *self.archive.write().await = Some(archive);
    }

    async fn redis(&self) -> Option<Arc<super::redis_backend::RedisBackend>> {
        self.redis.read().await.clone()
    }

    async fn archive(&self) -> Option<Arc<SymbolLedgerArchive>> {
        self.archive.read().await.clone()
    }

    pub async fn get_local(&self, logical_id: &Uuid) -> Option<LogicalSymbolLedgerEntry> {
        let g = self.local.read().await;
        g.get(logical_id).cloned()
    }

    async fn remove_local(&self, logical_id: &Uuid) {
        let mut g = self.local.write().await;
        g.remove(logical_id);
    }

    async fn cache_local(&self, logical_id: Uuid, entry: LogicalSymbolLedgerEntry) {
        let mut g = self.local.write().await;
        g.insert(logical_id, entry);
    }

    /// Load durable postcard bytes from Redis, then object-store archive. Does not hydrate.
    pub async fn load_durable(&self, logical_id: &Uuid) -> DurableSymbolLedgerLoad {
        if let Some(redis) = self.redis().await.as_ref() {
            if let Some(bytes) = redis.get_bytes(&ledger_key(logical_id)).await {
                return match decode_durable_bytes(&bytes) {
                    Ok(snap) => DurableSymbolLedgerLoad::Found(Box::new(snap)),
                    Err(err) => {
                        warn!(?err, %logical_id, "invalid symbol ledger redis blob");
                        classify_decode_error(err)
                    }
                };
            }
        }
        if let Some(archive) = self.archive().await.as_ref() {
            if let Some(bytes) = archive.get(logical_id).await {
                return match decode_durable_bytes(&bytes) {
                    Ok(snap) => {
                        if let Some(redis) = self.redis().await.as_ref() {
                            if !redis.set_bytes(&ledger_key(logical_id), &bytes).await {
                                warn!(%logical_id, "failed to warm redis from archive symbol ledger");
                            }
                        }
                        DurableSymbolLedgerLoad::Found(Box::new(snap))
                    }
                    Err(err) => {
                        warn!(?err, %logical_id, "invalid symbol ledger archive blob");
                        classify_decode_error(err)
                    }
                };
            }
        }
        DurableSymbolLedgerLoad::NotFound
    }

    /// Hydrate a decoded snapshot, cache locally, and return the live entry (single decode path).
    pub async fn hydrate_and_cache(
        &self,
        logical_id: Uuid,
        snap: PersistedSymbolLedger,
        catalog_cgs: &IndexMap<String, Arc<plasm_core::CGS>>,
    ) -> Result<LogicalSymbolLedgerEntry, PersistedSymbolLedgerDecodeError> {
        let exposure = snap.hydrate(catalog_cgs)?;
        let entry = LogicalSymbolLedgerEntry {
            catalog_cgs_hashes: snap.catalog_cgs_hashes.clone(),
            exposure: Arc::new(exposure),
        };
        self.cache_local(logical_id, entry.clone()).await;
        Ok(entry)
    }

    /// Local memory → Redis blob → object-store blob. Cold loads require `catalog_cgs` to hydrate.
    pub async fn get(
        &self,
        logical_id: &Uuid,
        catalog_cgs: Option<&IndexMap<String, Arc<plasm_core::CGS>>>,
    ) -> Option<LogicalSymbolLedgerEntry> {
        if let Some(entry) = self.get_local(logical_id).await {
            if let Some(redis) = self.redis().await.as_ref() {
                redis.touch(&ledger_key(logical_id)).await;
            }
            return Some(entry);
        }
        let catalog_cgs = catalog_cgs?;
        match self.load_durable(logical_id).await {
            DurableSymbolLedgerLoad::Found(snap) => self
                .hydrate_and_cache(*logical_id, *snap, catalog_cgs)
                .await
                .ok(),
            _ => None,
        }
    }

    pub async fn upsert_preencoded(
        &self,
        logical_id: Uuid,
        catalog_cgs_hashes: IndexMap<String, String>,
        symbol_ledger_bytes: Vec<u8>,
        exposure: TeachingExposureSession,
    ) -> Result<(), SymbolLedgerUpsertError> {
        if symbol_ledger_bytes.is_empty() {
            return self.upsert(logical_id, catalog_cgs_hashes, exposure).await;
        }
        let entry = LogicalSymbolLedgerEntry {
            catalog_cgs_hashes,
            exposure: Arc::new(exposure),
        };
        self.cache_local(logical_id, entry).await;
        if let Some(redis) = self.redis().await.as_ref() {
            if !redis
                .set_bytes(&ledger_key(&logical_id), &symbol_ledger_bytes)
                .await
            {
                self.remove_local(&logical_id).await;
                return Err(SymbolLedgerUpsertError::RedisWriteFailed { logical_id });
            }
        }
        if let Some(archive) = self.archive().await.as_ref() {
            archive.put(&logical_id, symbol_ledger_bytes).await;
        }
        Ok(())
    }

    pub async fn upsert(
        &self,
        logical_id: Uuid,
        catalog_cgs_hashes: IndexMap<String, String>,
        exposure: TeachingExposureSession,
    ) -> Result<(), SymbolLedgerUpsertError> {
        let snap = PersistedSymbolLedger::from_session(&exposure, catalog_cgs_hashes.clone())?;
        let bytes = snap.encode()?;
        let entry = LogicalSymbolLedgerEntry {
            catalog_cgs_hashes,
            exposure: Arc::new(exposure),
        };
        self.cache_local(logical_id, entry).await;
        if let Some(redis) = self.redis().await.as_ref() {
            if !redis.set_bytes(&ledger_key(&logical_id), &bytes).await {
                self.remove_local(&logical_id).await;
                return Err(SymbolLedgerUpsertError::RedisWriteFailed { logical_id });
            }
        }
        if let Some(archive) = self.archive().await.as_ref() {
            archive.put(&logical_id, bytes).await;
        }
        Ok(())
    }

    pub async fn remove(&self, logical_id: &Uuid) {
        self.remove_local(logical_id).await;
        if let Some(redis) = self.redis().await.as_ref() {
            redis.delete(&ledger_key(logical_id)).await;
        }
        if let Some(archive) = self.archive().await.as_ref() {
            archive.delete(logical_id).await;
        }
    }

    pub async fn purge_durable_layers(&self) -> u64 {
        {
            let mut g = self.local.write().await;
            g.clear();
        }
        let redis_deleted = if let Some(redis) = self.redis().await {
            redis.delete_keys_matching_prefix(LEDGER_KEY_PREFIX).await
        } else {
            0
        };
        let archive_deleted = if let Some(archive) = self.archive().await.as_ref() {
            archive.purge_all().await
        } else {
            0
        };
        redis_deleted.saturating_add(archive_deleted)
    }

    #[cfg(test)]
    pub(crate) async fn clear_local_for_test(&self) {
        let mut g = self.local.write().await;
        g.clear();
    }

    #[cfg(test)]
    pub(crate) async fn delete_archive_for_test(&self, logical_id: &Uuid) {
        if let Some(archive) = self.archive().await.as_ref() {
            archive.delete(logical_id).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn delete_redis_for_test(&self, logical_id: &Uuid) {
        if let Some(redis) = self.redis().await.as_ref() {
            redis.delete(&ledger_key(logical_id)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn matrix_exposure() -> TeachingExposureSession {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA", "HomographRowB"])
    }

    fn matrix_catalog_cgs() -> IndexMap<String, Arc<plasm_core::CGS>> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        let mut catalog_cgs = IndexMap::new();
        catalog_cgs.insert("langmatrix".to_string(), Arc::new(cgs));
        catalog_cgs
    }

    fn file_archive(dir: &TempDir) -> Arc<SymbolLedgerArchive> {
        let url = format!("file://{}", dir.path().display());
        Arc::new(
            SymbolLedgerArchive::from_url(&url, Some("symbol_ledgers"))
                .expect("url")
                .expect("archive"),
        )
    }

    #[tokio::test]
    async fn upsert_preencoded_matches_full_encode_path() {
        let exp = matrix_exposure();
        let hashes = plasm_core::catalog_cgs_hashes_from_session(&exp);
        let snap = plasm_core::PersistedSymbolLedger::from_session(&exp, hashes.clone())
            .expect("from_session");
        let bytes = snap.encode().expect("encode");
        let registry = LogicalSymbolLedgerRegistry::new_in_memory();
        let via_encode = Uuid::new_v4();
        let via_preencoded = Uuid::new_v4();
        registry
            .upsert(via_encode, hashes.clone(), exp.clone())
            .await
            .expect("upsert");
        registry
            .upsert_preencoded(via_preencoded, hashes, bytes, exp)
            .await
            .expect("preencoded");
        let a = registry.get_local(&via_encode).await.expect("encode local");
        let b = registry
            .get_local(&via_preencoded)
            .await
            .expect("preencoded local");
        assert_eq!(a.catalog_cgs_hashes, b.catalog_cgs_hashes);
        assert_eq!(
            a.exposure
                .qualified_entity_symbol("langmatrix", "HomographRowA"),
            b.exposure
                .qualified_entity_symbol("langmatrix", "HomographRowA"),
        );
    }

    #[tokio::test]
    async fn cold_object_store_restore_preserves_entity_symbols() {
        let exp = matrix_exposure();
        let hashes = plasm_core::catalog_cgs_hashes_from_session(&exp);
        let label_a = exp
            .qualified_entity_symbol("langmatrix", "HomographRowA")
            .expect("e#");
        let dir = TempDir::new().expect("tempdir");
        let registry = LogicalSymbolLedgerRegistry::new_in_memory();
        registry.attach_archive(file_archive(&dir)).await;
        let id = Uuid::new_v4();
        registry
            .upsert(id, hashes.clone(), exp)
            .await
            .expect("upsert");
        registry.clear_local_for_test().await;
        let catalog_cgs = matrix_catalog_cgs();
        let entry = registry
            .get(&id, Some(&catalog_cgs))
            .await
            .expect("cold restore");
        assert_eq!(
            entry
                .exposure
                .qualified_entity_symbol("langmatrix", "HomographRowA"),
            Some(label_a)
        );
    }

    #[tokio::test]
    async fn archive_only_restore_after_redis_key_cleared() {
        let exp = matrix_exposure();
        let hashes = plasm_core::catalog_cgs_hashes_from_session(&exp);
        let label_b = exp
            .qualified_entity_symbol("langmatrix", "HomographRowB")
            .expect("e#");
        let dir = TempDir::new().expect("tempdir");
        let registry = LogicalSymbolLedgerRegistry::new_in_memory();
        registry.attach_archive(file_archive(&dir)).await;
        let id = Uuid::new_v4();
        registry.upsert(id, hashes, exp).await.expect("upsert");
        registry.clear_local_for_test().await;
        registry.delete_redis_for_test(&id).await;
        let entry = registry
            .get(&id, Some(&matrix_catalog_cgs()))
            .await
            .expect("archive-only restore");
        assert_eq!(
            entry
                .exposure
                .qualified_entity_symbol("langmatrix", "HomographRowB"),
            Some(label_b)
        );
    }

    #[tokio::test]
    async fn hydrate_and_cache_populates_local_without_re_reading_archive() {
        let exp = matrix_exposure();
        let hashes = plasm_core::catalog_cgs_hashes_from_session(&exp);
        let label_a = exp
            .qualified_entity_symbol("langmatrix", "HomographRowA")
            .expect("e#");
        let dir = TempDir::new().expect("tempdir");
        let registry = LogicalSymbolLedgerRegistry::new_in_memory();
        registry.attach_archive(file_archive(&dir)).await;
        let id = Uuid::new_v4();
        registry.upsert(id, hashes, exp).await.expect("upsert");
        registry.clear_local_for_test().await;
        let snap = match registry.load_durable(&id).await {
            DurableSymbolLedgerLoad::Found(s) => *s,
            other => panic!("expected durable snapshot: {other:?}"),
        };
        registry.delete_archive_for_test(&id).await;
        let entry = registry
            .hydrate_and_cache(id, snap, &matrix_catalog_cgs())
            .await
            .expect("hydrate");
        assert_eq!(
            entry
                .exposure
                .qualified_entity_symbol("langmatrix", "HomographRowA"),
            Some(label_a)
        );
        assert!(registry.get_local(&id).await.is_some());
    }

    #[tokio::test]
    async fn unsupported_version_blob_classified_for_reset() {
        let exp = matrix_exposure();
        let mut bytes = PersistedSymbolLedger::from_session(
            &exp,
            plasm_core::catalog_cgs_hashes_from_session(&exp),
        )
        .expect("from_session")
        .encode()
        .expect("encode");
        bytes[4] = 99;
        let dir = TempDir::new().expect("tempdir");
        let archive = file_archive(&dir);
        let id = Uuid::new_v4();
        archive.put(&id, bytes).await;
        let registry = LogicalSymbolLedgerRegistry::new_in_memory();
        registry.attach_archive(archive).await;
        assert!(matches!(
            registry.load_durable(&id).await,
            DurableSymbolLedgerLoad::UnsupportedVersion(99)
        ));
    }

    #[tokio::test]
    async fn upsert_round_trip_through_local_cache() {
        let exp = matrix_exposure();
        let hashes = plasm_core::catalog_cgs_hashes_from_session(&exp);
        let registry = LogicalSymbolLedgerRegistry::new_in_memory();
        let id = Uuid::new_v4();
        registry
            .upsert(id, hashes, exp.clone())
            .await
            .expect("upsert");
        let entry = registry.get_local(&id).await.expect("local");
        assert_eq!(
            exp.qualified_entity_symbol("langmatrix", "HomographRowA"),
            entry
                .exposure
                .qualified_entity_symbol("langmatrix", "HomographRowA")
        );
    }
}
