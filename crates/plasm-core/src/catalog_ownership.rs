//! Canonical catalog ownership resolution for invoke materialization and federated relation hops.

use crate::cgs_federation::{FederationDispatch, FederationResolveError, QualifiedEntityKey};
use crate::expr::Expr;
use crate::schema::CGS;
use crate::symbol_tuning::SymbolMap;

pub const FEDERATED_RELATION_MISSING_OWNERSHIP: &str =
    "federated relation continuation requires catalog ownership from the source row (use session e# / binding continuation, not bare wire entity names)";

const INVOKE_MISSING_OWNERSHIP_SUFFIX: &str =
    "requires catalog ownership on the receiver — use e# / binding continuation, not bare wire entity names";

/// Parser/session context for invoke catalog resolution when the receiver lacks a stamped `e#`.
#[derive(Debug, Clone, Copy)]
pub struct InvokeCatalogResolutionContext<'a> {
    pub pending_session_catalog_entry_id: Option<&'a str>,
    pub active_entity_entry_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogOwnershipError {
    pub entity: String,
    pub context: CatalogOwnershipContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogOwnershipContext {
    Invoke,
    FederatedRelation,
}

impl std::fmt::Display for CatalogOwnershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.context {
            CatalogOwnershipContext::Invoke => write!(
                f,
                "capability invoke on `{}` {INVOKE_MISSING_OWNERSHIP_SUFFIX}",
                self.entity
            ),
            CatalogOwnershipContext::FederatedRelation => {
                f.write_str(FEDERATED_RELATION_MISSING_OWNERSHIP)
            }
        }
    }
}

impl std::error::Error for CatalogOwnershipError {}

/// Resolve registry `entry_id` for cap-qualified invoke arg materialization.
pub fn catalog_entry_id_for_invoke(
    source: &Expr,
    raw_method_label: Option<&str>,
    sym_map: &SymbolMap,
    ctx: InvokeCatalogResolutionContext<'_>,
) -> Result<String, CatalogOwnershipError> {
    if let Some(eid) = source.session_catalog_entry_id() {
        return Ok(eid.to_string());
    }
    if let Some(raw) = raw_method_label {
        if let Some((entry_id, domain, _)) = sym_map.resolve_method_symbol_triple(raw) {
            if domain == source.primary_entity() {
                return Ok(entry_id.to_string());
            }
        }
    }
    if let Some(eid) = ctx
        .pending_session_catalog_entry_id
        .or(ctx.active_entity_entry_id)
    {
        return Ok(eid.to_string());
    }
    Err(CatalogOwnershipError {
        entity: source.primary_entity().to_string(),
        context: CatalogOwnershipContext::Invoke,
    })
}

/// Infer `(entry_id, entity)` when the expr stamps session catalog ownership.
pub fn infer_qualified_entity_from_stamped_source(source: &Expr) -> Option<QualifiedEntityKey> {
    let entry_id = source.session_catalog_entry_id()?;
    Some(QualifiedEntityKey::new(
        entry_id.to_string(),
        source.primary_entity().to_string(),
    ))
}

/// Resolve relation-hop catalog row under federation; fail closed when ownership is missing.
pub fn require_relation_source_qualified_entity(
    source: &Expr,
    federated: bool,
    source_row_qe: Option<&QualifiedEntityKey>,
) -> Result<Option<QualifiedEntityKey>, CatalogOwnershipError> {
    if let Some(qe) = source_row_qe {
        return Ok(Some(qe.clone()));
    }
    if let Some(qe) = infer_qualified_entity_from_stamped_source(source) {
        return Ok(Some(qe));
    }
    if federated {
        return Err(CatalogOwnershipError {
            entity: source.primary_entity().to_string(),
            context: CatalogOwnershipContext::FederatedRelation,
        });
    }
    Ok(None)
}

/// Resolve owning [`CGS`] for a stamped catalog + entity under federation (fail closed on bad stamp).
pub fn resolve_cgs_for_stamped_catalog<'a>(
    catalog_entry_id: Option<&str>,
    entity: &str,
    fed: &'a FederationDispatch,
    fallback: &'a CGS,
) -> Result<&'a CGS, FederationResolveError> {
    if let Some(eid) = catalog_entry_id {
        return fed
            .cgs_for_catalog_entry_id(eid, entity)
            .ok_or_else(|| FederationResolveError::EntityNotInAnyCatalog {
                entity: format!(
                    "entity `{entity}` is not defined in catalog `{eid}` (use the session `e#` from the teaching table)"
                ),
            });
    }
    fed.resolve_entity(
        entity,
        crate::row_composition::ResolutionHint::default(),
        fallback,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CgsContext;
    use indexmap::IndexMap;
    use std::sync::Arc;

    fn matrix_sym_map() -> std::sync::Arc<SymbolMap> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = Arc::new(crate::loader::load_schema_dir(&dir).expect("matrix"));
        crate::symbol_tuning::symbol_map_for_prompt(
            cgs.as_ref(),
            crate::symbol_tuning::FocusSpec::All,
            true,
        )
        .expect("symbol map")
    }

    #[test]
    fn catalog_entry_id_for_invoke_prefers_session_stamp() {
        let map = matrix_sym_map();
        let expr = Expr::Query(crate::QueryExpr::all("LangItem"))
            .with_session_catalog_entry_id(Some("default".into()));
        let eid = catalog_entry_id_for_invoke(
            &expr,
            None,
            &map,
            InvokeCatalogResolutionContext {
                pending_session_catalog_entry_id: None,
                active_entity_entry_id: None,
            },
        )
        .expect("stamped");
        assert_eq!(eid, "default");
    }

    #[test]
    fn catalog_entry_id_for_invoke_uses_parser_pending_context() {
        let map = matrix_sym_map();
        let expr = Expr::Query(crate::QueryExpr::all("LangItem"));
        let eid = catalog_entry_id_for_invoke(
            &expr,
            None,
            &map,
            InvokeCatalogResolutionContext {
                pending_session_catalog_entry_id: Some("default"),
                active_entity_entry_id: None,
            },
        )
        .expect("pending");
        assert_eq!(eid, "default");
    }

    #[test]
    fn require_relation_source_fails_closed_in_federation() {
        let expr = Expr::Query(crate::QueryExpr::all("LangItem"));
        let err = require_relation_source_qualified_entity(&expr, true, None).expect_err("fed");
        assert!(matches!(
            err.context,
            CatalogOwnershipContext::FederatedRelation
        ));
    }

    #[test]
    fn resolve_cgs_for_stamped_catalog_honors_entry_id() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = Arc::new(crate::loader::load_schema_dir(&dir).expect("matrix"));
        let mut by_entry = IndexMap::new();
        by_entry.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        let fed = FederationDispatch::from_contexts_only(by_entry);
        let resolved =
            resolve_cgs_for_stamped_catalog(Some("default"), "LangItem", &fed, cgs.as_ref())
                .expect("resolve");
        assert!(std::ptr::eq(resolved, cgs.as_ref()));
    }
}
