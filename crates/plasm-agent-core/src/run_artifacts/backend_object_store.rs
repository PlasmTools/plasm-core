use super::backend::RunArtifactBackend;
use super::evidence_sidecar::{
    evidence_head_object_key, evidence_object_key, evidence_run_head_pointer_key,
};
use super::keys::{
    artifact_object_key, code_plan_index_pointer_key, code_plan_object_key,
    resource_index_pointer_key,
};
use super::types::{RunArtifactError, RunArtifactId};
use async_trait::async_trait;
use object_store::{path::Path as StorePath, ObjectStore, ObjectStoreExt};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) struct ObjectStoreRunArtifactBackend {
    pub(crate) store: Arc<dyn ObjectStore>,
    pub(crate) prefix: StorePath,
}

#[async_trait]
impl RunArtifactBackend for ObjectStoreRunArtifactBackend {
    async fn insert_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        encoded: Vec<u8>,
    ) -> Result<usize, RunArtifactError> {
        let n = encoded.len();
        let key = artifact_object_key(&self.prefix, prompt_hash, session_id, run_id);
        self.store
            .put(&key, encoded.into())
            .await
            .map_err(|e| RunArtifactError::ObjectStore(e.to_string()))?;
        Ok(n)
    }

    async fn get_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>> {
        let key = artifact_object_key(&self.prefix, prompt_hash, session_id, run_id);
        let res = self.store.get(&key).await.ok()?;
        res.bytes().await.ok().map(|b| b.to_vec())
    }

    async fn put_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
        run_id: RunArtifactId,
    ) -> Result<(), RunArtifactError> {
        let key = resource_index_pointer_key(&self.prefix, prompt_hash, session_id, resource_index);
        let body = run_id.to_wire();
        self.store
            .put(&key, body.into_bytes().into())
            .await
            .map_err(|e| RunArtifactError::ObjectStore(e.to_string()))?;
        Ok(())
    }

    async fn get_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
    ) -> Option<RunArtifactId> {
        let key = resource_index_pointer_key(&self.prefix, prompt_hash, session_id, resource_index);
        let res = self.store.get(&key).await.ok()?;
        let bytes = res.bytes().await.ok()?;
        let s = std::str::from_utf8(bytes.as_ref()).ok()?;
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
        let key = code_plan_object_key(&self.prefix, prompt_hash, session_id, plan_id);
        self.store
            .put(&key, encoded.into())
            .await
            .map_err(|e| RunArtifactError::ObjectStore(e.to_string()))?;
        let idx = code_plan_index_pointer_key(&self.prefix, prompt_hash, session_id, plan_index);
        self.store
            .put(
                &idx,
                plan_id.as_hyphenated().to_string().into_bytes().into(),
            )
            .await
            .map_err(|e| RunArtifactError::ObjectStore(e.to_string()))?;
        Ok(n)
    }

    async fn get_plan_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
    ) -> Option<Vec<u8>> {
        let key = code_plan_object_key(&self.prefix, prompt_hash, session_id, plan_id);
        let res = self.store.get(&key).await.ok()?;
        res.bytes().await.ok().map(|b| b.to_vec())
    }

    async fn get_plan_id_for_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_index: u64,
    ) -> Option<Uuid> {
        let key = code_plan_index_pointer_key(&self.prefix, prompt_hash, session_id, plan_index);
        let res = self.store.get(&key).await.ok()?;
        let bytes = res.bytes().await.ok()?;
        let s = std::str::from_utf8(bytes.as_ref()).ok()?;
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
        let key = evidence_object_key(&self.prefix, prompt_hash, session_id, run_id);
        self.store
            .put(&key, encoded.to_vec().into())
            .await
            .map_err(|e| RunArtifactError::ObjectStore(e.to_string()))?;
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
        let key = evidence_head_object_key(&self.prefix, prompt_hash, session_id, head_hex);
        self.store
            .put(&key, encoded.to_vec().into())
            .await
            .map_err(|e| RunArtifactError::ObjectStore(e.to_string()))?;
        Ok(n)
    }

    async fn put_evidence_run_head_pointer(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        head_hex: &str,
    ) -> Result<(), RunArtifactError> {
        let key = evidence_run_head_pointer_key(&self.prefix, prompt_hash, session_id, run_id);
        self.store
            .put(&key, head_hex.as_bytes().to_vec().into())
            .await
            .map_err(|e| RunArtifactError::ObjectStore(e.to_string()))?;
        Ok(())
    }

    async fn get_evidence_sidecar(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>> {
        let ptr_key = evidence_run_head_pointer_key(&self.prefix, prompt_hash, session_id, run_id);
        if let Ok(res) = self.store.get(&ptr_key).await {
            if let Ok(bytes) = res.bytes().await {
                if let Ok(head) = std::str::from_utf8(bytes.as_ref()) {
                    let head_key = evidence_head_object_key(
                        &self.prefix,
                        prompt_hash,
                        session_id,
                        head.trim(),
                    );
                    if let Ok(res) = self.store.get(&head_key).await {
                        if let Ok(b) = res.bytes().await {
                            return Some(b.to_vec());
                        }
                    }
                }
            }
        }
        let key = evidence_object_key(&self.prefix, prompt_hash, session_id, run_id);
        let res = self.store.get(&key).await.ok()?;
        res.bytes().await.ok().map(|b| b.to_vec())
    }
}
