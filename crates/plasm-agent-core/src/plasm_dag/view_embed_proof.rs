//! Resolve validated view_embed producer proof during DAG lowering.

use super::prelude::*;
use super::relation::resolve_cgs_for_qualified_entity;
use super::types::{CompileState, DagNodeSource};
use plasm_core::expr::Expr;
use plasm_core::{ValidatedViewEmbedProof, CGS};

pub(in crate::plasm_dag) fn resolve_view_embed_proof(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    source_label: &str,
    view_key: &str,
    relation_wire: &str,
) -> Result<ValidatedViewEmbedProof, String> {
    let producer = find_view_producer_node(session, state, source_label, view_key)?;
    let cgs = resolve_cgs_for_view(session, &producer, view_key)?;
    let view = cgs
        .views
        .get(view_key)
        .ok_or_else(|| format!("view_embed references unknown composed view `{view_key}`"))?;
    validate_view_relation_output(cgs, view_key, view, relation_wire, &producer.node_id)?;
    Ok(ValidatedViewEmbedProof::new(
        view_key.to_string(),
        producer.node_id,
        relation_wire.to_string(),
    ))
}

struct ViewProducerMatch {
    node_id: String,
    row_entity: QualifiedEntityKey,
}

fn find_view_producer_node(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    start: &str,
    expected_view: &str,
) -> Result<ViewProducerMatch, String> {
    let mut cur = start.to_string();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..64 {
        if !visited.insert(cur.clone()) {
            return Err(format!(
                "view_embed source `{cur}` has a cyclic binding chain — cannot resolve view producer"
            ));
        }
        let node = state.get(cur.as_str()).ok_or_else(|| {
            format!("view_embed source references unknown binding `{cur}`")
        })?;
        match &node.source {
            DagNodeSource::Surface {
                parsed,
                qualified_entity,
                ..
            } => {
                return match_surface_view_producer(
                    session,
                    cur.as_str(),
                    &parsed.expr,
                    expected_view,
                    Some(qualified_entity),
                );
            }
            DagNodeSource::RelationTraversal {
                source_label,
                plan_relation,
                parsed,
                ..
            } => {
                if source_label == &cur {
                    return match_surface_view_producer(
                        session,
                        cur.as_str(),
                        &parsed.expr,
                        expected_view,
                        None,
                    );
                }
                if let Some(proof) = &plan_relation.view_embed_proof {
                    if proof.view == expected_view {
                        cur = proof.producer_node.clone();
                        continue;
                    }
                }
                cur = source_label.clone();
            }
            DagNodeSource::Compute { source, .. } | DagNodeSource::Derive { source, .. } => {
                cur = source.clone();
            }
            DagNodeSource::Data(_) => {
                return Err(format!(
                    "synthetic binding `{cur}` cannot produce view_embed parent rows for `{expected_view}`"
                ));
            }
            DagNodeSource::ForEach { source, .. } => cur = source.clone(),
        }
    }
    Err(format!(
        "view_embed source `{start}` did not resolve to a view producer for `{expected_view}` within 64 hops"
    ))
}

fn match_surface_view_producer(
    session: &ExecuteSession,
    node_id: &str,
    expr: &Expr,
    expected_view: &str,
    entity_hint: Option<&QualifiedEntityKey>,
) -> Result<ViewProducerMatch, String> {
    let root = chain_root(expr);
    let qe = entity_hint
        .cloned()
        .or_else(|| qualified_entity_for_chain_root(session, root, node_id).ok())
        .ok_or_else(|| {
            format!(
                "view_embed binding `{node_id}` could not resolve catalog entity for chain root"
            )
        })?;
    let cgs = resolve_cgs_for_qualified_entity(session, &qe)
        .ok_or_else(|| format!("unknown catalog entity `{}`", qe.entity))?;
    if !surface_executes_view(cgs, root, &qe, expected_view)? {
        return Err(format!(
            "binding `{node_id}` is not a view root for `{expected_view}` — execute the view before navigating view_embed relations"
        ));
    }
    Ok(ViewProducerMatch {
        node_id: node_id.to_string(),
        row_entity: qe,
    })
}

fn chain_root(expr: &Expr) -> &Expr {
    match expr {
        Expr::Chain(chain) => chain.source.as_ref(),
        other => other,
    }
}

fn qualified_entity_for_chain_root(
    session: &ExecuteSession,
    root: &Expr,
    binding_label: &str,
) -> Result<QualifiedEntityKey, String> {
    let federated = session.contexts_by_entry.len() > 1;
    let row_qe = plasm_core::catalog_ownership::require_relation_source_qualified_entity(
        root,
        federated,
        None,
    )
    .map_err(|e| e.to_string())?;
    row_qe
        .map(|qe| QualifiedEntityKey {
            entry_id: qe.entry_id().to_string(),
            entity: qe.entity.to_string(),
        })
        .or_else(|| {
            let entity = root.primary_entity();
            crate::catalog_ownership::resolve_qualified_entity_key(session, entity, None).ok()
        })
        .ok_or_else(|| {
            format!(
                "view_embed binding `{binding_label}` could not resolve catalog entity for chain root"
            )
        })
}

