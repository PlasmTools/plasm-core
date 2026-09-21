//! Program compile pipeline entrypoints and per-node dispatch.

use super::binding_continuation;
use super::binding_contract::binding_contract;
use super::invoke_cardinality::validate_invoke_scalar_field_refs;
use super::plan_serialize::{
    collect_template_uses_from_expr, expression_template, infer_surface_contract, lower_plan_node,
    parse_plan_value_expr, stamp_plan_uses_result_qualified_entities,
};
use super::prelude::*;
use super::render_dag::compile_render_from_applicator;
use super::row_suffix::{
    compile_state_with_nodes, lower_suffix_stream, try_lower_row_suffix_expression,
};
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
        || src.trim_start().starts_with("iterate")
        // Classify syntax before validation so malformed pipelines keep their row-algebra diagnostic.
        || split_top_level(src, '|').is_ok_and(|parts| parts.len() > 1)
        || peel_collect_meta(src)
            .map(|(_, meta)| !meta.is_empty())
            .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn compile_surface_fixture_json(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    let plan = compile_plasm_surface_line_to_plan(
        pipeline,
        symbol_map_cross_cache,
        session,
        name,
        source,
    )?;
    serde_json::to_value(plan).map_err(|error| format!("fixture artifact serialization: {error}"))
}

