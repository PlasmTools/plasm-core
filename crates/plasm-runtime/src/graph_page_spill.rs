//! Incremental spill hook for paginated graph materialization (hot RAM + durable pages).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::cache::CachedEntity;
use crate::RuntimeError;

/// Bounds for entities kept in the in-process graph during long paginated reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphHotCacheBounds {
    /// Maximum entity rows retained in RAM; older rows may be evicted after durable spill.
    pub max_hot_entities: usize,
}

impl GraphHotCacheBounds {
    /// Default hot cap when graph persistence is enabled (override with `PLASM_GRAPH_HOT_MAX_ENTITIES`).
    pub const DEFAULT_WITH_PERSISTENCE: usize = 2_048;

    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("PLASM_GRAPH_HOT_MAX_ENTITIES").ok()?;
        let n: usize = raw.trim().parse().ok()?;
        (n > 0).then_some(Self {
            max_hot_entities: n,
        })
    }

    pub fn with_persistence_default() -> Self {
        Self::from_env().unwrap_or(Self {
            max_hot_entities: Self::DEFAULT_WITH_PERSISTENCE,
        })
    }
}

/// One durable page of graph entities (read from session delta log).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphPageDelta {
    pub page_index: usize,
    pub entity_type: String,
    pub schema_version: u32,
    pub entities: Vec<serde_json::Value>,
}

/// Host-provided spill sink: append each paginated page to durable storage, trim hot RAM, and re-read pages.
#[async_trait]
pub trait GraphPageSpill: Send + Sync {
    async fn append_page(
        &self,
        page_index: usize,
        entities: &[CachedEntity],
    ) -> Result<(), RuntimeError>;

    async fn graph_pages(&self) -> Result<Vec<GraphPageDelta>, RuntimeError>;

    fn hot_bounds(&self) -> GraphHotCacheBounds;

    fn session_key(&self) -> (&str, &str);
}

pub type GraphPageSpillHandle = Arc<dyn GraphPageSpill>;
