//! Row suffix stream decomposition and lowering.

use super::super::prelude::*;
use super::super::types::{CompileState, DagNode, DagNodeSource, ExpandedProgramSurface};
use super::super::binding_continuation;
use super::super::pipeline::compile_surface_node;
use super::super::plan_serialize::parse_field_list;
use super::super::relation::try_split_single_hop_surface_chain;
use super::super::schema_validate::{
    passthrough_identity_projection_fields, synthetic_schema_passthrough_rows,
};
use super::postfix_op::postfix_op_to_compute;

pub(in crate::plasm_dag) fn lower_row_expression(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    binding_id: &str,
    full_rhs: &str,
    final_id: Option<&str>,
) -> Result<Vec<DagNode>, String> {
    let (head, suffixes) = decompose_row_suffix_stream(session, state, full_rhs)?;
    if suffixes.is_empty() {
        return Ok(vec![compile_surface_node(
            session, state, binding_id, full_rhs,
        )?]);
    }
    lower_suffix_stream(
        session, state, binding_id, full_rhs, &head, suffixes, final_id,
    )
}

/// When `expr` carries postfix and/or relation suffixes, lower the full DAG spine.
/// Returns `None` for pure surface heads (no row suffix stream).
pub(in crate::plasm_dag) fn try_lower_row_suffix_expression(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
) -> Result<Option<Vec<DagNode>>, String> {
    let expr_trim = expr.trim();
    let expanded = ExpandedProgramSurface::new(session, state.pipeline, expr_trim);
    let (_, suffixes) = decompose_row_suffix_stream(session, state, expanded.as_str())?;
    if suffixes.is_empty() {
        return Ok(None);
    }
    Ok(Some(lower_row_expression(
        session,
        state,
        id,
        expr_trim,
        Some(id),
    )?))
}

/// Classify interleaved relation + transform suffixes after peeling postfix transforms and relation hops from the right.
pub(in crate::plasm_dag) fn decompose_row_suffix_stream(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    expr: &str,
) -> Result<(String, Vec<RowSuffix>), String> {
    let mut cur = expr.trim().to_string();
    let mut suffixes_rev: Vec<RowSuffix> = Vec::new();

    loop {
        let (core, ops) =
            peel_postfix_suffixes(&cur).map_err(|e| format!("row suffix stream: {e}"))?;
        for op in ops.iter().rev() {
            suffixes_rev.push(RowSuffix::from_postfix_op(op)?);
        }
        cur = core;

        if let Some((base, segment)) = try_split_single_hop_surface_chain(session, state, &cur) {
            suffixes_rev.push(RowSuffix::Relation { wire: segment });
            cur = base;
            continue;
        }
        break;
    }

    suffixes_rev.reverse();
    Ok((cur, suffixes_rev))
}

pub(in crate::plasm_dag) fn row_suffix_to_postfix(suffix: &RowSuffix) -> Option<PlasmPostfixOp> {
    match suffix {
        RowSuffix::Limit { count } => Some(PlasmPostfixOp::Limit(*count as usize)),
        RowSuffix::Project { fields } => Some(PlasmPostfixOp::Projection {
            fields: fields.join(","),
        }),
        RowSuffix::Sort { args } => Some(PlasmPostfixOp::Sort { args: args.clone() }),
        RowSuffix::Filter { body } => Some(PlasmPostfixOp::Filter { body: body.clone() }),
        RowSuffix::Aggregate { args } => Some(PlasmPostfixOp::Aggregate { args: args.clone() }),
        RowSuffix::GroupBy { args } => Some(PlasmPostfixOp::GroupBy { args: args.clone() }),
        RowSuffix::Dedupe { keys } => Some(PlasmPostfixOp::Dedupe { keys: keys.clone() }),
        RowSuffix::Distinct { keys } => Some(PlasmPostfixOp::Distinct { keys: keys.clone() }),
        RowSuffix::Singleton => Some(PlasmPostfixOp::Singleton),
        RowSuffix::PageSize { n } => Some(PlasmPostfixOp::PageSize(*n as usize)),
        RowSuffix::Relation { .. } => None,
    }
}

pub(in crate::plasm_dag) fn compile_state_with_nodes<'a>(
    state: &'a CompileState<'a>,
    nodes: &[DagNode],
) -> CompileState<'a> {
    let mut scratch = CompileState {
        nodes: state.nodes.clone(),
        labels: state.labels.clone(),
        pipeline: state.pipeline,
        cross_cache: state.cross_cache,
        sym_map: RefCell::new(state.sym_map.borrow().clone()),
    };
    for node in nodes {
        let idx = scratch.nodes.len();
        scratch.labels.insert(node.id.clone(), idx);
        scratch.nodes.push(node.clone());
    }
    scratch
}

