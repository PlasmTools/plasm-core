//! Opaque identifiers for frames, fused nodes, and engine handles.
//!
//! None of these appear on hashed [`crate::PlasmComp`].

use serde::{Deserialize, Serialize};

macro_rules! u64_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            #[must_use]
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            #[must_use]
            pub const fn as_u64(self) -> u64 {
                self.0
            }
        }
    };
}

u64_id! {
    /// Session-local ingested frame.
    FrameId
}

u64_id! {
    /// Node inside a fused [`super::RowPlan`] pipeline.
    RowNodeId
}

u64_id! {
    /// Adapter-private compiled plan handle. Never stored on `PlasmComp`.
    EnginePlanId
}

u64_id! {
    /// Language-matrix / unit-test scan.
    FixtureScanId
}

u64_id! {
    /// Graph-backed scan (hot snapshot identity).
    GraphSnapshotId
}

u64_id! {
    /// Hash of the surface `ComputeOp` chain (written order), not the fused engine plan.
    SurfaceMeaningId
}

impl SurfaceMeaningId {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&digest[..8]);
        Self(u64::from_be_bytes(raw))
    }
}