fn resolve_cgs_for_view<'a>(
    session: &'a ExecuteSession,
    producer: &ViewProducerMatch,
    view_key: &str,
) -> Result<&'a CGS, String> {
    let cgs = resolve_cgs_for_qualified_entity(session, &producer.row_entity).ok_or_else(|| {
        format!(
            "unknown catalog entity `{}` for view `{view_key}`",
            producer.row_entity.entity
        )
    })?;
    if !cgs.views.contains_key(view_key) {
        return Err(format!(
            "view `{view_key}` is not defined in catalog `{}`",
            producer.row_entity.entry_id
        ));
    }
    Ok(cgs)
}

fn surface_executes_view(
    cgs: &CGS,
    expr: &Expr,
    qe: &QualifiedEntityKey,
    expected_view: &str,
) -> Result<bool, String> {
    let view = cgs
        .views
        .get(expected_view)
        .ok_or_else(|| format!("view_embed references unknown composed view `{expected_view}`"))?;
    if view.entity.as_str() != qe.entity.as_str() {
        return Ok(false);
    }
    let cap = capability_for_surface_expr(cgs, expr, qe)?;
    Ok(view_capability_matches(cgs, expected_view, view, &cap))
}

fn view_capability_matches(
    cgs: &CGS,
    view_key: &str,
    view: &plasm_core::schema::ViewDefinition,
    cap: &plasm_core::CapabilityName,
) -> bool {
    if cap.as_str() == view.capability.as_str() {
        return true;
    }
    cgs.capabilities
        .get(cap.as_str())
        .is_some_and(|schema| {
            schema.domain.as_str() == view.entity.as_str()
                && schema.mapping.template.0.get("transport").and_then(|t| t.as_str())
                    == Some("view")
                && schema
                    .mapping
                    .template
                    .0
                    .get("view")
                    .and_then(|v| v.as_str())
                    == Some(view_key)
        })
}

fn capability_for_surface_expr(
    cgs: &CGS,
    expr: &Expr,
    qe: &QualifiedEntityKey,
) -> Result<plasm_core::CapabilityName, String> {
    let mut cur = expr;
    loop {
        match cur {
            Expr::Query(q) => {
                if let Some(cap) = &q.capability_name {
                    return Ok(cap.clone());
                }
                return infer_view_query_capability(cgs, qe);
            }
            Expr::Get(g) => {
                if let Some(cap) = &g.capability_name {
                    return Ok(cap.clone());
                }
                return cgs
                    .get_entity(qe.entity.as_str())
                    .and_then(|e| e.primary_read.clone())
                    .map(|s| plasm_core::CapabilityName::from(s.as_str()))
                    .ok_or_else(|| format!("get on `{}` lacks capability", qe.entity));
            }
            Expr::Chain(chain) => cur = chain.source.as_ref(),
            _ => {
                return Err(format!(
                    "view_embed producer must be a view query/get surface, not `{cur:?}`"
                ));
            }
        }
    }
}

fn infer_view_query_capability(
    cgs: &CGS,
    qe: &QualifiedEntityKey,
) -> Result<plasm_core::CapabilityName, String> {
    for view in cgs.views.values() {
        if view.entity.as_str() == qe.entity.as_str() {
            return Ok(plasm_core::CapabilityName::from(view.capability.as_str()));
        }
    }
    cgs.get_entity(qe.entity.as_str())
        .and_then(|e| e.primary_read.clone())
        .map(|s| plasm_core::CapabilityName::from(s.as_str()))
        .ok_or_else(|| format!("query on `{}` lacks capability", qe.entity))
}

fn validate_view_relation_output(
    cgs: &CGS,
    view_key: &str,
    view: &plasm_core::schema::ViewDefinition,
    relation_wire: &str,
    producer_label: &str,
) -> Result<(), String> {
    let ent = cgs
        .get_entity(view.entity.as_str())
        .ok_or_else(|| format!("view `{view_key}` entity `{}` is unknown", view.entity))?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        format!(
            "view entity `{}` has no relation `{relation_wire}` for view_embed proof",
            view.entity
        )
    })?;
    let ro_ok = view.relation_outputs.iter().any(|ro| {
        ro.relation.as_str() == relation_wire && ro.target.as_str() == rel.target_resource.as_str()
    });
    if !ro_ok {
        return Err(format!(
            "view `{view_key}` does not declare relation_output `{relation_wire}` required by binding `{producer_label}`"
        ));
    }
    Ok(())
}
