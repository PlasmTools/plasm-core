//! Point-in-time snapshots of execute runs for `GET /execute/.../artifacts/:run_id` and MCP `resources/read`.
//!
//! Storage backends:
//! - **In-memory** (default): [`RunArtifactStore::memory`].
//! - **Local directory** (OSS/self-host): set **`PLASM_RUN_ARTIFACTS_DIR`**; stores blobs and short-URI
//!   index files under a stable layout (see `FsRunArtifactBackend`).
//! - **Object store** (hosted/SaaS): set **`PLASM_RUN_ARTIFACTS_URL`** to an [`object_store`] URL (e.g.
//!   `s3://bucket/prefix`, `file:///path/to/dir` as advanced use).  
//!   **Precedence:** if **`PLASM_RUN_ARTIFACTS_URL`** is set, the object store backend is used and
//!   `PLASM_RUN_ARTIFACTS_DIR` is **ignored** for selection. If only `PLASM_RUN_ARTIFACTS_DIR` is set, the
//!   local filesystem backend is used. If neither is set, in-memory.
//!   Time-based GC (object store only) uses **`PLASM_RUN_ARTIFACTS_RETENTION_SECS`** and
//!   **`PLASM_RUN_ARTIFACTS_GC_INTERVAL_SECS`**.

mod backend;
mod backend_fs;
mod backend_memory;
mod backend_object_store;
mod evidence_sidecar;
mod gc;
mod keys;
mod persist;
mod types;
mod uri;

#[cfg(test)]
mod tests;

pub use persist::{
    mint_run_artifact_id_for_session, persist_execute_run, PersistExecuteRunError,
    PersistExecuteRunInput,
};
pub use types::*;

use backend::RunArtifactBackend;
use backend_fs::FsRunArtifactBackend;
use backend_memory::MemoryRunArtifactBackend;
use backend_object_store::ObjectStoreRunArtifactBackend;
use evidence_sidecar::EvidenceSidecarIndex;
use gc::{gc_interval_from_env, retention_from_env, spawn_run_artifact_gc_task};
use keys::{decode_payload, encode_payload};
use object_store::ObjectStore;
use plasm_core::expr_parser::ParsedExpr;
use plasm_runtime::ExecutionResult;
use std::path::PathBuf;
use std::sync::Arc;
pub use uri::{
    artifact_http_path, code_plan_handle, code_plan_http_path, logical_uuid_from_uri_segment,
    parse_code_plan_handle, parse_plasm_execute_plan_uri, parse_plasm_execute_run_uri,
    parse_plasm_session_short_plan_uri, parse_plasm_session_short_resource_uri,
    parse_plasm_short_resource_uri, plasm_code_plan_resource_uri, plasm_run_resource_uri,
    plasm_session_short_plan_uri, plasm_session_short_resource_uri, plasm_short_code_plan_uri,
    plasm_short_resource_uri, plasm_short_resource_uri_logical, strip_plasm_resource_read_source,
    LogicalSessionUriSegment,
};
use uuid::Uuid;

/// Execute run snapshot storage (memory or object store).
#[derive(Clone)]
pub struct RunArtifactStore {
    inner: Arc<dyn RunArtifactBackend>,
    evidence_index: Arc<std::sync::RwLock<EvidenceSidecarIndex>>,
}

impl RunArtifactStore {
    fn new(inner: Arc<dyn RunArtifactBackend>) -> Self {
        Self {
            inner,
            evidence_index: Arc::new(std::sync::RwLock::new(EvidenceSidecarIndex::default())),
        }
    }

    pub fn memory() -> Self {
        Self::new(Arc::new(MemoryRunArtifactBackend::default()))
    }

    pub async fn insert(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        doc: &RunArtifactDocument,
    ) -> Result<usize, RunArtifactError> {
        let bytes = serde_json::to_vec(doc)?;
        self.insert_payload(
            prompt_hash,
            session_id,
            run_id,
            doc.resource_index,
            &ArtifactPayload {
                metadata: ArtifactPayloadMetadata::json_default(),
                bytes: bytes.into(),
            },
        )
        .await
    }

    pub async fn insert_payload(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        resource_index: Option<u64>,
        payload: &ArtifactPayload,
    ) -> Result<usize, RunArtifactError> {
        let encoded = encode_payload(payload)?;
        let n = self
            .inner
            .insert_encoded(prompt_hash, session_id, run_id, encoded)
            .await?;
        if let Some(idx) = resource_index {
            self.inner
                .put_run_id_for_resource_index(prompt_hash, session_id, idx, run_id)
                .await?;
        }
        Ok(n)
    }

