use super::backend::RunArtifactBackend;
use super::types::{RunArtifactError, RunArtifactId};
use async_trait::async_trait;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Default)]
struct MemoryRunArtifactState {
    blobs: HashMap<(String, String, RunArtifactId), Vec<u8>>,
    by_resource_index: HashMap<(String, String, u64), RunArtifactId>,
    plan_blobs: HashMap<(String, String, Uuid), Vec<u8>>,
    plan_by_index: HashMap<(String, String, u64), Uuid>,
    evidence_blobs: HashMap<(String, String, RunArtifactId), Vec<u8>>,
    evidence_by_head: HashMap<(String, String, String), Vec<u8>>,
    evidence_run_heads: HashMap<(String, String, RunArtifactId), String>,
}

#[derive(Debug, Default)]
pub(crate) struct MemoryRunArtifactBackend {
    inner: std::sync::RwLock<MemoryRunArtifactState>,
}

#[async_trait]
impl RunArtifactBackend for MemoryRunArtifactBackend {
    async fn insert_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        encoded: Vec<u8>,
    ) -> Result<usize, RunArtifactError> {
        let n = encoded.len();
        let mut g = self.inner.write().expect("run artifact mutex poisoned");
        g.blobs.insert(
            (prompt_hash.to_string(), session_id.to_string(), run_id),
            encoded,
        );
        Ok(n)
    }

    async fn get_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>> {
        let g = self.inner.read().ok()?;
        g.blobs
            .get(&(prompt_hash.to_string(), session_id.to_string(), run_id))
            .cloned()
    }

    async fn put_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
        run_id: RunArtifactId,
    ) -> Result<(), RunArtifactError> {
        let mut g = self.inner.write().expect("run artifact mutex poisoned");
        g.by_resource_index.insert(
            (
                prompt_hash.to_string(),
                session_id.to_string(),
                resource_index,
            ),
            run_id,
        );
        Ok(())
    }

    async fn get_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
    ) -> Option<RunArtifactId> {
        let g = self.inner.read().ok()?;
        g.by_resource_index
            .get(&(
                prompt_hash.to_string(),
                session_id.to_string(),
                resource_index,
            ))
            .copied()
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
        let mut g = self.inner.write().expect("run artifact mutex poisoned");
        g.plan_blobs.insert(
            (prompt_hash.to_string(), session_id.to_string(), plan_id),
            encoded,
        );
        g.plan_by_index.insert(
            (prompt_hash.to_string(), session_id.to_string(), plan_index),
            plan_id,
        );
        Ok(n)
    }

    async fn get_plan_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
    ) -> Option<Vec<u8>> {
        let g = self.inner.read().ok()?;
        g.plan_blobs
            .get(&(prompt_hash.to_string(), session_id.to_string(), plan_id))
            .cloned()
    }

    async fn get_plan_id_for_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_index: u64,
    ) -> Option<Uuid> {
        let g = self.inner.read().ok()?;
        g.plan_by_index
            .get(&(prompt_hash.to_string(), session_id.to_string(), plan_index))
            .copied()
    }

    async fn insert_evidence_sidecar(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        encoded: &[u8],
    ) -> Result<usize, RunArtifactError> {
        let n = encoded.len();
        let mut g = self.inner.write().expect("run artifact mutex poisoned");
        g.evidence_blobs.insert(
            (prompt_hash.to_string(), session_id.to_string(), run_id),
            encoded.to_vec(),
        );
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
        let mut g = self.inner.write().expect("run artifact mutex poisoned");
        g.evidence_by_head.insert(
            (
                prompt_hash.to_string(),
                session_id.to_string(),
                head_hex.to_string(),
            ),
            encoded.to_vec(),
        );
        Ok(n)
    }

    async fn put_evidence_run_head_pointer(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        head_hex: &str,
    ) -> Result<(), RunArtifactError> {
        let mut g = self.inner.write().expect("run artifact mutex poisoned");
        g.evidence_run_heads.insert(
            (prompt_hash.to_string(), session_id.to_string(), run_id),
            head_hex.to_string(),
        );
        Ok(())
    }

    async fn get_evidence_sidecar(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>> {
        let g = self.inner.read().ok()?;
        let key = (prompt_hash.to_string(), session_id.to_string(), run_id);
        if let Some(head) = g.evidence_run_heads.get(&key) {
            return g
                .evidence_by_head
                .get(&(
                    prompt_hash.to_string(),
                    session_id.to_string(),
                    head.clone(),
                ))
                .cloned();
        }
        g.evidence_blobs.get(&key).cloned()
    }
}
