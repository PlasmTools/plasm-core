//! Typed, graph-aware discovery over CGS catalogs (lexical + graph gating).
//!
//! See [`AgentDiscovery`] and [`TypedDiscovery`].

use async_trait::async_trait;

mod decompose;
mod engine;
pub mod index;
pub mod index_cache;
mod metrics;
mod types;

pub use engine::TypedDiscovery;
pub use index_cache::CatalogIndexCache;
pub use types::*;

/// Stepwise discovery: single-shot [`Self::discover`] or clarification follow-ups.
#[async_trait]
pub trait AgentDiscovery: Send + Sync {
    async fn discover(&self, query: DiscoveryQuery) -> Result<DiscoveryDecision, DiscoveryError>;

    async fn answer_clarification(
        &self,
        state: ClarificationState,
        answer: ClarificationAnswer,
    ) -> Result<DiscoveryDecision, DiscoveryError>;
}
