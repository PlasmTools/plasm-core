//! Persistent session graph cache substrate (append deltas + snapshot manifests).
//!
//! Backends:
//! - Inactive when `PLASM_GRAPH_CACHE_URL` is unset.
//! - Object-store backed when `PLASM_GRAPH_CACHE_URL` is set (`object_store::parse_url_opts`).
//!
//! ## Spill delta ordering
//!
//! Paginated fetch-all appends one graph page per HTTP page via [`GraphPageSpill::append_page`],
//! allocating a monotonic `seq` per append. **`seq` order matches pagination append order**, and
//! each page body carries the authoritative `page_index` from the runtime.
//!
//! - **Streaming rehydrate** ([`Self::visit_graph_pages_in_seq_order`]) interleaves object-store
//!   **LIST** with **GET**: delta keys are buffered and read in ascending `seq` order even when LIST
//!   iteration order is undefined (per `object_store`). Early exit stops further GETs and further
//!   LIST pagination once the buffer is drained — no full-prefix inventory before the first read
//!   when LIST returns keys in append order.
//! - **Full merge** ([`Self::read_graph_pages`]) must still enumerate every delta key (bounded
//!   parallel GET after a full LIST); use only when the whole spilled graph is required.
//!
//! Callers must not append out-of-order pages for the same session; the spill host only appends
//! during sequential pagination.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use futures_util::{StreamExt, TryStreamExt};
use object_store::WriteMultipart;
use object_store::{path::Path as StorePath, ObjectStore, ObjectStoreExt};
use plasm_runtime::{GraphCache, GraphPageDelta};
use serde::Serialize;
use std::sync::Arc;
use thiserror::Error;

use axum::body::Bytes;

use crate::run_artifacts::{
    validate_artifact_payload_metadata, ArtifactPayload, ArtifactPayloadMetadata,
};

pub const GRAPH_PAGE_DELTA_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Error)]
pub enum SessionGraphPersistenceError {
    #[error("session graph object-store operation failed: {0}")]
    ObjectStore(#[from] object_store::Error),
    #[error("session graph JSON serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    ArtifactMetadata(#[from] crate::run_artifacts::ArtifactPayloadMetadataError),
    #[error("session graph delta frame is shorter than its header")]
    FrameTooShort,
    #[error(
        "session graph delta metadata is truncated: declared {declared} bytes, found {available}"
    )]
    MetadataTruncated { declared: usize, available: usize },
    #[error("graph page kind must be `graph_page`")]
    InvalidGraphPageKind,
    #[error("graph page is missing or has invalid field `{field}`")]
    InvalidGraphPageField { field: &'static str },
    #[error("graph page schema version must be {expected}; got {actual}")]
    GraphPageSchemaVersion { expected: u32, actual: u64 },
    #[error(
        "graph page contains entities that do not match its declared entity type `{entity_type}`"
    )]
    GraphPageEntityTypeMismatch { entity_type: String },
    #[error("graph page visitor failed: {source}")]
    Visitor {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("session graph URL is invalid")]
    InvalidUrl(#[source] url::ParseError),
    #[error("session graph object store could not be opened")]
    ObjectStoreInitialization(#[source] object_store::Error),
}

const DEFAULT_DELTA_READ_CONCURRENCY: usize = 16;

fn delta_read_concurrency() -> usize {
    std::env::var("PLASM_GRAPH_DELTA_READ_CONCURRENCY")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_DELTA_READ_CONCURRENCY)
}

fn parse_delta_seq_filename(name: &str) -> Option<u64> {
    name.trim_end_matches(".bin").parse().ok()
}

/// Whether `seq` is safe to read from a partial LIST (contiguous spill seqs).
fn spill_seq_ready_to_read(seq: u64, pending: &BTreeSet<u64>, list_done: bool) -> bool {
    list_done || pending.contains(&(seq + 1))
}

