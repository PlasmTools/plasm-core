//! Swappable in-process catalog for HTTP/MCP (`--catalog-dir` multi-entry or fixed in-memory registry).
//!
//! ## Snapshot contract
//!
//! - [`CatalogRuntime::snapshot`] (backed by [`arc_swap::ArcSwap::load_full`]) returns the **current**
//!   [`InMemoryCgsRegistry`] at call time. A concurrent [`CatalogRuntime::publish_catalog`] may replace
//!   the snapshot between two calls—this is intentional RCU-style behavior.
//! - **Do not** hold `Arc<InMemoryCgsRegistry>` across `.await` if you need a view that stays consistent
//!   with “latest reload” unless you intentionally pin a snapshot for one logical operation (e.g. one
//!   HTTP handler body). Execute sessions pin [`CGS`](plasm_core::schema::CGS) via
//!   [`catalog_cgs_hash`](crate::execute_session::SessionReuseKey), not the live swap pointer.
//! - For discovery / new session open, call `snapshot()` when you need the registry; long async chains
//!   should re-snapshot after await only if freshness matters (rare).

use arc_swap::ArcSwap;
use plasm_core::discovery::InMemoryCgsRegistry;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// How the catalog was bootstrapped — drives whether control-plane hot reload is allowed.
#[derive(Clone, Debug)]
pub enum CatalogBootstrap {
    /// Multi-entry catalogs from `--catalog-dir` (compiled JSON IL); [`CatalogRuntime::snapshot`] can be refreshed via reload endpoint.
    CatalogDir { path: PathBuf },
    /// Not hot-reloadable: `--schema`, synthetic `default` entry, or tests building [`PlasmHostState`](crate::server_state::PlasmHostState) manually.
    Fixed,
}

/// Owns the atomic catalog pointer, bootstrap metadata, and reload generation counter.
#[derive(Clone)]
pub struct CatalogRuntime {
    swap: Arc<ArcSwap<InMemoryCgsRegistry>>,
    pub bootstrap: CatalogBootstrap,
    reload_generation: Arc<AtomicU64>,
}

impl CatalogRuntime {
    pub fn new(initial: Arc<InMemoryCgsRegistry>, bootstrap: CatalogBootstrap) -> Self {
        Self {
            swap: Arc::new(ArcSwap::new(initial)),
            bootstrap,
            reload_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Current catalog snapshot (may change after a successful catalog-dir reload).
    #[inline]
    pub fn snapshot(&self) -> Arc<InMemoryCgsRegistry> {
        self.swap.load_full()
    }

    /// Publish a validated registry after load (used at startup and by reload handler).
    #[inline]
    pub fn publish_catalog(&self, reg: Arc<InMemoryCgsRegistry>) {
        self.swap.store(reg);
    }

    /// Increments on each successful `POST /internal/catalog-registry/v1/reload` (first success → 1).
    pub fn bump_reload_generation(&self) -> u64 {
        self.reload_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn catalog_dir_path(&self) -> Option<&Path> {
        match &self.bootstrap {
            CatalogBootstrap::CatalogDir { path } => Some(path.as_path()),
            CatalogBootstrap::Fixed => None,
        }
    }
}
