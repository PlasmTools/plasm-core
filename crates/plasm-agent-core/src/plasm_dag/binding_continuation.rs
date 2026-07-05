//! PLP-4 binding continuation: classify `label.<tail>` and lower to DAG nodes.
//!
//! See `docs/plasm-language-surface-invariants.md` in the monorepo.

use super::prelude::*;
use plasm_core::plp::{self, PlpId};
use super::types::{CompileState, DagNode, DagNodeSource};
use super::binding_contract::binding_contract;
use super::pipeline::compile_surface_node;
use super::postfix::lower_row_expression;
use super::relation::{
    lookup_relation_chain_meta, relation_binding_proofs_for_lower,
    relation_continuation_expr_from_source_row_hole, relation_materialize_for_lower,
    resolve_cgs_for_qualified_entity, resolve_relation_segment_for_continuation,
    resolve_relation_wire_on_entity,
};
use super::schema_validate::agent_program_error;

pub(in crate::plasm_dag) fn plp4_reject(id: &str, label: &str, tail: &str) -> String {
    plp::plp4_program(
        id,
        format!(
            "binding `{label}` cannot extend with `{tail}` — use postfix transforms, `label.<relation>`, or `label.m#(…)` on the bound row"
        ),
    )
}

fn is_row_producing_relation_source(state: &CompileState<'_>, label: &str) -> bool {
    match state.get(label).map(|n| &n.source) {
        Some(DagNodeSource::RelationTraversal { .. }) => true,
        Some(DagNodeSource::Surface { kind, parsed, .. }) => {
            matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_))
        }
        Some(DagNodeSource::Compute {
            op: crate::plasm_plan::ComputeOp::Limit { .. }
            | crate::plasm_plan::ComputeOp::Project { .. },
            source,
            ..
        }) => is_row_producing_relation_source(state, source),
        _ => false,
    }
}

fn relation_sourced_continuation_eligible(state: &CompileState<'_>, label: &str) -> bool {
    match state.get(label).map(|n| &n.source) {
        Some(DagNodeSource::RelationTraversal { .. }) => true,
        Some(DagNodeSource::Compute {
            op: crate::plasm_plan::ComputeOp::Limit { .. }
            | crate::plasm_plan::ComputeOp::Project { .. },
            source,
            ..
        }) => is_row_producing_relation_source(state, source),
        _ => false,
    }
}

fn relation_uses_from_parent_get(
    session: &ExecuteSession,
    row_qe: &crate::plasm_plan::QualifiedEntityKey,
    segment: &str,
) -> bool {
    use super::relation::resolve_cgs_for_qualified_entity;
    let Ok(cgs) = crate::catalog_ownership::resolve_cgs_for_entity(
        session,
        row_qe.entity.as_str(),
        resolve_cgs_for_qualified_entity(session, row_qe),
    ) else {
        return false;
    };
    let Some(ent) = cgs.get_entity(row_qe.entity.as_str()) else {
        return false;
    };
    let Some(wire) = resolve_relation_wire_on_entity(session, None, row_qe, segment, None) else {
        return false;
    };
    ent.relations
        .get(wire.as_str())
        .and_then(|r| r.materialize.as_ref())
        .is_some_and(|m| {
            matches!(
                m,
                plasm_core::RelationMaterialization::FromParentGet { .. }
                    | plasm_core::RelationMaterialization::PreferFromParentGet { .. }
            )
        })
}

fn prefer_row_hole_relation_continuation(
    state: &CompileState<'_>,
    contract: &ProgramBindingContract,
    segment: &str,
    session: &ExecuteSession,
) -> bool {
    if matches!(contract.anchor, ContinuationAnchor::BindingLabel) {
        return true;
    }
    if session.contexts_by_entry.len() > 1 {
        return true;
    }
    if relation_sourced_continuation_eligible(state, &contract.label) {
        return true;
    }
    if contract.anchor.allows_text_parse() {
        return false;
    }
    relation_uses_from_parent_get(session, &contract.row_entity, segment)
}

