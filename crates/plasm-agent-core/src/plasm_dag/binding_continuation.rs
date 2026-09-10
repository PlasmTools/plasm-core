//! PLP-4 binding continuation: classify `label.<tail>` and lower to DAG nodes.
//!
//! See `docs/plasm-language-surface-invariants.md` in the monorepo.

use super::binding_contract::binding_contract;
use super::pipeline::compile_surface_node;
use super::prelude::*;
use super::relation::{
    lookup_relation_chain_meta, relation_binding_proofs_for_lower,
    relation_continuation_expr_from_source_row_hole, relation_materialize_for_lower,
    resolve_relation_segment_for_continuation, resolve_relation_wire_on_entity,
};
use super::row_suffix::lower_suffix_stream;
use super::schema_validate::agent_program_error;
use super::types::{CompileState, DagNode, DagNodeSource};
use super::view_embed_proof::resolve_view_embed_proof;
use plasm_core::plp::{self, PlpId};

pub(in crate::plasm_dag) fn plp4_reject(id: &str, label: &str, tail: &str) -> String {
    plp::plp4_program(
        id,
        format!(
            "binding `{label}` cannot extend with `{tail}` — use `{label} | …` for row algebra, `{label} => _.r#` for plural relations, or `{label} => Entity.m#(…, _)` for per-row invokes"
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
            op:
                crate::plasm_plan::ComputeOp::Limit { .. }
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
            op:
                crate::plasm_plan::ComputeOp::Limit { .. }
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
    force_row_hole: bool,
) -> Result<plasm_core::expr_parser::ParsedExpr, String> {
    let relation_wire = resolve_relation_segment_for_continuation(
        session,
        state.cross_cache,
        &contract.row_entity,
        segment,
        Some(plasm_core::ProgramBindingLabel(contract.label.as_str())),
    )?;
    if force_row_hole || prefer_row_hole_relation_continuation(state, contract, segment, session) {
        return Ok(plasm_core::expr_parser::ParsedExpr::from_expr(
            relation_continuation_expr_from_source_row_hole(
                session,
                &contract.row_entity,
                &relation_wire,
            )?,
        ));
    }
    let refs = state.program_node_id_set();
    let try_expanded_chain = |expanded: &str| -> Option<plasm_core::expr_parser::ParsedExpr> {
        let parsed = parse_plasm_program_surface_for_dag(
            session,
            state.cross_cache,
            state.pipeline,
            expanded,
            &refs,
            false,
            None,
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
    Ok(plasm_core::expr_parser::ParsedExpr::from_expr(
        relation_continuation_expr_from_source_row_hole(
            session,
            &contract.row_entity,
            &relation_wire,
        )?,
    ))
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
            let node = state.get(label).ok_or_else(|| {
                plp::plp4_program(
                    "",
                    format!("unknown binding `{label}` for method continuation"),
                )
            })?;
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
    if !matches!(
        contract.row_cardinality,
        RowCardinalityProof::StaticSingleton
    ) {
        return Err(plp::plp4_program(
            id,
            format!(
                "method invoke `{label}.{tail}` requires a statically singleton binding — use `{label} => Entity.m#(…, _)` for per-row application"
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
    lower_relation_continuation_inner(session, state, id, expr, source_label, tail, true)
}

pub(in crate::plasm_dag) fn lower_relation_application(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    source_label: &str,
    tail: &str,
) -> Result<DagNode, String> {
    lower_relation_continuation_inner(session, state, id, expr, source_label, tail, false)
}

fn lower_relation_continuation_inner(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    source_label: &str,
    tail: &str,
    require_static_singleton: bool,
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
    if require_static_singleton
        && !matches!(
            contract.row_cardinality,
            RowCardinalityProof::StaticSingleton
        )
    {
        return Err(plp::plp4_program(
            id,
            format!(
                "relation continuation `{source_label}.{segment}` requires a statically singleton binding — use `{source_label} => _.r#` for plural relation fanout"
            ),
        ));
    }
    if !contract.anchor.is_present() {
        return Err(plp::plp4_program(
            id,
            format!(
                "`{source_label}.{segment}` requires a continuation anchor on `{source_label}` — bind an intermediate row before continuing the relation chain"
            ),
        ));
    }
    let parsed = parse_relation_continuation_expr(
        session,
        state,
        &contract,
        segment,
        !require_static_singleton,
    )?;
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
                "`{segment}` is not a field or relation on `{source_label}` — use wire field names or `r#` relation hops from the language card"
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
        expr: parsed.expr.clone(),
        projection: parsed.projection.clone(),
        display_expr: Some(expr.to_string()),
    };
    let binding_proofs =
        relation_binding_proofs_for_lower(session, &contract.row_entity, wire.as_str())
            .unwrap_or_default();
    let materialize = relation_materialize_for_lower(session, &contract.row_entity, wire.as_str())?;
    let view_embed_proof = view_embed_proof_for_materialize(
        session,
        state,
        source_label,
        &materialize,
        wire.as_str(),
    )?;
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
        view_embed_proof,
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
    matches!(name, "page_size" | "singleton")
}

fn looks_like_collect_meta_tail(tail: &str) -> bool {
    let t = tail.trim();
    if t.is_empty() {
        return false;
    }
    if !t.contains('(') {
        return false;
    }
    let head = t.split('(').next().unwrap_or(t);
    let name = head.trim().trim_start_matches('.');
    is_known_postfix_method(name)
}

fn parse_collect_meta_tail(
    tail: &str,
) -> Result<Option<Vec<plasm_core::expr_parser::CollectMeta>>, String> {
    let mut rest = tail.trim();
    if rest.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    loop {
        if let Some(stripped) = rest.strip_prefix('.') {
            rest = stripped.trim_start();
        }
        if let Some(stripped) = rest.strip_prefix("singleton()") {
            out.push(plasm_core::expr_parser::CollectMeta::Singleton);
            rest = stripped.trim_start();
        } else if let Some(stripped) = rest.strip_prefix("page_size(") {
            let close = stripped
                .find(')')
                .ok_or_else(|| "page_size(...) requires a closing `)`".to_string())?;
            let n_raw = stripped[..close].trim();
            let n = n_raw
                .parse::<usize>()
                .map_err(|_| "page_size(...) requires a positive integer".to_string())?;
            if n == 0 {
                return Err("page_size(...) requires a positive integer".to_string());
            }
            out.push(plasm_core::expr_parser::CollectMeta::PageSize(n));
            rest = stripped[close + 1..].trim_start();
        } else {
            return Ok(None);
        }
        if rest.is_empty() {
            break;
        }
        if !rest.starts_with('.') {
            return Err("collect-meta tails must chain as `.page_size(N).singleton()`".to_string());
        }
    }
    Ok(Some(out))
}

fn unknown_row_transform_error(id: &str, tail: &str) -> String {
    plp::surface_err(
        PlpId::Continuation,
        agent_program_error(
            format!("Unknown dotted row transform `{tail}` on `{id}`."),
            Some(
                "Dotted row algebra was removed. Use `label | where … | select … | summarize … | order by … | take N | distinct`.",
            ),
        ),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BindingContinuationRoute {
    MethodInvoke,
    CollectMetaTail {
        meta: Vec<plasm_core::expr_parser::CollectMeta>,
    },
    FieldExtract {
        wire: String,
    },
    RelationSingleHop,
    RelationMultiSegmentReparse,
}

pub(in crate::plasm_dag) fn lower_pipe_continuation(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
    pipe: &PipeExpr,
) -> Result<Vec<DagNode>, String> {
    let suffixes = pipe.row_suffixes()?;
    lower_suffix_stream(
        session,
        state,
        id,
        expr,
        pipe.head.as_str(),
        suffixes,
        Some(id),
    )
}

/// Catalog field wire on `contract.row_entity` that is **not** also a declared relation.
fn field_project_wire_for_continuation(
    session: &ExecuteSession,
    contract: &ProgramBindingContract,
    segment: &str,
) -> Option<String> {
    use super::relation::resolve_cgs_for_qualified_entity;
    let cgs = resolve_cgs_for_qualified_entity(session, &contract.row_entity)?;
    let ent = cgs.get_entity(contract.row_entity.entity.as_str())?;
    if ent.relations.contains_key(segment) {
        return None;
    }
    ent.fields
        .contains_key(segment)
        .then(|| segment.to_string())
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
        if looks_like_collect_meta_tail(tail_trim) {
            match parse_collect_meta_tail(tail_trim)? {
                Some(meta) if !meta.is_empty() => {
                    return Ok(BindingContinuationRoute::CollectMetaTail { meta });
                }
                _ => return Err(unknown_row_transform_error(id, tail_trim)),
            }
        }
        let relation_eligible = relation_sourced_continuation_eligible(state, label)
            || matches!(contract.anchor, ContinuationAnchor::BindingLabel)
            || contract.anchor.allows_text_parse();
        if relation_eligible {
            // Homograph law: declared relation wins over field-dot project sugar.
            if resolve_relation_wire_on_entity(
                session,
                state.cross_cache,
                &contract.row_entity,
                tail_trim,
                Some(plasm_core::ProgramBindingLabel(contract.label.as_str())),
            )
            .is_some()
            {
                return Ok(BindingContinuationRoute::RelationSingleHop);
            }
            // Field-dot sugar: `ℓ.wire` → same route as explicit `ℓ[wire]` postfix.
            if let Some(wire) = field_project_wire_for_continuation(session, contract, tail_trim) {
                return Ok(BindingContinuationRoute::FieldExtract { wire });
            }
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
    let parsed = parse_plasm_program_surface_for_dag(
        session,
        state.cross_cache,
        state.pipeline,
        &expanded,
        &refs,
        false,
        Some(id),
    )?;
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
            expr: parsed.expr.clone(),
            projection: parsed.projection.clone(),
            display_expr: Some(expr.to_string()),
        };
        let binding_proofs = relation_binding_proofs_for_lower(
            session,
            &contract.row_entity,
            chain.selector.as_str(),
        )
        .unwrap_or_default();
        let materialize =
            relation_materialize_for_lower(session, &contract.row_entity, chain.selector.as_str())?;
        let view_embed_proof = view_embed_proof_for_materialize(
            session,
            state,
            label,
            &materialize,
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
            view_embed_proof,
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
        BindingContinuationRoute::CollectMetaTail { meta } => lower_suffix_stream(
            session,
            state,
            id,
            expr,
            label,
            meta.iter().map(RowSuffix::from).collect(),
            Some(id),
        )?
        .pop()
        .ok_or_else(|| {
            format!("Plasm program `{id}`: collect-meta continuation `{expr}` produced no nodes")
        }),
        BindingContinuationRoute::FieldExtract { wire } => {
            super::scalar_extract::lower_binding_scalar_field_dot(
                id,
                expr,
                label,
                wire,
                contract.row_cardinality,
            )
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

fn view_embed_proof_for_materialize(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    source_label: &str,
    materialize: &plasm_core::RelationMaterialization,
    relation_wire: &str,
) -> Result<Option<plasm_core::ValidatedViewEmbedProof>, String> {
    match materialize {
        plasm_core::RelationMaterialization::ViewEmbed { view } => Ok(Some(
            resolve_view_embed_proof(session, state, source_label, view.as_str(), relation_wire)?,
        )),
        _ => Ok(None),
    }
}
