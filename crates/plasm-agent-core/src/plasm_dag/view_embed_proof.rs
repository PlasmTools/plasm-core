//! Resolve validated view_embed producer proof during DAG lowering.

use super::prelude::*;
use super::relation::resolve_cgs_for_qualified_entity;
use super::types::{CompileState, DagNodeSource};
use plasm_core::expr::Expr;
use plasm_core::{ValidatedViewEmbedProof, CGS};

#[derive(Debug, thiserror::Error)]
pub enum ViewEmbedProofError {
    #[error("view_embed references unknown composed view `{view}`")]
    UnknownView { view: String },
    #[error("view_embed source `{binding}` has a cyclic binding chain")]
    CyclicBinding { binding: String },
    #[error("view_embed source references unknown binding `{binding}`")]
    UnknownBinding { binding: String },
    #[error("synthetic binding `{binding}` cannot produce view_embed parent rows for `{view}`")]
    SyntheticProducer { binding: String, view: String },
    #[error(
        "view_embed source `{binding}` did not resolve to a producer for `{view}` within 64 hops"
    )]
    ProducerDepthExceeded { binding: String, view: String },
    #[error("view_embed binding `{binding}` could not resolve its catalog entity")]
    ProducerEntityMissing { binding: String },
    #[error("unknown catalog entity `{entity}`")]
    CatalogEntityMissing { entity: String },
    #[error("binding `{binding}` is not a view root for `{view}`; execute the view before navigating view_embed relations")]
    NotViewRoot { binding: String, view: String },
    #[error(transparent)]
    RelationOwnership(#[from] plasm_core::catalog_ownership::CatalogOwnershipError),
    #[error("view `{view}` is not defined in catalog `{entry_id}`")]
    CatalogViewMissing { view: String, entry_id: String },
    #[error("read on `{entity}` lacks a capability")]
    ReadCapabilityMissing { entity: String },
    #[error("view_embed producer must be a view query/get surface")]
    InvalidProducerSurface,
    #[error("view `{view}` entity `{entity}` is unknown")]
    ViewEntityMissing { view: String, entity: String },
    #[error("view entity `{entity}` has no relation `{relation}` for view_embed proof")]
    ViewRelationMissing { entity: String, relation: String },
    #[error("view `{view}` does not declare relation_output `{relation}` required by binding `{binding}`")]
    RelationOutputMissing {
        view: String,
        relation: String,
        binding: String,
    },
}

pub(in crate::plasm_dag) fn resolve_view_embed_proof(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    source_label: &str,
    view_key: &str,
    relation_wire: &str,
) -> Result<ValidatedViewEmbedProof, ViewEmbedProofError> {
    let producer = find_view_producer_node(session, state, source_label, view_key)?;
    let cgs = resolve_cgs_for_view(session, &producer, view_key)?;
    let view = cgs
        .views
        .get(view_key)
        .ok_or_else(|| ViewEmbedProofError::UnknownView {
            view: view_key.to_owned(),
        })?;
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
) -> Result<ViewProducerMatch, ViewEmbedProofError> {
    let mut cur = start.to_string();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..64 {
        if !visited.insert(cur.clone()) {
            return Err(ViewEmbedProofError::CyclicBinding { binding: cur });
        }
        let node = state
            .get(cur.as_str())
            .ok_or_else(|| ViewEmbedProofError::UnknownBinding {
                binding: cur.clone(),
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
            DagNodeSource::Compute { source, .. }
            | DagNodeSource::Derive { source, .. }
            | DagNodeSource::ScalarExtract { source, .. } => {
                cur = source.clone();
            }
            DagNodeSource::MapBody { body, .. }
                if matches!(body.output, plasm_core::plasm_monad::ScopedOutput::Filter) =>
            {
                // A filter selects original rows; it does not construct new owners.
                cur = body.parent.source.to_string();
            }
            DagNodeSource::MapBody { .. } | DagNodeSource::Data(_) => {
                return Err(ViewEmbedProofError::SyntheticProducer {
                    binding: cur,
                    view: expected_view.to_owned(),
                });
            }
            DagNodeSource::ForEach { source, .. }
            | DagNodeSource::IterateUntil { seed: source, .. } => cur = source.clone(),
        }
    }
    Err(ViewEmbedProofError::ProducerDepthExceeded {
        binding: start.to_owned(),
        view: expected_view.to_owned(),
    })
}

fn match_surface_view_producer(
    session: &ExecuteSession,
    node_id: &str,
    expr: &Expr,
    expected_view: &str,
    entity_hint: Option<&QualifiedEntityKey>,
) -> Result<ViewProducerMatch, ViewEmbedProofError> {
    let root = chain_root(expr);
    let qe = entity_hint
        .cloned()
        .or_else(|| qualified_entity_for_chain_root(session, root, node_id).ok())
        .ok_or_else(|| ViewEmbedProofError::ProducerEntityMissing {
            binding: node_id.to_owned(),
        })?;
    let cgs = resolve_cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
        ViewEmbedProofError::CatalogEntityMissing {
            entity: qe.entity.to_string(),
        }
    })?;
    if !surface_executes_view(cgs, root, &qe, expected_view)? {
        return Err(ViewEmbedProofError::NotViewRoot {
            binding: node_id.to_owned(),
            view: expected_view.to_owned(),
        });
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
) -> Result<QualifiedEntityKey, ViewEmbedProofError> {
    let federated = session.contexts_by_entry.len() > 1;
    let row_qe = plasm_core::catalog_ownership::require_relation_source_qualified_entity(
        root, federated, None,
    )?;
    row_qe
        .map(|qe| QualifiedEntityKey {
            entry_id: qe.entry_id().to_string(),
            entity: qe.entity.to_string(),
        })
        .or_else(|| {
            let entity = root.primary_entity();
            crate::catalog_ownership::resolve_qualified_entity_key(session, entity, None).ok()
        })
        .ok_or_else(|| ViewEmbedProofError::ProducerEntityMissing {
            binding: binding_label.to_owned(),
        })
}