fn parse_relation_continuation_expr(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    contract: &ProgramBindingContract,
    segment: &str,
) -> Result<plasm_core::expr_parser::ParsedExpr, String> {
    let relation_wire = resolve_relation_segment_for_continuation(
        session,
        state.cross_cache,
        &contract.row_entity,
        segment,
        Some(plasm_core::ProgramBindingLabel(contract.label.as_str())),
    )?;
    if prefer_row_hole_relation_continuation(state, contract, segment, session) {
        return Ok(plasm_core::expr_parser::ParsedExpr {
            expr: relation_continuation_expr_from_source_row_hole(
                session,
                &contract.row_entity,
                &relation_wire,
            )?,
            projection: None,
        });
    }
    let refs = state.program_node_id_set();
    let try_expanded_chain = |expanded: &str| -> Option<plasm_core::expr_parser::ParsedExpr> {
        let parsed = parse_plasm_surface_line_program(
            session,
            state.cross_cache,
            state.pipeline,
            expanded,
            Some(&refs),
            false,
        )
        .ok()?;
        matches!(parsed.expr, Expr::Chain(_)).then_some(parsed)
    };
    let try_label_row_chain = |expanded: &str| -> Option<plasm_core::expr_parser::ParsedExpr> {
        let parsed = try_expanded_chain(expanded)?;
        if let Expr::Chain(ref chain) = parsed.expr {
            if chain.source.primary_entity() == contract.row_entity.entity.as_str() {
                return Some(parsed);
            }
        }
        None
    };
    if contract.anchor.allows_text_parse() {
        if let Some(expanded) = contract.continuation_text_expansion(&relation_wire) {
            if let Some(parsed) = try_expanded_chain(&expanded) {
                return Ok(parsed);
            }
        }
        if relation_wire != segment {
            if let Some(expanded) = contract.continuation_text_expansion(segment) {
                if let Some(parsed) = try_expanded_chain(&expanded) {
                    return Ok(parsed);
                }
            }
        }
    }
    if matches!(contract.anchor, ContinuationAnchor::BindingLabel) {
        if let Some(expanded) = contract.continuation_text_expansion(&relation_wire) {
            if let Some(parsed) = try_label_row_chain(&expanded) {
                if let Expr::Chain(ref chain) = parsed.expr {
                    if matches!(chain.source.as_ref(), Expr::Get(_)) {
                        return Ok(parsed);
                    }
                }
            }
        }
    }
    Ok(plasm_core::expr_parser::ParsedExpr {
        expr: relation_continuation_expr_from_source_row_hole(
            session,
            &contract.row_entity,
            &relation_wire,
        )?,
        projection: None,
    })
}

