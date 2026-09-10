//! Swappable in-process catalog for HTTP/MCP (`--catalog-dir` multi-entry or fixed in-memory registry).
//!
//! ## Snapshot contract
//!
//! - [`CatalogRuntime::snapshot`] (backed by [`arc_swap::ArcSwap::load_full`]) returns the **current**
//!   [`CgsRegistry`] at call time. A concurrent [`CatalogRuntime::publish_catalog`] may replace
//!   the snapshot between two calls—this is intentional RCU-style behavior.
//! - **Do not** hold `Arc<CgsRegistry>` across `.await` if you need a view that stays consistent
//!   with “latest reload” unless you intentionally pin a snapshot for one logical operation (e.g. one
//!   HTTP handler body). Execute sessions pin [`CGS`](plasm_core::schema::CGS) via
//!   [`catalog_cgs_hash`](crate::execute_session::SessionReuseKey), not the live swap pointer.
//! - For discovery / new session open, call `snapshot()` when you need the registry; long async chains
//!   should re-snapshot after await only if freshness matters (rare).

use arc_swap::ArcSwap;
use plasm_core::discovery::CgsRegistry;
use plasm_core::CgsCatalog;
use std::collections::HashMap;
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
    swap: Arc<ArcSwap<CatalogGeneration>>,
    pub bootstrap: CatalogBootstrap,
    reload_generation: Arc<AtomicU64>,
    discovery: Arc<tokio::sync::OnceCell<crate::discovery_store::DiscoveryStore>>,
    generation: Arc<arc_swap::ArcSwapOption<String>>,
}

#[derive(Clone)]
struct CatalogGeneration {
    registry: Arc<CgsRegistry>,
    compiled_by_entry: Arc<HashMap<String, Arc<plasm_compile::CompiledCatalog>>>,
}

fn compile_fixed_generation(registry: Arc<CgsRegistry>) -> CatalogGeneration {
    let compiled_by_entry = registry
        .list_entries()
        .into_iter()
        .map(|meta| {
            let context = registry
                .load_context(&meta.entry_id)
                .unwrap_or_else(|error| panic!("cannot load catalog `{}`: {error}", meta.entry_id));
            let compiled = plasm_compile::compile_cgs_capability_templates(&context.cgs)
                .unwrap_or_else(|error| {
                    panic!("cannot compile catalog `{}`: {error}", meta.entry_id)
                });
            (meta.entry_id, Arc::new(compiled))
        })
        .collect();
    CatalogGeneration {
        registry,
        compiled_by_entry: Arc::new(compiled_by_entry),
    }
}

impl CatalogRuntime {
    pub fn new(initial: Arc<CgsRegistry>, bootstrap: CatalogBootstrap) -> Self {
        let generation = match &bootstrap {
            CatalogBootstrap::CatalogDir { path } => {
                let loaded = crate::catalog_data::load_catalog_set_from_dir_with_progress(
                    path,
                    &mut |_: &str| {},
                )
                .unwrap_or_else(|error| panic!("cannot load compiled catalog generation: {error}"));
                CatalogGeneration {
                    registry: loaded.registry,
                    compiled_by_entry: loaded.compiled_by_entry,
                }
            }
            CatalogBootstrap::Fixed => compile_fixed_generation(initial),
        };
        Self {
            swap: Arc::new(ArcSwap::new(Arc::new(generation))),
            bootstrap,
            reload_generation: Arc::new(AtomicU64::new(0)),
            discovery: Arc::new(tokio::sync::OnceCell::new()),
            generation: Arc::new(arc_swap::ArcSwapOption::empty()),
        }
    }

    /// Connect once. Explicit compilation and execution never call this method.
    pub async fn discovery_store(&self) -> anyhow::Result<&crate::discovery_store::DiscoveryStore> {
        self.discovery
            .get_or_try_init(|| async {
                let url = std::env::var("PLASM_DISCOVERY_DATABASE_URL")
                    .or_else(|_| std::env::var("DATABASE_URL"))
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "discovery requires PLASM_DISCOVERY_DATABASE_URL or DATABASE_URL"
                        )
                    })?;
                let store = crate::discovery_store::DiscoveryStore::connect(&url).await?;
                store.migrate().await?;
                Ok(store)
            })
            .await
    }

    /// Import one complete manifest set and its explicit deployment bindings.
    pub async fn activate_discovery(&self) -> anyhow::Result<String> {
        let path = self.catalog_dir_path().ok_or_else(|| {
            anyhow::anyhow!("discovery requires a packed format-3 catalog directory")
        })?;
        let manifests =
            plasm_core::catalog_il::read_catalog_set(path).map_err(anyhow::Error::msg)?;
        let prepared = manifests
            .iter()
            .map(|manifest| crate::discovery_store::PreparedCatalog::load(manifest))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let binding_path = std::env::var_os("PLASM_DISCOVERY_BINDINGS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| path.join("deployment-bindings.json"));
        let bindings = serde_json::from_slice(&tokio::fs::read(binding_path).await?)?;
        let deployment =
            std::env::var("PLASM_DISCOVERY_DEPLOYMENT").unwrap_or_else(|_| "default".into());
        let generation = self
            .discovery_store()
            .await?
            .import(&deployment, prepared, &bindings)
            .await?;
        let view = self.pinned_view(&generation).await?;
        self.swap.store(view.swap.load_full());
        self.generation.store(Some(Arc::new(generation.clone())));
        Ok(generation)
    }

    /// Request-local catalog view. Shared execution stores stay attached to the host.
    pub async fn pinned_view(&self, generation: &str) -> anyhow::Result<Self> {
        let (catalogs, compiled_catalogs, _) = self
            .discovery_store()
            .await?
            .load_generation(generation)
            .await?;
        let pairs = catalogs
            .into_iter()
            .map(|(id, cgs)| (id.clone(), id, Vec::new(), Arc::new(cgs)))
            .collect();
        let compiled_by_entry = compiled_catalogs.into_iter().collect();
        let generation_view = CatalogGeneration {
            registry: Arc::new(CgsRegistry::from_pairs(pairs)),
            compiled_by_entry: Arc::new(compiled_by_entry),
        };
        let mut view = self.clone();
        view.swap = Arc::new(ArcSwap::new(Arc::new(generation_view)));
        view.generation = Arc::new(arc_swap::ArcSwapOption::from(Some(Arc::new(
            generation.to_owned(),
        ))));
        Ok(view)
    }

    pub fn discovery_generation(&self) -> anyhow::Result<Arc<String>> {
        self.generation
            .load_full()
            .ok_or_else(|| anyhow::anyhow!("no activated discovery generation"))
    }

    /// Current catalog snapshot (may change after a successful catalog-dir reload).
    #[inline]
    pub fn snapshot(&self) -> Arc<CgsRegistry> {
        self.swap.load_full().registry.clone()
    }

    /// Exact compiled request recipes paired with the current catalog generation.
    pub fn compiled_catalog(
        &self,
        entry_id: &str,
    ) -> Result<Arc<plasm_compile::CompiledCatalog>, String> {
        self.swap
            .load_full()
            .compiled_by_entry
            .get(entry_id)
            .cloned()
            .ok_or_else(|| format!("no compiled request recipes for catalog `{entry_id}`"))
    }

    /// Publish a validated registry after load (used at startup and by reload handler).
    #[inline]
    pub fn publish_catalog(&self, reg: Arc<CgsRegistry>) {
        self.swap.store(Arc::new(compile_fixed_generation(reg)));
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
