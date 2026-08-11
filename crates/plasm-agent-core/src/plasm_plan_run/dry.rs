//! Dry-run evaluation.

use super::*;
use crate::plasm_comp_lift::ExecutablePlasmComp;
use crate::plasm_step_convert::step_payload_to_validated_node;
use plasm_core::PlasmCompArtifact;

#[path = "dry_render.rs"]
mod dry_render;
pub use dry_render::render_node_operation;

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
        if let Some(surface) = n.as_surface() {
            match surface_parsed_expr(surface, step_idx) {
                Ok(Some(pe)) => {
                    let scoped_es =
                        entry_scoped_execute_session(es, surface.qualified_entity.as_ref())?;
                    let normalized = if surface.ir.is_some() {
                        crate::execute_pipeline::PlasmPreflight::preflight_node_compile_dispatch(
                            es, &scoped_es, surface, &pe, step_idx,
                        )?
                    } else {
                        crate::execute_pipeline::PlasmPreflight::preflight_template_surface(
                            es, &scoped_es, &pe, step_idx,
                        )?
                    };
                    let parsed = normalized.parsed();
                    let simulation = if normalized.is_simulatable() {
                        let (intent, il, bindings) =
                            dry_run_simulation_for_session(&scoped_es, parsed);
                        serde_json::json!({
                            "intent": intent,
                            "il": il,
                            "bindings": bindings
                        })
                    } else {
                        serde_json::json!({
                            "kind": "template_stage",
                            "execution": "typechecked only; row holes prevent CML compile at dry time"
                        })
                    };
                    let expr = surface
                        .ir
                        .as_ref()
                        .and_then(|ir| ir.display_expr.as_deref())
                        .or_else(|| {
                            surface
                                .ir_template
                                .as_ref()
                                .and_then(|t| t.display_expr.as_deref())
                        })
                        .or(surface.display_expr.as_deref())
                        .unwrap_or("<ir>");
                    let compact_expr = crate::plan_dry_compact::compact_agent_surface_expr(expr);
                    let compact_ir =
                        crate::plan_dry_compact::compact_ir_expr_json_for_agent_snapshot(
                            serde_json::to_value(&parsed.expr).unwrap_or_default(),
                        );
                    out.push(serde_json::json!({
                        "index": step_idx,
                        "ok": true,
                        "id": n.id().as_str(),
                        "kind": n.kind(),
                        "operation": crate::plan_dry_compact::compact_agent_surface_expr(
                            &render_node_operation(&n),
                        ),
                        "qualified_entity": surface.qualified_entity,
                        "effect_class": n.effect_class(),
                        "result_shape": n.result_shape(),
                        "projection": surface.projection,
                        "predicates": surface.predicates,
                        "ir": {
                            "expr": compact_ir,
                            "projection": parsed.projection
                        },
                        "execution_contract": {
                            "entry_id": surface.qualified_entity.as_ref().map(|q| q.entry_id.as_str()).unwrap_or(es.entry_id.as_str()),
                            "entity": surface.qualified_entity.as_ref().map(|q| q.entity.as_str()),
                            "display_expr": compact_expr,
                            "projection": parsed.projection
                        },
                        "type_check": "ok",
                        "simulation": simulation
                    }));
                    continue;
                }
                Ok(None) => {
                    if n.depends_on().is_empty() && n.uses_result().is_empty() {
                        return Err(format!(
                            "plan.nodes[{step_idx}] requires ir or ir_template for executable surface"
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
        }

        staged_nodes.push(format!("{} ({:?})", n.id(), n.kind()));
        out.push(dry_stage_result(step_idx, &n));
    }
    let prepared = crate::plan_prepare::prepare_executable_plan_for_session(es, comp, executable)?;
    dry_validate_render_nodes(es, prepared.validated.artifact())?;
    dry_validate_staged_surfaces(es, prepared.validated.artifact())?;
    let flow_catalog = es.build_flow_catalog_view();
    let topological_order: Vec<String> = executable
        .steps_topo
        .iter()
        .map(|(id, _)| id.as_str().to_string())
        .collect();
    let flow_checked = crate::plan_flow::verify_plan_flow(
        prepared.validated.artifact(),
        &topological_order,
        &flow_catalog,
        &es.flow_policy,
    );
    let flow_analysis = flow_checked.analysis;
    attach_flow_approval_gates(&mut out, &flow_analysis);
    let mut graph_summary = prepared.graph_summary;
    enrich_graph_summary_flow(&mut graph_summary, &flow_analysis);
    enrich_graph_summary_bind_execution(&mut graph_summary, &executable.bind);
    let parallel_root_surfaces_only =
        compute_parallel_root_surfaces_only(prepared.validated.artifact());
    Ok(DryPlasmPlanEvaluation {
        version,
        name: comp.name.clone(),
        artifact: artifact.clone(),
        executable: executable.clone(),
        cached_validated: std::cell::OnceCell::from(prepared.validated),
        topological_order,
        node_results: out,
        parallel_root_surfaces_only,
        staged_nodes,
        execution_unsupported,
        graph_summary,
        review: prepared.review,
        flow: flow_analysis,
    })
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

pub(crate) fn enrich_graph_summary_flow(
    summary: &mut serde_json::Value,
    analysis: &crate::plan_flow::PlanFlowAnalysis,
) {
    let verdict = match analysis.verdict {
        crate::plan_flow::FlowVerdict::Clean => "clean",
        crate::plan_flow::FlowVerdict::NeedsReview => "needs_review",
        crate::plan_flow::FlowVerdict::Denied => "denied",
    };
    let mut allow = 0usize;
    let mut approve = 0usize;
    let mut review = 0usize;
    let mut deny = 0usize;
    for disposition in analysis.node_dispositions.values() {
        match disposition {
            crate::plan_flow::NodeDisposition::Allow => allow += 1,
            crate::plan_flow::NodeDisposition::Approve { .. } => approve += 1,
            crate::plan_flow::NodeDisposition::Review => review += 1,
            crate::plan_flow::NodeDisposition::Deny => deny += 1,
        }
    }
    let Some(obj) = summary.as_object_mut() else {
        return;
    };
    obj.insert("security_verdict".into(), serde_json::json!(verdict));
    obj.insert(
        "flow_summary".into(),
        serde_json::json!({
            "verdict": verdict,
            "violation_count": analysis.violations.len(),
            "node_count": analysis.node_dispositions.len(),
            "dispositions": {
                "allow": allow,
                "approve": approve,
                "review": review,
                "deny": deny,
            },
        }),
    );
    obj.insert(
        "approval_gates".into(),
        serde_json::Value::Array(analysis.approval_gates_json()),
    );
}

pub(crate) fn enrich_graph_summary_bind_execution(
    summary: &mut serde_json::Value,
    bind: &plasm_core::PlasmBindGraph,
) {
    let Some(obj) = summary.as_object_mut() else {
        return;
    };
    let Ok(exec) = super::plan_schedule::bind_execution_graph_summary(bind) else {
        return;
    };
    obj.insert(
        "execution_layers".into(),
        serde_json::json!(exec.execution_layers),
    );
    obj.insert(
        "parallelizable_roots".into(),
        serde_json::json!(exec.parallelizable_roots),
    );
    obj.insert(
        "parallelizable_roots_note".into(),
        serde_json::json!(super::plan_schedule::PARALLELIZABLE_ROOTS_NOTE),
    );
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
    let flow_override = Some(crate::plan_gate::plan_dry_verdict_from_flow(&dry.flow));
    plan_dry_display::build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        es,
        flow_override,
    )
}

pub fn node_dependencies(node: &ValidatedPlanNode) -> Vec<String> {
    crate::plan_node_graph::node_dependencies(node)
}

pub(crate) fn plan_has_query_limit_row_filter_chain(plan: &Plan<ValidatedPlanState>) -> bool {
    let by_id: std::collections::HashMap<&str, &ValidatedPlanNode> =
        plan.nodes.iter().map(|n| (n.id().as_str(), n)).collect();
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
    let mut warnings = Vec::new();
    let mut boundedness_facts = Vec::new();

    let mut has_narrowed_search_root = false;
    let mut has_narrowed_filter_root = false;
    let mut has_explicit_limit = false;
    let mut has_full_collection_compute = false;
    let mut relation_traversal_nodes = 0usize;
    let mut singleton_foreach_writes = 0usize;
    for n in &plan.nodes {
        match n.effect_class() {
            EffectClass::Read => read_nodes.push(n.id().as_str().to_string()),
            EffectClass::Write | EffectClass::SideEffect => {
                write_or_side_effect_nodes.push(n.id().as_str().to_string())
            }
            EffectClass::ArtifactRead => derive_nodes.push(n.id().as_str().to_string()),
        }
        if let ValidatedPlanNode::ForEach(fe) = n {
            template_nodes.push(n.id().as_str().to_string());
            // D1: singleton source `=>` write is exactly one effect, not a fanout.
            if crate::plasm_plan::validated_source_is_static_singleton(plan, fe.source.as_str())
                && crate::plasm_plan_run::for_each_body_mutates_remote(
                    fe.effect_template.kind,
                    fe.effect_template.effect_class,
                )
            {
                singleton_foreach_writes += 1;
            }
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
        if let ValidatedPlanNode::Compute(c) = n {
            if matches!(c.compute.op, ComputeOp::Limit { .. }) {
                has_explicit_limit = true;
            } else if crate::plan_prepare::compute_op_is_full_collection(&c.compute.op) {
                has_full_collection_compute = true;
            }
        }
        if let ValidatedPlanNode::RelationTraversal(_) = n {
            relation_traversal_nodes += 1;
        }
    }

    let has_unprojected_multi_row_read =
        crate::plan_prepare::return_path_has_unprojected_multi_row_read(plan);

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
            "Paginated list reads without `.page_size(n)` use the default host page; continue with `page(...)` when more rows exist."
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
            "Root read is unnarrowed; it returns the default host page (not all pages) — add API filters/search text or .page_size(n) / .limit(n) to shape the result when cost or latency is uncertain"
                .to_string(),
        );
    }
    if has_full_collection_compute {
        warnings.push(
            "Aggregates/group_by/sort run over the full logical row set before `.limit`; narrow reads (filters + projected fields) when counts are uncertain."
                .to_string(),
        );
    }
    if singleton_foreach_writes > 0 {
        boundedness_facts.push(format!(
            "Singleton source `=>` write performs exactly {} write{} (no fanout).",
            singleton_foreach_writes,
            if singleton_foreach_writes == 1 {
                ""
            } else {
                "s"
            }
        ));
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
    let has_unbounded_relation_embed_hydrate =
        crate::plan_prepare::return_path_has_unbounded_relation_embed_hydrate(plan);
    if has_unbounded_relation_embed_hydrate {
        warnings.push(
            "Relation embed hydrate is unbounded: `.limit(n)` after a relation does not cap GET-hydrate unless the limit chain is pushable (no sort/group_by between limit and relation)."
                .to_string(),
        );
        boundedness_facts.push(
            "Relation embed hydrate may fetch every embedded ref before `.limit` (add a direct `.limit` on the relation chain)."
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
        has_unbounded_relation_embed_hydrate,
        unused_seeds: Vec::new(),
        unused_bindings: Vec::new(),
    };

    (
        serde_json::json!({
            "node_count": plan.nodes.len(),
            "read_nodes": read_nodes,
            "write_or_side_effect_nodes": write_or_side_effect_nodes,
            "derive_nodes": derive_nodes,
            "template_nodes": template_nodes,
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

pub(crate) fn attach_flow_approval_gates(
    node_results: &mut [serde_json::Value],
    analysis: &crate::plan_flow::PlanFlowAnalysis,
) {
    for node in node_results {
        let id = node.get("id").and_then(|v| v.as_str()).map(str::to_string);
        let Some(id) = id else {
            continue;
        };
        let Some(obj) = node.as_object_mut() else {
            continue;
        };
        if let Some(gate) = analysis.approval_gate_for_node(id.as_str()) {
            obj.insert("approval_gate".into(), gate);
        } else {
            obj.remove("approval_gate");
        }
    }
}

pub(crate) fn remote_mutation_effect(kind: PlanNodeKind, effect_class: EffectClass) -> bool {
    crate::plan_flow::is_remote_mutation(kind, effect_class)
}

/// Remote mutation inside a `for_each` body (fan-out / multi-write risk). Read-only bodies excluded.
pub(crate) fn for_each_body_mutates_remote(kind: PlanNodeKind, effect_class: EffectClass) -> bool {
    remote_mutation_effect(kind, effect_class)
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
    match crate::plan_surface_policy::surface_qualified_entity_policy(
        surface,
        es.contexts_by_entry.len() > 1,
    ) {
        Ok(crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::PageWithoutEntity)
        | Ok(crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::EntityOptional) => Ok(()),
        Ok(crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::RequiresQualifiedEntity(
            q,
        )) => {
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
        Err(reason) => Err(format!("plan.nodes[{index}] {reason}")),
    }
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
    let federated = es.contexts_by_entry.len() > 1;
    let row_qe = plasm_core::catalog_ownership::require_relation_source_qualified_entity(
        &chain.source,
        federated,
        None,
    )
    .map_err(|e| e.to_string())?;
    let source_cgs = if let Some(qe) = row_qe.as_ref() {
        es.contexts_by_entry
            .get(qe.entry_id())
            .map(|ctx| ctx.cgs.as_ref())
            .ok_or_else(|| {
                format!(
                    "plan.nodes[{index}].relation unknown catalog entity `{}` for entry `{}`",
                    qe.entity,
                    qe.entry_id()
                )
            })?
    } else {
        crate::catalog_ownership::resolve_cgs_for_entity(es, root_entity, None)?
    };
    let source_entity = chain
        .source
        .relation_navigation_entity(source_cgs)
        .ok_or_else(|| {
            format!(
                "plan.nodes[{index}].relation could not resolve navigation entity for chain root {root_entity:?}"
            )
        })?;
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

/// True when every validated node is an independent root surface (parallel-safe).
fn compute_parallel_root_surfaces_only(plan: &Plan<ValidatedPlanState>) -> bool {
    !plan.nodes.is_empty()
        && plan.nodes.iter().all(|n| {
            matches!(n, ValidatedPlanNode::Surface(_))
                && n.depends_on().is_empty()
                && n.uses_result().is_empty()
        })
}

fn surface_parsed_expr(
    surface: &crate::plasm_plan::ValidatedSurfaceNode,
    step_idx: usize,
) -> Result<Option<ParsedExpr>, String> {
    if let Some(ir) = &surface.ir {
        return Ok(Some(ParsedExpr {
            expr: ir.expr.clone(),
            projection: ir.projection.clone(),
        }));
    }
    if let Some(template) = &surface.ir_template {
        let expr: Expr = serde_json::from_value(template.expr.clone()).map_err(|e| {
            format!("plan.nodes[{step_idx}].ir_template.expr must deserialize to Plasm IR: {e}")
        })?;
        return Ok(Some(ParsedExpr {
            expr,
            projection: template.projection.clone(),
        }));
    }
    Ok(None)
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
