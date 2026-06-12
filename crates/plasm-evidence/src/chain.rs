use crate::canonical::{hash_segment_body, segment_body_for_hash};
use crate::digest::{ChainHead, SegmentDigest};
use crate::segment::{EvidenceKind, EvidenceSegment};

#[derive(Clone)]
pub struct ChainBuilder {
    prev: Option<SegmentDigest>,
    seq: u64,
    segments: Vec<EvidenceSegment>,
}

/// Typical segment count: intent + comp + steps + run_sealed (+ margin).
pub const CHAIN_BUILDER_DEFAULT_CAPACITY: usize = 8;

impl ChainBuilder {
    pub fn new() -> Self {
        Self::with_capacity(CHAIN_BUILDER_DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            prev: None,
            seq: 0,
            segments: Vec::with_capacity(capacity),
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.segments.reserve(additional);
    }

    pub fn head(&self) -> Option<ChainHead> {
        self.prev.map(ChainHead::from_segment)
    }

    pub fn segments(&self) -> &[EvidenceSegment] {
        &self.segments
    }

    pub fn push(&mut self, kind: EvidenceKind, at_ms: Option<u64>) -> Result<SegmentDigest, crate::verify::EvidenceError> {
        let body = segment_body_for_hash(self.seq, self.prev, &kind);
        let digest = hash_segment_body(&body)?;
        let segment = EvidenceSegment {
            seq: self.seq,
            prev: self.prev,
            kind,
            emitted_at_ms: at_ms,
        };
        self.segments.push(segment);
        self.prev = Some(digest);
        self.seq += 1;
        Ok(digest)
    }

    pub fn finish(self) -> EvidenceChain {
        EvidenceChain {
            segments: self.segments,
            head: self.prev.map(ChainHead::from_segment),
        }
    }

    /// Finalize after incremental `push` validation (each link hashed at append time).
    pub fn finish_trusted(self) -> EvidenceChain {
        self.finish()
    }
}

impl Default for ChainBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceChain {
    pub segments: Vec<EvidenceSegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<ChainHead>,
}

impl EvidenceChain {
    pub fn verify_integrity(&self) -> Result<ChainHead, crate::verify::EvidenceError> {
        let mut prev: Option<SegmentDigest> = None;
        for segment in &self.segments {
            if segment.prev != prev {
                return Err(crate::verify::EvidenceError::BrokenPrevLink {
                    seq: segment.seq,
                });
            }
            let body = segment_body_for_hash(segment.seq, segment.prev, &segment.kind);
            let digest = hash_segment_body(&body)?;
            prev = Some(digest);
        }
        prev.map(ChainHead::from_segment)
            .ok_or(crate::verify::EvidenceError::EmptyChain)
    }
}