fn looks_like_method_invoke_continuation_tail(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    contract: &ProgramBindingContract,
    tail: &str,
) -> bool {
    if !tail.contains('(') {
        return false;
    }
    let head = tail.split('(').next().unwrap_or(tail).trim();
    if head.len() > 1 && head.starts_with('m') && head[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    resolve_relation_wire_on_entity(
        session,
        state.cross_cache,
        &contract.row_entity,
        head,
        Some(plasm_core::ProgramBindingLabel(contract.label.as_str())),
    )
    .is_none()
}

fn method_invoke_expanded_surface(
    state: &CompileState<'_>,
    label: &str,
    contract: &ProgramBindingContract,
    tail: &str,
) -> Result<String, String> {
    match &contract.anchor {
        ContinuationAnchor::RootSurface(prefix) | ContinuationAnchor::RelationExpand(prefix) => {
            Ok(format!("{prefix}.{tail}"))
        }
        ContinuationAnchor::BindingLabel => {
            let node = state
                .get(label)
                .ok_or_else(|| plp::plp4_program("", format!("unknown binding `{label}` for method continuation")))?;
            Ok(format!("{}.{tail}", node.expr.trim()))
        }
        ContinuationAnchor::None => Err(plp::plp4_program(
            "",
            format!("binding `{label}` has no continuation anchor for method invoke `{tail}`"),
        )),
    }
}

fn lower_method_invoke_continuation(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    _expr: &str,
    label: &str,
    contract: &ProgramBindingContract,
    tail: &str,
) -> Result<DagNode, String> {
    if matches!(contract.row_cardinality, RowCardinalityProof::StaticPlural) {
        return Err(plp::plp4_program(
            id,
            format!(
                "side-effect invoke `{label}.{tail}` requires a singleton binding — use `rows => e#.m#(p#=_.…)` or `.limit(1)` / `.singleton()` first"
            ),
        ));
    }
    let expanded = method_invoke_expanded_surface(state, label, contract, tail)?;
    compile_surface_node(session, state, id, &expanded)
}

fn relation_result_shape(
    rel_cardinality: RelationCardinality,
    source_card: RelationSourceCardinality,
) -> crate::plasm_plan::ResultShape {
    match (rel_cardinality, source_card) {
        (RelationCardinality::Many, _) => crate::plasm_plan::ResultShape::List,
        (RelationCardinality::One, RelationSourceCardinality::Many) => {
            crate::plasm_plan::ResultShape::List
        }
        (RelationCardinality::One, _) => crate::plasm_plan::ResultShape::Single,
    }
}

pub(in crate::plasm_dag) fn lower_relation_continuation(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    source_label: &str,
    tail: &str,
) -> Result<DagNode, String> {
    let segment = tail.split('.').next().unwrap_or(tail).trim();
    if segment.is_empty() || tail.contains('.') {
        return Err(plp::plp4_program(
            id,
            format!(
                "`{source_label}.{tail}` — node-ref continuation supports a single CGS relation segment only"
            ),
        ));
    }
    let contract = binding_contract(state, source_label).ok_or_else(|| {
        plp::plp4_program(
            id,
            format!("unknown binding `{source_label}` for relation continuation"),
        )
    })?;
    if !contract.anchor.is_present() {
        return Err(plp::plp4_program(
            id,
            format!(
                "`{source_label}.{segment}` requires a continuation anchor on `{source_label}` — bind an intermediate row before continuing the relation chain"
            ),
        ));
    }
    let parsed = parse_relation_continuation_expr(session, state, &contract, segment)?;
    let Expr::Chain(ref chain) = parsed.expr else {
        return Err(plp::plp4_program(
            id,
            format!("`{source_label}.{segment}` did not lower to a relation chain"),
        ));
    };
    let Some(wire) = resolve_relation_wire_on_entity(
        session,
        state.cross_cache,
        &contract.row_entity,
        segment,
        Some(plasm_core::ProgramBindingLabel(source_label)),
    ) else {
        return Err(plp::plp4_program(
            id,
            format!(
                "`{segment}` is not a field or relation on `{source_label}` — use `p#` / `r#` from the teaching table"
            ),
        ));
    };
    if chain.selector.as_str() != wire.as_str() {
        return Err(plp::plp4_program(
            id,
            format!("relation wire mismatch for `{source_label}.{segment}`"),
        ));
    }
    let (target_qe, rel_cardinality) = lookup_relation_chain_meta(
        session,
        state.cross_cache,
        chain,
        Some(&contract.row_entity),
    )?;
    let expanded = contract
        .continuation_text_expansion(segment)
        .unwrap_or_else(|| format!("{source_label}.{segment}"));
    let source_card = contract.relation_source_cardinality();
    let result_shape = relation_result_shape(rel_cardinality, source_card);
    let ir = PlanExprIr {
        expr: serde_json::to_value(&parsed.expr).map_err(|e| e.to_string())?,
        projection: parsed.projection.clone(),
        display_expr: Some(expr.to_string()),
    };
    let binding_proofs =
        relation_binding_proofs_for_lower(session, &contract.row_entity, wire.as_str())
            .unwrap_or_default();
    let materialize = relation_materialize_for_lower(session, &contract.row_entity, wire.as_str())?;
    let plan_relation = PlanRelationTraversal {
        source: source_label.to_string(),
        relation: wire,
        target: target_qe.clone(),
        cardinality: rel_cardinality,
        source_cardinality: source_card,
        expr: expanded.clone(),
        ir: ir.clone(),
        binding_proofs,
        materialize: Some(materialize),
    };
    Ok(DagNode {
        id: id.to_string(),
        expr: expr.to_string(),
        singleton: false,
        page_size: None,
        source: DagNodeSource::RelationTraversal {
            source_label: source_label.to_string(),
            expanded_plasm: expanded,
            parsed,
            plan_relation,
            qualified_entity: target_qe,
            effect_class: EffectClass::Read,
            result_shape,
        },
    })
}

fn is_known_postfix_method(name: &str) -> bool {
    matches!(
        name,
        "limit"
            | "page_size"
            | "sort"
            | "filter"
            | "aggregate"
            | "group_by"
            | "dedupe"
            | "distinct"
            | "singleton"
    )
}

fn looks_like_surface_postfix_tail(tail: &str) -> bool {
    let t = tail.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with('[') {
        return true;
    }
    if !(t.contains('(') || t.contains('{')) {
        return false;
    }
    let head = t
        .split('(')
        .next()
        .unwrap_or(t)
        .split('{')
        .next()
        .unwrap_or(t);
    let name = head.trim().trim_start_matches('.');
    is_known_postfix_method(name)
}

fn unknown_row_transform_error(id: &str, tail: &str) -> String {
    plp::surface_err(
        PlpId::Continuation,
        agent_program_error(
            format!("Unknown row transform `{tail}` on `{id}`."),
            Some(
                "Use postfix on a binding: `.limit(N)`, `.filter{p#=…}`, `.sort(p#)`, `.group_by(p#)`, `[p#,…]`, etc.",
            ),
        ),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BindingContinuationRoute {
    MethodInvoke,
    Postfix { synthetic: String },
    RelationSingleHop,
    RelationMultiSegmentReparse,
}

fn classify_binding_continuation_route(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    label: &str,
    tail_trim: &str,
    contract: &ProgramBindingContract,
) -> Result<BindingContinuationRoute, String> {
    if contract.supports_method_invoke()
        && looks_like_method_invoke_continuation_tail(session, state, contract, tail_trim)
    {
        return Ok(BindingContinuationRoute::MethodInvoke);
    }
    if !tail_trim.contains('.') {
        if looks_like_surface_postfix_tail(tail_trim) {
            let synthetic = format!("{label}.{tail_trim}");
            match peel_postfix_suffixes(&synthetic) {
                Ok((_, ops)) if !ops.is_empty() => {
                    return Ok(BindingContinuationRoute::Postfix { synthetic });
                }
                _ => return Err(unknown_row_transform_error(id, tail_trim)),
            }
        }
        if relation_sourced_continuation_eligible(state, label)
            || matches!(contract.anchor, ContinuationAnchor::BindingLabel)
            || contract.anchor.allows_text_parse()
        {
            return Ok(BindingContinuationRoute::RelationSingleHop);
        }
    } else if contract.anchor.allows_text_parse() {
        return Ok(BindingContinuationRoute::RelationMultiSegmentReparse);
    }
    Err(plp4_reject(id, label, tail_trim))
}

fn lower_multi_segment_relation_continuation(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    label: &str,
    tail_trim: &str,
    contract: &ProgramBindingContract,
) -> Result<DagNode, String> {
    let expanded = contract
        .continuation_text_expansion(tail_trim)
        .ok_or_else(|| {
            plp::plp4_program(
                id,
                format!("`{label}` has no continuation anchor for `{tail_trim}`"),
            )
        })?;
    let refs = state.program_node_id_set();
    let parsed = parse_plasm_surface_line_program(
        session,
        state.cross_cache,
        state.pipeline,
        &expanded,
        Some(&refs),
        false,
    )
    .map_err(|e| {
        format_session_symbolic_parse_error(
            session,
            state.cross_cache,
            state.pipeline,
            &expanded,
            &e,
        )
    })?;
    if let Expr::Chain(ref chain) = parsed.expr {
        let (target_qe, rel_cardinality) = lookup_relation_chain_meta(
            session,
            state.cross_cache,
            chain,
            Some(&contract.row_entity),
        )?;
        let source_card = contract.relation_source_cardinality();
        let result_shape = relation_result_shape(rel_cardinality, source_card);
        let ir = PlanExprIr {
            expr: serde_json::to_value(&parsed.expr).map_err(|e| e.to_string())?,
            projection: parsed.projection.clone(),
            display_expr: Some(expr.to_string()),
        };
        let binding_proofs = relation_binding_proofs_for_lower(
            session,
            &contract.row_entity,
            chain.selector.as_str(),
        )
        .unwrap_or_default();
        let materialize = relation_materialize_for_lower(
            session,
            &contract.row_entity,
            chain.selector.as_str(),
        )?;
        let plan_relation = PlanRelationTraversal {
            source: label.to_string(),
            relation: chain.selector.clone(),
            target: target_qe.clone(),
            cardinality: rel_cardinality,
            source_cardinality: source_card,
            expr: expanded.clone(),
            ir: ir.clone(),
            binding_proofs,
            materialize: Some(materialize),
        };
        return Ok(DagNode {
            id: id.to_string(),
            expr: expr.to_string(),
            singleton: false,
            page_size: None,
            source: DagNodeSource::RelationTraversal {
                source_label: label.to_string(),
                expanded_plasm: expanded,
                parsed,
                plan_relation,
                qualified_entity: target_qe,
                effect_class: EffectClass::Read,
                result_shape,
            },
        });
    }
    Err(plp::plp4_program(
        id,
        format!(
            "`{label}.…` expands to a non-relation Plasm expression; node-ref continuation supports CGS relation chains (`label.<relation>`) only"
        ),
    ))
}

pub(in crate::plasm_dag) fn dispatch_binding_continuation(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    label: &str,
    tail_trim: &str,
    contract: &ProgramBindingContract,
) -> Result<DagNode, String> {
    match classify_binding_continuation_route(session, state, id, label, tail_trim, contract)? {
        BindingContinuationRoute::MethodInvoke => {
            lower_method_invoke_continuation(session, state, id, expr, label, contract, tail_trim)
        }
        BindingContinuationRoute::Postfix { synthetic } => {
            let mut nodes = lower_row_expression(session, state, id, &synthetic, Some(id))?;
            nodes.pop().ok_or_else(|| {
                format!(
                    "Plasm program `{id}`: postfix continuation `{synthetic}` produced no nodes"
                )
            })
        }
        BindingContinuationRoute::RelationSingleHop => {
            lower_relation_continuation(session, state, id, expr, label, tail_trim)
        }
        BindingContinuationRoute::RelationMultiSegmentReparse => {
            lower_multi_segment_relation_continuation(
                session, state, id, expr, label, tail_trim, contract,
            )
        }
    }
}
