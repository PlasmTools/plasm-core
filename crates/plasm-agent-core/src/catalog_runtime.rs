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
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CatalogRuntimeError {
    #[error(transparent)]
    Template(#[from] plasm_compile::CatalogTemplateError),
    #[error(transparent)]
    CatalogLoad(#[from] crate::catalog_data::CatalogLoadError),
    #[error(transparent)]
    CatalogIl(#[from] plasm_core::catalog_il::CatalogIlError),
    #[error(transparent)]
    CompiledCatalog(#[from] plasm_compile::CmlError),
    #[error(transparent)]
    Discovery(#[from] plasm_core::discovery::DiscoveryError),
    #[error(transparent)]
    DiscoveryStore(#[from] crate::discovery_store::DiscoveryStoreError),
    #[error(transparent)]
    Prerequisite(#[from] plasm_core::prerequisites::PrerequisiteError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("discovery requires PLASM_DISCOVERY_DATABASE_URL or DATABASE_URL")]
    MissingDiscoveryDatabaseUrl,
    #[error("discovery requires a packed format-3 catalog directory")]
    RequiresCatalogDirectory,
    #[error("no activated discovery generation")]
    NoActivatedGeneration,
    #[error("no compiled request recipes for catalog `{entry_id}`")]
    CompiledCatalogUnavailable { entry_id: String },
    #[error("invalid deployment bindings at {path}: {source}")]
    DeploymentBindingsJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

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
    prerequisite_deployments: plasm_core::prerequisites::DeploymentBindings,
}

fn compile_fixed_generation(
    registry: Arc<CgsRegistry>,
) -> Result<CatalogGeneration, CatalogRuntimeError> {
    let compiled_by_entry = registry
        .list_entries()
        .into_iter()
        .map(|meta| {
            let context = registry.load_context(&meta.entry_id)?;
            let compiled = plasm_compile::compile_cgs_capability_templates(&context.cgs)?;
            Ok((meta.entry_id, Arc::new(compiled)))
        })
        .collect::<Result<HashMap<_, _>, CatalogRuntimeError>>()?;
    Ok(CatalogGeneration {
        registry,
        compiled_by_entry: Arc::new(compiled_by_entry),
        prerequisite_deployments: plasm_core::prerequisites::DeploymentBindings::default(),
    })
}

impl CatalogRuntime {
    pub fn new(
        initial: Arc<CgsRegistry>,
        bootstrap: CatalogBootstrap,
    ) -> Result<Self, CatalogRuntimeError> {
        let generation = match &bootstrap {
            CatalogBootstrap::CatalogDir { path } => {
                let loaded = crate::catalog_data::load_catalog_set_from_dir_with_progress(
                    path,
                    &mut |_: &str| {},
                )?;
                CatalogGeneration {
                    registry: loaded.registry,
                    compiled_by_entry: loaded.compiled_by_entry,
                    prerequisite_deployments: read_optional_deployment_bindings(path)?,
                }
            }
            CatalogBootstrap::Fixed => compile_fixed_generation(initial)?,
        };
        Ok(Self {
            swap: Arc::new(ArcSwap::new(Arc::new(generation))),
            bootstrap,
            reload_generation: Arc::new(AtomicU64::new(0)),
            discovery: Arc::new(tokio::sync::OnceCell::new()),
            generation: Arc::new(arc_swap::ArcSwapOption::empty()),
        })
    }

    /// Connect once. Explicit compilation and execution never call this method.
    pub async fn discovery_store(
        &self,
    ) -> Result<&crate::discovery_store::DiscoveryStore, CatalogRuntimeError> {
        self.discovery
            .get_or_try_init(|| async {
                let url = std::env::var("PLASM_DISCOVERY_DATABASE_URL")
                    .or_else(|_| std::env::var("DATABASE_URL"))
                    .map_err(|_| CatalogRuntimeError::MissingDiscoveryDatabaseUrl)?;
                let store = crate::discovery_store::DiscoveryStore::connect(&url).await?;
                store.migrate().await?;
                Ok(store)
            })
            .await
    }

    /// Import one complete manifest set and its explicit deployment bindings.
    pub async fn activate_discovery(&self) -> Result<String, CatalogRuntimeError> {
        let path = self
            .catalog_dir_path()
            .ok_or_else(|| CatalogRuntimeError::RequiresCatalogDirectory)?;
        let manifests = plasm_core::catalog_il::read_catalog_set(path)?;
        let prepared = manifests
            .iter()
            .map(|manifest| crate::discovery_store::PreparedCatalog::load(manifest))
            .collect::<Result<Vec<_>, _>>()?;
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
    pub async fn pinned_view(&self, generation: &str) -> Result<Self, CatalogRuntimeError> {
        let (catalogs, compiled_catalogs, prerequisite_deployments) = self
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
            prerequisite_deployments,
        };
        let mut view = self.clone();
        view.swap = Arc::new(ArcSwap::new(Arc::new(generation_view)));
        view.generation = Arc::new(arc_swap::ArcSwapOption::from(Some(Arc::new(
            generation.to_owned(),
        ))));
        Ok(view)
    }

    pub fn discovery_generation(&self) -> Result<Arc<String>, CatalogRuntimeError> {
        self.generation
            .load_full()
            .ok_or(CatalogRuntimeError::NoActivatedGeneration)
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
    ) -> Result<Arc<plasm_compile::CompiledCatalog>, CatalogRuntimeError> {
        self.swap
            .load_full()
            .compiled_by_entry
            .get(entry_id)
            .cloned()
            .ok_or_else(|| CatalogRuntimeError::CompiledCatalogUnavailable {
                entry_id: entry_id.to_owned(),
            })
    }

    /// Publish a validated registry after load (used at startup and by reload handler).
    #[inline]
    pub fn publish_catalog(&self, reg: Arc<CgsRegistry>) -> Result<(), CatalogRuntimeError> {
        self.swap.store(Arc::new(compile_fixed_generation(reg)?));
        Ok(())
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

    /// Explicit prerequisite deployments pinned with this catalog generation (RA-17).
    pub fn prerequisite_deployments(&self) -> plasm_core::prerequisites::DeploymentBindings {
        self.swap.load_full().prerequisite_deployments.clone()
    }
}

fn read_optional_deployment_bindings(
    dir: &Path,
) -> Result<plasm_core::prerequisites::DeploymentBindings, CatalogRuntimeError> {
    let path = dir.join("deployment-bindings.json");
    if !path.is_file() {
        return Ok(plasm_core::prerequisites::DeploymentBindings::default());
    }
    let bytes = std::fs::read(&path)?;
    serde_json::from_slice(&bytes)
        .map_err(|source| CatalogRuntimeError::DeploymentBindingsJson { path, source })
}