fn resolve_cgs_for_view<'a>(
    session: &'a ExecuteSession,
    producer: &ViewProducerMatch,
    view_key: &str,
) -> Result<&'a CGS, ViewEmbedProofError> {
    let cgs = resolve_cgs_for_qualified_entity(session, &producer.row_entity).ok_or_else(|| {
        ViewEmbedProofError::CatalogEntityMissing {
            entity: producer.row_entity.entity.to_string(),
        }
    })?;
    if !cgs.views.contains_key(view_key) {
        return Err(ViewEmbedProofError::CatalogViewMissing {
            view: view_key.to_owned(),
            entry_id: producer.row_entity.entry_id.to_string(),
        });
    }
    Ok(cgs)
}

fn surface_executes_view(
    cgs: &CGS,
    expr: &Expr,
    qe: &QualifiedEntityKey,
    expected_view: &str,
) -> Result<bool, ViewEmbedProofError> {
    let view = cgs
        .views
        .get(expected_view)
        .ok_or_else(|| ViewEmbedProofError::UnknownView {
            view: expected_view.to_owned(),
        })?;
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
    cgs.capabilities.get(cap.as_str()).is_some_and(|schema| {
        schema.domain.as_str() == view.entity.as_str()
            && schema.mapping.as_ref().is_some_and(|m| {
                m.template.0.get("transport").and_then(|t| t.as_str()) == Some("view")
                    && m.template.0.get("view").and_then(|v| v.as_str()) == Some(view_key)
            })
    })
}

fn capability_for_surface_expr(
    cgs: &CGS,
    expr: &Expr,
    qe: &QualifiedEntityKey,
) -> Result<plasm_core::CapabilityName, ViewEmbedProofError> {
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
                    .ok_or_else(|| ViewEmbedProofError::ReadCapabilityMissing {
                        entity: qe.entity.to_string(),
                    });
            }
            Expr::Chain(chain) => cur = chain.source.as_ref(),
            _ => {
                return Err(ViewEmbedProofError::InvalidProducerSurface);
            }
        }
    }
}

fn infer_view_query_capability(
    cgs: &CGS,
    qe: &QualifiedEntityKey,
) -> Result<plasm_core::CapabilityName, ViewEmbedProofError> {
    for view in cgs.views.values() {
        if view.entity.as_str() == qe.entity.as_str() {
            return Ok(plasm_core::CapabilityName::from(view.capability.as_str()));
        }
    }
    cgs.get_entity(qe.entity.as_str())
        .and_then(|e| e.primary_read.clone())
        .map(|s| plasm_core::CapabilityName::from(s.as_str()))
        .ok_or_else(|| ViewEmbedProofError::ReadCapabilityMissing {
            entity: qe.entity.to_string(),
        })
}

fn validate_view_relation_output(
    cgs: &CGS,
    view_key: &str,
    view: &plasm_core::schema::ViewDefinition,
    relation_wire: &str,
    producer_label: &str,
) -> Result<(), ViewEmbedProofError> {
    let ent = cgs.get_entity(view.entity.as_str()).ok_or_else(|| {
        ViewEmbedProofError::ViewEntityMissing {
            view: view_key.to_owned(),
            entity: view.entity.to_string(),
        }
    })?;
    let rel = ent.relations.get(relation_wire).ok_or_else(|| {
        ViewEmbedProofError::ViewRelationMissing {
            entity: view.entity.to_string(),
            relation: relation_wire.to_owned(),
        }
    })?;
    let ro_ok = view.relation_outputs.iter().any(|ro| {
        ro.relation.as_str() == relation_wire && ro.target.as_str() == rel.target_resource.as_str()
    });
    if !ro_ok {
        return Err(ViewEmbedProofError::RelationOutputMissing {
            view: view_key.to_owned(),
            relation: relation_wire.to_owned(),
            binding: producer_label.to_owned(),
        });
    }
    Ok(())
}
