//! Persistent session graph cache substrate (append deltas + snapshot manifests).
//!
//! Backends:
//! - Inactive when `PLASM_GRAPH_CACHE_URL` is unset.
//! - Object-store backed when `PLASM_GRAPH_CACHE_URL` is set (`object_store::parse_url_opts`).

use std::collections::BTreeMap;

use futures_util::StreamExt;
use object_store::WriteMultipart;
use object_store::{path::Path as StorePath, ObjectStore, ObjectStoreExt};
use plasm_runtime::{GraphCache, GraphPageDelta};
use serde::Serialize;
use std::sync::Arc;

use crate::run_artifacts::{ArtifactPayload, ArtifactPayloadMetadata};

#[derive(Clone)]
pub struct SessionGraphPersistence {
    store: Arc<dyn ObjectStore>,
    sessions_root: StorePath,
}

#[derive(Debug, Serialize)]
pub struct SnapshotManifest {
    pub through_seq: u64,
    pub snapshot_content_type: String,
    pub snapshot_key: String,
}

impl SessionGraphPersistence {
    pub fn new(store: Arc<dyn ObjectStore>, prefix: StorePath) -> Self {
        Self {
            store,
            sessions_root: prefix.join("v1").join("sessions"),
        }
    }

    fn delta_prefix(&self, prompt_hash: &str, session_id: &str) -> StorePath {
        self.sessions_root
            .clone()
            .join(prompt_hash)
            .join(session_id)
            .join("delta")
    }

    fn decode_framed_delta(bytes: &[u8]) -> Result<ArtifactPayload, String> {
        if bytes.len() < 4 {
            return Err("delta frame too short".into());
        }
        let meta_len = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
        if bytes.len() < 4 + meta_len {
            return Err("delta metadata truncated".into());
        }
        let metadata: ArtifactPayloadMetadata =
            serde_json::from_slice(&bytes[4..4 + meta_len]).map_err(|e| e.to_string())?;
        let body = bytes[4 + meta_len..].to_vec();
        Ok(ArtifactPayload {
            metadata,
            bytes: axum::body::Bytes::from(body),
        })
    }