    pub async fn get_payload(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<ArtifactPayload> {
        self.get_payload_result(prompt_hash, session_id, run_id)
            .await
            .ok()
            .flatten()
    }

    pub async fn get_payload_result(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Result<Option<ArtifactPayload>, RunArtifactError> {
        let encoded = self
            .inner
            .get_encoded(prompt_hash, session_id, run_id)
            .await;
        match encoded {
            Some(bytes) => decode_payload(&bytes).map(Some),
            None => Ok(None),
        }
    }

    pub async fn get(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>> {
        let payload = self.get_payload(prompt_hash, session_id, run_id).await?;
        Some(payload.bytes.to_vec())
    }

    pub async fn get_payload_result_by_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
    ) -> Result<Option<ArtifactPayload>, RunArtifactError> {
        let Some(run_id) = self
            .inner
            .get_run_id_for_resource_index(prompt_hash, session_id, resource_index)
            .await
        else {
            return Ok(None);
        };
        self.get_payload_result(prompt_hash, session_id, run_id)
            .await
    }

    /// Resolve canonical `run_id` for a short-URI resource index (archive / object-store mapping).
    pub async fn resolve_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
    ) -> Option<RunArtifactId> {
        self.inner
            .get_run_id_for_resource_index(prompt_hash, session_id, resource_index)
            .await
    }

    pub async fn insert_code_plan(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
        plan_index: u64,
        doc: &CodePlanArchiveDocument,
    ) -> Result<CodePlanArchiveHandle, RunArtifactError> {
        let bytes = serde_json::to_vec(doc)?;
        let payload = ArtifactPayload {
            metadata: ArtifactPayloadMetadata::json_default(),
            bytes: bytes.into(),
        };
        let encoded = encode_payload(&payload)?;
        let n = self
            .inner
            .insert_plan_encoded(prompt_hash, session_id, plan_id, plan_index, encoded)
            .await?;
        Ok(CodePlanArchiveHandle {
            plan_id,
            plan_index,
            plan_handle: code_plan_handle(plan_index),
            plasm_uri: plasm_short_code_plan_uri(plan_index),
            canonical_plasm_uri: plasm_code_plan_resource_uri(prompt_hash, session_id, &plan_id),
            http_path: code_plan_http_path(prompt_hash, session_id, &plan_id),
            payload_len: n,
            plan_hash: doc.plan_hash.clone(),
        })
    }

    pub async fn get_code_plan_payload_result(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
    ) -> Result<Option<ArtifactPayload>, RunArtifactError> {
        let encoded = self
            .inner
            .get_plan_encoded(prompt_hash, session_id, plan_id)
            .await;
        match encoded {
            Some(bytes) => decode_payload(&bytes).map(Some),
            None => Ok(None),
        }
    }

    pub async fn get_code_plan_payload_result_by_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_index: u64,
    ) -> Result<Option<ArtifactPayload>, RunArtifactError> {
        let Some(plan_id) = self
            .inner
            .get_plan_id_for_index(prompt_hash, session_id, plan_index)
            .await
        else {
            return Ok(None);
        };
        self.get_code_plan_payload_result(prompt_hash, session_id, plan_id)
            .await
    }

    pub async fn resolve_code_plan_id_for_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_index: u64,
    ) -> Option<Uuid> {
        self.inner
            .get_plan_id_for_index(prompt_hash, session_id, plan_index)
            .await
    }
}

impl Default for RunArtifactStore {
    fn default() -> Self {
        Self::memory()
    }
}

#[cfg(test)]
impl RunArtifactStore {
    fn from_fs_root_for_test(root: PathBuf) -> Self {
        Self::new(Arc::new(FsRunArtifactBackend { root }))
    }
}

/// Policy for default local filesystem run-artifact dir when env vars are unset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunArtifactInitPolicy {
    /// OSS `plasm`: under [`crate::oss_local_state::resolve_local_state_root`] when enabled.
    OssFilesystemDefaults,
    /// Hosted `plasm-mcp-app`: in-memory when URL and dir unset (no `~/.plasm` writes).
    HostedExplicitOnly,
}

/// Build [`RunArtifactStore`] from environment: **object store** (`PLASM_RUN_ARTIFACTS_URL`) if set,
/// else **local directory** (`PLASM_RUN_ARTIFACTS_DIR`) if set, else **in-memory** (see module docs for precedence).
///
/// - **`PLASM_RUN_ARTIFACTS_URL`**: [`object_store::parse_url_opts`] (hosted / multi-replica; wins over `PLASM_RUN_ARTIFACTS_DIR` when set).
/// - **`PLASM_RUN_ARTIFACTS_DIR`**: local directory root (OSS/self-host durable tier when URL unset).
pub fn init_from_env() -> Result<Arc<RunArtifactStore>, String> {
    init_from_env_with_policy(RunArtifactInitPolicy::HostedExplicitOnly)
}

