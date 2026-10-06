//! Synthetic compute schemas and render column inference.

use super::super::plan_serialize::{schema_from_output_fields, single_unknown_schema};
use super::super::prelude::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PassthroughSchemaError {
    #[error("source does not resolve to a catalog entity row")]
    SourceEntityMissing,
    #[error("catalog entity has no materializable row fields")]
    EntityFieldsMissing,
    #[error(transparent)]
    Catalog(#[from] super::catalog::SchemaCatalogError),
    #[error(transparent)]
    Path(#[from] super::path_validate::SchemaPathValidationError),
    #[error(transparent)]
    ValueContract(#[from] plasm_core::value_contract::ValueContractError),
    #[error(transparent)]
    PlanAtom(#[from] plasm_core::plasm_monad::PlanAtomError),
}

#[derive(Debug, Error)]
pub enum RenderColumnInferenceError {
    #[error("render source is missing upstream node `{node}`")]
    MissingUpstream { node: String },
    #[error("data literals cannot provide inferred template columns")]
    DataLiteral,
    #[error("render source does not have an object row shape")]
    NonObjectSource,
    #[error("iteration bindings cannot provide inferred template columns")]
    IterationSource,
    #[error(transparent)]
    OutputName(#[from] plasm_core::plasm_monad::PlanAtomError),
    #[error(transparent)]
    Catalog(#[from] super::catalog::SchemaCatalogError),
}
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
) -> Result<Vec<OutputName>, RenderColumnInferenceError> {
    match &node.source {
        DagNodeSource::Compute {
            op,
            schema,
            source: parent_id,
            ..
        } => match op {
            ComputeOp::Project { fields } => Ok(fields.keys().cloned().collect()),
            ComputeOp::Aggregate { .. } => {
                Ok(schema.fields.iter().map(|f| f.name.clone()).collect())
            }
            ComputeOp::GroupBy { keys, aggregates } => {
                let mut cols = Vec::new();
                for key in keys {
                    cols.push(OutputName::new(key.dotted())?);
                }
                cols.extend(aggregates.iter().map(|a| a.name.clone()));
                Ok(cols)
            }
            ComputeOp::Sort { .. }
            | ComputeOp::Limit { .. }
            | ComputeOp::DedupeBy { .. }
            | ComputeOp::Filter { .. } => {
                let parent =
                    lookup_dag_node(state, staged, parent_id.as_str()).ok_or_else(|| {
                        RenderColumnInferenceError::MissingUpstream {
                            node: parent_id.clone(),
                        }
                    })?;
                infer_render_columns_for_node(session, state, staged, parent)
            }
            ComputeOp::With { .. } | ComputeOp::Union { .. } | ComputeOp::MergeBranches { .. } => {
                Ok(schema.fields.iter().map(|f| f.name.clone()).collect())
            }
            ComputeOp::Render { .. } | ComputeOp::Python { .. } => {
                Ok(schema.fields.iter().map(|f| f.name.clone()).collect())
            }
        },
        DagNodeSource::Surface {
            qualified_entity, ..
        }
        | DagNodeSource::RelationTraversal {
            qualified_entity, ..
        } => Ok(infer_entity_row_columns(session, qualified_entity)?),
        DagNodeSource::MapBody { schema, .. } => {
            Ok(schema.fields.iter().map(|f| f.name.clone()).collect())
        }
        DagNodeSource::Data(_) => Err(RenderColumnInferenceError::DataLiteral),
        DagNodeSource::Derive {
            value: PlanValue::Object { fields },
            ..
        } => fields
            .keys()
            .map(|name| OutputName::new(name.clone()).map_err(Into::into))
            .collect(),
        DagNodeSource::Derive { .. } | DagNodeSource::ScalarExtract { .. } => {
            Err(RenderColumnInferenceError::NonObjectSource)
        }
        DagNodeSource::ForEach { .. } | DagNodeSource::IterateUntil { .. } => {
            Err(RenderColumnInferenceError::IterationSource)
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
) -> Result<SyntheticResultSchema, PassthroughSchemaError> {
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string())
        .ok_or(PassthroughSchemaError::SourceEntityMissing)?;
    let cols = infer_entity_row_columns(session, &qe)?;
    if cols.is_empty() {
        return Err(PassthroughSchemaError::EntityFieldsMissing);
    }
    let mut schema =
        schema_from_output_fields(qe.entity.as_str(), cols.iter(), SyntheticValueKind::Unknown);
    let cgs = super::catalog::cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
        PassthroughSchemaError::Catalog(super::catalog::SchemaCatalogError::CatalogNotLoaded {
            entry_id: qe.entry_id.to_string(),
            entity: qe.entity.to_string(),
        })
    })?;
    let entity = cgs.get_entity(&qe.entity).ok_or_else(|| {
        PassthroughSchemaError::Catalog(super::catalog::SchemaCatalogError::EntityNotFound {
            entry_id: qe.entry_id.to_string(),
            entity: qe.entity.to_string(),
        })
    })?;
    schema.optional_fields = schema
        .fields
        .iter()
        .map(|field| field.name.to_string())
        .collect();
    for output in &mut schema.fields {
        if let Some(field) = entity.fields.get(output.name.as_str()) {
            let mut value_type = plasm_core::value_contract::ValueContract::from_domain(
                cgs.as_ref(),
                &qe.entry_id,
                field.kind.registry_key(),
            )?;
            value_type.nullable = !field.required;
            output.value_kind = value_type.summary();
            output.value_type = Some(value_type);
        } else if let Some(relation) = entity.relations.get(output.name.as_str()) {
            let value_type = crate::python_compute::observed_relation_type(relation, &qe.entry_id);
            output.value_kind = value_type.summary();
            output.value_type = Some(value_type);
        }
    }
    Ok(schema)
}

/// Identity [`ComputeOp::Project`] map plus schema for passthrough compute nodes (e.g. bare-label
/// `.page_size(n)` lowering).
pub(in crate::plasm_dag) fn passthrough_identity_projection_fields(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
) -> Result<(BTreeMap<OutputName, FieldPath>, SyntheticResultSchema), PassthroughSchemaError> {
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
