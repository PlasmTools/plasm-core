use super::backend::RunArtifactBackend;
use super::evidence_sidecar::{evidence_head_sidecar_filename, evidence_sidecar_filename};
use super::keys::run_artifact_blob_filename;
use super::types::{RunArtifactError, RunArtifactId};
use async_trait::async_trait;
use std::path::PathBuf;
use uuid::Uuid;

/// Local filesystem run artifacts: `execute/{prompt_hash}/{session_id}/{hex64}.artifact` and
/// `execute/.../resource-index/{n}.txt` (wire text) for `plasm://r/{n}` resolution.
#[derive(Debug, Clone)]
pub(crate) struct FsRunArtifactBackend {
    pub(crate) root: PathBuf,
}

fn run_artifact_fs_segment(s: &str) -> Result<&str, RunArtifactError> {
    if s.is_empty() || s.contains("..") || s.contains('/') || s.contains('\\') {
        return Err(RunArtifactError::Filesystem(format!(
            "invalid path segment in run artifact key: {s:?}"
        )));
    }
    Ok(s)
}

impl FsRunArtifactBackend {
    fn blob_path(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Result<PathBuf, RunArtifactError> {
        let ph = run_artifact_fs_segment(prompt_hash)?;
        let sid = run_artifact_fs_segment(session_id)?;
        let fname_owned = run_artifact_blob_filename(run_id);
        let fname = run_artifact_fs_segment(&fname_owned)?;
        Ok(self.root.join("execute").join(ph).join(sid).join(fname))
    }

    fn resource_index_path(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
    ) -> Result<PathBuf, RunArtifactError> {
        let ph = run_artifact_fs_segment(prompt_hash)?;
        let sid = run_artifact_fs_segment(session_id)?;
        Ok(self
            .root
            .join("execute")
            .join(ph)
            .join(sid)
            .join("resource-index")
            .join(format!("{resource_index}.txt")))
    }

    fn plan_blob_path(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
    ) -> Result<PathBuf, RunArtifactError> {
        let ph = run_artifact_fs_segment(prompt_hash)?;
        let sid = run_artifact_fs_segment(session_id)?;
        Ok(self
            .root
            .join("code-plans")
            .join(ph)
            .join(sid)
            .join(format!("{plan_id}.artifact")))
    }

    fn plan_index_path(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_index: u64,
    ) -> Result<PathBuf, RunArtifactError> {
        let ph = run_artifact_fs_segment(prompt_hash)?;
        let sid = run_artifact_fs_segment(session_id)?;
        Ok(self
            .root
            .join("code-plans")
            .join(ph)
            .join(sid)
            .join("plan-index")
            .join(format!("{plan_index}.txt")))
    }

    fn evidence_path(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Result<PathBuf, RunArtifactError> {
        let ph = run_artifact_fs_segment(prompt_hash)?;
        let sid = run_artifact_fs_segment(session_id)?;
        let fname_owned = evidence_sidecar_filename(run_id);
        let fname = run_artifact_fs_segment(&fname_owned)?;
        Ok(self
            .root
            .join("execute")
            .join(ph)
            .join(sid)
            .join("evidence")
            .join(fname))
    }

    fn evidence_head_path(
        &self,
        prompt_hash: &str,
        session_id: &str,
        head_hex: &str,
    ) -> Result<PathBuf, RunArtifactError> {
        let ph = run_artifact_fs_segment(prompt_hash)?;
        let sid = run_artifact_fs_segment(session_id)?;
        let fname_owned = evidence_head_sidecar_filename(head_hex);
        let fname = run_artifact_fs_segment(&fname_owned)?;
        Ok(self
            .root
            .join("execute")
            .join(ph)
            .join(sid)
            .join("evidence")
            .join("heads")
            .join(fname))
    }

    fn evidence_run_head_pointer_path(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Result<PathBuf, RunArtifactError> {
        let ph = run_artifact_fs_segment(prompt_hash)?;
        let sid = run_artifact_fs_segment(session_id)?;
        let fname_owned = format!("{}.head", run_id.to_wire());
        let fname = run_artifact_fs_segment(&fname_owned)?;
        Ok(self
            .root
            .join("execute")
            .join(ph)
            .join(sid)
            .join("evidence")
            .join("run-heads")
            .join(fname))
    }
}

#[async_trait]
impl RunArtifactBackend for FsRunArtifactBackend {
    async fn insert_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        encoded: Vec<u8>,
    ) -> Result<usize, RunArtifactError> {
        let n = encoded.len();
        let path = self.blob_path(prompt_hash, session_id, run_id)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        }
        tokio::fs::write(&path, encoded)
            .await
            .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        Ok(n)
    }

    async fn get_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>> {
        let path = self.blob_path(prompt_hash, session_id, run_id).ok()?;
        tokio::fs::read(&path).await.ok()
    }

    async fn put_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
        run_id: RunArtifactId,
    ) -> Result<(), RunArtifactError> {
        let path = self.resource_index_path(prompt_hash, session_id, resource_index)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        }
        let body = run_id.to_wire();
        tokio::fs::write(&path, body)
            .await
            .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        Ok(())
    }

    async fn get_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
    ) -> Option<RunArtifactId> {
        let path = self
            .resource_index_path(prompt_hash, session_id, resource_index)
            .ok()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        let s = std::str::from_utf8(&bytes).ok()?;
        RunArtifactId::from_wire(s.trim())
    }

    async fn insert_plan_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
        plan_index: u64,
        encoded: Vec<u8>,
    ) -> Result<usize, RunArtifactError> {
        let n = encoded.len();
        let path = self.plan_blob_path(prompt_hash, session_id, plan_id)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        }
        tokio::fs::write(&path, encoded)
            .await
            .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        let index_path = self.plan_index_path(prompt_hash, session_id, plan_index)?;
        if let Some(parent) = index_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        }
        tokio::fs::write(&index_path, plan_id.as_hyphenated().to_string())
            .await
            .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        Ok(n)
    }

    async fn get_plan_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
    ) -> Option<Vec<u8>> {
        let path = self.plan_blob_path(prompt_hash, session_id, plan_id).ok()?;
        tokio::fs::read(&path).await.ok()
    }

    async fn get_plan_id_for_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_index: u64,
    ) -> Option<Uuid> {
        let path = self
            .plan_index_path(prompt_hash, session_id, plan_index)
            .ok()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        let s = std::str::from_utf8(&bytes).ok()?;
        Uuid::parse_str(s.trim()).ok()
    }

    async fn insert_evidence_sidecar(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        encoded: &[u8],
    ) -> Result<usize, RunArtifactError> {
        let n = encoded.len();
        let path = self.evidence_path(prompt_hash, session_id, run_id)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        }
        tokio::fs::write(&path, encoded)
            .await
            .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        Ok(n)
    }

    async fn insert_evidence_sidecar_by_head(
        &self,
        prompt_hash: &str,
        session_id: &str,
        head_hex: &str,
        encoded: &[u8],
    ) -> Result<usize, RunArtifactError> {
        let n = encoded.len();
        let path = self.evidence_head_path(prompt_hash, session_id, head_hex)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        }
        tokio::fs::write(&path, encoded)
            .await
            .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        Ok(n)
    }

    async fn put_evidence_run_head_pointer(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        head_hex: &str,
    ) -> Result<(), RunArtifactError> {
        let path = self.evidence_run_head_pointer_path(prompt_hash, session_id, run_id)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        }
        tokio::fs::write(&path, head_hex)
            .await
            .map_err(|e| RunArtifactError::Filesystem(e.to_string()))?;
        Ok(())
    }

    async fn get_evidence_sidecar(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>> {
        if let Ok(ptr) = self.evidence_run_head_pointer_path(prompt_hash, session_id, run_id) {
            if let Ok(head_bytes) = tokio::fs::read(&ptr).await {
                if let Ok(head) = std::str::from_utf8(&head_bytes) {
                    let head = head.trim();
                    if let Ok(path) = self.evidence_head_path(prompt_hash, session_id, head) {
                        if let Ok(bytes) = tokio::fs::read(&path).await {
                            return Some(bytes);
                        }
                    }
                }
            }
        }
        let path = self.evidence_path(prompt_hash, session_id, run_id).ok()?;
        tokio::fs::read(&path).await.ok()
    }
}
