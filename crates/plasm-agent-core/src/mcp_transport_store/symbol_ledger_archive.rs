//! Object-store archive for durable symbol ledger blobs (`symbol_ledgers/{uuid}.psl`).

use object_store::path::Path as StorePath;
use object_store::{ObjectStore, ObjectStoreExt};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

const OBJECT_SUFFIX: &str = ".psl";
const DEFAULT_PREFIX: &str = "symbol_ledgers";

#[derive(Clone)]
pub struct SymbolLedgerArchive {
    store: Arc<dyn ObjectStore>,
    prefix: StorePath,
}

impl SymbolLedgerArchive {
    fn object_key(prefix: &StorePath, logical_id: &Uuid) -> StorePath {
        prefix.join(format!("{logical_id}{OBJECT_SUFFIX}"))
    }

    /// `PLASM_SYMBOL_LEDGER_URL` when set; otherwise reuse `PLASM_RUN_ARTIFACTS_URL` bucket/prefix.
    pub fn from_env() -> Result<Option<Self>, String> {
        if let Ok(url_raw) = std::env::var("PLASM_SYMBOL_LEDGER_URL") {
            if !url_raw.trim().is_empty() {
                return Self::from_url(&url_raw, None);
            }
        }
        if let Ok(url_raw) = std::env::var("PLASM_RUN_ARTIFACTS_URL") {
            if !url_raw.trim().is_empty() {
                return Self::from_url(&url_raw, Some(DEFAULT_PREFIX));
            }
        }
        Ok(None)
    }

    pub(crate) fn from_url(url_raw: &str, subprefix: Option<&str>) -> Result<Option<Self>, String> {
        let url = url::Url::parse(url_raw.trim())
            .map_err(|e| format!("symbol ledger object store URL invalid: {e}"))?;
        let (boxed, mut prefix) = object_store::parse_url_opts(&url, std::env::vars())
            .map_err(|e| format!("symbol ledger object store open failed: {e}"))?;
        if let Some(sub) = subprefix {
            prefix = prefix.join(sub);
        }
        Ok(Some(Self {
            store: Arc::from(boxed),
            prefix,
        }))
    }

    pub async fn get(&self, logical_id: &Uuid) -> Option<Vec<u8>> {
        let key = Self::object_key(&self.prefix, logical_id);
        let res = self.store.get(&key).await.ok()?;
        res.bytes().await.ok().map(|b| b.to_vec())
    }

    pub async fn put(&self, logical_id: &Uuid, bytes: Vec<u8>) {
        let key = Self::object_key(&self.prefix, logical_id);
        if let Err(err) = self.store.put(&key, bytes.into()).await {
            warn!(?err, %logical_id, "symbol ledger object store put failed");
        }
    }

    pub async fn delete(&self, logical_id: &Uuid) {
        let key = Self::object_key(&self.prefix, logical_id);
        let _ = self.store.delete(&key).await;
    }

    /// Best-effort delete of all objects under the configured prefix (catalog purge).
    pub async fn purge_all(&self) -> u64 {
        let mut deleted = 0u64;
        let mut stream = self.store.list(Some(&self.prefix));
        use futures_util::StreamExt;
        while let Some(meta) = stream.next().await {
            let Ok(meta) = meta else { continue };
            if self.store.delete(&meta.location).await.is_ok() {
                deleted = deleted.saturating_add(1);
            }
        }
        deleted
    }
}