/// Serialize a compiled fixture for tests that assert the artifact wire contract.
#[cfg(test)]
pub(crate) fn compile_plasm_dag_to_plan(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<serde_json::Value, String> {
    let plan =
        compile_plasm_dag_to_plan_inner(pipeline, symbol_map_cross_cache, session, name, source)?;
    serde_json::to_value(plan).map_err(|error| format!("fixture artifact serialization: {error}"))
}

// compile_plasm_program / compile_plasm_expression live in plasm_compile.rs

pub(crate) fn compile_plasm_dag_to_plan_inner(
    pipeline: &PromptPipelineConfig,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    session: &ExecuteSession,
    name: &str,
    source: &str,
) -> Result<crate::plasm_plan::Plan, String> {
    let mut state = CompileState::new(pipeline, symbol_map_cross_cache);
    let flattened = expand_flattened_program_statements(&collect_program_statement_lines(source)?);
    let statements = flattened.statements;
    if statements.is_empty() {
        return Err(program_empty_error());
    }
    validate_program_statement_order(&statements)?;
    let mut final_roots: Option<Vec<String>> = None;
    for stmt in statements {
        if let Some(assignment) = classify_top_level_assignment(&stmt) {
            let (id, rhs) = match assignment {
                TopLevelAssignment::Binding { label, rhs } => (label, rhs),
                TopLevelAssignment::InvalidLabel { label } => {
                    return Err(program_invalid_binding_label_error(label));
                }
            };
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
        .map(|n| lower_plan_node(n.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let return_value = if roots.len() == 1 {
        crate::plasm_plan::PlanReturn::Node {
            node: roots[0].clone(),
        }
    } else {
        crate::plasm_plan::PlanReturn::Parallel { nodes: roots }
    };
    let mut metadata = BTreeMap::new();
    metadata.insert("language".to_string(), serde_json::json!("plasm-dag"));
    if let Some(label) = flattened.coerced_default_return {
        metadata.insert(
            "coerced_default_return".to_string(),
            serde_json::json!(label),
        );
    }
    let mut plan =
        crate::plasm_plan::Plan::from_nodes(Some(name.to_owned()), nodes, return_value, metadata);
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
) -> Result<crate::plasm_plan::Plan, String> {
    let trimmed = line.trim();
    if is_plasm_dag_source(trimmed) {
        return compile_plasm_dag_to_plan_inner(
            pipeline,
            symbol_map_cross_cache,
            session,
            name,
            trimmed,
        );
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
        .map(|n| lower_plan_node(n.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let return_value = if roots.len() == 1 {
        crate::plasm_plan::PlanReturn::Node {
            node: roots[0].clone(),
        }
    } else {
        crate::plasm_plan::PlanReturn::Parallel { nodes: roots }
    };
    let mut plan = crate::plasm_plan::Plan::from_nodes(
        Some(name.to_owned()),
        nodes,
        return_value,
        BTreeMap::from([("language".to_owned(), serde_json::json!("plasm-dag"))]),
    );
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

/// Apply the root read/operation per row, then lower relation hops through the
/// same traversal nodes used by ordinary bindings. A chain is not a unary leaf.
fn lower_catalog_application(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    display: &str,
    source: &str,
    surface: &str,
    parsed: plasm_core::expr_parser::ParsedExpr,
) -> Result<Vec<DagNode>, String> {
    if let Expr::Chain(chain) = &parsed.expr {
        if !matches!(chain.step, plasm_core::ChainStep::AutoGet) {
            return Err(format!(
                "Plasm program `{id}`: bind the relation rows before applying an explicit continuation"
            ));
        }
        let parent_id = format!("__plasm_{id}_apply_parent");
        let mut parent = parsed.clone();
        parent.expr = (*chain.source).clone();
        parent.projection = None;
        let mut nodes = lower_catalog_application(
            session, state, &parent_id, display, source, surface, parent,
        )?;
        let staged = compile_state_with_nodes(state, &nodes);
        let mut relation = binding_continuation::lower_relation_application(
            session,
            &staged,
            id,
            display,
            &parent_id,
            chain.selector.as_str(),
        )?;
        if let DagNodeSource::RelationTraversal {
            parsed: lowered,
            plan_relation,
            ..
        } = &mut relation.source
        {
            lowered.projection = parsed.projection.clone();
            plan_relation.ir.projection = parsed.projection;
        }
        nodes.push(relation);
        return Ok(nodes);
    }
    let uses =
        collect_template_uses_from_expr(&parsed.expr, Some("_"), &state.program_node_id_set());
    let (kind, qualified, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    if !kind.is_template_allowed() {
        return Err(format!(
            "Plasm program `{id}` row application must be a catalog read or operation expression"
        ));
    }
    Ok(vec![DagNode {
        id: id.to_string(),
        expr: display.to_string(),
        singleton: false,
        page_size: None,
        source: DagNodeSource::ForEach {
            source: source.to_string(),
            parsed_template: expression_template(&parsed, &uses),
            display_expr: surface.to_string(),
            effect_kind: kind,
            effect_class,
            result_shape,
            qualified_entity: qualified,
            uses_result: uses,
        },
    }])
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
                Applicator::Apply { surface } => {
                    let surface = if let Some(tail) = surface.trim().strip_prefix("_.") {
                        binding_continuation::row_receiver_surface(
                            session, state, source, "_", tail,
                        )?
                    } else {
                        surface
                    };
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
                    lower_catalog_application(
                        session,
                        state,
                        id,
                        display,
                        source,
                        surface.trim(),
                        parsed,
                    )?
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
        RowExpr::Iterate(it) => lower_iterate_until(session, state, id, display, it),
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
                return compile_surface_nodes(session, state, id, head);
            }
            let suffixes: Vec<RowSuffix> = collect_meta.iter().map(RowSuffix::from).collect();
            lower_suffix_stream(session, state, id, display, head, suffixes, Some(id))
        }
    }
}

fn lower_iterate_until(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    display: &str,
    it: plasm_core::expr_parser::IterateUntilExpr,
) -> Result<Vec<DagNode>, String> {
    let seed_is_label = plasm_core::expr_parser::iterate_seed_is_label(it.seed.as_str());
    let seed_id = format!("__plasm_{id}_iterate_seed");
    let mut prefix = if seed_is_label {
        if !state.contains(it.seed.as_str()) {
            return Err(format!(
                "Plasm program `{id}`: iterate seed binding `{}` is unknown",
                it.seed
            ));
        }
        // Proven binding — require StaticSingleton at binding-contract time; reuse label as seed.
        Vec::new()
    } else {
        compile_surface_nodes(session, state, &seed_id, it.seed.as_str())?
    };
    let seed_label = if seed_is_label {
        it.seed.clone()
    } else {
        seed_id
    };
    if seed_is_label {
        require_node(state, seed_label.as_str())?;
    } else {
        // Seed nodes must exist under seed_label before step parse can see `_` from uses.
        let scratch = compile_state_with_nodes(state, &prefix);
        require_node(&scratch, seed_label.as_str())?;
    }
    let seed_node = if seed_is_label {
        state.get(seed_label.as_str())
    } else {
        prefix.iter().find(|n| n.id == seed_label)
    }
    .ok_or_else(|| format!("Plasm program `{id}`: iterate seed `{seed_label}` is unknown"))?;
    require_iterate_seed_get_identity(seed_node, seed_label.as_str())?;

    let scratch = if prefix.is_empty() {
        None
    } else {
        Some(compile_state_with_nodes(state, &prefix))
    };
    let state = scratch.as_ref().unwrap_or(state);

    let refs = state.program_node_id_set();
    let parsed = parse_plasm_program_surface_for_dag(
        session,
        state.cross_cache,
        state.pipeline,
        it.step.trim(),
        &refs,
        true,
        Some(id),
    )?;
    let mut uses =
        collect_template_uses_from_expr(&parsed.expr, Some("_"), &state.program_node_id_set());
    let (kind, qualified, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    if !matches!(
        kind,
        PlanNodeKind::Create | PlanNodeKind::Update | PlanNodeKind::Delete | PlanNodeKind::Action
    ) {
        return Err(format!(
            "Plasm program `{id}` iterate step must be a write/side-effect expression"
        ));
    }

    // Compile until predicate against seed entity (fail closed at lower time).
    let cgs = cgs_for_qualified_entity(session, &qualified).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for iterate entity `{}`",
            qualified.entry_id, qualified.entity
        )
    })?;
    let layer = plasm_core::CgsLayer::new(qualified.entry_id.as_str(), cgs.as_ref());
    let stack = [layer];
    let sym_map = state.sym_map_for(session);
    let row_pred = plasm_core::parse_row_predicate_list(
        qualified.entity.as_str(),
        it.until.as_str(),
        &stack,
        sym_map,
        &[],
        &state.program_node_id_set(),
    )
    .map_err(|e| format!("Plasm program `{id}` iterate until predicate: {e}"))?;
    let until_predicates = crate::row_predicate_lower::lower_row_predicate_to_plan(
        &row_pred,
        session,
        &qualified,
        state.cross_cache,
        &[],
    )
    .map_err(|e| format!("Plasm program `{id}` iterate until lower: {e}"))?;

    for predicate in &until_predicates {
        for label in predicate.value.dependencies() {
            let contract = super::binding_contract::binding_contract(state, &label)
                .ok_or_else(|| format!("unknown until predicate binding `{label}`"))?;
            if !contract.row_cardinality.permits_scalar_field_extract() {
                return Err(format!(
                    "until predicate binding `{label}` is plural; select exactly one row"
                ));
            }
            uses.push(super::plan_serialize::result_use(&label, &label));
        }
    }
    let uses = super::plan_serialize::dedupe_uses(uses);

    prefix.push(DagNode {
        id: id.to_string(),
        expr: display.to_string(),
        singleton: true,
        page_size: None,
        source: DagNodeSource::IterateUntil {
            seed: seed_label,
            parsed_step_template: expression_template(&parsed, &uses),
            step_display: it.step.trim().to_string(),
            effect_kind: kind,
            effect_class,
            result_shape,
            qualified_entity: qualified,
            until_body: it.until.clone(),
            until_predicates,
            take: it.take,
            uses_result: uses,
        },
    });
    Ok(prefix)
}

/// Get identity is re-observable: seed kind is Get (`ir` or `ir_template` + binding).
fn dag_node_is_get_identity(node: &DagNode) -> bool {
    match &node.source {
        DagNodeSource::Surface { kind, .. } => *kind == PlanNodeKind::Get,
        _ => false,
    }
}

fn require_iterate_seed_get_identity(node: &DagNode, seed: &str) -> Result<(), String> {
    if dag_node_is_get_identity(node) {
        return Ok(());
    }
    Err(plasm_core::expr_parser::iterate_seed_must_be_get_identity(
        seed,
    ))
}

/// When the pipe head is not a bound label, decide catalog materialization vs unknown binding.
fn pipe_head_materializes_catalog(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    head: &str,
) -> Result<bool, String> {
    if state.contains(head) {
        return Ok(false);
    }
    if pipe_head_has_catalog_surface_syntax(head) {
        return Ok(true);
    }
    if is_valid_program_label(head) {
        return match crate::catalog_ownership::resolve_cgs_for_entity(session, head, None) {
            Ok(_) => Ok(true),
            Err(_) => Err(format!("unknown binding `{head}`")),
        };
    }
    Ok(true)
}

/// Compile a closed row expression to DAG nodes and the result node id (RA-13 membership RHS).
pub(in crate::plasm_dag) fn compile_row_expr_nodes(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    display: &str,
    row: &RowExpr,
) -> Result<(Vec<DagNode>, String), String> {
    stage_row_expr_to_source(session, state, id, display, row, Some(id))
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
            if !pipe_head_materializes_catalog(session, state, pipe.head.as_str())? {
                return Err(format!("unknown binding `{}`", pipe.head));
            }
            let suffixes = pipe.row_suffixes()?;
            if suffixes.is_empty() {
                return Ok((
                    compile_surface_nodes(session, state, &out_id, pipe.head.as_str())?,
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
                    compile_surface_nodes(session, state, &out_id, head)?
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
        RowExpr::Iterate(_) => Err(format!(
            "Plasm program `{id}`: `iterate … until … take N` cannot be staged as an apply left-hand; bind it as its own expression"
        )),
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
        return binding_continuation::lower_pipe_continuation(session, state, id, display, &pipe);
    }
    if !pipe_head_materializes_catalog(session, state, pipe.head.as_str())? {
        return Err(format!("unknown binding `{}`", pipe.head));
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
    let mut nodes = compile_surface_nodes(session, state, id, expr)?;
    if nodes.len() != 1 {
        return Err(format!(
            "Plasm program `{id}`: surface `{expr}` lowered to {} nodes — use `compile_surface_nodes` for multi-node surfaces (PLP-1 field-dot extract)",
            nodes.len()
        ));
    }
    nodes
        .pop()
        .ok_or_else(|| format!("Plasm program `{id}`: empty surface"))
}

/// Compile a catalog / binding-continuation surface; may return Get + scalar Derive (PLP-1).
pub(in crate::plasm_dag) fn compile_surface_nodes(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    id: &str,
    expr: &str,
) -> Result<Vec<DagNode>, String> {
    if let Some(nodes) = try_lower_row_suffix_expression(session, state, id, expr)? {
        return Ok(nodes);
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
            return Ok(vec![binding_continuation::dispatch_binding_continuation(
                session, state, id, expr, &label, tail_trim, &contract,
            )?]);
        }
        return Err(plasm_core::plp::plp4_program(
            id,
            format!(
                "binding `{label}` cannot be extended with `{tail_trim}` — use postfix transforms on the binding expression or a CGS relation chain (`label.<relation>`)"
            ),
        ));
    }
    let refs = state.program_node_id_set();
    let mut parsed = parse_plasm_program_surface_for_dag(
        session,
        state.cross_cache,
        state.pipeline,
        expr,
        &refs,
        false,
        Some(id),
    )?;
    plasm_core::apply_required_selection_defaults_in_expr(
        &mut parsed.expr,
        session.cgs.as_ref(),
        expr,
    )
    .map_err(|e| e.to_string())?;
    if let Some(wire) = parsed.field_dot_extract.take() {
        return super::scalar_extract::compile_catalog_singleton_field_dot(
            session, state, id, expr, parsed, wire,
        );
    }
    validate_invoke_scalar_field_refs(session, state, id, &parsed.expr)?;
    super::password_domain::validate_password_domain_bind(session, state, id, &parsed.expr)?;
    super::prerequisite_seats::validate_prerequisite_seat_bind(session, state, id, &parsed.expr)?;
    let mut extra_nodes = super::scalar_extract::expand_get_scalar_extracts_in_expr(
        session,
        state,
        id,
        &mut parsed.expr,
    )?;
    let uses = collect_template_uses_from_expr(&parsed.expr, None, &state.program_node_id_set());
    let (kind, qualified_entity, effect_class, result_shape) =
        infer_surface_contract(session, &parsed.expr)?;
    let view_singleton = match &parsed.expr {
        Expr::Query(query) => {
            cgs_for_qualified_entity(session, &qualified_entity).is_some_and(|cgs| {
                let cap = match query.capability_name.as_deref() {
                    Some(name) => cgs.get_capability(name),
                    None => cgs.primary_query_capability(query.entity.as_str()),
                };
                cap.and_then(|cap| cap.mapping.as_ref())
                    .is_some_and(|mapping| {
                        mapping
                            .template
                            .0
                            .get("transport")
                            .and_then(serde_json::Value::as_str)
                            == Some("view")
                    })
            })
        }
        _ => false,
    };
    let node = DagNode {
        id: id.to_string(),
        expr: expr.to_string(),
        singleton: matches!(parsed.expr, Expr::Get(_)),
        page_size: None,
        source: DagNodeSource::Surface {
            view_singleton,
            parsed,
            kind,
            qualified_entity,
            effect_class,
            result_shape,
            uses_result: uses,
        },
    };
    validate_surface_inline_projection(session, state, &node)?;
    extra_nodes.push(node);
    Ok(extra_nodes)
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
