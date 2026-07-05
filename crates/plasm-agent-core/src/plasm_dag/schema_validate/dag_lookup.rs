//! DAG node lookup and qualified-entity tracing.

use super::super::prelude::*;
use super::super::types::{CompileState, DagNode, DagNodeSource};
use super::catalog::{
    capability_for_surface_expr, cgs_for_qualified_entity, logical_row_field_paths_from_names,
};

pub(in crate::plasm_dag) fn resolve_surface_dag_node<'a>(
    state: &'a CompileState<'_>,
    staged: &'a [DagNode],
    mut node_id: String,
) -> Option<&'a DagNode> {
    for _ in 0..512 {
        let node = lookup_dag_node(state, staged, node_id.as_str())?;
        match &node.source {
            DagNodeSource::Surface { .. } | DagNodeSource::RelationTraversal { .. } => {
                return Some(node);
            }
            DagNodeSource::Compute { source, .. } => node_id = source.clone(),
            DagNodeSource::Derive { .. }
            | DagNodeSource::Data(_)
            | DagNodeSource::ForEach { .. } => return None,
        }
    }
    None
}
/// Row keys projected by the upstream surface capability (`provides`), when narrower than the entity.
pub(in crate::plasm_dag) fn logical_row_field_paths_for_surface_node(
    session: &ExecuteSession,
    node: &DagNode,
) -> Result<Option<BTreeSet<Vec<String>>>, String> {
    let (parsed, qe) = match &node.source {
        DagNodeSource::Surface {
            parsed,
            qualified_entity,
            ..
        } => (parsed, qualified_entity),
        DagNodeSource::RelationTraversal {
            parsed,
            qualified_entity,
            ..
        } => (parsed, qualified_entity),
        _ => return Ok(None),
    };
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let Some(cap) = capability_for_surface_expr(cgs.as_ref(), &parsed.expr)? else {
        return Ok(None);
    };
    let provides = cgs.effective_provides(cap);
    if provides.is_empty() {
        return Ok(None);
    }
    Ok(Some(logical_row_field_paths_from_names(&provides)))
}
pub(in crate::plasm_dag) fn lookup_dag_node<'a>(
    state: &'a CompileState<'_>,
    staged: &'a [DagNode],
    id: &str,
) -> Option<&'a DagNode> {
    state.get(id).or_else(|| staged.iter().find(|n| n.id == id))
}
pub(in crate::plasm_dag) fn resolve_immediate_compute_schema(
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
) -> Option<SyntheticResultSchema> {
    let node = staged
        .iter()
        .find(|n| n.id == source_id)
        .or_else(|| state.get(source_id))?;
    match &node.source {
        DagNodeSource::Compute { schema, .. } => Some(schema.clone()),
        _ => None,
    }
}
/// Walk [`DagNodeSource::Compute`] chains to the nearest surface or relation node that carries a
/// [`QualifiedEntityKey`] (the row entity after decode).
pub(in crate::plasm_dag) fn resolve_qualified_entity_for_dag_source(
    state: &CompileState<'_>,
    staged: &[DagNode],
    mut node_id: String,
) -> Option<QualifiedEntityKey> {
    for _ in 0..512 {
        let node = staged
            .iter()
            .find(|n| n.id == node_id)
            .or_else(|| state.get(node_id.as_str()))?;
        match &node.source {
            DagNodeSource::Surface {
                qualified_entity, ..
            }
            | DagNodeSource::RelationTraversal {
                qualified_entity, ..
            } => {
                return Some(qualified_entity.clone());
            }
            DagNodeSource::Compute { source, .. } => node_id = source.clone(),
            DagNodeSource::Derive { .. }
            | DagNodeSource::Data(_)
            | DagNodeSource::ForEach { .. } => return None,
        }
    }
    None
}