/// Advance a delta LIST stream to the next `{seq:020}.bin` entry (skips unrelated keys).
async fn next_delta_seq_from_list<S>(
    stream: &mut S,
) -> Result<Option<u64>, SessionGraphPersistenceError>
where
    S: StreamExt<Item = Result<object_store::ObjectMeta, object_store::Error>> + Unpin,
{
    while let Some(meta) = stream.next().await {
        let meta = meta?;
        if let Some(seq) = parse_delta_seq_filename(meta.location.filename().unwrap_or("")) {
            return Ok(Some(seq));
        }
    }
    Ok(None)
}

fn decode_framed_delta(bytes: Bytes) -> Result<ArtifactPayload, SessionGraphPersistenceError> {
    if bytes.len() < 4 {
        return Err(SessionGraphPersistenceError::FrameTooShort);
    }
    let meta_len = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let header_end = 4 + meta_len;
    if bytes.len() < header_end {
        return Err(SessionGraphPersistenceError::MetadataTruncated {
            declared: meta_len,
            available: bytes.len().saturating_sub(4),
        });
    }
    let metadata: ArtifactPayloadMetadata = serde_json::from_slice(&bytes[4..header_end])?;
    validate_artifact_payload_metadata(&metadata)?;
    Ok(ArtifactPayload {
        metadata,
        bytes: bytes.slice(header_end..),
    })
}

#[derive(Clone)]
pub struct SessionGraphPersistence {
    store: Arc<dyn ObjectStore>,
    sessions_root: StorePath,
}

#[derive(Debug, Serialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub through_seq: u64,
    pub snapshot_content_type: String,
    pub snapshot_key: String,
}

/// Exact-match graph page delta wire validation (schema v3 cutover).
pub fn validate_graph_page_delta(
    body: &serde_json::Value,
) -> Result<GraphPageDelta, SessionGraphPersistenceError> {
    parse_graph_page_body(body)
}

