//! CGS relation resolution and chain metadata for lowering.

use super::prelude::*;
use super::types::CompileState;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelationLoweringError {
    #[error(transparent)]
    Symbol(#[from] plasm_core::symbol_tuning::SymbolResolveError),
    #[error(
        "relation symbol `{symbol}` does not belong to catalog `{entry_id}` entity `{entity}`"
    )]
    SymbolOwnerMismatch {
        symbol: String,
        entry_id: String,
        entity: String,
    },
    #[error("catalog `{entry_id}` has no entity `{entity}` for relation lowering")]
    CatalogEntityNotFound { entry_id: String, entity: String },
    #[error("entity `{entity}` has no relation `{relation}`{hint}")]
    RelationNotFound {
        entity: String,
        relation: String,
        hint: String,
    },
    #[error("relation `{relation}` does not support query braces")]
    RelationQueryBraces { relation: String },
    #[error("symbol `{symbol}` ({wire}) has the wrong role for a relation on `{entity}`")]
    RelationWrongRole {
        symbol: String,
        wire: String,
        entity: String,
    },
    #[error("relation `{relation}` on `{entity}` targets missing entity `{target}`")]
    TargetEntityNotFound {
        entity: String,
        relation: String,
        target: String,
    },
    #[error(transparent)]
    CatalogOwnership(#[from] crate::catalog_ownership::CatalogOwnershipError),
    #[error("relation source is not qualified for this session: {0}")]
    RelationSource(#[source] plasm_core::catalog_ownership::CatalogOwnershipError),
    #[error("relation binding proof validation failed: {0}")]
    BindingProof(#[source] plasm_core::RelationBindingProofError),
}

pub(in crate::plasm_dag) fn resolve_cgs_for_qualified_entity<'a>(
    session: &'a ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Option<&'a plasm_core::CGS> {
    session
        .contexts_by_entry
        .get(&qe.entry_id)
        .map(|ctx| ctx.cgs.as_ref())
        .filter(|cgs| cgs.entities.contains_key(qe.entity.as_str()))
        .or_else(|| {
            if session.entry_id == qe.entry_id
                && session.cgs.entities.contains_key(qe.entity.as_str())
            {
                Some(session.cgs.as_ref())
            } else {
                None
            }
        })
}

pub(in crate::plasm_dag) fn relation_segment_context<'a>(
    map: &'a dyn plasm_core::SymbolSession,
    qe: &'a QualifiedEntityKey,
    ent: &'a plasm_core::EntityDef,
    binding_label: Option<plasm_core::ProgramBindingLabel<'a>>,
    allow_lhs_coercion: bool,
) -> plasm_core::RelationSegmentContext<'a> {
    plasm_core::RelationSegmentContext {
        map,
        entity: qe.entity.as_str(),
        relations: &ent.relations,
        binding_label,
        allow_lhs_coercion,
    }
}

/// Opaque relation symbols belong to a catalog as well as a source entity.
/// Check before repair coercion can reinterpret an out-of-scope token.
fn validate_relation_symbol_owner(
    map: &dyn plasm_core::SymbolSession,
    qe: &QualifiedEntityKey,
    segment: &str,
) -> Result<(), RelationLoweringError> {
    if plasm_core::SymbolMap::is_opaque_r_sym(segment) {
        let binding = map
            .resolve_session_relation(segment)
            .map_err(RelationLoweringError::Symbol)?;
        if binding.entry_id.as_str() != qe.entry_id || binding.source_entity.as_str() != qe.entity {
            return Err(RelationLoweringError::SymbolOwnerMismatch {
                symbol: segment.to_string(),
                entry_id: qe.entry_id.to_string(),
                entity: qe.entity.to_string(),
            });
        }
    }
    Ok(())
}

