//! CGS relation resolution and chain metadata for lowering.

use super::prelude::*;
use super::types::CompileState;

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
) -> Result<String, String> {
    let cgs = resolve_cgs_for_qualified_entity(session, row_qe).ok_or_else(|| {
        format!(
            "unknown catalog entity `{}` for relation continuation",
            row_qe.entity
        )
    })?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation continuation",
            row_qe.entity
        )
    })?;
    let map = symbol_map_for_plasm_surface_parse(session, cross_cache);
    let ctx = relation_segment_context(map.as_ref(), row_qe, ent, binding_label, true);
    match plasm_core::resolve_relation_segment(&ctx, segment) {
        plasm_core::RelationSegmentOutcome::Wire(w) => Ok(w),
        plasm_core::RelationSegmentOutcome::WrongRole { sym, wire } => {
            Err(plasm_core::plp::surface_err(
                plasm_core::plp::PlpId::Continuation,
                plasm_core::relation_segment_wrong_role_message(
                    &sym,
                    &wire,
                    row_qe.entity.as_str(),
                ),
            ))
        }
        plasm_core::RelationSegmentOutcome::NotFound => Err(plasm_core::plp::plp4_program(
            "",
            format!("entity `{}` has no relation `{segment}`", row_qe.entity),
        )),
    }
}

pub(in crate::plasm_dag) fn relation_continuation_expr_from_source_row_hole(
    session: &ExecuteSession,
    row_qe: &QualifiedEntityKey,
    relation_wire: &str,
) -> Result<Expr, String> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation continuation",
            row_qe.entity
        )
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        format!(
            "entity `{}` has no relation `{relation_wire}` for row-hole continuation",
            row_qe.entity
        )
    })?;
    let _target_ent = cgs
        .get_entity(rel.target_resource.as_str())
        .ok_or_else(|| {
            format!(
                "relation `{relation_wire}` on `{}` targets unknown entity `{}`",
                row_qe.entity, rel.target_resource
            )
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
        )?
    };
    let source_get = {
        let mut get = if ent.key_vars.is_empty() {
            let path_key = ent.id_field.as_str().to_string();
            let hole = Value::PlasmInputRef(PlasmInputRef::NodeInput {
                node: "source".into(),
                path: vec![path_key.clone()],
            });
            GetExpr::from_ref_with_path_vars(
                Ref::new(row_qe.entity.as_str(), ""),
                Some(indexmap::IndexMap::from([(path_key, hole)])),
            )
        } else {
            let mut path_vars = indexmap::IndexMap::new();
            for key in &ent.key_vars {
                path_vars.insert(
                    key.as_str().to_string(),
                    Value::PlasmInputRef(PlasmInputRef::NodeInput {
                        node: "source".into(),
                        path: vec![key.as_str().to_string()],
                    }),
                );
            }
            GetExpr::from_ref_with_path_vars(
                Ref {
                    entity_type: row_qe.entity.as_str().into(),
                    key: EntityKey::Compound(BTreeMap::new()),
                },
                Some(path_vars),
            )
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
    let parsed = parse_plasm_surface_line_program(
        session,
        state.cross_cache,
        state.pipeline,
        expr,
        Some(&refs),
        false,
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
    let base_parsed = parse_plasm_surface_line_program(
        session,
        state.cross_cache,
        state.pipeline,
        &base_expr,
        Some(&refs),
        false,
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
) -> Result<plasm_core::RelationMaterialization, String> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation materialize",
            row_qe.entity
        )
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        format!(
            "entity `{}` has no relation `{relation_wire}` for materialize",
            row_qe.entity
        )
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
) -> Result<Vec<plasm_core::RelationBindingProof>, String> {
    let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    )?;
    let ent = cgs.get_entity(row_qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` for relation binding proofs",
            row_qe.entity
        )
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        format!(
            "entity `{}` has no relation `{relation_wire}` for binding proofs",
            row_qe.entity
        )
    })?;
    plasm_core::collect_relation_binding_proofs(cgs, ent, rel)
}

/// Resolve relation metadata for a parsed [`Expr::Chain`] (declared CGS relation on the source entity).
pub(in crate::plasm_dag) fn lookup_relation_chain_meta(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    chain: &plasm_core::ChainExpr,
    source_row_qe: Option<&QualifiedEntityKey>,
) -> Result<(QualifiedEntityKey, RelationCardinality), String> {
    let federated = session.contexts_by_entry.len() > 1;
    let explicit_qe = source_row_qe
        .map(|qe| plasm_core::QualifiedEntityKey::new(qe.entry_id.clone(), qe.entity.clone()));
    let row_qe = plasm_core::catalog_ownership::require_relation_source_qualified_entity(
        &chain.source,
        federated,
        explicit_qe.as_ref(),
    )
    .map_err(|e| e.to_string())?;
    let cgs = if let Some(row_qe) = row_qe.as_ref() {
        let agent_qe = QualifiedEntityKey {
            entry_id: row_qe.entry_id().to_string(),
            entity: row_qe.entity.to_string(),
        };
        resolve_cgs_for_qualified_entity(session, &agent_qe).ok_or_else(|| {
            format!(
                "unknown catalog entity `{}` for entry `{}`",
                agent_qe.entity, agent_qe.entry_id
            )
        })?
    } else {
        let root_entity = chain.source.primary_entity();
        crate::catalog_ownership::resolve_cgs_for_entity(session, root_entity, None)?
    };
    let root_entity = chain.source.primary_entity();
    let source_entity = chain
        .source
        .relation_navigation_entity(cgs)
        .ok_or_else(|| {
            format!(
                "could not resolve relation navigation entity for chain continuing `{root_entity}`"
            )
        })?;
    let source_entity = source_entity.as_str();
    let ent = cgs.get_entity(source_entity).ok_or_else(|| {
        format!("unknown entity `{source_entity}` (Plasm program relation continuation)")
    })?;
    let rel = ent.relations.get(chain.selector.as_str()).ok_or_else(|| {
        let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
        let sym_note = row_qe
            .as_ref()
            .map(|qe| {
                let sym = map.ident_sym_relation_for(
                    qe.entry_id(),
                    source_entity,
                    chain.selector.as_str(),
                );
                if sym.as_str() != chain.selector.as_str() {
                    format!(
                        " Active teaching-table relation symbol for `{0}` on `{source_entity}` is `{sym}`.",
                        chain.selector
                    )
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        format!(
            "entity `{source_entity}` has no relation `{}` — use a declared catalog relation wire name or the `.p#` navigation slot from the active TSV teaching rows for `{source_entity}`.{sym_note}",
            chain.selector
        )
    })?;
    let target_ent = rel.target_resource.as_str();
    if cgs.get_entity(target_ent).is_none() {
        return Err(format!(
            "relation `{}` on entity `{}` targets unknown entity `{}` in the resolved catalog — `target` must name an `entities:` key (see CGS load validation / domain.yaml); field projection after this chain cannot be typed",
            chain.selector, source_entity, target_ent
        ));
    }
    let qe = if let Some(row_qe) = row_qe {
        QualifiedEntityKey {
            entry_id: row_qe.entry_id().to_string(),
            entity: target_ent.to_string(),
        }
    } else {
        crate::catalog_ownership::resolve_qualified_entity_key(session, target_ent, Some(cgs))?
    };
    let cardinality = match rel.cardinality {
        plasm_core::Cardinality::One => RelationCardinality::One,
        plasm_core::Cardinality::Many => RelationCardinality::Many,
    };
    Ok((qe, cardinality))
}