fn parse_graph_page_body(
    body: &serde_json::Value,
) -> Result<GraphPageDelta, SessionGraphPersistenceError> {
    if body.get("kind").and_then(|v| v.as_str()) != Some("graph_page") {
        return Err(SessionGraphPersistenceError::InvalidGraphPageKind);
    }
    let schema_version = body.get("schema_version").and_then(|v| v.as_u64()).ok_or(
        SessionGraphPersistenceError::InvalidGraphPageField {
            field: "schema_version",
        },
    )?;
    if schema_version != GRAPH_PAGE_DELTA_SCHEMA_VERSION as u64 {
        return Err(SessionGraphPersistenceError::GraphPageSchemaVersion {
            expected: GRAPH_PAGE_DELTA_SCHEMA_VERSION,
            actual: schema_version,
        });
    }
    let page_index = body.get("page_index").and_then(|v| v.as_u64()).ok_or(
        SessionGraphPersistenceError::InvalidGraphPageField {
            field: "page_index",
        },
    )? as usize;
    let entity_type = body
        .get("entity_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(SessionGraphPersistenceError::InvalidGraphPageField {
            field: "entity_type",
        })?
        .to_string();
    let entities: Vec<plasm_runtime::CachedEntity> = serde_json::from_value(
        body.get("entities")
            .cloned()
            .ok_or(SessionGraphPersistenceError::InvalidGraphPageField { field: "entities" })?,
    )?;
    if entities
        .iter()
        .any(|entity| entity.reference.entity_type.as_str() != entity_type)
    {
        return Err(SessionGraphPersistenceError::GraphPageEntityTypeMismatch { entity_type });
    }
    Ok(GraphPageDelta {
        page_index,
        entity_type,
        schema_version: GRAPH_PAGE_DELTA_SCHEMA_VERSION,
        entities,
    })
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

    pub async fn append_delta(
        &self,
        prompt_hash: &str,
        session_id: &str,
        seq: u64,
        payload: &ArtifactPayload,
    ) -> Result<(), SessionGraphPersistenceError> {
        let key = self
            .delta_prefix(prompt_hash, session_id)
            .join(format!("{seq:020}.bin"));
        let mut framed = Vec::with_capacity(256 + payload.bytes.len());
        let metadata = serde_json::to_vec(&payload.metadata)?;
        framed.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        framed.extend_from_slice(&metadata);
        framed.extend_from_slice(&payload.bytes);
        self.store.put(&key, framed.into()).await?;
        Ok(())
    }

    pub async fn list_delta_seqs(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Result<Vec<u64>, SessionGraphPersistenceError> {
        let prefix = self.delta_prefix(prompt_hash, session_id);
        let mut stream = self.store.list(Some(&prefix));
        let mut seqs = Vec::new();
        while let Some(seq) = next_delta_seq_from_list(&mut stream).await? {
            seqs.push(seq);
        }
        seqs.sort_unstable();
        Ok(seqs)
    }

    pub async fn read_delta(
        &self,
        prompt_hash: &str,
        session_id: &str,
        seq: u64,
    ) -> Result<ArtifactPayload, SessionGraphPersistenceError> {
        let key = self
            .delta_prefix(prompt_hash, session_id)
            .join(format!("{seq:020}.bin"));
        let got = self.store.get(&key).await?;
        let bytes = got.bytes().await?;
        decode_framed_delta(bytes)
    }

    pub async fn read_graph_pages(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Result<Vec<GraphPageDelta>, SessionGraphPersistenceError> {
        self.load_graph_pages_sorted(prompt_hash, session_id).await
    }

    pub(crate) async fn read_delta_graph_page(
        &self,
        prompt_hash: &str,
        session_id: &str,
        seq: u64,
    ) -> Result<GraphPageDelta, SessionGraphPersistenceError> {
        let payload = self.read_delta(prompt_hash, session_id, seq).await?;
        let body: serde_json::Value = serde_json::from_slice(&payload.bytes)?;
        parse_graph_page_body(&body)
    }

    /// Walk spill deltas in append (`seq`) order — interleaved LIST+GET; early exit cancels both.
    pub async fn visit_graph_pages_in_seq_order<F>(
        &self,
        prompt_hash: &str,
        session_id: &str,
        mut visit: F,
    ) -> Result<usize, SessionGraphPersistenceError>
    where
        F: FnMut(
            GraphPageDelta,
        ) -> Result<ControlFlow<()>, Box<dyn std::error::Error + Send + Sync>>,
    {
        let prefix = self.delta_prefix(prompt_hash, session_id);
        let mut stream = self.store.list(Some(&prefix));
        let mut pending = BTreeSet::new();
        let mut list_done = false;
        let mut pages_read = 0usize;

        loop {
            while let Some(&seq) = pending.first() {
                if !spill_seq_ready_to_read(seq, &pending, list_done) {
                    break;
                }
                pending.remove(&seq);
                let page = self
                    .read_delta_graph_page(prompt_hash, session_id, seq)
                    .await?;
                pages_read += 1;
                if visit(page)
                    .map_err(|source| SessionGraphPersistenceError::Visitor { source })?
                    .is_break()
                {
                    return Ok(pages_read);
                }
            }

            if list_done {
                break;
            }

            match next_delta_seq_from_list(&mut stream).await? {
                Some(seq) => {
                    pending.insert(seq);
                }
                None => list_done = true,
            }
        }

        Ok(pages_read)
    }

    async fn load_graph_pages_sorted(
        &self,
        prompt_hash: &str,
        session_id: &str,
    ) -> Result<Vec<GraphPageDelta>, SessionGraphPersistenceError> {
        let seqs = self.list_delta_seqs(prompt_hash, session_id).await?;
        if seqs.is_empty() {
            return Ok(Vec::new());
        }
        let concurrency = delta_read_concurrency();
        let ph = prompt_hash.to_string();
        let sid = session_id.to_string();
        let mut pages: Vec<GraphPageDelta> = futures_util::stream::iter(seqs)
            .map(|seq| {
                let persistence = self.clone();
                let ph = ph.clone();
                let sid = sid.clone();
                async move { persistence.read_delta_graph_page(&ph, &sid, seq).await }
            })
            .buffer_unordered(concurrency)
            .try_collect()
            .await?;
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
    ) -> Result<(), SessionGraphPersistenceError> {
        let pages = self.read_graph_pages(prompt_hash, session_id).await?;
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
    ) -> Result<(), SessionGraphPersistenceError> {
        let snapshot_key = self
            .sessions_root
            .clone()
            .join(prompt_hash)
            .join(session_id)
            .join("snapshots")
            .join(format!("{through_seq:020}.bin"));

        let mut merged: indexmap::IndexMap<&plasm_core::Ref, &plasm_runtime::CachedEntity> =
            indexmap::IndexMap::new();
        for page in pages {
            for row in &page.entities {
                merged.insert(&row.reference, row);
            }
        }
        for r in cache.all_references() {
            if let Some(entity) = cache.get(r) {
                merged.insert(r, entity);
            }
        }

        let upload = self.store.put_multipart(&snapshot_key).await?;
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
            serde_json::to_writer(&mut scratch, v)?;
            writer.write(&scratch);
        }
        writer.write(b"]");
        writer.finish().await?;

        let manifest = SnapshotManifest {
            schema_version: GRAPH_PAGE_DELTA_SCHEMA_VERSION,
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
        let bytes = serde_json::to_vec(&manifest)?;
        self.store.put(&manifest_key, bytes.into()).await?;
        let _ = content_type;
        Ok(())
    }
}

pub fn init_from_env() -> Result<Option<Arc<SessionGraphPersistence>>, SessionGraphPersistenceError>
{
    let url_raw = match std::env::var("PLASM_GRAPH_CACHE_URL") {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return Ok(None),
    };
    let url = url::Url::parse(&url_raw).map_err(SessionGraphPersistenceError::InvalidUrl)?;
    let (boxed, prefix) = object_store::parse_url_opts(&url, std::env::vars())
        .map_err(SessionGraphPersistenceError::ObjectStoreInitialization)?;
    let store: Arc<dyn ObjectStore> = Arc::from(boxed);
    Ok(Some(Arc::new(SessionGraphPersistence::new(store, prefix))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn membership_evidence_survives_snapshot_compaction() {
        use plasm_core::Ref;
        use plasm_runtime::CachedEntity;
        let store: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let persistence = SessionGraphPersistence::new(store.clone(), StorePath::from("proof"));
        let identity = |id: &str, owner: &str| {
            Ref::compound(
                "Parent",
                [("id".into(), id.into()), ("owner".into(), owner.into())].into(),
            )
        };
        let mut old = CachedEntity::new(identity("same,owner=first", "second"), 0);
        old.relations.insert("children".into(), {
            use plasm_core::collection_codec::{
                CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
            };
            let references = vec![Ref::new("Child", "a"), Ref::new("Child", "a")];
            let observation = Observation::ExactOutput {
                decoded: references.len(),
            };
            plasm_core::row_contract::RelationMembership::from_record(
                RecordingCodec::new()
                    .record(
                        CollectionIdentity::for_untyped_observation(&"relation_fixture").unwrap(),
                        references,
                        observation,
                    )
                    .unwrap(),
            )
        });
        let mut retained = old.clone();
        retained.reference = identity("same", "first,owner=second");
        assert_eq!(old.reference.to_string(), retained.reference.to_string());
        assert_ne!(old.reference, retained.reference);
        let pages = vec![GraphPageDelta {
            page_index: 0,
            entity_type: "Parent".into(),
            schema_version: GRAPH_PAGE_DELTA_SCHEMA_VERSION,
            entities: vec![old.clone(), retained.clone()],
        }];
        let mut current = old;
        current.relations.insert("children".into(), {
            use plasm_core::collection_codec::{
                CollectionCodec, CollectionIdentity, Observation, RecordingCodec,
            };
            let references = vec![];
            let observation = Observation::ExactOutput {
                decoded: references.len(),
            };
            plasm_core::row_contract::RelationMembership::from_record(
                RecordingCodec::new()
                    .record(
                        CollectionIdentity::for_untyped_observation(&"relation_fixture").unwrap(),
                        references,
                        observation,
                    )
                    .unwrap(),
            )
        });
        let mut cache = GraphCache::new();
        cache.insert(current.clone()).unwrap();
        persistence
            .write_snapshot_merged("hash", "session", 1, "application/json", &cache, &pages)
            .await
            .unwrap();
        let manifest_key = persistence
            .sessions_root
            .clone()
            .join("hash")
            .join("session")
            .join("manifest.json");
        let manifest: serde_json::Value = serde_json::from_slice(
            &store
                .get(&manifest_key)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["schema_version"], GRAPH_PAGE_DELTA_SCHEMA_VERSION);
        let key = StorePath::from(manifest["snapshot_key"].as_str().unwrap());
        let rows: Vec<CachedEntity> =
            serde_json::from_slice(&store.get(&key).await.unwrap().bytes().await.unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .find(|row| row.reference == current.reference)
                .unwrap()
                .relations,
            current.relations
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.reference == retained.reference)
                .unwrap()
                .relations,
            retained.relations
        );
    }

    #[test]
    fn parse_graph_page_current_schema_body() {
        let body = serde_json::json!({
            "kind": "graph_page",
            "schema_version": GRAPH_PAGE_DELTA_SCHEMA_VERSION,
            "entity_type": "Berry",
            "page_index": 3,
            "entities": [plasm_runtime::CachedEntity::new(plasm_core::Ref::new("Berry", "1"), 0)]
        });
        let page = validate_graph_page_delta(&body).expect("page");
        assert_eq!(page.page_index, 3);
        assert_eq!(page.entity_type, "Berry");
        assert_eq!(page.schema_version, GRAPH_PAGE_DELTA_SCHEMA_VERSION);
    }

    #[test]
    fn decode_framed_delta_slices_body_without_copy() {
        let body = br#"{"kind":"graph_page"}"#;
        let meta = serde_json::to_vec(&ArtifactPayloadMetadata::json_default()).unwrap();
        let mut framed = Vec::new();
        framed.extend_from_slice(&(meta.len() as u32).to_be_bytes());
        framed.extend_from_slice(&meta);
        framed.extend_from_slice(body);
        let payload = decode_framed_delta(Bytes::from(framed)).expect("decode");
        assert_eq!(&payload.bytes[..], body);
        assert_eq!(
            payload.metadata.schema_version,
            crate::run_artifacts::RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION
        );
    }

    #[test]
    fn parse_graph_page_rejects_stale_schema_version() {
        let body = serde_json::json!({
            "kind": "graph_page",
            "schema_version": 1,
            "entity_type": "Berry",
            "page_index": 0,
            "entities": []
        });
        let err = validate_graph_page_delta(&body).unwrap_err();
        assert!(matches!(
            err,
            SessionGraphPersistenceError::GraphPageSchemaVersion {
                expected: GRAPH_PAGE_DELTA_SCHEMA_VERSION,
                actual: 1,
            }
        ));
    }

    #[test]
    fn spill_seq_ready_to_read_waits_for_successor_or_list_done() {
        let mut pending = BTreeSet::from([2]);
        assert!(!spill_seq_ready_to_read(2, &pending, false));
        pending.insert(3);
        assert!(spill_seq_ready_to_read(2, &pending, false));
        assert!(spill_seq_ready_to_read(2, &BTreeSet::from([2]), true));
    }

    #[tokio::test]
    async fn visit_reads_shuffled_delta_list_in_seq_order() {
        use std::fmt;
        use std::sync::Arc;

        use async_trait::async_trait;
        use futures_util::stream::{self, BoxStream, StreamExt, TryStreamExt};
        use object_store::path::Path as StorePath;
        use object_store::{
            CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta,
            ObjectStore, PutMultipartOptions, PutOptions, PutPayload, PutResult, RenameOptions,
        };

        #[derive(Debug)]
        struct ShuffledListStore {
            inner: Arc<dyn ObjectStore>,
        }

        impl fmt::Display for ShuffledListStore {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "ShuffledListStore({})", self.inner)
            }
        }

        #[async_trait]
        impl ObjectStore for ShuffledListStore {
            async fn put_opts(
                &self,
                location: &StorePath,
                payload: PutPayload,
                opts: PutOptions,
            ) -> object_store::Result<PutResult> {
                self.inner.put_opts(location, payload, opts).await
            }

            async fn put_multipart_opts(
                &self,
                location: &StorePath,
                opts: PutMultipartOptions,
            ) -> object_store::Result<Box<dyn MultipartUpload>> {
                self.inner.put_multipart_opts(location, opts).await
            }

            async fn get_opts(
                &self,
                location: &StorePath,
                options: GetOptions,
            ) -> object_store::Result<GetResult> {
                self.inner.get_opts(location, options).await
            }

            fn delete_stream(
                &self,
                locations: BoxStream<'static, object_store::Result<StorePath>>,
            ) -> BoxStream<'static, object_store::Result<StorePath>> {
                self.inner.delete_stream(locations)
            }

            fn list(
                &self,
                prefix: Option<&StorePath>,
            ) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
                let inner = Arc::clone(&self.inner);
                let prefix = prefix.cloned();
                stream::once(
                    async move { inner.list(prefix.as_ref()).try_collect::<Vec<_>>().await },
                )
                .flat_map(|result| match result {
                    Ok(mut entries) => {
                        if entries.len() >= 2 {
                            entries.reverse();
                        }
                        stream::iter(entries.into_iter().map(Ok)).boxed()
                    }
                    Err(e) => stream::once(async move { Err(e) }).boxed(),
                })
                .boxed()
            }

            async fn list_with_delimiter(
                &self,
                prefix: Option<&StorePath>,
            ) -> object_store::Result<ListResult> {
                self.inner.list_with_delimiter(prefix).await
            }

            async fn copy_opts(
                &self,
                from: &StorePath,
                to: &StorePath,
                options: CopyOptions,
            ) -> object_store::Result<()> {
                self.inner.copy_opts(from, to, options).await
            }

            async fn rename_opts(
                &self,
                from: &StorePath,
                to: &StorePath,
                options: RenameOptions,
            ) -> object_store::Result<()> {
                self.inner.rename_opts(from, to, options).await
            }
        }

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let store_root = std::env::temp_dir().join(format!("plasm-shuffled-list-test-{nonce}"));
        std::fs::create_dir_all(&store_root).expect("mkdir");
        let url = url::Url::from_directory_path(&store_root).expect("file url");
        let (inner, prefix) =
            object_store::parse_url_opts(&url, std::env::vars()).expect("object store");
        let inner: Arc<dyn ObjectStore> = Arc::from(inner);
        let store: Arc<dyn ObjectStore> = Arc::new(ShuffledListStore { inner });
        let persistence = SessionGraphPersistence::new(store, prefix);

        let prompt_hash = "ph_shuffled";
        let session_id = "sid_shuffled";
        for (seq, page_index) in [(1u64, 0usize), (2, 1), (3, 2)] {
            let body = format!(
                r#"{{"kind":"graph_page","schema_version":{GRAPH_PAGE_DELTA_SCHEMA_VERSION},"entity_type":"Berry","page_index":{page_index},"entities":[]}}"#
            );
            let payload = ArtifactPayload {
                metadata: ArtifactPayloadMetadata::json_default(),
                bytes: Bytes::from(body.into_bytes()),
            };
            persistence
                .append_delta(prompt_hash, session_id, seq, &payload)
                .await
                .expect("append delta");
        }

        let mut visited = Vec::new();
        persistence
            .visit_graph_pages_in_seq_order(prompt_hash, session_id, |page| {
                visited.push(page.page_index);
                Ok(ControlFlow::Continue(()))
            })
            .await
            .expect("visit");
        assert_eq!(visited, vec![0, 1, 2]);
    }
}
