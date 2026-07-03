use super::types::{
    validate_artifact_payload_metadata, ArtifactPayload, ArtifactPayloadMetadata, RunArtifactError,
    RunArtifactId,
};
use axum::body::Bytes;
use object_store::path::Path as StorePath;
use uuid::Uuid;

pub(crate) fn run_artifact_blob_filename(run_id: RunArtifactId) -> String {
    format!("{}.artifact", hex::encode(run_id.as_bytes()))
}

pub(crate) fn artifact_object_key(
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
        .join(run_artifact_blob_filename(run_id))
}

pub(crate) fn resource_index_pointer_key(
    prefix: &StorePath,
    prompt_hash: &str,
    session_id: &str,
    resource_index: u64,
) -> StorePath {
    prefix
        .clone()
        .join("execute")
        .join(prompt_hash)
        .join(session_id)
        .join("_index")
        .join(format!("{resource_index}.run_id"))
}

pub(crate) fn code_plan_object_key(
    prefix: &StorePath,
    prompt_hash: &str,
    session_id: &str,
    plan_id: Uuid,
) -> StorePath {
    prefix
        .clone()
        .join("code-plans")
        .join(prompt_hash)
        .join(session_id)
        .join(format!("{plan_id}.artifact"))
}

pub(crate) fn code_plan_index_pointer_key(
    prefix: &StorePath,
    prompt_hash: &str,
    session_id: &str,
    plan_index: u64,
) -> StorePath {
    prefix
        .clone()
        .join("code-plans")
        .join(prompt_hash)
        .join(session_id)
        .join("_index")
        .join(format!("{plan_index}.plan_id"))
}

const ARTIFACT_MAGIC: &[u8] = b"PLAR1\n";

pub(crate) fn encode_payload(payload: &ArtifactPayload) -> Result<Vec<u8>, RunArtifactError> {
    let meta = serde_json::to_vec(&payload.metadata)?;
    let mut out = Vec::with_capacity(ARTIFACT_MAGIC.len() + 4 + meta.len() + payload.bytes.len());
    out.extend_from_slice(ARTIFACT_MAGIC);
    out.extend_from_slice(&(meta.len() as u32).to_be_bytes());
    out.extend_from_slice(&meta);
    out.extend_from_slice(payload.bytes.as_ref());
    Ok(out)
}

pub(crate) fn decode_payload(encoded: &[u8]) -> Result<ArtifactPayload, RunArtifactError> {
    let header = ARTIFACT_MAGIC.len() + 4;
    if encoded.len() < header || &encoded[..ARTIFACT_MAGIC.len()] != ARTIFACT_MAGIC {
        return Err(RunArtifactError::Decode(
            "invalid artifact framing header".into(),
        ));
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&encoded[ARTIFACT_MAGIC.len()..header]);
    let meta_len = u32::from_be_bytes(len_bytes) as usize;
    if encoded.len() < header + meta_len {
        return Err(RunArtifactError::Decode(
            "invalid artifact framing metadata length".into(),
        ));
    }
    let metadata: ArtifactPayloadMetadata =
        serde_json::from_slice(&encoded[header..header + meta_len])?;
    validate_artifact_payload_metadata(&metadata).map_err(RunArtifactError::Decode)?;
    let bytes = Bytes::copy_from_slice(&encoded[header + meta_len..]);
    Ok(ArtifactPayload { metadata, bytes })
}
