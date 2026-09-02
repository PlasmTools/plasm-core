//! Program compile pipeline entrypoints and per-node dispatch.

use super::binding_continuation;
use super::binding_contract::binding_contract;
use super::invoke_cardinality::validate_invoke_scalar_field_refs;
use super::plan_serialize::{
    collect_template_uses_from_expr, expr_template_json, infer_surface_contract, node_to_json,
    parse_plan_value_expr, stamp_plan_uses_result_qualified_entities,
};
use super::row_suffix::{
    compile_state_with_nodes, lower_suffix_stream, try_lower_row_suffix_expression,
};
use super::prelude::*;
use super::render_dag::compile_render_from_applicator;
use super::schema_validate::{cgs_for_qualified_entity, validate_surface_inline_projection};
use super::types::{CompileState, DagNode, DagNodeSource, ExpandedProgramSurface};

#[allow(dead_code)]
pub(crate) fn is_plasm_dag_candidate(expressions: &[String]) -> bool {
    if expressions.len() != 1 {
        return false;
    }
    is_plasm_dag_source(expressions[0].trim())
}

pub(crate) fn is_plasm_dag_source(src: &str) -> bool {
    src.lines().any(|line| {
        let line = strip_line_comment(line).trim();
        !line.is_empty() && split_assignment_at_top_level(line).is_some()
    }) || src.contains("=>")
        || parse_pipe_expr(src).is_ok_and(|pipe| pipe.is_some())
        || peel_collect_meta(src)
            .map(|(_, meta)| !meta.is_empty())
            .unwrap_or(false)
}

/// Compile one program expression to plan JSON (DAG program vs single surface line).
#[allow(dead_code)]
pub(crate) fn compile_plasm_expression_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    if is_plasm_dag_source(source.trim()) {
        compile_plasm_dag_to_plan(pipeline, symbol_map_cross_cache, session, name, source)
    } else {
        compile_plasm_surface_line_to_plan(pipeline, symbol_map_cross_cache, session, name, source)
    }
}

pub(crate) fn compile_plasm_dag_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    compile_plasm_dag_to_plan_inner(pipeline, symbol_map_cross_cache, session, name, source)
}

// compile_plasm_program / compile_plasm_expression live in plasm_compile.rs

