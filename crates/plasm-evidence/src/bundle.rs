use crate::chain::EvidenceChain;
use crate::digest::ChainHead;
use crate::scope::EvidenceScope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct EvidenceAnchors {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_commit_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_call_index: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSignature {
    pub public_key_hex: String,
    pub signature_hex: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceBundle {
    pub scope: EvidenceScope,
    pub chain: EvidenceChain,
    #[serde(default, skip_serializing_if = "EvidenceAnchors::is_empty")]
    pub anchors: EvidenceAnchors,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<EvidenceSignature>,
}

impl EvidenceAnchors {
    pub fn is_empty(&self) -> bool {
        self.plan_commit_ref.is_none() && self.trace_id.is_none() && self.plan_call_index.is_none()
    }
}

impl EvidenceBundle {
    pub fn chain_head(&self) -> Option<ChainHead> {
        self.chain.head.or_else(|| self.chain.verify_integrity().ok())
    }
}
