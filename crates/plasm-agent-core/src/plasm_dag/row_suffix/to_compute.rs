//! Typed [`RowSuffix`] -> compute DAG node.

use super::super::plan_serialize::{
    parse_aggregates, parse_field_list, parse_group_by_key_and_aggregate_tail,
    parse_sort_field_and_direction, schema_from_output_fields, AggregateSpecError, SortSpecError,
};
use super::super::prelude::*;
use super::super::schema_validate::{
    cgs_for_qualified_entity, compute_passthrough_or_fallback_schema,
    is_opaque_passthrough_compute_schema, resolve_immediate_compute_schema,
    resolve_qualified_entity_for_dag_source, resolve_schema_field_path,
    validate_compute_paths_for_dag_source,
};
use super::super::types::{CompileState, DagNode, DagNodeSource};

#[derive(Debug, thiserror::Error)]
pub enum RowSuffixLoweringError {
    #[error(transparent)]
    Aggregate(#[from] AggregateSpecError),
    #[error(transparent)]
    RenderField(#[from] crate::plasm_render_compile::RenderFieldListError),
    #[error(transparent)]
    GroupBy(#[from] super::super::plan_serialize::GroupByError),
    #[error(transparent)]
    Sort(#[from] SortSpecError),
    #[error(transparent)]
    Atom(#[from] plasm_core::plasm_monad::PlanAtomError),
    #[error(transparent)]
    Reduction(#[from] super::ReductionLoweringError),
    #[error(transparent)]
    RowContract(#[from] plasm_core::row_plan::contracts::RowContractError),
    #[error(transparent)]
    Predicate(#[from] crate::row_predicate_lower::RowPredicateLoweringError),
    #[error(transparent)]
    BooleanFilter(#[from] plasm_core::BooleanFilterError),
    #[error(transparent)]
    RowPredicate(Box<plasm_core::RowPredicateError>),
    #[error(transparent)]
    Type(#[from] plasm_core::TypeError),
    #[error(transparent)]
    SchemaPath(#[from] super::super::schema_validate::SchemaPathValidationError),
    #[error(transparent)]
    SchemaCatalog(#[from] super::super::schema_validate::SchemaCatalogError),
    #[error(transparent)]
    WithExpression(#[from] plasm_core::plasm_monad::WithExprError),
    #[error("sort requires a non-empty field")]
    EmptySortField,
    #[error("filter source `{binding_source}` has no catalog entity row")]
    FilterSourceEntityMissing { binding_source: String },
    #[error("filter source catalog `{entry_id}` is not loaded for entity `{entity}`")]
    FilterCatalogMissing { entry_id: String, entity: String },
    #[error("membership pipe RHS must be rewritten to a binding before filter lowering")]
    UnrewrittenMembershipPipe,
    #[error("membership RHS `{binding}` is not a bound one-column rowset")]
    MembershipRhsNotBound { binding: String },
    #[error("scalar predicate binding `{binding}` is unknown")]
    UnknownScalarPredicateBinding { binding: String },
    #[error(
        "scalar predicate binding `{binding}` is plural; select one row before comparing its field"
    )]
    PluralScalarPredicateBinding { binding: String },
    #[error("filter requires at least one predicate")]
    EmptyFilter,
    #[error("group_by with multiple keys requires explicit aggregates")]
    GroupByAggregatesRequired,
    #[error("union RHS `{binding}` is not a bound rowset")]
    UnionRhsNotBound { binding: String },
    #[error("union requires known row columns on both inputs")]
    UnionColumnsUnknown,
    #[error("union input columns differ")]
    UnionColumnsDiffer {
        left: Vec<String>,
        right: Vec<String>,
    },
    #[error("union field `{field}` has incompatible value shapes")]
    UnionFieldShapeMismatch { field: String },
    #[error("singleton and page_size modifiers must be handled before compute lowering")]
    TailModifierInComputeLowering,
    #[error("relation suffix must be lowered through binding continuation")]
    RelationInComputeLowering,
    #[error("membership RHS `{binding}` must contain exactly one column")]
    MembershipRhsColumnCount { binding: String },
    #[error(transparent)]
    MembershipParse {
        source: Box<plasm_core::RowMembershipParseError>,
    },
}

impl From<plasm_core::RowPredicateError> for RowSuffixLoweringError {
    fn from(error: plasm_core::RowPredicateError) -> Self {
        Self::RowPredicate(Box::new(error))
    }
}

/// Shared typed sort lowering for surface and Python programs.
pub(in crate::plasm_dag) fn lower_sort_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source: &str,
    id: &str,
    expr_display: &str,
    (key, descending): (&str, bool),
) -> Result<DagNode, RowSuffixLoweringError> {
    if key.is_empty() {
        return Err(RowSuffixLoweringError::EmptySortField);
    }
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
    let source_schema = resolve_immediate_compute_schema(state, staged, source);
    let key_fp = resolve_schema_field_path(
        session,
        state.cross_cache,
        qe.as_ref(),
        source_schema.as_ref(),
        &FieldPath::from_dotted(key)?,
    )?;
    validate_compute_paths_for_dag_source(
        session,
        state,
        staged,
        source,
        std::slice::from_ref(&key_fp),
        "sort(...)",
    )?;
    let schema = compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanSort");
    Ok(DagNode {
        id: id.to_owned(),
        expr: expr_display.to_owned(),
        singleton: false,
        page_size: None,
        source: DagNodeSource::Compute {
            source: source.to_owned(),
            op: ComputeOp::Sort {
                key: key_fp,
                descending,
            },
            schema,
            collection_alias: None,
        },
    })
}

/// Lower typed derived columns while retaining source-field provenance.
pub(in crate::plasm_dag) fn lower_with_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source: &str,
    id: &str,
    expr_display: &str,
    mut columns: Vec<plasm_core::WithColumn>,
) -> Result<DagNode, RowSuffixLoweringError> {
    let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
    let source_schema = resolve_immediate_compute_schema(state, staged, source);
    for column in &mut columns {
        column.expr = column.expr.try_map_fields(&mut |path| {
            let resolved = resolve_schema_field_path(
                session,
                state.cross_cache,
                qe.as_ref(),
                source_schema.as_ref(),
                path,
            )?;
            validate_compute_paths_for_dag_source(
                session,
                state,
                staged,
                source,
                std::slice::from_ref(&resolved),
                "computed expression",
            )?;
            Ok::<_, RowSuffixLoweringError>(resolved)
        })?;
    }
    // Immediate grain when known (already-projected `| select`); else entity passthrough.
    // Assignment onto an existing field name replaces — `| select receiver_email = sender_email`
    // must not emit duplicate schema fields (plan validate rejects that as dishonest).
    let mut schema =
        compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanWith");
    let input_schema = schema.clone();
    for col in &columns {
        schema.optional_fields.remove(col.name.as_str());
        let remat = rematerialize_with_column(&input_schema, col)?;
        if let Some(existing) = schema
            .fields
            .iter_mut()
            .find(|f| f.name.as_str() == col.name.as_str())
        {
            existing.value_type = remat.value_type;
            existing.value_kind = remat.value_kind;
            existing.source = remat.source;
        } else {
            schema.fields.push(remat);
        }
    }
    Ok(DagNode {
        id: id.to_owned(),
        expr: expr_display.to_owned(),
        singleton: false,
        page_size: None,
        source: DagNodeSource::Compute {
            source: source.to_owned(),
            op: ComputeOp::With { columns },
            schema,
            collection_alias: None,
        },
    })
}

/// Lower one typed [`RowSuffix`] transform into a compute DAG node.
pub(in crate::plasm_dag) fn row_suffix_to_compute(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    suffix: &RowSuffix,
    source: &str,
    id: &str,
    expr_display: &str,
) -> Result<DagNode, RowSuffixLoweringError> {
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
                .ok_or_else(|| RowSuffixLoweringError::FilterSourceEntityMissing {
                    binding_source: source.to_owned(),
                })?;
            let cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
                RowSuffixLoweringError::FilterCatalogMissing {
                    entry_id: qe.entry_id.to_string(),
                    entity: qe.entity.to_string(),
                }
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
            let predicates = tree.try_flat_map(&mut |clause| -> Result<
                plasm_core::BooleanExpr<PlanPredicate>,
                RowSuffixLoweringError,
            > {
                let clauses = [clause.as_str()];
                let mut membership_preds = Vec::new();
                let mut scalar_clauses = Vec::new();
                for clause in clauses {
                    match plasm_core::parse_membership_clause(clause).map_err(|source| {
                        RowSuffixLoweringError::MembershipParse {
                            source: Box::new(source),
                        }
                    })? {
                        Some(m) => {
                            let rhs = match m.rhs {
                                plasm_core::MembershipRhs::Binding(name) => name,
                                plasm_core::MembershipRhs::Pipe(_) => {
                                    return Err(RowSuffixLoweringError::UnrewrittenMembershipPipe);
                                }
                            };
                            if !state.contains(rhs.as_str()) && !staged.iter().any(|n| n.id == rhs)
                            {
                                return Err(RowSuffixLoweringError::MembershipRhsNotBound {
                                    binding: rhs,
                                });
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
                        &row_schema_fields,
                        &state.program_node_id_set(),
                    )?
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
                        let node = staged
                            .iter()
                            .find(|n| n.id == label)
                            .or_else(|| state.get(&label))
                            .ok_or_else(|| {
                                RowSuffixLoweringError::UnknownScalarPredicateBinding {
                                    binding: label.clone(),
                                }
                            })?;
                        let contract = super::super::binding_contract::binding_contract_for_node(
                            state, &label, node,
                        );
                        if !contract.row_cardinality.permits_scalar_field_extract() {
                            return Err(RowSuffixLoweringError::PluralScalarPredicateBinding {
                                binding: label,
                            });
                        }
                    }
                }
                predicates.extend(membership_preds);
                if predicates.is_empty() {
                    return Err(RowSuffixLoweringError::EmptyFilter);
                }
                let mut catalog_pred = row_pred.clone();
                if !catalog_pred.0.is_empty() {
                    if row_schema_fields.is_empty() {
                        plasm_core::type_check_row_predicate(&catalog_pred, &tc_ctx)?;
                    } else {
                        catalog_pred.0.retain(|c| {
                            cgs.get_entity(qe.entity.as_str())
                                .is_some_and(|ent| ent.fields.contains_key(c.field.as_str()))
                        });
                        if !catalog_pred.0.is_empty() {
                            plasm_core::type_check_row_predicate(&catalog_pred, &tc_ctx)?;
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
            lower_sort_compute(
                session,
                state,
                staged,
                source,
                id,
                expr_display,
                (&key, descending),
            )
        }
        RowSuffix::Aggregate { args } => super::lower_reduction_compute(
            session,
            state,
            staged,
            source,
            id,
            expr_display,
            None,
            parse_aggregates(args)?,
        )
        .map_err(RowSuffixLoweringError::Reduction),
        RowSuffix::GroupBy { args } => {
            let (keys, tail) = parse_group_by_key_and_aggregate_tail(args)?;
            let aggregates = if tail.trim().is_empty() {
                if keys.len() != 1 {
                    return Err(RowSuffixLoweringError::GroupByAggregatesRequired);
                }
                parse_aggregates("count=count")?
            } else {
                parse_aggregates(&tail)?
            };
            let keys = keys
                .iter()
                .map(|key| FieldPath::from_dotted(key))
                .collect::<Result<_, _>>()?;
            super::lower_reduction_compute(
                session,
                state,
                staged,
                source,
                id,
                expr_display,
                Some(keys),
                aggregates,
            )
            .map_err(RowSuffixLoweringError::Reduction)
        }
        RowSuffix::Dedupe { keys } | RowSuffix::Distinct { keys: Some(keys) } => {
            let keys = keys
                .split(',')
                .map(|key| FieldPath::from_dotted(key.trim()))
                .collect::<Result<_, _>>()?;
            super::lower_distinct_compute(session, state, staged, source, id, expr_display, keys)
                .map_err(RowSuffixLoweringError::Reduction)
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
            let columns = plasm_core::parse_with_body(body)?;
            lower_with_compute(session, state, staged, source, id, expr_display, columns)
        }
        RowSuffix::Project { fields } => {
            let fields_joined = fields.join(",");
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source.to_string());
            let source_schema = resolve_immediate_compute_schema(state, staged, source);
            let mut map = BTreeMap::new();
            let parsed_fields =
                parse_field_list(session, state.cross_cache, qe.as_ref(), &fields_joined);
            let fields = match parsed_fields {
                Ok(fields) => fields,
                Err(_) => fields
                    .iter()
                    .map(|raw| {
                        let path = FieldPath::from_dotted(raw)?;
                        let resolved = resolve_schema_field_path(
                            session,
                            state.cross_cache,
                            qe.as_ref(),
                            source_schema.as_ref(),
                            &path,
                        )?;
                        Ok::<_, RowSuffixLoweringError>(resolved.dotted())
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            for field in fields {
                map.insert(
                    OutputName::new(field.clone())?,
                    FieldPath::from_dotted(&field)?,
                );
            }
            let paths: Vec<FieldPath> = map.values().cloned().collect();
            validate_compute_paths_for_dag_source(
                session,
                state,
                staged,
                source,
                &paths,
                "postfix projection",
            )?;
            let input = compute_passthrough_or_fallback_schema(
                session,
                state,
                staged,
                source,
                "PlanProject",
            );
            let mut schema = schema_from_output_fields(
                input.entity.as_deref().unwrap_or("PlanProject"),
                map.keys(),
                SyntheticValueKind::Unknown,
            );
            for field in &mut schema.fields {
                if let Some(original) = input.fields.iter().find(|f| f.name == field.name) {
                    field.value_type = original.value_type.clone();
                    field.value_kind = original.value_kind;
                    field.source = original.source.clone();
                }
            }
            Ok(mk(ComputeOp::Project { fields: map }, schema, false))
        }
        RowSuffix::Union { rhs } => {
            if !state.contains(rhs.as_str()) && !staged.iter().any(|n| n.id == *rhs) {
                return Err(RowSuffixLoweringError::UnionRhsNotBound {
                    binding: rhs.clone(),
                });
            }
            let left =
                compute_passthrough_or_fallback_schema(session, state, staged, source, "PlanUnion");
            let right =
                compute_passthrough_or_fallback_schema(session, state, staged, rhs, "PlanUnion");
            if is_opaque_passthrough_compute_schema(&left)
                || is_opaque_passthrough_compute_schema(&right)
            {
                return Err(RowSuffixLoweringError::UnionColumnsUnknown);
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
                return Err(RowSuffixLoweringError::UnionColumnsDiffer {
                    left: left_names.into_iter().map(str::to_owned).collect(),
                    right: right_names.into_iter().map(str::to_owned).collect(),
                });
            }
            let mut schema = left;
            for field in &mut schema.fields {
                let other = right
                    .fields
                    .iter()
                    .find(|other| other.name == field.name)
                    .expect("union columns checked above");
                field.value_type = match (field.value_type.take(), other.value_type.clone()) {
                    (Some(left), Some(right)) => {
                        if left.shape != right.shape {
                            return Err(RowSuffixLoweringError::UnionFieldShapeMismatch {
                                field: field.name.to_string(),
                            });
                        }
                        Some(plasm_core::value_contract::ValueContract::join(left, right))
                    }
                    _ => None,
                };
                if let Some(contract) = &field.value_type {
                    field.value_kind = contract.summary();
                }
                if field.source != other.source {
                    field.source = None;
                }
            }
            schema.optional_fields.extend(right.optional_fields);
            Ok(mk(
                ComputeOp::Union {
                    other: OutputName::new(rhs.clone())?,
                },
                schema,
                false,
            ))
        }
        RowSuffix::Singleton | RowSuffix::PageSize { .. } => {
            Err(RowSuffixLoweringError::TailModifierInComputeLowering)
        }
        RowSuffix::Relation { .. } => Err(RowSuffixLoweringError::RelationInComputeLowering),
    }
}

/// RA-13: membership RHS must project exactly one column.
pub(in crate::plasm_dag) fn membership_rhs_column_path(
    state: &CompileState<'_>,
    staged: &[DagNode],
    rhs: &str,
) -> Result<Vec<String>, RowSuffixLoweringError> {
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
        _ => Err(RowSuffixLoweringError::MembershipRhsColumnCount {
            binding: rhs.to_owned(),
        }),
    }
}

/// RA-14: `| select dest = src` copies src type, identity path, and policy source onto dest.
fn rematerialize_with_column(
    schema: &SyntheticResultSchema,
    col: &plasm_core::WithColumn,
) -> Result<plasm_core::SyntheticFieldSchema, plasm_core::row_plan::contracts::RowContractError> {
    let plasm_core::WithExpr::Field(fp) = &col.expr else {
        let value_type =
            plasm_core::value_contract::ValueContract::with_expr(&col.expr, &mut |path| {
                schema
                    .fields
                    .iter()
                    .find(|f| f.name.as_str() == path.dotted())
                    .and_then(|f| f.value_type.clone())
                    .ok_or_else(|| {
                        plasm_core::row_plan::contracts::RowContractError::ComputedFieldMissing {
                            field: path.dotted(),
                        }
                    })
            })?;
        return Ok(plasm_core::SyntheticFieldSchema {
            value_kind: value_type.summary(),
            value_type: Some(value_type),
            name: col.name.clone(),
            source: None,
        });
    };
    if let Some(src) = schema.fields.iter().find(|f| {
        f.name.as_str() == fp.dotted()
            || f.source.as_ref().is_some_and(|s| s.dotted() == fp.dotted())
    }) {
        return Ok(plasm_core::SyntheticFieldSchema {
            value_type: src.value_type.clone(),
            name: col.name.clone(),
            value_kind: src.value_kind,
            source: src.source.clone().or_else(|| Some(fp.clone())),
        });
    }
    Err(
        plasm_core::row_plan::contracts::RowContractError::ComputedFieldMissing {
            field: fp.dotted(),
        },
    )
}
