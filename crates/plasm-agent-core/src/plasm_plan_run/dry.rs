//! Dry-run evaluation.

use super::*;
use crate::plasm_comp_lift::ExecutablePlasmComp;
use crate::plasm_step_convert::step_payload_to_validated_node;
use plasm_core::PlasmCompArtifact;

#[path = "dry_render.rs"]
mod dry_render;
pub use dry_render::render_node_operation;
use dry_render::{render_return_lines, render_uses_result};

pub fn evaluate_plasm_comp_dry(
    es: &ExecuteSession,
    bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
) -> Result<DryPlasmPlanEvaluation, String> {
    evaluate_executable_comp_dry(es, bundle.executable(), bundle.artifact())
}

pub fn evaluate_executable_comp_dry(
    es: &ExecuteSession,
    executable: &ExecutablePlasmComp,
    artifact: &PlasmCompArtifact,
) -> Result<DryPlasmPlanEvaluation, String> {
    let comp = &artifact.comp;
    // Record comp commit when evidence chain is active (noop when disabled).
    if let Some(evidence) = crate::evidence_chain::chain(es) {
        evidence
            .record_comp_committed(comp)
            .map_err(|e| format!("evidence comp_committed: {e}"))?;
    }
    let version = serde_json::json!(comp.version);
    let mut out = Vec::new();
    let mut parallel_root_surfaces_only = true;
    let mut staged_nodes = Vec::new();
    let execution_unsupported = Vec::new();
    for (step_idx, (step_id, payload)) in executable.steps_topo.iter().enumerate() {
        let n = step_payload_to_validated_node(step_id, payload, &executable.bind)?;
        ensure_node_dispatchable(es, &n, step_idx)?;
        if let ValidatedPlanNode::RelationTraversal(relation) = &n {
            let pe = ParsedExpr {
                expr: relation.relation.ir.expr.clone(),
                projection: relation.relation.ir.projection.clone(),
            };
            typecheck_parsed_for_session(es, &pe)
                .map_err(|e| format!("type check in plan.nodes[{step_idx}].relation.expr: {e}"))?;
            ensure_relation_expr_matches_plan(es, relation, &pe, step_idx)?;
        }
        let inferred_approval = inferred_node_approval(&n);
        if n.depends_on().is_empty() && n.uses_result().is_empty() {
            let Some(surface) = n.as_surface() else {
                parallel_root_surfaces_only = false;
                staged_nodes.push(format!("{} ({:?})", n.id(), n.kind()));
                out.push(dry_stage_result(step_idx, &n));
                continue;
            };
            let ir = surface
                .ir
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{step_idx}] requires staged IR execution"))?;
            let scoped_es = entry_scoped_execute_session(es, surface.qualified_entity.as_ref())?;
            let pe = ParsedExpr {
                expr: ir.expr.clone(),
                projection: ir.projection.clone(),
            };
            let normalized =
                crate::execute_pipeline::PlasmPreflight::preflight_node_compile_dispatch(
                    es, &scoped_es, surface, &pe, step_idx,
                )?;
            let (intent, il, bindings) = dry_run_simulation_for_session(&scoped_es, &normalized);
            let expr = ir
                .display_expr
                .as_deref()
                .or(surface.display_expr.as_deref())
                .unwrap_or("<ir>");
            out.push(serde_json::json!({
                "index": step_idx,
                "ok": true,
                "id": n.id().as_str(),
                "kind": n.kind(),
                "operation": render_node_operation(&n),
                "qualified_entity": surface.qualified_entity,
                "effect_class": n.effect_class(),
                "result_shape": n.result_shape(),
                "projection": surface.projection,
                "predicates": surface.predicates,
                "approval_gate": inferred_approval,
                "ir": {
                    "expr": normalized.expr,
                    "projection": normalized.projection
                },
                "execution_contract": {
                    "entry_id": surface.qualified_entity.as_ref().map(|q| q.entry_id.as_str()).unwrap_or(es.entry_id.as_str()),
                    "entity": surface.qualified_entity.as_ref().map(|q| q.entity.as_str()),
                    "display_expr": expr,
                    "ir": normalized.expr,
                    "projection": normalized.projection
                },
                "type_check": "ok",
                "simulation": {
                    "intent": intent,
                    "il": il,
                    "bindings": bindings
                }
            }));
            continue;
        }

        parallel_root_surfaces_only = false;
        staged_nodes.push(format!("{} ({:?})", n.id(), n.kind()));
        out.push(dry_stage_result(step_idx, &n));
    }
    let prepared = crate::plan_prepare::prepare_executable_plan_for_session(es, comp, executable)?;
    dry_validate_render_nodes(es, prepared.validated.artifact())?;
    Ok(DryPlasmPlanEvaluation {
        version,
        name: comp.name.clone(),
        artifact: artifact.clone(),
        executable: executable.clone(),
        cached_validated: std::cell::OnceCell::from(prepared.validated),
        topological_order: executable
            .steps_topo
            .iter()
            .map(|(id, _)| id.as_str().to_string())
            .collect(),
        node_results: out,
        parallel_root_surfaces_only,
        staged_nodes,
        execution_unsupported,
        graph_summary: prepared.graph_summary,
        review: prepared.review,
    })
}

