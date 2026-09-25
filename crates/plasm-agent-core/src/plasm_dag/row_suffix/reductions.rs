//! Shared typed reduction lowering for both source frontends.
use super::super::plan_serialize::{schema_from_aggregates, schema_from_group_by};
use super::super::prelude::*;
use super::super::schema_validate::{
    compute_passthrough_or_fallback_schema, resolve_immediate_compute_schema,
    resolve_qualified_entity_for_dag_source, resolve_sort_field_path,
    validate_compute_paths_for_dag_source,
};
use super::super::types::{CompileState, DagNode, DagNodeSource};
use plasm_core::plasm_monad::AggregateSpec;

fn resolve_paths(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source: &str,
    paths: &mut [FieldPath],
) -> Result<(), String> {
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_owned());
    let schema = resolve_immediate_compute_schema(state, staged, source);
    for path in paths.iter_mut() {
        *path = resolve_sort_field_path(
            session,
            state.cross_cache,
            qe.as_ref(),
            schema.as_ref(),
            path,
        )?;
    }
    validate_compute_paths_for_dag_source(session, state, staged, source, paths, "row reduction")
}

// Shared lowering receives the same explicit compilation context as row suffixes.
#[allow(clippy::too_many_arguments)]
pub(in crate::plasm_dag) fn lower_reduction_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source: &str,
    id: &str,
    display: &str,
    keys: Option<Vec<FieldPath>>,
    mut aggregates: Vec<AggregateSpec>,
) -> Result<DagNode, String> {
    if aggregates.is_empty() {
        return Err("reduction requires at least one named aggregate".into());
    }
    let mut keys = keys;
    if let Some(keys) = &mut keys {
        if keys.is_empty() {
            return Err("group_by requires at least one key".into());
        }
        resolve_paths(session, state, staged, source, keys)?;
    }
    let mut names = std::collections::BTreeSet::new();
    for key in keys.iter().flatten() {
        if !names.insert(key.dotted()) {
            return Err("duplicate group key".into());
        }
    }
    for aggregate in &mut aggregates {
        if !names.insert(aggregate.name.as_str().to_owned()) {
            return Err("duplicate reduction output column".into());
        }
        if let Some(field) = &mut aggregate.field {
            resolve_paths(session, state, staged, source, std::slice::from_mut(field))?;
        }
    }
    let singleton = keys.is_none();
    let mut schema = match &keys {
        Some(keys) => schema_from_group_by("PlanGroup", keys, &aggregates),
        None => schema_from_aggregates("PlanAggregate", &aggregates),
    };
    // Retain the current row grain's types for keys and value-preserving reductions.
    let input = compute_passthrough_or_fallback_schema(
        session,
        state,
        staged,
        source,
        "PlanReductionInput",
    );
    let source_field = |path: &FieldPath| {
        input
            .fields
            .iter()
            .find(|field| field.name.as_str() == path.dotted())
    };
    let key_count = keys.as_ref().map_or(0, Vec::len);
    for (output, key) in schema.fields.iter_mut().zip(keys.iter().flatten()) {
        if let Some(field) = source_field(key) {
            output.value_type = field.value_type.clone();
            output.value_kind = field.value_kind;
            output.source = field.source.clone();
        }
    }
    for (output, aggregate) in schema.fields.iter_mut().skip(key_count).zip(&aggregates) {
        let input_type = aggregate
            .field
            .as_ref()
            .and_then(source_field)
            .and_then(|f| f.value_type.as_ref());
        let value_type =
            plasm_core::value_contract::ValueContract::aggregate(aggregate.function, input_type);
        output.value_kind = value_type.summary();
        output.value_type = Some(value_type);
    }
    let op = match keys {
        Some(keys) => ComputeOp::GroupBy { keys, aggregates },
        None => ComputeOp::Aggregate { aggregates },
    };
    Ok(DagNode {
        id: id.into(),
        expr: display.into(),
        singleton,
        page_size: None,
        source: DagNodeSource::Compute {
            source: source.into(),
            op,
            schema,
            collection_alias: None,
        },
    })
}

pub(in crate::plasm_dag) fn lower_distinct_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source: &str,
    id: &str,
    display: &str,
    mut keys: Vec<FieldPath>,
) -> Result<DagNode, String> {
    resolve_paths(session, state, staged, source, &mut keys)?;
    let mut seen = std::collections::BTreeSet::new();
    if keys.iter().any(|key| !seen.insert(key.dotted())) {
        return Err("duplicate distinct key".into());
    }
    let schema =
        compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanDedupe");
    Ok(DagNode {
        id: id.into(),
        expr: display.into(),
        singleton: false,
        page_size: None,
        source: DagNodeSource::Compute {
            source: source.into(),
            op: ComputeOp::DedupeBy { keys },
            schema,
            collection_alias: None,
        },
    })
}
