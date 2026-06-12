use crate::digest::{IntentDigest, SegmentDigest};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSegment {
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<SegmentDigest>,
    pub kind: EvidenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted_at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceKind {
    IntentBound {
        intent_digest: IntentDigest,
        intent_len: u32,
    },
    CompCommitted {
        plan_commit_id_hex: String,
        comp_semantic_sha256: SegmentDigest,
        step_topo: Vec<String>,
    },
    StepExecuted {
        step_id: String,
        step_index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
        request_fingerprints: Vec<String>,
        source_line: String,
        parsed_expr_digest: SegmentDigest,
    },
    RunSealed {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource_index: Option<u64>,
        run_bundle_digest: SegmentDigest,
    },
}