pub(crate) fn unused_seed_hints(
    es: &ExecuteSession,
    plan: &Plan<ValidatedPlanState>,
) -> Vec<String> {
    let used = crate::plan_prepare::collect_plan_entity_names(plan);
    es.entities
        .iter()
        .filter(|e| !used.contains(e.as_str()))
        .map(|e| {
            format!(
                "{}:{}",
                crate::catalog_ownership::entry_id_for_entity_trace(es, e.as_str()),
                e
            )
        })
        .collect()
}

pub(crate) fn enrich_graph_summary_auth_scoped_reads(
    es: &ExecuteSession,
    plan: &Plan<ValidatedPlanState>,
    summary: &mut serde_json::Value,
) {
    let exp = match es.teaching_exposure.as_ref() {
        Some(e) => e.clone(),
        None => return,
    };
    let fed = plasm_core::FederationDispatch::from_contexts_and_exposure(
        es.contexts_by_entry.clone(),
        &exp,
    );
    let mut auth_scoped = false;
    for n in &plan.nodes {
        let ValidatedPlanNode::Surface(s) = n else {
            continue;
        };
        if !node_dependencies(n).is_empty() {
            continue;
        }
        let Some(ir) = &s.ir else {
            continue;
        };
        let plasm_core::Expr::Query(q) = &ir.expr else {
            continue;
        };
        if q.predicate.is_some() {
            continue;
        }
        let cgs = fed
            .resolve_entity(
                q.entity.as_str(),
                plasm_core::ResolutionHint::default(),
                es.cgs.as_ref(),
            )
            .unwrap_or(es.cgs.as_ref());
        if plasm_core::resolve_query_capability(q, cgs)
            .ok()
            .is_some_and(|c| c.name.as_str() == "auth_user_repos_query")
        {
            auth_scoped = true;
            break;
        }
    }
    if auth_scoped {
        if let Some(facts) = summary
            .get_mut("boundedness_facts")
            .and_then(|v| v.as_array_mut())
        {
            facts.push(serde_json::Value::String(
                "Lists repos visible to the authenticated GitHub token; use Repository~\"…\" or user_repos_query for other scopes.".into(),
            ));
        }
    }
}

/// Render the compact agent-facing dry-run plan text.
pub fn render_plasm_plan_dry_text(
    dry: &DryPlasmPlanEvaluation,
    archive: Option<PlasmPlanDryRunTextMeta<'_>>,
) -> String {
    render_plasm_plan_dry_text_for_session(dry, archive, None)
}

/// Same as [`render_plasm_plan_dry_text`] with optional execute session for teaching table-aware surface expr.
pub fn render_plasm_plan_dry_text_for_session(
    dry: &DryPlasmPlanEvaluation,
    archive: Option<PlasmPlanDryRunTextMeta<'_>>,
    es: Option<&ExecuteSession>,
) -> String {
    let view = plan_dry_compact_view(dry, es);
    plan_dry_display::render_plan_dry_compact_text(&view, archive.as_ref().map(|a| a.plan_handle))
}

