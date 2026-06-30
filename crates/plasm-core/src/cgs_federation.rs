//! Federated execute sessions: **no** merged [`crate::schema::CGS`]. Each catalog row keeps its own
//! graph; [`crate::CgsContext`] (with [`crate::Prefix::Entry`]) is stored per `entry_id`. Prompt and
//! execution dispatch resolve the owning context by catalog + entity.

use crate::cgs_context::CgsContext;
use crate::schema::CGS;
use crate::symbol_tuning::TeachingExposureSession;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::Arc;

/// Stable key for which catalog + CGS entity an `e#` row refers to (session-scoped).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct QualifiedEntityKey {
    pub catalog_entry_id: String,
    pub entity: String,
}

impl QualifiedEntityKey {
    pub fn new(catalog_entry_id: impl Into<String>, entity: impl Into<String>) -> Self {
        Self {
            catalog_entry_id: catalog_entry_id.into(),
            entity: entity.into(),
        }
    }

    /// Registry row id (same wire name as agent plan `QualifiedEntityKey::entry_id`).
    pub fn entry_id(&self) -> &str {
        self.catalog_entry_id.as_str()
    }
}

/// Federated catalog resolution failure (fail closed; no blind primary fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FederationResolveError {
    EntityNotInAnyCatalog {
        entity: String,
    },
    AmbiguousEntity {
        entity: String,
        entry_ids: Vec<String>,
    },
}

impl std::fmt::Display for FederationResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EntityNotInAnyCatalog { entity } => {
                write!(f, "entity `{entity}` is not defined in any loaded catalog")
            }
            Self::AmbiguousEntity { entity, entry_ids } => {
                write!(
                    f,
                    "entity `{entity}` is ambiguous across federated catalogs: {entry_ids:?}"
                )
            }
        }
    }
}

impl std::error::Error for FederationResolveError {}

/// Maps exposed entity names to their owning registry row and [`CgsContext`] (HTTP backend, auth).
#[derive(Clone, Debug)]
pub struct FederationDispatch {
    pub by_entry: IndexMap<String, Arc<CgsContext>>,
    entity_to_entry: HashMap<String, String>,
}

/// Federated catalog resolution — alias for [`FederationDispatch`] (Wave D CatalogResolver cutover).
pub type CatalogResolver = FederationDispatch;

