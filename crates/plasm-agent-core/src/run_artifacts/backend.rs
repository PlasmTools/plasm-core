use super::types::*;
use async_trait::async_trait;
use uuid::Uuid;

#[async_trait]
pub trait RunArtifactBackend: Send + Sync {
    async fn insert_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        encoded: Vec<u8>,
    ) -> Result<usize, RunArtifactError>;

    async fn get_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>>;

    /// Persist `resource_index → run_id` under the same session prefix as blob artifacts.
    async fn put_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
        run_id: RunArtifactId,
    ) -> Result<(), RunArtifactError>;

    async fn get_run_id_for_resource_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        resource_index: u64,
    ) -> Option<RunArtifactId>;

    async fn insert_plan_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
        plan_index: u64,
        encoded: Vec<u8>,
    ) -> Result<usize, RunArtifactError>;

    async fn get_plan_encoded(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_id: Uuid,
    ) -> Option<Vec<u8>>;

    async fn get_plan_id_for_index(
        &self,
        prompt_hash: &str,
        session_id: &str,
        plan_index: u64,
    ) -> Option<Uuid>;

    #[allow(dead_code)] // legacy per-run layout; reads still supported via get_evidence_sidecar
    async fn insert_evidence_sidecar(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        encoded: &[u8],
    ) -> Result<usize, RunArtifactError>;

    async fn insert_evidence_sidecar_by_head(
        &self,
        prompt_hash: &str,
        session_id: &str,
        head_hex: &str,
        encoded: &[u8],
    ) -> Result<usize, RunArtifactError>;

    async fn put_evidence_run_head_pointer(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        head_hex: &str,
    ) -> Result<(), RunArtifactError>;

    async fn get_evidence_sidecar(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Option<Vec<u8>>;
}