/// Same as [`init_from_env`], with optional OSS default directory `{local_state}/run-artifacts`.
pub fn init_from_env_with_policy(
    policy: RunArtifactInitPolicy,
) -> Result<Arc<RunArtifactStore>, String> {
    if let Ok(url_raw) = std::env::var("PLASM_RUN_ARTIFACTS_URL") {
        if !url_raw.trim().is_empty() {
            let url = url::Url::parse(&url_raw)
                .map_err(|e| format!("PLASM_RUN_ARTIFACTS_URL is not a valid URL: {e}"))?;
            let (boxed, prefix) = object_store::parse_url_opts(&url, std::env::vars())
                .map_err(|e| format!("PLASM_RUN_ARTIFACTS_URL could not open object store: {e}"))?;
            let store: Arc<dyn ObjectStore> = Arc::from(boxed);
            let retention = retention_from_env();
            let interval = gc_interval_from_env();
            let backend = Arc::new(ObjectStoreRunArtifactBackend {
                store: store.clone(),
                prefix: prefix.clone(),
            });
            spawn_run_artifact_gc_task(store, prefix, retention, interval);
            tracing::info!(
                retention_secs = retention.as_secs(),
                gc_interval_secs = interval.as_secs(),
                "run artifacts: object store backend (time-based GC)"
            );
            return Ok(Arc::new(RunArtifactStore::new(backend)));
        }
    }
    if let Ok(dir) = std::env::var("PLASM_RUN_ARTIFACTS_DIR") {
        if !dir.trim().is_empty() {
            let root: PathBuf = dir.trim().to_string().into();
            if let Err(e) = std::fs::create_dir_all(&root) {
                return Err(format!(
                    "PLASM_RUN_ARTIFACTS_DIR: could not create {root:?}: {e}"
                ));
            }
            tracing::info!(path = %root.display(), "run artifacts: local filesystem backend");
            return Ok(Arc::new(RunArtifactStore::new(Arc::new(
                FsRunArtifactBackend { root: root.clone() },
            ))));
        }
    }
    if policy == RunArtifactInitPolicy::OssFilesystemDefaults
        && crate::oss_local_state::oss_local_persistence_enabled()
    {
        if let Some(base) = crate::oss_local_state::resolve_local_state_root() {
            let root = base.join("run-artifacts");
            if let Err(e) = std::fs::create_dir_all(&root) {
                return Err(format!(
                    "OSS default run-artifacts dir: could not create {root:?}: {e}"
                ));
            }
            tracing::info!(
                path = %root.display(),
                "run artifacts: OSS default local filesystem backend (~/.plasm/local/run-artifacts or PLASM_LOCAL_STATE_DIR)"
            );
            return Ok(Arc::new(RunArtifactStore::new(Arc::new(
                FsRunArtifactBackend { root: root.clone() },
            ))));
        }
        tracing::warn!(
            target: "plasm_agent::run_artifacts",
            "OSS default run artifacts requested but HOME/PLASM_LOCAL_STATE_DIR unset; falling back to in-memory store"
        );
    }
    tracing::warn!(
        target: "plasm_agent::run_artifacts",
        "PLASM_RUN_ARTIFACTS_URL and PLASM_RUN_ARTIFACTS_DIR unset: using in-process memory for execute run snapshots; set an object store URL, or PLASM_RUN_ARTIFACTS_DIR for local OSS durable refs"
    );
    Ok(Arc::new(RunArtifactStore::memory()))
}

/// Arguments for [`document_from_run`].
pub struct DocumentFromRun<'a> {
    pub run_id: RunArtifactId,
    pub prompt_hash: &'a str,
    pub session_id: &'a str,
    pub entry_id: &'a str,
    pub principal: Option<String>,
    pub display_lines: Vec<String>,
    pub parsed_preimage: &'a ParsedExpr,
    pub result: &'a ExecutionResult,
    pub resource_index: Option<u64>,
}

pub fn document_from_run(d: DocumentFromRun<'_>) -> RunArtifactDocument {
    let entities: Vec<serde_json::Value> = d
        .result
        .entities
        .iter()
        .map(|e| e.payload_to_json())
        .collect();
    RunArtifactDocument {
        run_id: d.run_id.to_wire(),
        prompt_hash: d.prompt_hash.to_string(),
        session_id: d.session_id.to_string(),
        entry_id: d.entry_id.to_string(),
        resource_index: d.resource_index,
        principal: d.principal,
        parsed_preimage: d.parsed_preimage.clone(),
        display_lines: d.display_lines,
        request_fingerprints: d.result.request_fingerprints.clone(),
        entities,
        source: d.result.source,
        stats: d.result.stats.clone(),
    }
}