impl FederationDispatch {
    /// Build from loaded contexts and a [`TeachingExposureSession`] (parallel `entities` /
    /// `entity_catalog_entry_ids`).
    pub fn from_contexts_and_exposure(
        by_entry: IndexMap<String, Arc<CgsContext>>,
        exposure: &TeachingExposureSession,
    ) -> Self {
        let mut entity_to_entry: HashMap<String, String> = HashMap::new();
        for (i, ent) in exposure.entities.iter().enumerate() {
            if let Some(eid) = exposure.entity_catalog_entry_ids.get(i) {
                match entity_to_entry.entry(ent.clone()) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(eid.clone());
                    }
                    std::collections::hash_map::Entry::Occupied(o) => {
                        if o.get() != eid {
                            o.remove();
                        }
                    }
                }
            }
        }
        Self {
            by_entry,
            entity_to_entry,
        }
    }

    pub fn cgs_for_entity(&self, entity: &str) -> Option<&CGS> {
        let eid = self.entity_to_entry.get(entity)?;
        self.cgs_for_catalog_entry_id(eid, entity)
    }

    /// Owning [`CGS`] for one registry row when the surface stamps `catalog_entry_id` (`e#`, federated query).
    pub fn cgs_for_catalog_entry_id(&self, entry_id: &str, entity: &str) -> Option<&CGS> {
        self.by_entry
            .get(entry_id)
            .map(|ctx| ctx.cgs.as_ref())
            .filter(|cgs| cgs.entities.contains_key(entity))
    }

    /// Prefer per-catalog backend; used when selecting HTTP origin for an operation.
    pub fn http_backend_for_entity(&self, entity: &str) -> Option<&str> {
        let eid = self.entity_to_entry.get(entity)?;
        self.by_entry.get(eid).map(|c| c.cgs.http_backend.as_str())
    }

    /// Hint-aware catalog resolution (teaching exposure is prompt-only).
    pub fn resolve_entity<'a>(
        &'a self,
        entity: &str,
        hint: crate::row_composition::ResolutionHint<'a>,
        fallback: &'a CGS,
    ) -> Result<&'a CGS, FederationResolveError> {
        self.resolve_cgs_with_hint(entity, hint, fallback)
    }

    /// Resolve CGS for `entity`, else `fallback` (primary session graph).
    #[deprecated(note = "use resolve_entity with ResolutionHint — exposure is prompt-only")]
    pub fn resolve_cgs<'a>(&'a self, entity: &str, fallback: &'a CGS) -> &'a CGS {
        self.cgs_for_entity(entity).unwrap_or(fallback)
    }

    /// Resolve CGS for schema lookup with [`ResolutionHint`] (relation targets, plan QE, owning catalog).
    ///
    /// Order: `plan_qe` when entity matches → exposure map → `owning_cgs` when it defines `entity`
    /// → unique context scan → `fallback` only when that catalog defines `entity`.
    pub fn resolve_cgs_with_hint<'a>(
        &'a self,
        entity: &str,
        hint: crate::row_composition::ResolutionHint<'a>,
        fallback: &'a CGS,
    ) -> Result<&'a CGS, FederationResolveError> {
        if let Some(qe) = hint.plan_qe {
            if qe.entity == entity {
                if let Some(ctx) = self.by_entry.get(qe.catalog_entry_id.as_str()) {
                    if ctx.cgs.entities.contains_key(entity) {
                        return Ok(ctx.cgs.as_ref());
                    }
                }
            }
        }
        if let Some(cgs) = self.cgs_for_entity(entity) {
            return Ok(cgs);
        }
        if let Some(cgs) = hint.owning_cgs {
            if cgs.entities.contains_key(entity) && self.entry_id_for_cgs_ptr(cgs).is_some() {
                return Ok(cgs);
            }
        }
        let owners: Vec<&str> = self
            .by_entry
            .iter()
            .filter(|(_, ctx)| ctx.cgs.entities.contains_key(entity))
            .map(|(entry_id, _)| entry_id.as_str())
            .collect();
        match owners.len() {
            0 => {
                if fallback.entities.contains_key(entity) {
                    Ok(fallback)
                } else {
                    Err(FederationResolveError::EntityNotInAnyCatalog {
                        entity: entity.to_string(),
                    })
                }
            }
            1 => Ok(self.by_entry.get(owners[0]).unwrap().cgs.as_ref()),
            _ => Err(FederationResolveError::AmbiguousEntity {
                entity: entity.to_string(),
                entry_ids: owners.into_iter().map(str::to_string).collect(),
            }),
        }
    }

    fn entry_id_for_cgs_ptr(&self, cgs: &CGS) -> Option<&str> {
        self.by_entry
            .iter()
            .find(|(_, ctx)| std::ptr::eq(ctx.cgs.as_ref(), cgs))
            .map(|(entry_id, _)| entry_id.as_str())
    }

    /// Loaded contexts without teaching exposure (entity → unique owning entry when unambiguous).
    pub fn from_contexts_only(by_entry: IndexMap<String, Arc<CgsContext>>) -> Self {
        let mut entity_to_entry: HashMap<String, String> = HashMap::new();
        for (entry_id, ctx) in &by_entry {
            for ent in ctx.cgs.entities.keys() {
                let name = ent.as_str();
                if let Some(prev) = entity_to_entry.insert(name.to_string(), entry_id.clone()) {
                    if prev != *entry_id {
                        entity_to_entry.remove(name);
                    }
                }
            }
        }
        Self {
            by_entry,
            entity_to_entry,
        }
    }

    /// Owning registry `entry_id` for a loaded [`CGS`] pointer.
    pub fn entry_id_for_cgs<'a>(&'a self, cgs: &'a CGS, primary_entry_id: &'a str) -> &'a str {
        self.entry_id_for_cgs_ptr(cgs).unwrap_or(primary_entry_id)
    }

    /// Resolve `(entry_id, entity)` with the same doctrine as [`Self::resolve_cgs_with_hint`].
    pub fn resolve_qualified_entity_key(
        &self,
        entity: &str,
        owning_cgs: Option<&CGS>,
        fallback: &CGS,
        primary_entry_id: &str,
    ) -> Result<QualifiedEntityKey, FederationResolveError> {
        if let Some(qe) = self.qualified_entity_for_exposed_entity(entity) {
            return Ok(qe);
        }
        let cgs = self.resolve_cgs_with_hint(
            entity,
            crate::row_composition::ResolutionHint {
                owning_cgs,
                source_entity: None,
                plan_qe: None,
            },
            fallback,
        )?;
        Ok(QualifiedEntityKey::new(
            self.entry_id_for_cgs(cgs, primary_entry_id).to_string(),
            entity.to_string(),
        ))
    }

    /// Owning loaded context for an exposed entity name (HTTP backend + auth on the inner [`CGS`]).
    pub fn context_for_entity(&self, entity: &str) -> Option<&Arc<CgsContext>> {
        let eid = self.entity_to_entry.get(entity)?;
        self.by_entry.get(eid)
    }

    /// Owning `(catalog entry id, CGS entity name)` for an exposed federation entity name.
    ///
    /// Prefer this over [`Self::catalog_entry_id_for_entity`] at planner boundaries.
    pub fn qualified_entity_for_exposed_entity(&self, entity: &str) -> Option<QualifiedEntityKey> {
        self.entity_to_entry
            .get(entity)
            .map(|eid| QualifiedEntityKey::new(eid.clone(), entity.to_string()))
    }

    /// Owning registry `entry_id` for an exposed entity name (trace / dispatch labeling).
    #[deprecated(
        note = "use qualified_entity_for_exposed_entity — catalog ownership is (entry_id, entity)"
    )]
    pub fn catalog_entry_id_for_entity(&self, entity: &str) -> Option<&str> {
        self.entity_to_entry.get(entity).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CgsContext;
    use indexmap::IndexMap;
    use std::sync::Arc;

    fn matrix_cgs() -> Arc<CGS> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        Arc::new(crate::loader::load_schema_dir(&dir).expect("matrix cgs"))
    }

    #[test]
    fn resolve_entity_honors_plan_qe_hint() {
        let primary = matrix_cgs();
        let secondary = matrix_cgs();
        let mut by_entry = IndexMap::new();
        by_entry.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", primary.clone())),
        );
        by_entry.insert(
            "pokeapi".into(),
            Arc::new(CgsContext::entry("pokeapi", secondary.clone())),
        );
        let fed = FederationDispatch::from_contexts_only(by_entry);
        let qe = QualifiedEntityKey::new("pokeapi", "LangSummary");
        let hint = crate::row_composition::ResolutionHint {
            owning_cgs: None,
            source_entity: None,
            plan_qe: Some(&qe),
        };
        let cgs = fed
            .resolve_entity("LangSummary", hint, primary.as_ref())
            .expect("plan qe routes to pokeapi catalog");
        assert!(std::ptr::eq(cgs, secondary.as_ref()));
    }

    #[test]
    fn from_exposure_drops_ambiguous_bare_entity_name() {
        let cgs = matrix_cgs();
        let mut by_entry = IndexMap::new();
        by_entry.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        by_entry.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", cgs.clone())),
        );
        let layers: Vec<&CGS> = vec![cgs.as_ref(), cgs.as_ref()];
        let mut exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["LangItem"]);
        exp.expose_entities(&layers, cgs.clone(), "linear", &["LangItem"]);
        let fed = FederationDispatch::from_contexts_and_exposure(by_entry, &exp);
        assert!(fed.cgs_for_entity("LangItem").is_none());
        assert!(fed
            .qualified_entity_for_exposed_entity("LangItem")
            .is_none());
    }
}
