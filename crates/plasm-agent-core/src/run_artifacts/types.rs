//! Run snapshot wire types.
//!
//! **Invariant:** [`ParsedExpr`] preimages are minted once at compile/execute and stored in
//! [`RunArtifactDocument::parsed_preimage`]. [`RunArtifactDocument::display_lines`] is human
//! lineage only — never parsed for semantics or digest recovery.

use axum::body::Bytes;
use plasm_core::expr_parser::ParsedExpr;
use plasm_runtime::{ExecutionSource, ExecutionStats};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// ASCII prefix for deterministic run artifact wire ids (`pr` + 64 lowercase hex = full SHA256 digest).
pub const RUN_ARTIFACT_WIRE_PREFIX: &str = "pr";

/// Metadata + JSON body schema for framed execute run snapshots (`PLAR1` envelope).
pub const RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION: u32 = 2;

/// Canonical 32-byte identity for a stored execute run snapshot (content-addressed).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunArtifactId([u8; 32]);

impl RunArtifactId {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// On-the-wire / URL / header form: `RUN_ARTIFACT_WIRE_PREFIX` + 64 lowercase hex nybbles.
    #[must_use]
    pub fn to_wire(&self) -> String {
        format!("{}{}", RUN_ARTIFACT_WIRE_PREFIX, hex::encode(self.0))
    }

    /// Parse strict wire form (prefix + 64 hex). UUID-shaped strings are rejected.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        let rest = s.strip_prefix(RUN_ARTIFACT_WIRE_PREFIX)?;
        if rest.len() != 64 {
            return None;
        }
        let bytes = rest.as_bytes();
        if !bytes.iter().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = hex_nibble(pair[0])?;
            let lo = hex_nibble(pair[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }

    /// Deterministic digest from canonical plan bundle (see product docs / AGENTS).
    pub fn from_plan_bundle_inputs(
        catalog_cgs_hash: &str,
        domain_revision: u32,
        entry_id: &str,
        source_line: &str,
        parsed: &ParsedExpr,
        request_fingerprints: &[String],
    ) -> Result<Self, plasm_evidence::CanonicalError> {
        let digest = plasm_evidence::compute_run_bundle_digest(
            catalog_cgs_hash,
            domain_revision,
            entry_id,
            source_line,
            parsed,
            request_fingerprints,
        )?;
        Ok(Self(*digest.as_bytes()))
    }
}

impl fmt::Debug for RunArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RunArtifactId")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl Serialize for RunArtifactId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_wire())
    }
}

/// Inbound path/header segment: strict `pr` + 64 hex only (full cutover, no UUID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunArtifactWire(pub RunArtifactId);

impl FromStr for RunArtifactWire {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RunArtifactId::from_wire(s.trim())
            .map(RunArtifactWire)
            .ok_or_else(|| {
                format!(
                    "invalid `run_id`: expected `{RUN_ARTIFACT_WIRE_PREFIX}` + 64 hex digits (got {:?})",
                    s.chars().take(80).collect::<String>()
                )
            })
    }
}

/// Handle for a stored run snapshot (HTTP path + MCP resource URI).
#[derive(Debug, Clone)]
pub struct RunArtifactHandle {
    pub run_id: RunArtifactId,
    /// Monotonic `plasm://r/{n}` index for this execute session (matches snapshot JSON).
    pub resource_index: u64,
    /// LLM-facing short URI (`plasm://r/{n}`), valid with MCP `resources/read` while the same execute session is bound.
    pub plasm_uri: String,
    /// Canonical long URI (`plasm://execute/.../run/{run_id}`) for logs and HTTP-adjacent tools.
    pub canonical_plasm_uri: String,
    pub http_path: String,
    pub payload_len: usize,
    pub request_fingerprints: Vec<String>,
}

/// Handle for a stored serialized Plasm program plan (permanent plan archive, not run snapshot GC).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodePlanArchiveHandle {
    pub plan_id: Uuid,
    pub plan_index: u64,
    pub plan_handle: String,
    pub plasm_uri: String,
    pub canonical_plasm_uri: String,
    pub http_path: String,
    pub payload_len: usize,
    pub plan_hash: String,
}

/// Payload metadata for cache deltas / run artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactPayloadMetadata {
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<String>,
    pub schema_version: u32,
    pub producer: String,
}

impl ArtifactPayloadMetadata {
    pub fn json_default() -> Self {
        Self {
            content_type: "application/json".into(),
            content_encoding: None,
            schema_version: RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION,
            producer: "plasm".into(),
        }
    }
}

/// Reject stale framed artifact metadata (exact schema cutover).
pub fn validate_artifact_payload_metadata(m: &ArtifactPayloadMetadata) -> Result<(), String> {
    if m.schema_version != RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION {
        return Err(format!(
            "artifact metadata schema_version must be {RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION} (got {})",
            m.schema_version
        ));
    }
    if m.content_type.trim().is_empty() {
        return Err("artifact metadata content_type missing".into());
    }
    if m.producer.trim().is_empty() {
        return Err("artifact metadata producer missing".into());
    }
    Ok(())
}