pub(in crate::plasm_dag) fn resolve_relation_wire_on_entity(
    session: &ExecuteSession,
    cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    segment: &str,
    binding_label: Option<plasm_core::ProgramBindingLabel<'_>>,
) -> Option<String> {
    let cgs = resolve_cgs_for_qualified_entity(session, qe)?;
    let ent = cgs.get_entity(qe.entity.as_str())?;
    let map = symbol_map_for_plasm_surface_parse(session, cross_cache);
    validate_relation_symbol_owner(map.as_ref(), qe, segment).ok()?;
    let ctx = relation_segment_context(map.as_ref(), qe, ent, binding_label, true);
    match plasm_core::resolve_relation_segment(&ctx, segment) {
        plasm_core::RelationSegmentOutcome::Wire(w) => Some(w),
        _ => None,
    }
}

pub(in crate::plasm_dag) fn resolve_relation_segment_for_continuation(
    session: &ExecuteSession,
    cross_cache: Option<&SymbolMapCrossRequestCache>,
    row_qe: &QualifiedEntityKey,
    segment: &str,
    binding_label: Option<plasm_core::ProgramBindingLabel<'_>>,
) -> Result<String, RelationLoweringError> {
    let cgs = resolve_cgs_for_qualified_entity(session, row_qe).ok_or_else(|| {
        RelationLoweringError::CatalogEntityNotFound {
            entry_id: row_qe.entry_id.to_string(),
            entity: row_qe.entity.to_string(),
        }
    })?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        RelationLoweringError::CatalogEntityNotFound {
            entry_id: row_qe.entry_id.to_string(),
            entity: row_qe.entity.to_string(),
        }
    })?;
    let map = symbol_map_for_plasm_surface_parse(session, cross_cache);
    validate_relation_symbol_owner(map.as_ref(), row_qe, segment)?;
    let ctx = relation_segment_context(map.as_ref(), row_qe, ent, binding_label, true);
    if let Some((relation, _)) = segment.split_once('{') {
        if let plasm_core::RelationSegmentOutcome::Wire(wire) =
            plasm_core::resolve_relation_segment(&ctx, relation.trim())
        {
            return Err(RelationLoweringError::RelationQueryBraces { relation: wire });
        }
    }
    match plasm_core::resolve_relation_segment(&ctx, segment) {
        plasm_core::RelationSegmentOutcome::Wire(w) => Ok(w),
        plasm_core::RelationSegmentOutcome::WrongRole { sym, wire } => {
            Err(RelationLoweringError::RelationWrongRole {
                symbol: sym,
                wire,
                entity: row_qe.entity.to_string(),
            })
        }
        plasm_core::RelationSegmentOutcome::NotFound => {
            Err(RelationLoweringError::RelationNotFound {
                entity: row_qe.entity.to_string(),
                relation: segment.to_string(),
                hint: String::new(),
            })
        }
    }
}

pub(in crate::plasm_dag) fn relation_continuation_expr_from_source_row_hole(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    relation_wire: &str,
) -> Result<Expr, RelationLoweringError> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )
    .map_err(RelationLoweringError::CatalogOwnership)?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        RelationLoweringError::CatalogEntityNotFound {
            entry_id: row_qe.entry_id.to_string(),
            entity: row_qe.entity.to_string(),
        }
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        RelationLoweringError::RelationNotFound {
            entity: row_qe.entity.to_string(),
            relation: relation_wire.to_string(),
            hint: String::new(),
        }
    })?;
    let _target_ent = cgs
        .get_entity(rel.target_resource.as_str())
        .ok_or_else(|| {
            format!(
                "relation `{relation_wire}` on `{}` targets unknown entity `{}`",
                row_qe.entity, rel.target_resource
            )
        })
        .map_err(|_| RelationLoweringError::TargetEntityNotFound {
            entity: row_qe.entity.to_string(),
            relation: relation_wire.to_string(),
            target: rel.target_resource.to_string(),
        })?;
    let _target_qe = if cgs.entities.contains_key(rel.target_resource.as_str()) {
        QualifiedEntityKey {
            entry_id: row_qe.entry_id.clone(),
            entity: rel.target_resource.to_string(),
        }
    } else {
        crate::catalog_ownership::resolve_qualified_entity_key(
            session,
            rel.target_resource.as_str(),
            Some(cgs),
        )
        .map_err(RelationLoweringError::CatalogOwnership)?
    };
    let source_get = {
        let mut get = if ent.key_vars.is_empty() {
            let path_key = ent.id_field.as_str().to_string();
            let hole = PlasmInputRef::NodeInput {
                node: "source".into(),
                path: vec![path_key],
            };
            GetExpr::from_ref(Ref::simple_binding(row_qe.entity.as_str(), hole))
        } else {
            let mut slots = BTreeMap::new();
            for key in &ent.key_vars {
                slots.insert(
                    key.as_str().to_string(),
                    plasm_core::IdentitySlot::binding(PlasmInputRef::NodeInput {
                        node: "source".into(),
                        path: vec![key.as_str().to_string()],
                    }),
                );
            }
            GetExpr::from_ref(Ref::compound_slots(row_qe.entity.as_str(), slots))
        };
        get.catalog_entry_id = plasm_core::CatalogEntryStamp::some(
            plasm_core::RegistryEntryId::from(row_qe.entry_id.as_str()),
        );
        Expr::Get(get)
    };
    Ok(Expr::Chain(ChainExpr::auto_get(
        source_get,
        relation_wire.to_string(),
    )))
}