/// Fuse `.group_by(keys).aggregate(specs)` into one `group_by` args tail for plan lowering.
pub(in crate::plasm_dag) fn coalesce_group_by_aggregate_suffixes(steps: Vec<RowSuffix>) -> Vec<RowSuffix> {
    let mut out = Vec::with_capacity(steps.len());
    let mut i = 0;
    while i < steps.len() {
        if let RowSuffix::GroupBy { args: gb } = &steps[i] {
            if let Some(RowSuffix::Aggregate { args: agg }) = steps.get(i + 1) {
                out.push(RowSuffix::GroupBy {
                    args: format!("{gb},{agg}"),
                });
                i += 2;
                continue;
            }
        }
        out.push(steps[i].clone());
        i += 1;
    }
    out
}

/// Fold an ordered [`RowSuffix`] stream onto a surface or label head.
pub(in crate::plasm_dag) fn lower_suffix_stream(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    binding_id: &str,
    full_rhs: &str,
    head: &str,
    suffixes: Vec<RowSuffix>,
    final_id: Option<&str>,
) -> Result<Vec<DagNode>, String> {
    if suffixes.is_empty() {
        return Err("internal: lower_suffix_stream requires non-empty suffixes".into());
    }

    let tail_singleton = suffixes.iter().any(|s| matches!(s, RowSuffix::Singleton));
    let tail_page_size = suffixes.iter().find_map(|s| {
        if let RowSuffix::PageSize { n } = s {
            Some(*n as usize)
        } else {
            None
        }
    });

    let mut out: Vec<DagNode> = Vec::new();
    let head_trim = head.trim();

    let mut steps: Vec<RowSuffix> = suffixes
        .iter()
        .filter(|s| !matches!(s, RowSuffix::Singleton | RowSuffix::PageSize { .. }))
        .cloned()
        .collect();
    steps = coalesce_group_by_aggregate_suffixes(steps);

    if steps.is_empty() && (tail_singleton || tail_page_size.is_some()) {
        let out_id = final_id
            .map(str::to_string)
            .unwrap_or_else(|| binding_id.to_string());
        if state.contains(head_trim) {
            let staged: &[DagNode] = &out;
            let node = if tail_singleton {
                let schema = synthetic_schema_passthrough_rows(session, state, staged, head_trim)?;
                DagNode {
                    id: out_id,
                    expr: full_rhs.to_string(),
                    singleton: true,
                    page_size: tail_page_size,
                    source: DagNodeSource::Compute {
                        source: head_trim.to_string(),
                        op: ComputeOp::Limit { count: 1 },
                        schema,
                        collection_alias: None,
                    },
                }
            } else {
                let (fields, schema) =
                    passthrough_identity_projection_fields(session, state, staged, head_trim)?;
                DagNode {
                    id: out_id,
                    expr: full_rhs.to_string(),
                    singleton: false,
                    page_size: tail_page_size,
                    source: DagNodeSource::Compute {
                        source: head_trim.to_string(),
                        op: ComputeOp::Project { fields },
                        schema,
                        collection_alias: None,
                    },
                }
            };
            return Ok(vec![node]);
        }
        let mut node = compile_surface_node(session, state, &out_id, head_trim)?;
        node.singleton |= tail_singleton;
        node.page_size = tail_page_size.or(node.page_size);
        node.expr = full_rhs.to_string();
        return Ok(vec![node]);
    }

    let mut cur_id = if state.contains(head_trim) {
        head_trim.to_string()
    } else {
        let bid = format!("__plasm_{binding_id}_b0");
        let base = compile_surface_node(session, state, &bid, head_trim)?;
        out.push(base);
        bid
    };

    for (i, suffix) in steps.iter().enumerate() {
        let is_last = i + 1 == steps.len();
        let nid = if is_last {
            final_id
                .map(str::to_string)
                .unwrap_or_else(|| binding_id.to_string())
        } else if matches!(suffix, RowSuffix::Relation { .. }) {
            format!("__plasm_{binding_id}_r{i}")
        } else {
            format!("__plasm_{binding_id}_s{i}")
        };

        if let RowSuffix::Relation { wire } = suffix {
            let scratch = compile_state_with_nodes(state, &out);
            let rel = binding_continuation::lower_relation_continuation(
                session,
                &scratch,
                &nid,
                &format!("{cur_id}.{wire}"),
                &cur_id,
                wire,
            )?;
            out.push(rel);
            cur_id = nid;
            continue;
        }

        if let Some(op) = row_suffix_to_postfix(suffix) {
            let node = postfix_op_to_compute(session, state, &out, &op, &cur_id, &nid, full_rhs)?;
            out.push(node);
            cur_id = nid;
        }
    }

    if let Some(ps) = tail_page_size {
        if let Some(first_surface) = out.iter_mut().find(|n| {
            matches!(
                n.source,
                DagNodeSource::Surface { .. } | DagNodeSource::RelationTraversal { .. }
            )
        }) {
            first_surface.page_size = Some(ps);
        }
    }
    if let Some(last) = out.last_mut() {
        last.singleton |= tail_singleton;
        last.expr = full_rhs.to_string();
        if let Some(ps) = tail_page_size {
            last.page_size = Some(ps);
        } else {
            last.page_size = tail_page_size.or(last.page_size);
        }
    }

    Ok(out)
}