pub(crate) fn compile_plasm_dag_to_plan_inner(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    let mut state = CompileState::new(pipeline, symbol_map_cross_cache);
    let flattened = expand_flattened_program_statements(&collect_program_statement_lines(source)?);
    let statements = flattened.statements;
    if statements.is_empty() {
        return Err(program_empty_error());
    }
    validate_program_statement_order(&statements)?;
    let mut final_roots: Option<Vec<String>> = None;
    for stmt in statements {
        if let Some((id, rhs)) = split_assignment_for_binding(&stmt) {
            validate_program_label(id)?;
            for node in compile_node_expr(session, &state, id, rhs.trim())? {
                state.insert(node)?;
            }
        } else {
            let stmt = stmt.trim();
            if stmt.starts_with("return ") {
                return Err(program_return_keyword_error());
            }
            final_roots = Some(split_return_list(stmt, &mut state, session)?);
        }
    }
    let roots = final_roots.ok_or_else(missing_program_roots_error)?;
    if roots.is_empty() {
        return Err("Plasm program final roots list is empty".to_string());
    }
    let nodes = state
        .nodes
        .iter()
        .map(|n| node_to_json(n.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let return_value = if roots.len() == 1 {
        json!({ "kind": "node", "node": roots[0] })
    } else {
        json!({ "kind": "parallel", "nodes": roots })
    };
    let mut metadata = serde_json::Map::new();
    metadata.insert("language".to_string(), serde_json::json!("plasm-dag"));
    if let Some(label) = flattened.coerced_default_return {
        metadata.insert(
            "coerced_default_return".to_string(),
            serde_json::json!(label),
        );
    }
    let mut plan = json!({
        "version": 1,
        "kind": "program",
        "name": name,
        "nodes": nodes,
        "return": return_value,
        "metadata": serde_json::Value::Object(metadata),
    });
    stamp_plan_uses_result_qualified_entities(&mut plan)?;
    Ok(plan)
}

/// One line of surface Plasm (or `a, b` at top level) as a one-line program plan — same shape as
/// [`compile_plasm_dag_to_plan`], so the MCP and HTTP runtimes can always execute through the plan runner.
pub(crate) fn compile_plasm_surface_line_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    line: &str,
) -> Result<serde_json::Value, String> {
    let trimmed = line.trim();
    if is_plasm_dag_source(trimmed) {
        return compile_plasm_dag_to_plan(pipeline, symbol_map_cross_cache, session, name, trimmed);
    }
    let mut state = CompileState::new(pipeline, symbol_map_cross_cache);
    if trimmed.starts_with("return ") {
        return Err(program_return_keyword_error());
    }
    reject_bare_literal_noop_root(trimmed)?;
    let roots = split_return_list(trimmed, &mut state, session)?;
    if roots.is_empty() {
        return Err("expression is empty".to_string());
    }
    let nodes = state
        .nodes
        .iter()
        .map(|n| node_to_json(n.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let return_value = if roots.len() == 1 {
        json!({ "kind": "node", "node": &roots[0] })
    } else {
        json!({ "kind": "parallel", "nodes": roots })
    };
    let mut plan = json!({
        "version": 1,
        "kind": "program",
        "name": name,
        "nodes": nodes,
        "return": return_value,
        "metadata": { "language": "plasm-dag" }
    });
    stamp_plan_uses_result_qualified_entities(&mut plan)?;
    Ok(plan)
}
pub(in crate::plasm_dag) fn compile_node_expr(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    rhs: &str,
) -> Result<Vec<DagNode>, String> {
    let rhs_display = rhs.trim();
    if rhs_display.contains("=>") {
        reject_relation_arrow_trap(rhs_display)?;
    }
    let expanded = ExpandedProgramSurface::new(session, state.pipeline, rhs_display);
    let node = parse_expr_node(expanded.as_str())?;
    lower_expr_node(session, state, id, rhs_display, node)
}

fn lower_expr_node(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    display: &str,
    node: ExprNode,
) -> Result<Vec<DagNode>, String> {
    match node.apply {
        Some(Applicator::Render {
            kind: RenderApplicator::CrossBinding { sources, template },
        }) => compile_render_from_applicator(session, state, id, display, &sources, template),
        Some(apply) => {
            // Stage the row plane under a scratch id — never reuse `id` (the apply
            // node owns that label / return_* slot).
            let (mut prefix, source) =
                stage_row_expr_to_source(session, state, id, display, &node.row, None)?;
            let scratch = if prefix.is_empty() {
                None
            } else {
                Some(compile_state_with_nodes(state, &prefix))
            };
            let state = scratch.as_ref().unwrap_or(state);
            require_node(state, source.as_str())?;
            let source = source.as_str();
            let mut applied = match apply {
                Applicator::Render {
                    kind: RenderApplicator::Inferred { template },
                } => compile_render_from_applicator(
                    session,
                    state,
                    id,
                    display,
                    &[source.to_string()],
                    template,
                )?,
                Applicator::Render {
                    kind: RenderApplicator::CrossBinding { .. },
                } => unreachable!("cross-binding render handled above"),
                Applicator::ForEach { surface } => {
                    let refs = state.program_node_id_set();
                    let parsed = parse_plasm_program_surface_for_dag(
                        session,
                        state.cross_cache,
                        state.pipeline,
                        surface.trim(),
                        &refs,
                        true,
                        Some(id),
                    )?;
                    let uses = collect_template_uses_from_expr(&parsed.expr);
                    let (kind, qualified, _effect, _shape) =
                        infer_surface_contract(session, &parsed.expr)?;
                    if !matches!(
                        kind,
                        PlanNodeKind::Create
                            | PlanNodeKind::Update
                            | PlanNodeKind::Delete
                            | PlanNodeKind::Action
                    ) {
                        return Err(format!(
                            "Plasm program `{id}` for_each right side must be a write/side-effect expression"
                        ));
                    }
                    vec![DagNode {
                        id: id.to_string(),
                        expr: display.to_string(),
                        singleton: false,
                        page_size: None,
                        source: DagNodeSource::ForEach {
                            source: source.to_string(),
                            parsed_template: expr_template_json(&parsed, &uses)?,
                            display_expr: surface.trim().to_string(),
                            effect_kind: kind,
                            qualified_entity: qualified,
                            uses_result: uses,
                        },
                    }]
                }
                Applicator::Relation { wire } => {
                    vec![binding_continuation::lower_relation_application(
                        session,
                        state,
                        id,
                        display,
                        source,
                        wire.as_str(),
                    )?]
                }
                Applicator::Derive { body } => {
                    let (value, inputs) = parse_plan_value_expr(body.trim(), state, Some("_"))?;
                    let relation_wires = relation_wire_names_for_source(session, state, source);
                    reject_derive_map_invalid_rhs(&value, &relation_wires)?;
                    vec![DagNode {
                        id: id.to_string(),
                        expr: display.to_string(),
                        singleton: false,
                        page_size: None,
                        source: DagNodeSource::Derive {
                            source: source.to_string(),
                            value,
                            inputs,
                        },
                    }]
                }
            };
            prefix.append(&mut applied);
            Ok(prefix)
        }
        None => lower_row_only_expr(session, state, id, display, node.row),
    }
}

fn lower_row_only_expr(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    display: &str,
    row: RowExpr,
) -> Result<Vec<DagNode>, String> {
    match row {
        RowExpr::Pipe(pipe) => lower_pipe_row_expression(session, state, id, display, pipe),
        RowExpr::Primary { head, collect_meta } => {
            let head = head.trim();
            if collect_meta.is_empty() {
                if let Some(nodes) = try_lower_row_suffix_expression(session, state, id, head)? {
                    return Ok(nodes);
                }
                if let Ok(value) = parse_plan_value_expr(head, state, None) {
                    if looks_like_data_literal(head) {
                        return Ok(vec![DagNode {
                            id: id.to_string(),
                            expr: display.to_string(),
                            singleton: true,
                            page_size: None,
                            source: DagNodeSource::Data(value.0),
                        }]);
                    }
                }
                return Ok(vec![compile_surface_node(session, state, id, head)?]);
            }
            let suffixes: Vec<RowSuffix> = collect_meta.iter().map(RowSuffix::from).collect();
            lower_suffix_stream(session, state, id, display, head, suffixes, Some(id))
        }
    }
}

fn stage_row_expr_to_source(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    display: &str,
    row: &RowExpr,
    final_id: Option<&str>,
) -> Result<(Vec<DagNode>, String), String> {
    match row {
        RowExpr::Pipe(pipe) => {
            let out_id = final_id
                .map(str::to_string)
                .unwrap_or_else(|| format!("__plasm_{id}_apply_src"));
            if state.contains(pipe.head.as_str()) {
                if pipe.explicit_from {
                    return Err(format!(
                        "`from` is forbidden on binding label `{}`; use `{} | …`",
                        pipe.head, pipe.head
                    ));
                }
                let suffixes = pipe.row_suffixes()?;
                if suffixes.is_empty() {
                    return Ok((Vec::new(), pipe.head.clone()));
                }
                return Ok((
                    lower_suffix_stream(
                        session,
                        state,
                        &out_id,
                        display,
                        pipe.head.as_str(),
                        suffixes,
                        Some(&out_id),
                    )?,
                    out_id,
                ));
            }
            if !pipe.explicit_from {
                return Err(format!(
                    "unknown binding `{}`; catalog pipe heads require `from {} | …`",
                    pipe.head, pipe.head
                ));
            }
            let suffixes = pipe.row_suffixes()?;
            if suffixes.is_empty() {
                return Ok((
                    vec![compile_surface_node(session, state, &out_id, pipe.head.as_str())?],
                    out_id,
                ));
            }
            if let Some(mut prefix) =
                try_lower_row_suffix_expression(session, state, &out_id, pipe.head.as_str())?
            {
                let staged_state = compile_state_with_nodes(state, &prefix);
                let mut lowered = lower_suffix_stream(
                    session,
                    &staged_state,
                    &out_id,
                    display,
                    &out_id,
                    suffixes,
                    Some(&out_id),
                )?;
                prefix.append(&mut lowered);
                return Ok((prefix, out_id));
            }
            Ok((
                lower_suffix_stream(
                    session,
                    state,
                    &out_id,
                    display,
                    pipe.head.as_str(),
                    suffixes,
                    Some(&out_id),
                )?,
                out_id,
            ))
        }
        RowExpr::Primary { head, collect_meta } => {
            let head = head.trim();
            if collect_meta.is_empty() && state.contains(head) {
                return Ok((Vec::new(), head.to_string()));
            }
            let out_id = final_id
                .map(str::to_string)
                .unwrap_or_else(|| format!("__plasm_{id}_apply_src"));
            let suffixes: Vec<RowSuffix> = collect_meta.iter().map(RowSuffix::from).collect();
            let nodes = if suffixes.is_empty() {
                if let Some(nodes) = try_lower_row_suffix_expression(session, state, &out_id, head)?
                {
                    nodes
                } else {
                    vec![compile_surface_node(session, state, &out_id, head)?]
                }
            } else {
                lower_suffix_stream(
                    session,
                    state,
                    &out_id,
                    display,
                    head,
                    suffixes,
                    Some(&out_id),
                )?
            };
            Ok((nodes, out_id))
        }
    }
}

fn lower_pipe_row_expression(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    display: &str,
    pipe: PipeExpr,
) -> Result<Vec<DagNode>, String> {
    if state.contains(pipe.head.as_str()) {
        if pipe.explicit_from {
            return Err(format!(
                "`from` is forbidden on binding label `{}`; use `{} | …`",
                pipe.head, pipe.head
            ));
        }
        return binding_continuation::lower_pipe_continuation(session, state, id, display, &pipe);
    }
    if !pipe.explicit_from {
        return Err(format!(
            "unknown binding `{}`; catalog pipe heads require `from {} | …`",
            pipe.head, pipe.head
        ));
    }
    let suffixes = pipe.row_suffixes()?;
    let head_id = format!("__plasm_{id}_pipe_head");
    if let Some(mut prefix) =
        try_lower_row_suffix_expression(session, state, &head_id, pipe.head.as_str())?
    {
        let scratch = compile_state_with_nodes(state, &prefix);
        let mut suffix_nodes = lower_suffix_stream(
            session,
            &scratch,
            id,
            display,
            head_id.as_str(),
            suffixes,
            Some(id),
        )?;
        prefix.append(&mut suffix_nodes);
        return Ok(prefix);
    }
    lower_suffix_stream(
        session,
        state,
        id,
        display,
        pipe.head.as_str(),
        suffixes,
        Some(id),
    )
}

/// Longest bound label match so `repos.foo` wins over `repo.foo` when both exist.
pub(in crate::plasm_dag) fn longest_matching_bound_prefix(
    expr: &str,
    state: &CompileState<'_>,
) -> Option<(String, String)> {
    let expr = expr.trim();
    let mut best: Option<(usize, String, String)> = None;
    for label in state.labels.keys() {
        let prefix = format!("{label}.");
        if expr.starts_with(&prefix) {
            let tail = expr[prefix.len()..].to_string();
            if best.as_ref().is_none_or(|(len, _, _)| label.len() > *len) {
                best = Some((label.len(), label.clone(), tail));
            }
        }
    }
    best.map(|(_, l, t)| (l, t))
}

/// Unified binding contract for a program label (replaces parallel walkers).
pub(in crate::plasm_dag) fn relation_wire_names_for_source(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    source: &str,
) -> Vec<String> {
    let contract = match binding_contract(state, source) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let cgs = match cgs_for_qualified_entity(session, &contract.row_entity) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let ent = match cgs.get_entity(contract.row_entity.entity.as_str()) {
        Some(e) => e,
        None => return Vec::new(),
    };
    ent.relations
        .keys()
        .map(|k| k.as_str().to_string())
        .collect()
}

pub(in crate::plasm_dag) fn compile_surface_node(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
) -> Result<DagNode, String> {
    if let Some(mut nodes) = try_lower_row_suffix_expression(session, state, id, expr)? {
        return nodes.pop().ok_or_else(|| {
            format!("Plasm program `{id}`: postfix/relation chain `{expr}` produced no nodes")
        });
    }
    if let Some((label, tail)) = longest_matching_bound_prefix(expr, state) {
        let contract = binding_contract(state, &label).ok_or_else(|| {
            format!("Plasm program `{id}`: unknown binding `{label}` for continuation")
        })?;
        let tail_trim = tail.trim();
        if tail_trim == "content" || tail_trim.starts_with("content.") {
            let site = if id.starts_with("return_") {
                ContentReferenceSite::ProgramRoot
            } else {
                ContentReferenceSite::Continuation
            };
            return Err(content_reference_error(&label, site, contract.continuation));
        }
        if matches!(contract.continuation, ContinuationCapability::Terminal) {
            return Err(format!(
                "Plasm program `{id}`: `{label}` is not a Plasm expression anchor — only surface/relation bindings and row-preserving projection bindings can be extended with `{label}.…`; aggregate/render/derive/data/for_each bindings must use postfix transforms or an explicit entity constructor"
            ));
        }
        if contract.supports_relation_dot() {
            return binding_continuation::dispatch_binding_continuation(
                session, state, id, expr, &label, tail_trim, &contract,
            );
        }
        return Err(plasm_core::plp::plp4_program(
            id,
            format!(
                "binding `{label}` cannot be extended with `{tail_trim}` — use postfix transforms on the binding expression or a CGS relation chain (`label.<relation>`)"
            ),
        ));
    }
    let refs = state.program_node_id_set();
    let parsed = parse_plasm_program_surface_for_dag(
        session,
        state.cross_cache,
        state.pipeline,
        expr,
        &refs,
        false,
        Some(id),
    )?;
    let uses = collect_template_uses_from_expr(&parsed.expr);
    let (kind, qualified_entity, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    let node = DagNode {
        id: id.to_string(),
        expr: expr.to_string(),
        singleton: matches!(parsed.expr, Expr::Get(_)),
        page_size: None,
        source: DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            effect_class,
            result_shape,
            uses_result: uses,
        },
    };
    validate_surface_inline_projection(session, state, &node)?;
    if let DagNodeSource::Surface { parsed, .. } = &node.source {
        validate_invoke_scalar_field_refs(session, state, id, &parsed.expr)?;
    }
    Ok(node)
}
pub(in crate::plasm_dag) fn split_return_list(
    line: &str,
    state: &mut CompileState<'_>,
    session: &ExecuteSession,
) -> Result<Vec<String>, String> {
    let mut roots = Vec::new();
    let parts = if parse_pipe_expr(line)?.is_some() {
        vec![line]
    } else {
        split_top_level(line, ',')?
    };
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let part = part.to_string();
        if state.contains(part.as_str()) {
            roots.push(part);
        } else {
            reject_bare_literal_noop_root(part.as_str())?;
            let id = format!("return_{}", roots.len() + 1);
            for node in compile_node_expr(session, state, &id, part.as_str())? {
                state.insert(node)?;
            }
            roots.push(id);
        }
    }
    Ok(roots)
}

pub(in crate::plasm_dag) fn require_node(
    state: &CompileState<'_>,
    node: &str,
) -> Result<(), String> {
    if state.contains(node) {
        Ok(())
    } else {
        Err(format!("unknown Plasm program node `{node}`"))
    }
}