/// Reject partial or legacy run snapshot JSON bodies (in-process / post-decode).
pub fn validate_run_artifact_document(doc: &RunArtifactDocument) -> Result<(), String> {
    if doc.run_id.trim().is_empty() {
        return Err("run artifact run_id missing".into());
    }
    if RunArtifactId::from_wire(doc.run_id.trim()).is_none() {
        return Err(format!(
            "run artifact run_id must be `{RUN_ARTIFACT_WIRE_PREFIX}` + 64 hex digits"
        ));
    }
    if doc.prompt_hash.trim().is_empty() {
        return Err("run artifact prompt_hash missing".into());
    }
    if doc.session_id.trim().is_empty() {
        return Err("run artifact session_id missing".into());
    }
    if doc.entry_id.trim().is_empty() {
        return Err("run artifact entry_id missing".into());
    }
    Ok(())
}

/// Wire JSON ingress gate — requires `parsed_preimage` (schema v2 cutover) before decode.
pub fn validate_run_artifact_document_json(v: &serde_json::Value) -> Result<(), String> {
    parse_run_artifact_document_value(v.clone()).map(|_| ())
}

/// Parse JSON run snapshot bytes after wire validation (single ingress path).
pub fn parse_run_artifact_document_bytes(
    bytes: &[u8],
) -> Result<RunArtifactDocument, RunArtifactError> {
    let v: serde_json::Value = serde_json::from_slice(bytes)?;
    parse_run_artifact_document_value(v).map_err(RunArtifactError::Decode)
}

fn parse_run_artifact_document_value(v: serde_json::Value) -> Result<RunArtifactDocument, String> {
    if !v.is_object() {
        return Err("run artifact must be object".into());
    }
    if v.get("parsed_preimage").is_none() {
        return Err("run artifact parsed_preimage missing (schema v2 cutover)".into());
    }
    let doc: RunArtifactDocument =
        serde_json::from_value(v).map_err(|e| format!("run artifact JSON: {e}"))?;
    validate_run_artifact_document(&doc)?;
    Ok(doc)
}

/// Opaque artifact bytes plus explicit metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPayload {
    pub metadata: ArtifactPayloadMetadata,
    pub bytes: Bytes,
}

/// JSON document returned by artifact GET and MCP `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunArtifactDocument {
    pub run_id: String,
    pub prompt_hash: String,
    pub session_id: String,
    pub entry_id: String,
    /// Monotonic per `(prompt_hash, session_id)` execute session; drives `plasm://r/{n}` and archive index lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// Typed digest/evidence preimage; required on schema v2 artifacts.
    pub parsed_preimage: ParsedExpr,
    /// Human-readable lineage only (never re-parsed).
    #[serde(alias = "expressions")]
    pub display_lines: Vec<String>,
    pub request_fingerprints: Vec<String>,
    pub entities: Vec<serde_json::Value>,
    pub source: ExecutionSource,
    pub stats: ExecutionStats,
}

/// Agent-facing run snapshot projection (slim read path; canonical doc retains evidence fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunArtifactAgentView {
    pub run_id: String,
    pub entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_index: Option<u64>,
    pub request_fingerprints: Vec<String>,
    pub entities: Vec<serde_json::Value>,
}

impl RunArtifactDocument {
    pub fn agent_view(&self) -> RunArtifactAgentView {
        RunArtifactAgentView {
            run_id: self.run_id.clone(),
            entry_id: self.entry_id.clone(),
            resource_index: self.resource_index,
            request_fingerprints: self.request_fingerprints.clone(),
            entities: self.entities.clone(),
        }
    }
}

/// Permanent archived Plasm program plan document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodePlanArchiveDocument {
    pub kind: String,
    pub plan_id: String,
    pub prompt_hash: String,
    pub session_id: String,
    pub entry_id: String,
    /// Monotonic per `(prompt_hash, session_id)` program-plan index; drives `plasm://.../p/{n}`.
    pub plan_index: u64,
    pub plan_handle: String,
    pub name: String,
    pub code: String,
    pub plan_hash: String,
    /// Canonical typed comp wire.
    pub comp: serde_json::Value,
    pub catalog_cgs_hash: String,
    pub domain_revision: u32,
    pub entities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    pub created_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RunArtifactError {
    #[error("run artifact JSON: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("run artifact decode: {0}")]
    Decode(String),
    #[error("run artifact integrity: {0}")]
    Integrity(String),
    #[error("run artifact object store: {0}")]
    ObjectStore(String),
    #[error("run artifact filesystem: {0}")]
    Filesystem(String),
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn run_artifact_id_from_wire_roundtrip_without_alloc_decode() {
        let id = RunArtifactId::from_bytes([0xab; 32]);
        let wire = id.to_wire();
        assert_eq!(RunArtifactId::from_wire(&wire), Some(id));
        assert!(RunArtifactId::from_wire("pr").is_none());
        assert!(RunArtifactId::from_wire("przz").is_none());
    }

    #[test]
    fn validate_run_artifact_document_json_requires_parsed_preimage() {
        let body = serde_json::json!({
            "run_id": "praaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "prompt_hash": "ph",
            "session_id": "sid",
            "entry_id": "linear",
            "entities": [],
            "display_lines": [],
            "request_fingerprints": [],
            "source": "live",
            "stats": { "duration_ms": 0, "cache_hits": 0 }
        });
        let err = validate_run_artifact_document_json(&body).unwrap_err();
        assert!(err.contains("parsed_preimage missing"), "{err}");
    }

    #[test]
    fn validate_artifact_payload_metadata_rejects_stale_version() {
        let meta = ArtifactPayloadMetadata {
            content_type: "application/json".into(),
            content_encoding: None,
            schema_version: 1,
            producer: "plasm".into(),
        };
        let err = validate_artifact_payload_metadata(&meta).unwrap_err();
        assert!(err.contains("schema_version must be 2"), "{err}");
    }
}