pub(in crate::plasm_dag) fn try_split_single_hop_surface_chain(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    expr: &str,
) -> Option<(String, String)> {
    let refs = state.program_node_id_set();
    let parsed = parse_plasm_program_surface_for_dag(
        session,
        state.cross_cache,
        state.pipeline,
        expr,
        &refs,
        false,
        None,
    )
    .ok()?;
    let Expr::Chain(chain) = parsed.expr else {
        return None;
    };
    if matches!(chain.source.as_ref(), Expr::Chain(_)) {
        return None;
    }
    let segment = chain.selector.clone();
    let trimmed = expr.trim();
    let suffix = format!(".{segment}");
    if trimmed.ends_with(&suffix) {
        let base_expr = trimmed[..trimmed.len() - suffix.len()].trim().to_string();
        if !base_expr.is_empty() {
            return Some((base_expr, segment));
        }
    }
    // Opaque relation symbols (e.g. `.r2`) may differ from wire names (e.g. `.pokemon`).
    let dot = trimmed.rfind('.')?;
    let base_expr = trimmed[..dot].trim().to_string();
    if base_expr.is_empty() {
        return None;
    }
    let base_parsed = parse_plasm_program_surface_for_dag(
        session,
        state.cross_cache,
        state.pipeline,
        &base_expr,
        &refs,
        false,
        None,
    )
    .ok()?;
    if base_parsed.expr == *chain.source {
        return Some((base_expr, segment));
    }
    None
}

pub(in crate::plasm_dag) fn relation_materialize_for_lower(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    relation_wire: &str,
) -> Result<plasm_core::RelationMaterialization, RelationLoweringError> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )
    .map_err(RelationLoweringError::CatalogOwnership)?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        RelationLoweringError::CatalogEntityNotFound {
            entry_id: row_qe.entry_id.to_string(),
            entity: row_qe.entity.to_string(),
        }
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        RelationLoweringError::RelationNotFound {
            entity: row_qe.entity.to_string(),
            relation: relation_wire.to_string(),
            hint: String::new(),
        }
    })?;
    Ok(rel
        .materialize
        .clone()
        .unwrap_or(plasm_core::RelationMaterialization::Unavailable))
}

pub(in crate::plasm_dag) fn relation_binding_proofs_for_lower(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    relation_wire: &str,
) -> Result<Vec<plasm_core::RelationBindingProof>, RelationLoweringError> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )
    .map_err(RelationLoweringError::CatalogOwnership)?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        RelationLoweringError::CatalogEntityNotFound {
            entry_id: row_qe.entry_id.to_string(),
            entity: row_qe.entity.to_string(),
        }
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        RelationLoweringError::RelationNotFound {
            entity: row_qe.entity.to_string(),
            relation: relation_wire.to_string(),
            hint: String::new(),
        }
    })?;
    let is_scoped_binding_materialize = matches!(
        rel.materialize.as_ref(),
        Some(
            plasm_core::RelationMaterialization::QueryScopedBindings { .. }
                | plasm_core::RelationMaterialization::GetScopedBindings { .. }
        )
    );
    if !is_scoped_binding_materialize {
        return Ok(Vec::new());
    }
    plasm_core::collect_relation_binding_proofs(cgs, ent, rel)
        .map_err(RelationLoweringError::BindingProof)
}

