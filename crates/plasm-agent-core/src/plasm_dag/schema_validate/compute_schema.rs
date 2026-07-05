//! Synthetic compute schemas and render column inference.

use super::super::plan_serialize::{schema_from_output_fields, single_unknown_schema};
use super::super::prelude::*;
use super::super::types::{CompileState, DagNode, DagNodeSource};
use super::catalog::{infer_entity_row_columns, is_opaque_passthrough_compute_schema};
use super::dag_lookup::{
    lookup_dag_node, resolve_immediate_compute_schema, resolve_qualified_entity_for_dag_source,
};
use super::path_validate::validate_compute_paths_for_entity;

pub(in crate::plasm_dag) fn infer_render_columns_for_node(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    node: &DagNode,
) -> Result<Vec<OutputName>, String> {
    match &node.source {
        DagNodeSource::Compute {
            op,
            schema,
            source: parent_id,
            ..
        } => match op {
            ComputeOp::Project { fields } => Ok(fields.keys().cloned().collect()),
            ComputeOp::Aggregate { .. } => Ok(schema
                .fields
                .iter()
                .map(|f| f.name.clone())
                .collect()),
            ComputeOp::GroupBy { keys, aggregates } => {
                let mut cols = Vec::new();
                for key in keys {
                    cols.push(OutputName::new(key.dotted()).map_err(|e| e.to_string())?);
                }
                cols.extend(aggregates.iter().map(|a| a.name.clone()));
                Ok(cols)
            }
            ComputeOp::Sort { .. } | ComputeOp::Limit { .. } | ComputeOp::DedupeBy { .. } => {
                let parent = lookup_dag_node(state, staged, parent_id.as_str()).ok_or_else(|| {
                    format!("template column inference: missing upstream node `{parent_id}`")
                })?;
                infer_render_columns_for_node(session, state, staged, parent)
            }
            ComputeOp::Render { .. } => Err(
                "cannot infer columns from a row-to-text template result; bind a row-producing query/relation/projection, or write explicit `[field,...] <<TAG` columns before the template".into(),
            ),
            ComputeOp::Filter { .. } => {
                let parent = lookup_dag_node(state, staged, parent_id.as_str()).ok_or_else(|| {
                    format!("template column inference: missing upstream node `{parent_id}`")
                })?;
                infer_render_columns_for_node(session, state, staged, parent)
            }
        },
        DagNodeSource::Surface {
            qualified_entity, ..
        }
        | DagNodeSource::RelationTraversal {
            qualified_entity, ..
        } => infer_entity_row_columns(session, qualified_entity),
        DagNodeSource::Data(_) => Err(
            "data literals cannot provide inferred template columns; use explicit `[field,...] <<TAG` columns or bind a query".into(),
        ),
        DagNodeSource::Derive { .. } => {
            Err("derive bindings cannot provide inferred template columns".into())
        }
        DagNodeSource::ForEach { .. } => {
            Err("for_each bindings cannot provide inferred template columns".into())
        }
    }
}
pub(in crate::plasm_dag) fn compute_passthrough_or_fallback_schema(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source: &str,
    fallback_entity: &str,
) -> SyntheticResultSchema {
    resolve_immediate_compute_schema(state, staged, source)
        .filter(|s| !is_opaque_passthrough_compute_schema(s))
        .unwrap_or_else(|| {
            synthetic_schema_passthrough_rows(session, state, staged, source)
                .unwrap_or_else(|_| single_unknown_schema(fallback_entity))
        })
}

/// Schema describing passthrough rows from `source_id` when it resolves to a catalog entity surface
/// or relation node (preserves [`SyntheticResultSchema::entity`] for downstream plan validation).
pub(in crate::plasm_dag) fn synthetic_schema_passthrough_rows(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
) -> Result<SyntheticResultSchema, String> {
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string())
        .ok_or_else(|| {
            format!(
                "bare-label `.singleton()` / `.page_size(...)` requires `{source_id}` to trace to a catalog entity row (surface query or relation); synthetic binds and literals cannot use this postfix here"
            )
        })?;
    let cols = infer_entity_row_columns(session, &qe)?;
    if cols.is_empty() {
        return Err(format!(
            "Plasm internal: cannot infer passthrough columns for entity `{}`",
            qe.entity
        ));
    }
    Ok(schema_from_output_fields(
        qe.entity.as_str(),
        cols.iter(),
        SyntheticValueKind::Unknown,
    ))
}

/// Identity [`ComputeOp::Project`] map plus schema for passthrough compute nodes (e.g. bare-label
/// `.page_size(n)` lowering).
pub(in crate::plasm_dag) fn passthrough_identity_projection_fields(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
) -> Result<(BTreeMap<OutputName, FieldPath>, SyntheticResultSchema), String> {
    let schema = synthetic_schema_passthrough_rows(session, state, staged, source_id)?;
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string())
        .expect("trace matches synthetic_schema_passthrough_rows");
    let mut map = BTreeMap::new();
    for field in &schema.fields {
        let path = FieldPath::from_dotted(field.name.as_str())?;
        map.insert(field.name.clone(), path);
    }
    let paths: Vec<FieldPath> = map.values().cloned().collect();
    validate_compute_paths_for_entity(
        session,
        state.cross_cache,
        &qe,
        &paths,
        "bare-label passthrough projection",
    )?;
    Ok((map, schema))
}
