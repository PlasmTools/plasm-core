//! Explicit historical output access. This boundary has no execution engine and
//! cannot dispatch, hydrate, retry, or substitute a current observation.
use super::{
    parse_run_artifact_document_bytes, verify_payload_run_id, RunArtifactError, RunArtifactId,
    RunArtifactStore,
};
use plasm_core::collection_codec::CollectionFault;
use plasm_runtime::{execution::ExecutionCollection, ResultCoverage};

#[derive(Debug, thiserror::Error)]
pub enum CompletedOutputError {
    #[error("recorded output is unavailable in this session; use an available output reference")]
    Unavailable,
    #[error("recorded output belongs to a different session")]
    SessionMismatch,
    #[error("recorded output is incomplete; it cannot supply a complete collection")]
    Incomplete,
    #[error("recorded output has no retained flow evidence; it cannot be reused")]
    MissingFlowEvidence,
    #[error(transparent)]
    Artifact(#[from] RunArtifactError),
    #[error(transparent)]
    Collection(#[from] CollectionFault),
}

/// Recorded data, not an executable producer or a claim about current state.
/// Construction is restricted to validated, session-scoped artifact lookup.
#[derive(Debug, Clone)]
pub struct CompletedOutput {
    id: RunArtifactId,
    entry_id: String,
    entity: String,
    collection: ExecutionCollection,
}

impl CompletedOutput {
    pub fn id(&self) -> RunArtifactId {
        self.id
    }
    pub fn entry_id(&self) -> &str {
        &self.entry_id
    }
    pub fn entity(&self) -> &str {
        &self.entity
    }
    pub fn collection(&self) -> &ExecutionCollection {
        &self.collection
    }
}

impl RunArtifactStore {
    /// Resolve a completed output without executing its producer. The caller
    /// supplies the authenticated execute-session namespace, never a namespace
    /// copied from the artifact body.
    pub async fn completed_output(
        &self,
        prompt_hash: &str,
        session_id: &str,
        id: RunArtifactId,
    ) -> Result<CompletedOutput, CompletedOutputError> {
        let payload = self
            .get_payload_result(prompt_hash, session_id, id)
            .await?
            .ok_or(CompletedOutputError::Unavailable)?;
        verify_payload_run_id(&payload, id)?;
        let document = parse_run_artifact_document_bytes(&payload.bytes)?;
        if document.prompt_hash != prompt_hash || document.session_id != session_id {
            return Err(CompletedOutputError::SessionMismatch);
        }
        let collection = document.recorded_collection()?;
        if collection.coverage() != ResultCoverage::Complete {
            return Err(CompletedOutputError::Incomplete);
        }
        Ok(CompletedOutput {
            id,
            entry_id: document.entry_id,
            entity: document.entity,
            collection,
        })
    }
}
