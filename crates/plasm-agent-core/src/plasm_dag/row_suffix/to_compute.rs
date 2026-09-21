//! Typed [`RowSuffix`] -> compute DAG node.

use super::super::plan_serialize::{
    parse_aggregates, parse_dedupe_key_paths, parse_field_list,
    parse_group_by_key_and_aggregate_tail, parse_sort_field_and_direction, schema_from_aggregates,
    schema_from_group_by, schema_from_output_fields,
};
use super::super::prelude::*;
use super::super::schema_validate::{
    cgs_for_qualified_entity, compute_passthrough_or_fallback_schema,
    is_opaque_passthrough_compute_schema, resolve_compute_field_path,
    resolve_immediate_compute_schema, resolve_qualified_entity_for_dag_source,
    resolve_sort_field_path, validate_compute_paths_for_dag_source,
};
use super::super::types::{CompileState, DagNode, DagNodeSource};

/// Lower one typed [`RowSuffix`] transform into a compute DAG node.
pub(in crate::plasm_dag) fn row_suffix_to_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    suffix: &RowSuffix,
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
    match suffix {
        RowSuffix::Limit { count } => Ok(mk(
            ComputeOp::Limit {
                count: *count as usize,
            },
            compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanLimit"),
            *count <= 1,
        )),
        RowSuffix::Filter { body } => {
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
            let extra = resolve_immediate_compute_schema(state, staged, source);
            let row_schema_fields: Vec<String> = extra
                .as_ref()
                .filter(|schema| !is_opaque_passthrough_compute_schema(schema))
                .map(|schema| {
                    schema
                        .fields
                        .iter()
                        .map(|f| f.name.as_str().to_string())
                        .collect()
                })
                .unwrap_or_default();
            let layer = plasm_core::CgsLayer::new(qe.entry_id.as_str(), cgs.as_ref());
            let stack = [layer];
            let sym_map = state.sym_map_for(session);
            let core_qe =
                plasm_core::QualifiedEntityKey::new(qe.entry_id.as_str(), qe.entity.as_str());
            let tree = plasm_core::parse_boolean_filter(body.as_str())?;
            let predicates = tree.try_flat_map(&mut |clause| -> Result<plasm_core::BooleanExpr<PlanPredicate>, String> {
            let clauses = [clause.as_str()];
            let mut membership_preds = Vec::new();
            let mut scalar_clauses = Vec::new();
            for clause in clauses {
                match plasm_core::parse_membership_clause(clause)? {
                    Some(m) => {
                        let rhs = match m.rhs {
                            plasm_core::MembershipRhs::Binding(name) => name,
                            plasm_core::MembershipRhs::Pipe(_) => {
                                return Err(
                                    "internal: membership pipe RHS must be rewritten to a binding before filter lower"
                                        .into(),
                                );
                            }
                        };
                        if !state.contains(rhs.as_str()) && !staged.iter().any(|n| n.id == rhs) {
                            return Err(format!(
                                "membership RHS `{rhs}` is not a bound one-column rowset (RA-13)"
                            ));
                        }
                        let path = membership_rhs_column_path(state, staged, &rhs)?;
                        membership_preds.push(PlanPredicate {
                            field_path: FieldPath::from_dotted(m.field.as_str())?,
                            op: if m.anti {
                                PlanPredicateOp::NotIn
                            } else {
                                PlanPredicateOp::In
                            },
                            value: PlanValue::BindingSymbol { binding: rhs, path },
                        });
                    }
                    None => scalar_clauses.push(clause.to_string()),
                }
            }
            let row_pred = if scalar_clauses.is_empty() {
                plasm_core::RowPredicate(vec![])
            } else {
                plasm_core::parse_row_predicate_list(
                    qe.entity.as_str(),
                    &scalar_clauses.join(", "),
                    &stack,
                    sym_map.clone(),
                    &row_schema_fields, &state.program_node_id_set())?
            };
            let tc_ctx = plasm_core::RowPredicateTypeCtx {
                qe: &core_qe,
                cgs: cgs.as_ref(),
                symbol_map: None,
            };
            let mut predicates = if row_pred.0.is_empty() {
                Vec::new()
            } else {
                crate::row_predicate_lower::lower_row_predicate_to_plan(
                    &row_pred,
                    session,
                    &qe,
                    state.cross_cache,
                    &row_schema_fields,
                )?
            };
            for predicate in &predicates {
                for label in predicate.value.dependencies() {
                    let node = staged.iter().find(|n| n.id == label).or_else(|| state.get(&label))
                        .ok_or_else(|| format!("unknown scalar predicate binding `{label}`"))?;
                    let contract = super::super::binding_contract::binding_contract_for_node(state, &label, node);
                    if !contract.row_cardinality.permits_scalar_field_extract() {
                        return Err(format!("scalar predicate binding `{label}` is plural; select exactly one row before comparing its field"));
                    }
                }
            }
            predicates.extend(membership_preds);
            if predicates.is_empty() {
                return Err("filter(...) requires at least one predicate".into());
            }
            let mut catalog_pred = row_pred.clone();
            if !catalog_pred.0.is_empty() {
                if row_schema_fields.is_empty() {
                    plasm_core::type_check_row_predicate(&catalog_pred, &tc_ctx)
                        .map_err(|e| e.to_string())?;
                } else {
                    catalog_pred.0.retain(|c| {
                        cgs.get_entity(qe.entity.as_str())
                            .is_some_and(|ent| ent.fields.contains_key(c.field.as_str()))
                    });
                    if !catalog_pred.0.is_empty() {
                        plasm_core::type_check_row_predicate(&catalog_pred, &tc_ctx)
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            let mut paths = Vec::new();
            for clause in &row_pred.0 {
                paths.push(FieldPath::from_dotted(clause.field.as_str())?);
            }
            for pred in &predicates {
                if matches!(pred.op, PlanPredicateOp::In | PlanPredicateOp::NotIn) {
                    paths.push(pred.field_path.clone());
                }
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
            Ok(predicates.into())
            })?;
            let schema = compute_passthrough_or_fallback_schema(
                session,
                state,
                staged,
                source,
                "PlanFilter",
            );
            Ok(mk(ComputeOp::Filter { predicates }, schema, false))
        }
        RowSuffix::Sort { args } => {
            let (key, descending) = parse_sort_field_and_direction(args)?;
            if key.is_empty() {
                return Err("sort(...) requires a non-empty field".into());
            }
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let source_schema = resolve_immediate_compute_schema(state, staged, source);
            let key_fp = resolve_sort_field_path(
                session,
                state.cross_cache,
                qe.as_ref(),
                source_schema.as_ref(),
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
        RowSuffix::Aggregate { args } => {
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
        RowSuffix::GroupBy { args } => {
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
        RowSuffix::Dedupe { keys } | RowSuffix::Distinct { keys: Some(keys) } => {
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
            let schema = compute_passthrough_or_fallback_schema(
                session,
                state,
                staged,
                source,
                "PlanDedupe",
            );
            Ok(mk(ComputeOp::DedupeBy { keys: key_fps }, schema, false))
        }
        RowSuffix::Distinct { keys: None } => {
            let schema = compute_passthrough_or_fallback_schema(
                session,
                state,
                staged,
                source,
                "PlanDistinct",
            );
            Ok(mk(ComputeOp::DedupeBy { keys: vec![] }, schema, false))
        }
        RowSuffix::With { body } => {
            let columns = plasm_core::parse_with_body(body).map_err(|e| e.to_string())?;
            // Immediate grain when known (already-projected `| select`); else entity passthrough.
            // Assignment onto an existing field name replaces — `| select receiver_email = sender_email`
            // must not emit duplicate schema fields (plan validate rejects that as dishonest).
            let mut schema =
                compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanWith");
            for col in &columns {
                let remat = rematerialize_with_column(&schema, col);
                if let Some(existing) = schema
                    .fields
                    .iter_mut()
                    .find(|f| f.name.as_str() == col.name.as_str())
                {
                    existing.value_kind = remat.value_kind;
                    existing.source = remat.source;
                } else {
                    schema.fields.push(remat);
                }
            }
            Ok(mk(ComputeOp::With { columns }, schema, false))
        }
        RowSuffix::Project { fields } => {
            let fields_joined = fields.join(",");
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let source_schema = resolve_immediate_compute_schema(state, staged, source);
            let mut map = BTreeMap::new();
            for field in parse_field_list(session, state.cross_cache, qe.as_ref(), &fields_joined)
                .or_else(|_| {
                fields
                    .iter()
                    .map(|raw| {
                        let path = FieldPath::from_dotted(raw)?;
                        let resolved = resolve_sort_field_path(
                            session,
                            state.cross_cache,
                            qe.as_ref(),
                            source_schema.as_ref(),
                            &path,
                        )?;
                        Ok(resolved.dotted())
                    })
                    .collect::<Result<Vec<String>, String>>()
            })? {
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
        RowSuffix::Union { rhs } => {
            if !state.contains(rhs.as_str()) && !staged.iter().any(|n| n.id == *rhs) {
                return Err(format!("union RHS `{rhs}` is not a bound rowset (RA-14)"));
            }
            let left =
                compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanUnion");
            let right =
                compute_passthrough_or_fallback_schema(session, state, staged, rhs, "PlanUnion");
            if is_opaque_passthrough_compute_schema(&left)
                || is_opaque_passthrough_compute_schema(&right)
            {
                return Err("union requires known row columns; use `| select` to declare the same columns on both inputs (RA-14)".into());
            }
            let left_names: std::collections::BTreeSet<_> = left
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect();
            let right_names: std::collections::BTreeSet<_> = right
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect();
            if left_names != right_names {
                return Err(format!(
                    "union requires the same columns; left has [{}], right has [{}] (RA-14)",
                    left_names.into_iter().collect::<Vec<_>>().join(", "),
                    right_names.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
            let schema = left;
            Ok(mk(
                ComputeOp::Union {
                    other: OutputName::new(rhs.clone())?,
                },
                schema,
                false,
            ))
        }
        RowSuffix::Singleton | RowSuffix::PageSize { .. } => {
            Err("internal: singleton/page_size must be split as tail flags before lowering".into())
        }
        RowSuffix::Relation { .. } => {
            Err("internal: relation suffixes lower via binding continuation, not compute".into())
        }
    }
}

/// RA-13: membership RHS must project exactly one column.
fn membership_rhs_column_path(
    state: &CompileState<'_>,
    staged: &[DagNode],
    rhs: &str,
) -> Result<Vec<String>, String> {
    let schema = resolve_immediate_compute_schema(state, staged, rhs);
    let names: Vec<String> = schema
        .as_ref()
        .filter(|schema| !is_opaque_passthrough_compute_schema(schema))
        .map(|schema| {
            schema
                .fields
                .iter()
                .map(|f| f.name.as_str().to_string())
                .collect()
        })
        .unwrap_or_default();
    match names.as_slice() {
        [] => Ok(Vec::new()),
        [one] => Ok(vec![one.clone()]),
        _ => Err(format!(
            "membership RHS `{rhs}` must be one column; write `({rhs} | select field)` (RA-13)"
        )),
    }
}

/// RA-14: `| select dest = src` copies src type, identity path, and policy source onto dest.
fn rematerialize_with_column(
    schema: &SyntheticResultSchema,
    col: &plasm_core::WithColumn,
) -> plasm_core::SyntheticFieldSchema {
    let plasm_core::WithExpr::Field(fp) = &col.expr else {
        return plasm_core::SyntheticFieldSchema {
            name: col.name.clone(),
            value_kind: SyntheticValueKind::Unknown,
            source: None,
        };
    };
    if let Some(src) = schema.fields.iter().find(|f| {
        f.name.as_str() == fp.dotted()
            || f.source.as_ref().is_some_and(|s| s.dotted() == fp.dotted())
    }) {
        return plasm_core::SyntheticFieldSchema {
            name: col.name.clone(),
            value_kind: src.value_kind,
            source: src.source.clone().or_else(|| Some(fp.clone())),
        };
    }
    plasm_core::SyntheticFieldSchema {
        name: col.name.clone(),
        value_kind: SyntheticValueKind::Unknown,
        source: Some(fp.clone()),
    }
}
