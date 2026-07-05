//! Single postfix op → compute DAG node.

use super::super::plan_serialize::{
    parse_aggregates, parse_dedupe_key_paths, parse_field_list,
    parse_group_by_key_and_aggregate_tail, parse_sort_field_and_direction, schema_from_aggregates,
    schema_from_group_by, schema_from_output_fields,
};
use super::super::prelude::*;
use super::super::schema_validate::{
    cgs_for_qualified_entity, compute_passthrough_or_fallback_schema, resolve_compute_field_path,
    resolve_qualified_entity_for_dag_source, synthetic_schema_passthrough_rows,
    validate_compute_paths_for_dag_source,
};
use super::super::types::{CompileState, DagNode, DagNodeSource};

pub(in crate::plasm_dag) fn postfix_op_to_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    op: &PlasmPostfixOp,
    source: &str,
    id: &str,
    expr_display: &str,
) -> Result<DagNode, String> {
    let mk = |op: ComputeOp, schema: SyntheticResultSchema, singleton: bool| -> DagNode {
        DagNode {
            id: id.to_string(),
            expr: expr_display.to_string(),
            singleton,
            page_size: None,
            source: DagNodeSource::Compute {
                source: source.to_string(),
                op,
                schema,
                collection_alias: None,
            },
        }
    };
    match op {
        PlasmPostfixOp::Limit(n) => Ok(mk(
            ComputeOp::Limit { count: *n },
            compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanLimit"),
            *n <= 1,
        )),
        PlasmPostfixOp::Filter { body } => {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string())
                .ok_or_else(|| {
                    format!("filter(...) on `{source}` requires an upstream catalog entity row")
                })?;
            let cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
                format!(
                    "catalog `{}` is not loaded for entity `{}`",
                    qe.entry_id, qe.entity
                )
            })?;
            let layer = plasm_core::CgsLayer::new(qe.entry_id.as_str(), cgs.as_ref());
            let stack = [layer];
            let sym_map = state.sym_map_for(session);
            let core_qe =
                plasm_core::QualifiedEntityKey::new(qe.entry_id.as_str(), qe.entity.as_str());
            let row_pred = plasm_core::parse_row_predicate_list(
                qe.entity.as_str(),
                body.as_str(),
                &stack,
                sym_map,
            )?;
            let tc_ctx = plasm_core::RowPredicateTypeCtx {
                qe: &core_qe,
                cgs: cgs.as_ref(),
                symbol_map: None,
            };
            plasm_core::type_check_row_predicate(&row_pred, &tc_ctx).map_err(|e| e.to_string())?;
            let mut paths = Vec::new();
            for clause in &row_pred.0 {
                paths.push(FieldPath::from_dotted(clause.field.as_str())?);
            }
            if !paths.is_empty() {
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "filter(...)",
                )?;
            }
            let predicates = crate::row_predicate_lower::lower_row_predicate_to_plan(
                &row_pred,
                session,
                &qe,
                state.cross_cache,
            )?;
            let schema = synthetic_schema_passthrough_rows(session, state, staged, source)?;
            Ok(mk(ComputeOp::Filter { predicates }, schema, false))
        }
        PlasmPostfixOp::Sort { args } => {
            let (key, descending) = parse_sort_field_and_direction(args)?;
            if key.is_empty() {
                return Err("sort(...) requires a non-empty field".into());
            }
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let key_fp = resolve_compute_field_path(
                session,
                state.cross_cache,
                qe.as_ref(),
                &FieldPath::from_dotted(&key)?,
            )?;
            validate_compute_paths_for_dag_source(
                session,
                state,
                staged,
                source,
                std::slice::from_ref(&key_fp),
                "sort(...)",
            )?;
            let schema =
                compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanSort");
            Ok(mk(
                ComputeOp::Sort {
                    key: key_fp,
                    descending,
                },
                schema,
                false,
            ))
        }
        PlasmPostfixOp::Aggregate { args } => {
            let mut aggregates = parse_aggregates(args)?;
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            if let Some(qe) = qe.as_ref() {
                for agg in &mut aggregates {
                    if let Some(field) = agg.field.as_ref() {
                        agg.field = Some(resolve_compute_field_path(
                            session,
                            state.cross_cache,
                            Some(qe),
                            field,
                        )?);
                    }
                }
                let paths: Vec<FieldPath> =
                    aggregates.iter().filter_map(|a| a.field.clone()).collect();
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "aggregate(...)",
                )?;
            }
            let schema = schema_from_aggregates("PlanAggregate", &aggregates);
            Ok(mk(ComputeOp::Aggregate { aggregates }, schema, true))
        }
        PlasmPostfixOp::GroupBy { args } => {
            let (key_names, agg_tail) = parse_group_by_key_and_aggregate_tail(args)?;
            let aggregates = if agg_tail.trim().is_empty() {
                if key_names.len() != 1 {
                    return Err(
                        "group_by(k1, k2, …) without aggregates requires .aggregate(...) — use group_by(k1, k2).aggregate(n=count) or group_by(k1, k2, n=count)".into(),
                    );
                }
                // Bare `group_by(key)` sugar alias (single key only).
                parse_aggregates("count=count")?
            } else {
                parse_aggregates(agg_tail.as_str())?
            };
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let mut key_fps = Vec::new();
            for key in &key_names {
                key_fps.push(resolve_compute_field_path(
                    session,
                    state.cross_cache,
                    qe.as_ref(),
                    &FieldPath::from_dotted(key)?,
                )?);
            }
            let mut aggregates = aggregates;
            if let Some(qe) = qe.as_ref() {
                for agg in &mut aggregates {
                    if let Some(field) = agg.field.as_ref() {
                        agg.field = Some(resolve_compute_field_path(
                            session,
                            state.cross_cache,
                            Some(qe),
                            field,
                        )?);
                    }
                }
                let mut paths = key_fps.clone();
                paths.extend(aggregates.iter().filter_map(|a| a.field.clone()));
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "group_by(...)",
                )?;
            }
            let schema = schema_from_group_by("PlanGroup", &key_fps, &aggregates);
            Ok(mk(
                ComputeOp::GroupBy {
                    keys: key_fps,
                    aggregates,
                },
                schema,
                false,
            ))
        }
        PlasmPostfixOp::Dedupe { keys } | PlasmPostfixOp::Distinct { keys: Some(keys) } => {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let key_fps = parse_dedupe_key_paths(session, state.cross_cache, qe.as_ref(), keys)?;
            validate_compute_paths_for_dag_source(
                session,
                state,
                staged,
                source,
                &key_fps,
                "dedupe(...)",
            )?;
            let schema = synthetic_schema_passthrough_rows(session, state, staged, source)?;
            Ok(mk(ComputeOp::DedupeBy { keys: key_fps }, schema, false))
        }
        PlasmPostfixOp::Distinct { keys: None } => {
            let schema = synthetic_schema_passthrough_rows(session, state, staged, source)?;
            Ok(mk(ComputeOp::DedupeBy { keys: vec![] }, schema, false))
        }
        PlasmPostfixOp::Projection { fields } => {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let mut map = BTreeMap::new();
            for field in parse_field_list(session, state.cross_cache, qe.as_ref(), fields)? {
                map.insert(
                    OutputName::new(field.clone())?,
                    FieldPath::from_dotted(&field)?,
                );
            }
            if let Some(qe) = qe {
                let paths: Vec<FieldPath> = map.values().cloned().collect();
                validate_compute_paths_for_dag_source(
                    session,
                    state,
                    staged,
                    source,
                    &paths,
                    "postfix projection",
                )?;
                let entity = qe.entity.as_str();
                let schema =
                    schema_from_output_fields(entity, map.keys(), SyntheticValueKind::Unknown);
                return Ok(mk(ComputeOp::Project { fields: map }, schema, false));
            }
            let schema =
                schema_from_output_fields("PlanProject", map.keys(), SyntheticValueKind::Unknown);
            Ok(mk(ComputeOp::Project { fields: map }, schema, false))
        }
        PlasmPostfixOp::Singleton | PlasmPostfixOp::PageSize(_) => {
            Err("internal: singleton/page_size must be split as tail flags before lowering".into())
        }
    }
}