/// Typed compact view for tests and UI.
pub fn plan_dry_compact_view(
    dry: &DryPlasmPlanEvaluation,
    es: Option<&ExecuteSession>,
) -> plan_dry_display::PlanDryCompactView {
    plan_dry_display::build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        es,
    )
}

/// Semantic plan DAG (`version`, `nodes`, `edges`, `topological_order`, `returns`) — stable input
/// for plan commit-id hashing (excludes session-local `name` and dry-run `summary`).
pub fn plan_semantic_dag_json(dry: &DryPlasmPlanEvaluation) -> serde_json::Value {
    let plan = dry.validated_plan();
    let nodes = plan
        .nodes
        .iter()
        .map(|node| {
            serde_json::json!({
                "id": node.id().as_str(),
                "kind": node.kind(),
                "effect_class": node.effect_class(),
                "result_shape": node.result_shape(),
                "dependencies": node_dependencies(node),
                "uses_result": render_uses_result(node),
                "operation": render_node_operation(node),
            })
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    for node in &plan.nodes {
        for from in node_dependencies(node) {
            edges.push(serde_json::json!({
                "from": from,
                "to": node.id().as_str(),
            }));
        }
    }
    serde_json::json!({
        "version": plan.version,
        "nodes": nodes,
        "edges": edges,
        "topological_order": dry.topological_order.clone(),
        "returns": render_return_lines(&plan.return_value),
    })
}

/// Trace/UI DAG payload (name + summary). Call sites outside `dry.rs` must use this helper.
pub fn plan_dag_trace_json(dry: &DryPlasmPlanEvaluation) -> serde_json::Value {
    plasm_plan_dag_json(dry)
}

/// Structured DAG payload for trace/UI renderers. This is the machine-readable companion to the
/// compact dry-run text, so clients do not have to parse Markdown to draw plan topology.
pub fn plasm_plan_dag_json(dry: &DryPlasmPlanEvaluation) -> serde_json::Value {
    let mut obj = plan_semantic_dag_json(dry)
        .as_object()
        .expect("semantic plan DAG is an object")
        .clone();
    obj.insert("name".into(), serde_json::json!(dry.name));
    obj.insert("summary".into(), dry.graph_summary.clone());
    serde_json::Value::Object(obj)
}

pub fn node_dependencies(node: &ValidatedPlanNode) -> Vec<String> {
    let mut out = Vec::new();
    push_unique(
        &mut out,
        node.depends_on().iter().map(|id| id.as_str().to_string()),
    );
    push_unique(&mut out, node.uses_result().iter().map(|u| u.node.clone()));
    match node {
        ValidatedPlanNode::Derive(n) => {
            push_unique(&mut out, std::iter::once(n.source.as_str().to_string()));
            push_unique(
                &mut out,
                n.inputs.iter().map(|input| input.node.as_str().to_string()),
            );
        }
        ValidatedPlanNode::Compute(n) => {
            push_unique(&mut out, std::iter::once(n.compute.source.clone()));
        }
        ValidatedPlanNode::ForEach(n) => {
            push_unique(&mut out, std::iter::once(n.source.as_str().to_string()));
        }
        ValidatedPlanNode::RelationTraversal(n) => {
            push_unique(
                &mut out,
                std::iter::once(n.relation.source.as_str().to_string()),
            );
        }
        _ => {}
    }
    out
}

pub(crate) fn push_unique(out: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    for value in values {
        if !out.iter().any(|seen| seen == &value) {
            out.push(value);
        }
    }
}

pub(crate) fn plan_has_query_limit_row_filter_chain(plan: &Plan<ValidatedPlanState>) -> bool {
    let by_id: std::collections::HashMap<&str, &ValidatedPlanNode> =
        plan.nodes.iter().map(|n| (n.id().as_str(), n)).collect();
    for n in &plan.nodes {
        let ValidatedPlanNode::Compute(c) = n else {
            continue;
        };
        let ComputeOp::Limit { .. } = c.compute.op else {
            continue;
        };
        let Some(q_node) = by_id.get(c.compute.source.as_str()) else {
            continue;
        };
        let ValidatedPlanNode::Surface(s) = q_node else {
            continue;
        };
        if s.kind == PlanNodeKind::Query {
            return true;
        }
    }
    for n in &plan.nodes {
        let ValidatedPlanNode::Compute(c) = n else {
            continue;
        };
        let ComputeOp::Filter { .. } = c.compute.op else {
            continue;
        };
        let Some(limit_node) = by_id.get(c.compute.source.as_str()) else {
            continue;
        };
        let ValidatedPlanNode::Compute(limit_c) = limit_node else {
            continue;
        };
        let ComputeOp::Limit { .. } = limit_c.compute.op else {
            continue;
        };
        let Some(q_node) = by_id.get(limit_c.compute.source.as_str()) else {
            continue;
        };
        let ValidatedPlanNode::Surface(s) = q_node else {
            continue;
        };
        if s.kind == PlanNodeKind::Query {
            return true;
        }
    }
    false
}

pub(crate) fn graph_summary(
    plan: &Plan<ValidatedPlanState>,
    boundedness: &crate::plan_prepare::ReadBoundedness,
) -> (serde_json::Value, PlanDryReview) {
    let mut read_nodes = Vec::new();
    let mut write_or_side_effect_nodes = Vec::new();
    let mut derive_nodes = Vec::new();
    let mut template_nodes = Vec::new();
    let mut approval_gates = Vec::new();
    let mut parallelizable_roots = Vec::new();
    let mut warnings = Vec::new();
    let mut boundedness_facts = Vec::new();

    let mut has_narrowed_search_root = false;
    let mut has_narrowed_filter_root = false;
    let mut has_explicit_limit = false;
    let mut has_full_collection_compute = false;
    let mut relation_traversal_nodes = 0usize;
    let mut has_unprojected_multi_row_read = false;

    for n in &plan.nodes {
        if node_dependencies(n).is_empty() {
            parallelizable_roots.push(n.id().as_str().to_string());
        }
        match n.effect_class() {
            EffectClass::Read => read_nodes.push(n.id().as_str().to_string()),
            EffectClass::Write | EffectClass::SideEffect => {
                write_or_side_effect_nodes.push(n.id().as_str().to_string())
            }
            EffectClass::ArtifactRead => derive_nodes.push(n.id().as_str().to_string()),
        }
        if matches!(n, ValidatedPlanNode::ForEach(_)) {
            template_nodes.push(n.id().as_str().to_string());
        }
        if let Some(approval) = inferred_node_approval(n) {
            approval_gates.push(approval);
        }
        if matches!(n.result_shape(), crate::plasm_plan::ResultShape::List)
            && n.effect_class() == EffectClass::Read
            && node_dependencies(n).is_empty()
        {
            match n {
                ValidatedPlanNode::Surface(surface)
                    if surface.kind == PlanNodeKind::Search || !surface.predicates.is_empty() =>
                {
                    if surface.kind == PlanNodeKind::Search {
                        has_narrowed_search_root = true;
                    } else {
                        has_narrowed_filter_root = true;
                    }
                }
                _ => {}
            }
        }
        if matches!(n, ValidatedPlanNode::Compute(_)) {
            let op = render_node_operation(n);
            if op.contains("limit ") {
                has_explicit_limit = true;
            } else {
                has_full_collection_compute = true;
            }
        }
        if let ValidatedPlanNode::RelationTraversal(_) = n {
            relation_traversal_nodes += 1;
        }
        match n {
            ValidatedPlanNode::Surface(s)
                if s.effect_class == EffectClass::Read
                    && s.projection.is_empty()
                    && matches!(
                        s.result_shape,
                        crate::plasm_plan::ResultShape::List | crate::plasm_plan::ResultShape::Page
                    ) =>
            {
                has_unprojected_multi_row_read = true;
            }
            ValidatedPlanNode::ForEach(fe)
                if fe.effect_class == EffectClass::Read
                    && fe.projection.is_empty()
                    && matches!(
                        fe.result_shape,
                        crate::plasm_plan::ResultShape::List | crate::plasm_plan::ResultShape::Page
                    ) =>
            {
                has_unprojected_multi_row_read = true;
            }
            _ => {}
        }
    }

    let has_unbounded_read_root = boundedness.has_unbounded_read_root;
    let has_paginated_list_fetch_all_default = boundedness.has_paginated_list_fetch_all_default;
    let has_relation_many_source_fanout = boundedness.has_relation_many_source_fanout;
    let has_foreach_fanout_risk = boundedness.has_foreach_fanout_risk;

    if has_narrowed_search_root {
        boundedness_facts.push("Root read narrowed by search text".to_string());
    }
    if has_narrowed_filter_root {
        boundedness_facts.push("Root read narrowed by API-side filters".to_string());
    }
    if has_explicit_limit {
        boundedness_facts.push(
            "Explicit .limit(n) pushes read budget upstream (page_size / top-k / row filter early-stop)."
                .to_string(),
        );
    }
    if has_paginated_list_fetch_all_default {
        boundedness_facts.push(
            "Paginated list reads consume all API pages by default (runtime page cap); use .limit(n) or .page_size(n) to bound."
                .to_string(),
        );
    }
    if relation_traversal_nodes > 0 {
        boundedness_facts.push("Includes relation traversal".to_string());
    }
    if has_relation_many_source_fanout && has_paginated_list_fetch_all_default {
        boundedness_facts.push(
            "Parent list reads materialize all pages before relation fanout unless .page_size(n) caps the read."
                .to_string(),
        );
    }

    if has_unprojected_multi_row_read {
        warnings.push(
            "List/page reads without `[field,…]` projection materialize full rows; project at the read or add an explicit project step."
                .to_string(),
        );
    }
    if has_unbounded_read_root {
        warnings.push(
            "Unbounded root read; paginated APIs fetch all pages by default — add API filters/search text or .limit(n) / .page_size(n) when cost or latency is uncertain"
                .to_string(),
        );
    }
    if has_full_collection_compute {
        warnings.push(
            "Aggregates/group_by/sort run over the full logical row set before `.limit`; narrow reads (filters + projected fields) when counts are uncertain."
                .to_string(),
        );
    }
    if has_foreach_fanout_risk {
        warnings.push(
            "Mutating for_each may fan out over every source row; keep the upstream source bounded when cost or latency matters"
                .to_string(),
        );
    }
    if has_relation_many_source_fanout {
        warnings.push(
            "Relation traversal fans out one scoped query per upstream row (source_cardinality: many); bound the parent list with .limit(n), filters, or .page_size(n) when API cost matters"
                .to_string(),
        );
    }

    let review = PlanDryReview {
        has_unprojected_multi_row_read,
        has_unbounded_read_root,
        has_full_collection_compute,
        has_foreach_fanout_risk,
        has_relation_many_source_fanout,
        has_query_limit_row_filter: plan_has_query_limit_row_filter_chain(plan),
        has_paginated_list_fetch_all_default,
        unused_seeds: Vec::new(),
    };

    (
        serde_json::json!({
            "node_count": plan.nodes.len(),
            "read_nodes": read_nodes,
            "write_or_side_effect_nodes": write_or_side_effect_nodes,
            "derive_nodes": derive_nodes,
            "template_nodes": template_nodes,
            "approval_gates": approval_gates,
            "parallelizable_roots": parallelizable_roots,
            "warnings": warnings,
            "boundedness_facts": boundedness_facts,
            "dry_review": {
                "has_unbounded_read_root": has_unbounded_read_root,
                "has_full_collection_compute": has_full_collection_compute,
                "has_foreach_fanout_risk": has_foreach_fanout_risk,
                "has_relation_many_source_fanout": has_relation_many_source_fanout,
                "has_unprojected_multi_row_read": has_unprojected_multi_row_read,
            }
        }),
        review,
    )
}

pub(crate) fn inferred_node_approval(node: &ValidatedPlanNode) -> Option<serde_json::Value> {
    match node {
        ValidatedPlanNode::ForEach(n) => inferred_template_approval(n),
        ValidatedPlanNode::Surface(n) if node_requires_approval(n.kind, n.effect_class) => {
            let q = n.qualified_entity.as_ref()?;
            Some(approval_gate_json(
                n.id.as_str(),
                q,
                n.kind,
                None,
                n.approval.as_deref(),
            ))
        }
        _ => None,
    }
}

pub(crate) fn inferred_template_approval(node: &ValidatedForEachNode) -> Option<serde_json::Value> {
    if !node_requires_approval(node.effect_template.kind, node.effect_template.effect_class) {
        return None;
    }
    let action_name = if node.effect_template.kind == PlanNodeKind::Action {
        action_name_from_template(node.effect_template.expr_template.as_str())
    } else {
        None
    };
    Some(approval_gate_json(
        node.id.as_str(),
        &node.effect_template.qualified_entity,
        node.effect_template.kind,
        action_name.as_deref(),
        node.approval.as_deref(),
    ))
}

pub(crate) fn remote_mutation_effect(kind: PlanNodeKind, effect_class: EffectClass) -> bool {
    matches!(
        kind,
        PlanNodeKind::Create | PlanNodeKind::Update | PlanNodeKind::Delete | PlanNodeKind::Action
    ) || matches!(effect_class, EffectClass::Write | EffectClass::SideEffect)
}

pub(crate) fn node_requires_approval(kind: PlanNodeKind, effect_class: EffectClass) -> bool {
    remote_mutation_effect(kind, effect_class)
}

/// Remote mutation inside a `for_each` body (fan-out / multi-write risk). Read-only bodies excluded.
pub(crate) fn for_each_body_mutates_remote(kind: PlanNodeKind, effect_class: EffectClass) -> bool {
    remote_mutation_effect(kind, effect_class)
}

pub(crate) fn approval_gate_json(
    node_id: &str,
    q: &QualifiedEntityKey,
    kind: PlanNodeKind,
    action_name: Option<&str>,
    author_label: Option<&str>,
) -> serde_json::Value {
    let operation = action_name.unwrap_or(match kind {
        PlanNodeKind::Create => "create",
        PlanNodeKind::Update => "update",
        PlanNodeKind::Delete => "delete",
        PlanNodeKind::Action => "action",
        PlanNodeKind::Data => "data",
        PlanNodeKind::Query => "query",
        PlanNodeKind::Search => "search",
        PlanNodeKind::Get => "get",
        PlanNodeKind::Derive => "derive",
        PlanNodeKind::Compute => "compute",
        PlanNodeKind::ForEach => "for_each",
        PlanNodeKind::Relation => "relation",
    });
    serde_json::json!({
        "node": node_id,
        "required": true,
        "host_policy": "host.auto_approve",
        "default_decision": "approved",
        "policy_key": format!("{}.{}.{}", q.entry_id, q.entity, operation),
        "entry_id": q.entry_id,
        "entity": q.entity,
        "operation": operation,
        "author_label": author_label,
        "reason": format!("mutating capability {:?} on {}.{}", kind, q.entry_id, q.entity),
    })
}

pub(crate) fn action_name_from_template(expr_template: &str) -> Option<String> {
    let after_ref = expr_template.split(").").nth(1)?;
    let name = after_ref
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or_default()
        .trim();
    (!name.is_empty()).then(|| name.to_string())
}

pub(crate) fn ensure_node_dispatchable(
    es: &ExecuteSession,
    node: &ValidatedPlanNode,
    index: usize,
) -> Result<(), String> {
    if let ValidatedPlanNode::RelationTraversal(relation) = node {
        let Some(ctx) = es.contexts_by_entry.get(&relation.relation.target.entry_id) else {
            return Err(format!(
                "plan.nodes[{index}].relation.target.entry_id {:?} is not loaded in this session",
                relation.relation.target.entry_id
            ));
        };
        let target = relation.relation.target.entity.as_str();
        if !ctx.cgs.entities.contains_key(target) {
            return Err(format!(
                "plan.nodes[{index}].relation.target entity {:?} is not present under entry_id {:?}",
                relation.relation.target.entity, relation.relation.target.entry_id
            ));
        }
        return Ok(());
    };

    let ValidatedPlanNode::Surface(surface) = node else {
        return Ok(());
    };
    if surface.result_shape == crate::plasm_plan::ResultShape::Page {
        return Ok(());
    }
    let Some(q) = surface.qualified_entity.as_ref() else {
        return if es.contexts_by_entry.len() > 1 {
            Err(format!(
                "plan.nodes[{index}] is missing qualified_entity in a federated session"
            ))
        } else {
            Ok(())
        };
    };
    let Some(ctx) = es.contexts_by_entry.get(&q.entry_id) else {
        return Err(format!(
            "plan.nodes[{index}].qualified_entity.entry_id {:?} is not loaded in this session",
            q.entry_id
        ));
    };
    if !ctx.cgs.entities.contains_key(q.entity.as_str()) {
        return Err(format!(
            "plan.nodes[{index}].qualified_entity entity {:?} is not present under entry_id {:?}",
            q.entity, q.entry_id
        ));
    }
    Ok(())
}

pub(crate) fn ensure_relation_expr_matches_plan(
    es: &ExecuteSession,
    relation: &crate::plasm_plan::ValidatedRelationTraversalNode,
    pe: &ParsedExpr,
    index: usize,
) -> Result<(), String> {
    let Expr::Chain(chain) = &pe.expr else {
        return Err(format!(
            "plan.nodes[{index}].relation.expr must parse to a Plasm relation chain"
        ));
    };
    if chain.selector != relation.relation.relation.as_str() {
        return Err(format!(
            "plan.nodes[{index}].relation relation {:?} does not match parsed selector {:?}",
            relation.relation.relation.as_str(),
            chain.selector
        ));
    }
    let root_entity = chain.source.primary_entity();
    let source_entity = chain
        .source
        .relation_navigation_entity(es.cgs.as_ref())
        .ok_or_else(|| {
            format!(
                "plan.nodes[{index}].relation could not resolve navigation entity for chain root {root_entity:?}"
            )
        })?;
    let source_cgs =
        crate::catalog_ownership::resolve_cgs_for_entity(es, source_entity.as_str(), None)?;
    let Some(source_def) = source_cgs.get_entity(source_entity.as_str()) else {
        return Err(format!(
            "plan.nodes[{index}].relation source entity {source_entity:?} is not present"
        ));
    };
    let Some(schema_relation) = source_def
        .relations
        .get(relation.relation.relation.as_str())
    else {
        return Err(format!(
            "plan.nodes[{index}].relation source entity {source_entity:?} has no relation {:?}",
            relation.relation.relation.as_str()
        ));
    };
    if schema_relation.target_resource.as_str() != relation.relation.target.entity {
        return Err(format!(
            "plan.nodes[{index}].relation target {:?} does not match CGS target {:?}",
            relation.relation.target.entity,
            schema_relation.target_resource.as_str()
        ));
    }
    let expected_cardinality = match schema_relation.cardinality {
        plasm_core::Cardinality::One => crate::plasm_plan::RelationCardinality::One,
        plasm_core::Cardinality::Many => crate::plasm_plan::RelationCardinality::Many,
    };
    if relation.relation.cardinality != expected_cardinality {
        return Err(format!(
            "plan.nodes[{index}].relation cardinality {:?} does not match CGS cardinality {:?}",
            relation.relation.cardinality, expected_cardinality
        ));
    }
    Ok(())
}

pub(crate) fn dry_stage_result(index: usize, n: &ValidatedPlanNode) -> serde_json::Value {
    match n {
        ValidatedPlanNode::ForEach(for_each) => serde_json::json!({
            "index": index,
            "ok": true,
            "id": n.id().as_str(),
            "kind": n.kind(),
            "operation": render_node_operation(n),
            "effect_class": n.effect_class(),
            "result_shape": n.result_shape(),
            "projection": for_each.projection,
            "predicates": for_each.predicates,
            "depends_on": node_ids_json(n.depends_on()),
            "uses_result": n.uses_result(),
            "source": for_each.source.as_str(),
            "item_binding": for_each.item_binding.as_str(),
            "approval": for_each.approval,
            "approval_gate": inferred_node_approval(n),
            "effect_template": for_each.effect_template,
            "simulation": {
                "kind": "template_stage",
                "max_write_set": {
                    "source": for_each.source.as_str(),
                    "shape": "one template invocation per source row"
                },
                "execution": "requires phased Plan runner"
            }
        }),
        ValidatedPlanNode::Data(data) => serde_json::json!({
            "index": index,
            "ok": true,
            "id": n.id().as_str(),
            "kind": n.kind(),
            "operation": render_node_operation(n),
            "effect_class": n.effect_class(),
            "result_shape": n.result_shape(),
            "depends_on": node_ids_json(n.depends_on()),
            "uses_result": n.uses_result(),
            "approval_gate": inferred_node_approval(n),
            "data": data.data,
            "simulation": {
                "kind": "static_data",
                "execution": "materializes static Plan data through the phased Plan runner"
            }
        }),
        ValidatedPlanNode::Derive(derive) => serde_json::json!({
            "index": index,
            "ok": true,
            "id": n.id().as_str(),
            "kind": n.kind(),
            "operation": render_node_operation(n),
            "effect_class": n.effect_class(),
            "result_shape": n.result_shape(),
            "depends_on": node_ids_json(n.depends_on()),
            "uses_result": n.uses_result(),
            "approval_gate": inferred_node_approval(n),
            "source": derive.source.as_str(),
            "item_binding": derive.item_binding.as_str(),
            "inputs": validated_inputs_json(&derive.inputs),
            "value": derive.value,
            "simulation": {
                "kind": "local_derivation",
                "execution": "runs after dependencies are materialized by the phased Plan runner"
            }
        }),
        ValidatedPlanNode::Compute(compute) => serde_json::json!({
            "index": index,
            "ok": true,
            "id": n.id().as_str(),
            "kind": n.kind(),
            "operation": render_node_operation(n),
            "effect_class": n.effect_class(),
            "result_shape": n.result_shape(),
            "depends_on": node_ids_json(n.depends_on()),
            "uses_result": n.uses_result(),
            "approval_gate": inferred_node_approval(n),
            "compute": compute.compute,
            "simulation": {
                "kind": "deterministic_compute",
                "execution": "materializes a synthetic Plasm result set via the phased Plan runner"
            }
        }),
        ValidatedPlanNode::RelationTraversal(relation) => serde_json::json!({
            "index": index,
            "ok": true,
            "id": n.id().as_str(),
            "kind": n.kind(),
            "operation": render_node_operation(n),
            "effect_class": n.effect_class(),
            "result_shape": n.result_shape(),
            "depends_on": node_ids_json(n.depends_on()),
            "uses_result": n.uses_result(),
            "approval_gate": inferred_node_approval(n),
            "relation": {
                "source": relation.relation.source.as_str(),
                "name": relation.relation.relation.as_str(),
                "target": relation.relation.target,
                "cardinality": relation.relation.cardinality,
                "source_cardinality": relation.relation.source_cardinality,
                "expr": relation.relation.ir.display_expr,
            },
            "execution_contract": {
                "entry_id": relation.relation.target.entry_id.as_str(),
                "entity": relation.relation.target.entity.as_str(),
                "ir": relation.relation.ir.expr,
                "projection": relation.relation.ir.projection,
                "source": relation.relation.source.as_str(),
                "relation": relation.relation.relation.as_str(),
            },
            "simulation": {
                "kind": "relation_traversal",
                "execution": "lowers through the typed Plasm chain relation path after the source node is materialized"
            }
        }),
        _ => serde_json::json!({
            "index": index,
            "ok": true,
            "id": n.id().as_str(),
            "kind": n.kind(),
            "operation": render_node_operation(n),
            "effect_class": n.effect_class(),
            "result_shape": n.result_shape(),
            "depends_on": node_ids_json(n.depends_on()),
            "uses_result": n.uses_result(),
            "approval_gate": inferred_node_approval(n),
            "simulation": {
                "kind": "staged_effect",
                "execution": "requires phased Plan runner"
            }
        }),
    }
}

pub(crate) fn node_ids_json(ids: &[PlanNodeId]) -> Vec<&str> {
    ids.iter().map(PlanNodeId::as_str).collect()
}

pub(crate) fn validated_inputs_json(inputs: &[ValidatedPlanDataInput]) -> Vec<serde_json::Value> {
    inputs
        .iter()
        .map(|input| {
            serde_json::json!({
                "node": input.node.as_str(),
                "alias": input.alias.as_str(),
                "proof": input.proof,
            })
        })
        .collect()
}
