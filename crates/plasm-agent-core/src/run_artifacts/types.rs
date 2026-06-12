use axum::body::Bytes;
use plasm_core::expr_parser::ParsedExpr;
use plasm_runtime::{ExecutionSource, ExecutionStats};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// ASCII prefix for deterministic run artifact wire ids (`pr` + 64 lowercase hex = full SHA256 digest).
pub const RUN_ARTIFACT_WIRE_PREFIX: &str = "pr";

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
        if !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let decoded = hex::decode(rest).ok()?;
        if decoded.len() != 32 {
            return None;
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&decoded);
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
            schema_version: 1,
            producer: "plasm".into(),
        }
    }
}

/// Opaque artifact bytes plus explicit metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPayload {
    pub metadata: ArtifactPayloadMetadata,
    pub bytes: Bytes,
}

/// JSON document returned by artifact GET and MCP `resources/read`.
#[derive(Debug, Serialize)]
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
    pub expressions: Vec<String>,
    pub request_fingerprints: Vec<String>,
    pub entities: Vec<serde_json::Value>,
    pub source: ExecutionSource,
    pub stats: ExecutionStats,
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
    /// Canonical typed comp wire (legacy archive alias: `plan`).
    #[serde(alias = "plan")]
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
    #[error("run artifact object store: {0}")]
    ObjectStore(String),
    #[error("run artifact filesystem: {0}")]
    Filesystem(String),
}