    pub async fn append_delta(
        &self,
        prompt_hash: &str,
        session_id: &str,
        seq: u64,
        payload: &ArtifactPayload,
    ) -> Result<(), String> {
        let key = self
            .delta_prefix(prompt_hash, session_id)
            .join(format!("{seq:020}.bin"));
        let mut framed = Vec::with_capacity(256 + payload.bytes.len());
        let metadata = serde_json::to_vec(&payload.metadata).map_err(|e| e.to_string())?;
        framed.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        framed.extend_from_slice(&metadata);
        framed.extend_from_slice(&payload.bytes);
        self.store
            .put(&key, framed.into())
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn list_delta_seqs(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Result<Vec<u64>, String> {
        let prefix = self.delta_prefix(prompt_hash, session_id);
        let mut stream = self.store.list(Some(&prefix));
        let mut seqs = Vec::new();
        while let Some(meta) = stream.next().await {
            let meta = meta.map_err(|e| e.to_string())?;
            let name = meta.location.filename().unwrap_or("");
            if let Ok(seq) = name.trim_end_matches(".bin").parse::<u64>() {
                seqs.push(seq);
            }
        }
        seqs.sort_unstable();
        Ok(seqs)
    }

    pub async fn read_delta(
        &self,
        prompt_hash: &str,
        session_id: &str,
        seq: u64,
    ) -> Result<ArtifactPayload, String> {
        let key = self
            .delta_prefix(prompt_hash, session_id)
            .join(format!("{seq:020}.bin"));
        let got = self.store.get(&key).await.map_err(|e| e.to_string())?;
        let bytes = got.bytes().await.map_err(|e| e.to_string())?;
        Self::decode_framed_delta(&bytes)
    }

    pub fn parse_graph_page_body(body: &serde_json::Value) -> Option<GraphPageDelta> {
        if body.get("kind")?.as_str()? != "graph_page" {
            return None;
        }
        let schema_version = body
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32;
        let page_index = body.get("page_index")?.as_u64()? as usize;
        let entity_type = body
            .get("entity_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let entities = body.get("entities")?.as_array()?.to_vec();
        Some(GraphPageDelta {
            page_index,
            entity_type,
            schema_version,
            entities,
        })
    }

    pub async fn read_graph_pages(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Result<Vec<GraphPageDelta>, String> {
        self.load_graph_pages_sorted(prompt_hash, session_id).await
    }

    async fn load_graph_pages_sorted(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Result<Vec<GraphPageDelta>, String> {
        let seqs = self.list_delta_seqs(prompt_hash, session_id).await?;
        let mut pages = Vec::new();
        for seq in seqs {
            let payload = self.read_delta(prompt_hash, session_id, seq).await?;
            let body: serde_json::Value =
                serde_json::from_slice(&payload.bytes).map_err(|e| e.to_string())?;
            if let Some(page) = Self::parse_graph_page_body(&body) {
                pages.push(page);
            }
        }
        pages.sort_by_key(|p| p.page_index);
        Ok(pages)
    }

    pub async fn write_snapshot(
        &self,
        prompt_hash: &str,
        session_id: &str,
        through_seq: u64,
        content_type: &str,
        cache: &GraphCache,
    ) -> Result<(), String> {
        let pages = self
            .read_graph_pages(prompt_hash, session_id)
            .await
            .unwrap_or_default();
        self.write_snapshot_merged(
            prompt_hash,
            session_id,
            through_seq,
            content_type,
            cache,
            &pages,
        )
        .await
    }

    pub async fn write_snapshot_merged(
        &self,
        prompt_hash: &str,
        session_id: &str,
        through_seq: u64,
        content_type: &str,
        cache: &GraphCache,
        pages: &[GraphPageDelta],
    ) -> Result<(), String> {
        let snapshot_key = self
            .sessions_root
            .clone()
            .join(prompt_hash)
            .join(session_id)
            .join("snapshots")
            .join(format!("{through_seq:020}.bin"));

        let mut merged: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        for page in pages {
            for row in &page.entities {
                if let Some(r) = row.get("_ref").and_then(|v| v.as_str()) {
                    merged.insert(r.to_string(), row.clone());
                }
            }
        }
        for r in cache.all_references() {
            if let Ok(v) = cache.entity_to_json(r) {
                merged.insert(r.to_string(), v);
            }
        }

        let upload = self
            .store
            .put_multipart(&snapshot_key)
            .await
            .map_err(|e| e.to_string())?;
        let mut writer = WriteMultipart::new(upload);
        writer.write(b"[");
        let mut first = true;
        let mut scratch = Vec::with_capacity(4096);
        for v in merged.values() {
            if !first {
                writer.write(b",");
            }
            first = false;
            scratch.clear();
            serde_json::to_writer(&mut scratch, v).map_err(|e| e.to_string())?;
            writer.write(&scratch);
        }
        writer.write(b"]");
        writer.finish().await.map_err(|e| e.to_string())?;

        let manifest = SnapshotManifest {
            through_seq,
            snapshot_content_type: content_type.to_string(),
            snapshot_key: snapshot_key.to_string(),
        };
        let manifest_key = self
            .sessions_root
            .clone()
            .join(prompt_hash)
            .join(session_id)
            .join("manifest.json");
        let bytes = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
        self.store
            .put(&manifest_key, bytes.into())
            .await
            .map_err(|e| e.to_string())?;
        let _ = content_type;
        Ok(())
    }
}

pub fn init_from_env() -> Result<Option<Arc<SessionGraphPersistence>>, String> {
    let url_raw = match std::env::var("PLASM_GRAPH_CACHE_URL") {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return Ok(None),
    };
    let url =
        url::Url::parse(&url_raw).map_err(|e| format!("PLASM_GRAPH_CACHE_URL invalid URL: {e}"))?;
    let (boxed, prefix) = object_store::parse_url_opts(&url, std::env::vars())
        .map_err(|e| format!("PLASM_GRAPH_CACHE_URL could not open object store: {e}"))?;
    let store: Arc<dyn ObjectStore> = Arc::from(boxed);
    Ok(Some(Arc::new(SessionGraphPersistence::new(store, prefix))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_graph_page_v2_body() {
        let body = serde_json::json!({
            "kind": "graph_page",
            "schema_version": 2,
            "entity_type": "Berry",
            "page_index": 3,
            "entities": [{"_ref": "Berry:1", "name": "cheri"}]
        });
        let page = SessionGraphPersistence::parse_graph_page_body(&body).expect("page");
        assert_eq!(page.page_index, 3);
        assert_eq!(page.entity_type, "Berry");
        assert_eq!(page.schema_version, 2);
    }
}