/// Resolve relation metadata for a parsed [`Expr::Chain`] (declared CGS relation on the source entity).
pub(in crate::plasm_dag) fn lookup_relation_chain_meta(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    chain: &plasm_core::ChainExpr,
    source_row_qe: Option<&QualifiedEntityKey>,
) -> Result<(QualifiedEntityKey, RelationCardinality), RelationLoweringError> {
    let federated = session.contexts_by_entry.len() > 1;
    let explicit_qe = source_row_qe
        .map(|qe| plasm_core::QualifiedEntityKey::new(qe.entry_id.clone(), qe.entity.clone()));
    let row_qe = plasm_core::catalog_ownership::require_relation_source_qualified_entity(
        &chain.source,
        federated,
        explicit_qe.as_ref(),
    )
    .map_err(RelationLoweringError::RelationSource)?;
    let cgs = if let Some(row_qe) = row_qe.as_ref() {
        let agent_qe = QualifiedEntityKey {
            entry_id: row_qe.entry_id().to_string(),
            entity: row_qe.entity.to_string(),
        };
        resolve_cgs_for_qualified_entity(session, &agent_qe).ok_or_else(|| {
            RelationLoweringError::CatalogEntityNotFound {
                entry_id: agent_qe.entry_id.to_string(),
                entity: agent_qe.entity.to_string(),
            }
        })?
    } else {
        let root_entity = chain.source.primary_entity();
        crate::catalog_ownership::resolve_cgs_for_entity(session, root_entity, None)
            .map_err(RelationLoweringError::CatalogOwnership)?
    };
    let root_entity = chain.source.primary_entity();
    let source_entity = chain
        .source
        .relation_navigation_entity(cgs)
        .ok_or_else(|| RelationLoweringError::CatalogEntityNotFound {
            entry_id: session.entry_id.to_string(),
            entity: root_entity.to_string(),
        })?;
    let source_entity = source_entity.as_str();
    let ent = cgs.get_entity(source_entity).ok_or_else(|| {
        RelationLoweringError::CatalogEntityNotFound {
            entry_id: row_qe
                .as_ref()
                .map(|qe| qe.entry_id().to_string())
                .unwrap_or_else(|| session.entry_id.to_string()),
            entity: source_entity.to_string(),
        }
    })?;
    let rel = ent.relations.get(chain.selector.as_str()).ok_or_else(|| {
        let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
        let entry_id = row_qe
            .as_ref()
            .map(|qe| qe.entry_id())
            .unwrap_or(session.entry_id.as_str());
        let teaching_relation =
            map.ident_sym_relation_for(entry_id, source_entity, chain.selector.as_str());
        let hint = if teaching_relation.starts_with("r#") {
            format!("; use the taught relation symbol `{teaching_relation}`")
        } else {
            String::new()
        };
        RelationLoweringError::RelationNotFound {
            entity: source_entity.to_string(),
            relation: chain.selector.to_string(),
            hint,
        }
    })?;
    let target_ent = rel.target_resource.as_str();
    if cgs.get_entity(target_ent).is_none() {
        return Err(RelationLoweringError::TargetEntityNotFound {
            entity: source_entity.to_string(),
            relation: chain.selector.to_string(),
            target: target_ent.to_string(),
        });
    }
    let qe = if let Some(row_qe) = row_qe {
        QualifiedEntityKey {
            entry_id: row_qe.entry_id().to_string(),
            entity: target_ent.to_string(),
        }
    } else {
        crate::catalog_ownership::resolve_qualified_entity_key(session, target_ent, Some(cgs))
            .map_err(RelationLoweringError::CatalogOwnership)?
    };
    let cardinality = match rel.cardinality {
        plasm_core::Cardinality::One => RelationCardinality::One,
        plasm_core::Cardinality::Many => RelationCardinality::Many,
    };
    Ok((qe, cardinality))
}
