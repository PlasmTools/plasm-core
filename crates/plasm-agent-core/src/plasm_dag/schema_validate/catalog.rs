//! CGS catalog helpers and diagnostic formatting.

use super::super::prelude::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemaCatalogError {
    #[error("catalog `{entry_id}` is not loaded for entity `{entity}`")]
    CatalogNotLoaded { entry_id: String, entity: String },
    #[error("entity `{entity}` is not defined in catalog `{entry_id}`")]
    EntityNotFound { entry_id: String, entity: String },
    #[error("query capability `{capability}` is not defined for entity `{entity}`")]
    QueryCapabilityNotFound { entity: String, capability: String },
    #[error(transparent)]
    FieldTokenResolution(#[from] crate::plasm_plan_run::WireFieldTokenError),
    #[error("field path is invalid: {0}")]
    InvalidFieldPath(#[from] plasm_core::plasm_monad::PlanAtomError),
    #[error("query capability resolution failed: {0}")]
    QueryCapabilityResolution(#[source] plasm_core::query_resolve::QueryCapabilityResolveError),
}

pub(in crate::plasm_dag) fn cgs_for_qualified_entity(
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Option<Arc<plasm_core::schema::CGS>> {
    session
        .contexts_by_entry
        .get(qe.entry_id.as_str())
        .map(|c| c.cgs.clone())
        .or_else(|| (session.entry_id == qe.entry_id).then(|| session.cgs.clone()))
}

/// Logical row keys materialized by entity decode (`FieldDecoder` stores each field under its CGS name).
pub(in crate::plasm_dag) fn logical_row_field_paths_for_entity(
    ent: &EntityDef,
) -> BTreeSet<Vec<String>> {
    let mut set = BTreeSet::new();
    for name in ent.fields.keys() {
        set.insert(vec![name.as_str().to_string()]);
    }
    for rel_name in ent.relations.keys() {
        set.insert(vec![rel_name.as_str().to_string()]);
    }
    set
}

pub(in crate::plasm_dag) fn logical_row_field_paths_from_names(
    names: &[String],
) -> BTreeSet<Vec<String>> {
    names.iter().map(|n| vec![n.clone()]).collect()
}
pub(in crate::plasm_dag) fn capability_for_surface_expr<'a>(
    cgs: &'a plasm_core::schema::CGS,
    expr: &'a Expr,
) -> Result<Option<&'a CapabilitySchema>, SchemaCatalogError> {
    match expr {
        Expr::Query(q) => {
            let cap = if let Some(name) = q.capability_name.as_deref() {
                cgs.get_capability(name).ok_or_else(|| {
                    SchemaCatalogError::QueryCapabilityNotFound {
                        entity: q.entity.to_string(),
                        capability: name.to_string(),
                    }
                })?
            } else {
                query_resolve::resolve_query_capability(q, cgs)
                    .map_err(SchemaCatalogError::QueryCapabilityResolution)?
            };
            Ok(Some(cap))
        }
        Expr::Get(g) => Ok(cgs
            .find_capabilities(g.reference.entity_type.as_str(), CapabilityKind::Get)
            .into_iter()
            .next()),
        Expr::Create(c) => Ok(cgs.get_capability(c.capability.as_str())),
        Expr::Delete(d) => Ok(cgs.get_capability(d.capability.as_str())),
        Expr::Invoke(i) => Ok(cgs.get_capability(i.capability.as_str())),
        Expr::Chain(_)
        | Expr::TeachingValue { .. }
        | Expr::Page(_)
        | Expr::Wait(_)
        | Expr::Cancel(_) => Ok(None),
    }
}
pub(in crate::plasm_dag) fn infer_entity_row_columns(
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Result<Vec<OutputName>, SchemaCatalogError> {
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        SchemaCatalogError::CatalogNotLoaded {
            entry_id: qe.entry_id.to_string(),
            entity: qe.entity.to_string(),
        }
    })?;
    let ent =
        cgs.get_entity(qe.entity.as_str())
            .ok_or_else(|| SchemaCatalogError::EntityNotFound {
                entry_id: qe.entry_id.to_string(),
                entity: qe.entity.to_string(),
            })?;
    let paths = logical_row_field_paths_for_entity(ent);
    paths
        .into_iter()
        .map(|segs| OutputName::new(segs.join(".")))
        .collect::<Result<Vec<_>, _>>()
        .map_err(SchemaCatalogError::InvalidFieldPath)
}
pub(in crate::plasm_dag) fn single_segment_teaching_field_hint(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    path: &FieldPath,
) -> String {
    let segs = path.segments().to_vec();
    if segs.len() != 1 {
        return String::new();
    }
    let wire = segs[0].as_str();
    let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let sym = map.ident_sym_entity_field_for(qe.entry_id.as_str(), qe.entity.as_str(), wire);
    if sym != wire {
        format!(" For `{wire}` the active teaching-table symbol is `{sym}`.")
    } else {
        String::new()
    }
}

pub(in crate::plasm_dag) fn is_opaque_passthrough_compute_schema(
    schema: &SyntheticResultSchema,
) -> bool {
    schema.fields.len() == 1
        && schema.fields[0].name.as_str() == "value"
        && matches!(schema.fields[0].value_kind, SyntheticValueKind::Unknown)
        && schema.fields[0].value_type.is_none()
}

pub(in crate::plasm_dag) fn capability_input_param_wires(
    cap: &CapabilitySchema,
) -> BTreeSet<String> {
    cap.input_fields().map(|f| f.name.clone()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RowContractFieldError {
    #[error("`{field}` is a query/capability input on this fetch, not a row field. Use wire field names from the language card for row postfix (`.filter`, `[field,…]`).")]
    CapabilityInputNotRowField { field: String },
    #[error("`{field}` is not a row field on this binding's rows. Use wire field names from the language-card left column for this binding.")]
    MissingRowField { field: String },
}

#[allow(clippy::too_many_arguments)]
pub(in crate::plasm_dag) fn row_contract_field_error(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    _qe: &QualifiedEntityKey,
    cap: Option<&CapabilitySchema>,
    _path: &FieldPath,
    wire: &str,
    _allowed_cols: &[String],
    _op_label: &str,
) -> RowContractFieldError {
    let _ = (session, symbol_map_cross_cache);
    if let Some(cap) = cap {
        let inputs = capability_input_param_wires(cap);
        if inputs.contains(wire) {
            return RowContractFieldError::CapabilityInputNotRowField {
                field: wire.to_owned(),
            };
        }
    }
    RowContractFieldError::MissingRowField {
        field: wire.to_owned(),
    }
}
pub(in crate::plasm_dag) fn resolve_compute_field_path(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    path: &FieldPath,
) -> Result<FieldPath, SchemaCatalogError> {
    let segs = path.segments();
    if segs.len() != 1 {
        return Ok(path.clone());
    }
    let wire = crate::plasm_plan_run::resolve_wire_field_token(
        session,
        symbol_map_cross_cache,
        qe,
        segs[0].as_str(),
    )?;
    FieldPath::from_dotted(&wire).map_err(SchemaCatalogError::InvalidFieldPath)
}

/// Resolve declared output fields before consulting catalog tokens. Synthetic values
/// have typed schemas without entity receiver authority.
pub(in crate::plasm_dag) fn resolve_schema_field_path(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    source_schema: Option<&SyntheticResultSchema>,
    path: &FieldPath,
) -> Result<FieldPath, SchemaCatalogError> {
    let segs = path.segments();
    if segs.len() == 1 {
        let raw = segs[0].as_str();
        if let Some(schema) = source_schema {
            if schema.fields.iter().any(|f| f.name.as_str() == raw) {
                return FieldPath::from_dotted(raw).map_err(SchemaCatalogError::InvalidFieldPath);
            }
        }
    }
    resolve_compute_field_path(session, symbol_map_cross_cache, qe, path)
}
