//! Hash-chained evidence bundle sidecars (dedup by chain head, pointer index per run_id).

use super::{RunArtifactError, RunArtifactId, RunArtifactStore};
use object_store::path::Path as StorePath;
use std::collections::{HashMap, HashSet};

pub(crate) struct EvidenceSidecarIndex {
    /// `(prompt_hash, session_id, chain_head_hex)` → serialized bundle (dedup).
    pub bundles_by_head: HashMap<(String, String, String), Vec<u8>>,
    /// `(prompt_hash, session_id, run_id)` → chain head hex.
    pub run_to_head: HashMap<(String, String, RunArtifactId), String>,
    /// Durable backends: head already written for this session.
    pub persisted_heads: HashSet<(String, String, String)>,
}

impl Default for EvidenceSidecarIndex {
    fn default() -> Self {
        Self {
            bundles_by_head: HashMap::new(),
            run_to_head: HashMap::new(),
            persisted_heads: HashSet::new(),
        }
    }
}

pub(crate) fn evidence_head_sidecar_filename(head_hex: &str) -> String {
    format!("{head_hex}.evidence.json")
}

pub(crate) fn evidence_sidecar_filename(run_id: RunArtifactId) -> String {
    format!("{}.evidence.json", run_id.to_wire())
}

pub(crate) fn evidence_head_object_key(
    prefix: &StorePath,
    prompt_hash: &str,
    session_id: &str,
    head_hex: &str,
) -> StorePath {
    prefix
        .clone()
        .join("execute")
        .join(prompt_hash)
        .join(session_id)
        .join("evidence")
        .join("heads")
        .join(evidence_head_sidecar_filename(head_hex))
}

pub(crate) fn evidence_run_head_pointer_key(
    prefix: &StorePath,
    prompt_hash: &str,
    session_id: &str,
    run_id: RunArtifactId,
) -> StorePath {
    prefix
        .clone()
        .join("execute")
        .join(prompt_hash)
        .join(session_id)
        .join("evidence")
        .join("run-heads")
        .join(format!("{}.head", run_id.to_wire()))
}

pub(crate) fn evidence_object_key(
    prefix: &StorePath,
    prompt_hash: &str,
    session_id: &str,
    run_id: RunArtifactId,
) -> StorePath {
    prefix
        .clone()
        .join("execute")
        .join(prompt_hash)
        .join(session_id)
        .join("evidence")
        .join(evidence_sidecar_filename(run_id))
}

impl RunArtifactStore {
    pub fn evidence_http_path(prompt_hash: &str, session_id: &str, run_id: &RunArtifactId) -> String {
        format!(
            "/execute/{prompt_hash}/{session_id}/artifacts/{}/evidence",
            run_id.to_wire()
        )
    }

    pub async fn insert_evidence_bundles(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_ids: &[RunArtifactId],
        bundle: &plasm_evidence::EvidenceBundle,
    ) -> Result<usize, RunArtifactError> {
        if run_ids.is_empty() {
            return Ok(0);
        }
        let head_hex = bundle
            .chain_head()
            .ok_or_else(|| {
                RunArtifactError::Decode("evidence bundle missing chain head".into())
            })?
            .to_hex();
        let bytes = serde_json::to_vec(bundle)?;
        let n = bytes.len();
        let head_key = (
            prompt_hash.to_string(),
            session_id.to_string(),
            head_hex.clone(),
        );
        let persist_key = head_key.clone();
        let should_persist = {
            let mut g = self
                .evidence_index
                .write()
                .expect("evidence sidecar index lock");
            g.bundles_by_head.insert(head_key, bytes.clone());
            for run_id in run_ids {
                g.run_to_head.insert(
                    (
                        prompt_hash.to_string(),
                        session_id.to_string(),
                        *run_id,
                    ),
                    head_hex.clone(),
                );
            }
            !g.persisted_heads.contains(&persist_key)
        };
        if should_persist {
            self.inner
                .insert_evidence_sidecar_by_head(prompt_hash, session_id, &head_hex, &bytes)
                .await?;
            for run_id in run_ids {
                self.inner
                    .put_evidence_run_head_pointer(
                        prompt_hash,
                        session_id,
                        *run_id,
                        &head_hex,
                    )
                    .await?;
            }
            self.evidence_index
                .write()
                .expect("evidence sidecar index lock")
                .persisted_heads
                .insert(persist_key);
        }
        Ok(n)
    }

    pub async fn insert_evidence_bundle(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
        bundle: &plasm_evidence::EvidenceBundle,
    ) -> Result<usize, RunArtifactError> {
        self.insert_evidence_bundles(prompt_hash, session_id, std::slice::from_ref(&run_id), bundle)
            .await
    }

    pub async fn get_evidence_bundle(
        &self,
        prompt_hash: &str,
        session_id: &str,
        run_id: RunArtifactId,
    ) -> Result<Option<plasm_evidence::EvidenceBundle>, RunArtifactError> {
        let key = (
            prompt_hash.to_string(),
            session_id.to_string(),
            run_id,
        );
        if let Some(bytes) = self
            .evidence_index
            .read()
            .ok()
            .and_then(|g| {
                g.run_to_head
                    .get(&key)
                    .and_then(|head| {
                        g.bundles_by_head
                            .get(&(key.0.clone(), key.1.clone(), head.clone()))
                    })
                    .cloned()
            })
        {
            let bundle: plasm_evidence::EvidenceBundle = serde_json::from_slice(&bytes)?;
            return Ok(Some(bundle));
        }
        if let Some(bytes) = self
            .inner
            .get_evidence_sidecar(prompt_hash, session_id, run_id)
            .await
        {
            let bundle: plasm_evidence::EvidenceBundle = serde_json::from_slice(&bytes)?;
            return Ok(Some(bundle));
        }
        Ok(None)
    }
}
