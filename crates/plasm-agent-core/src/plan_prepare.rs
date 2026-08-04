//! Comp-backed plan preparation: lift executable comp → validated plan → read budgets.
//!
//! Dry-run review, MCP delivery policy, and live execute must share this prepared view.

use std::collections::{HashMap, HashSet};

use plasm_core::{ChainStep, Expr, PlasmComp, Predicate, TypedComparisonValue};

use crate::execute_session::ExecuteSession;
use crate::plan_dry_display::PlanDryReview;
use crate::plan_node_graph::{unused_binding_hints, unused_seed_hints};
use crate::plan_read_bounds::{apply_read_budgets, read_execution_is_expensive, PushedReadBudget};
use crate::plasm_comp_lift::ExecutablePlasmComp;
use crate::plasm_plan::{
    ComputeOp, Plan, PlanNodeKind, PlanValue, ValidatedPlan, ValidatedPlanNode, ValidatedPlanState,
    ValidatedSurfaceNode,
};
use crate::plasm_plan_run::graph_summary;
use crate::plasm_step_convert::build_validated_plan_from_executable;

/// Unified read-boundedness analysis on a **prepared** plan (after `apply_read_budgets`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadBoundedness {
    pub has_unbounded_read_root: bool,
    pub has_paginated_list_fetch_all_default: bool,
    pub has_relation_many_source_fanout: bool,
    pub has_foreach_fanout_risk: bool,
}

impl ReadBoundedness {
    /// True when live execute should spawn async / MCP server-await.
    #[must_use]
    pub fn execution_is_expensive(&self) -> bool {
        read_execution_is_expensive(
            self.has_unbounded_read_root,
            self.has_paginated_list_fetch_all_default,
            self.has_relation_many_source_fanout,
            self.has_foreach_fanout_risk,
        )
    }
}

/// Read budgets from a prepared validated plan, keyed by surface node id.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedSurfaceBudget {
    pub page_size: Option<usize>,
    pub pushed_read_budget: Option<PushedReadBudget>,
}

#[must_use]
pub(crate) fn prepared_surface_budget_lookup(
    plan: &Plan<ValidatedPlanState>,
) -> HashMap<String, PreparedSurfaceBudget> {
    plan.nodes
        .iter()
        .filter_map(|n| {
            let ValidatedPlanNode::Surface(s) = n else {
                return None;
            };
            Some((
                s.id.as_str().to_string(),
                PreparedSurfaceBudget {
                    page_size: s.page_size,
                    pushed_read_budget: s.pushed_read_budget.clone(),
                },
            ))
        })
        .collect()
}

/// Read budgets from a prepared validated plan, keyed by relation node id.
#[must_use]
pub(crate) fn prepared_relation_budget_lookup(
    plan: &Plan<ValidatedPlanState>,
) -> HashMap<String, PushedReadBudget> {
    plan.nodes
        .iter()
        .filter_map(|n| {
            let ValidatedPlanNode::RelationTraversal(r) = n else {
                return None;
            };
            r.pushed_read_budget
                .clone()
                .map(|budget| (r.id.as_str().to_string(), budget))
        })
        .collect()
}

pub(crate) fn apply_prepared_relation_budget(
    relation: &mut crate::plasm_plan::ValidatedRelationTraversalNode,
    lookup: &HashMap<String, PushedReadBudget>,
) {
    let Some(budget) = lookup.get(relation.id.as_str()) else {
        return;
    };
    relation.pushed_read_budget = Some(budget.clone());
}

pub(crate) fn apply_prepared_surface_budget(
    surface: &mut ValidatedSurfaceNode,
    lookup: &HashMap<String, PreparedSurfaceBudget>,
) {
    let Some(budget) = lookup.get(surface.id.as_str()) else {
        return;
    };
    if budget.page_size.is_some() {
        surface.page_size = budget.page_size;
    }
    if budget.pushed_read_budget.is_some() {
        surface.pushed_read_budget = budget.pushed_read_budget.clone();
    }
}

/// Host-prepared executable plan: validated DAG + pushed read budgets + review metadata.
#[derive(Debug, Clone)]
pub struct PreparedExecutablePlan {
    pub validated: ValidatedPlan,
    pub review: PlanDryReview,
    pub boundedness: ReadBoundedness,
    pub graph_summary: serde_json::Value,
}

/// Lift comp steps, apply read budgets, and derive review/boundedness for dry + live gates.
pub(crate) fn prepare_executable_plan_for_session(
    es: &ExecuteSession,
    comp: &PlasmComp,
    executable: &ExecutablePlasmComp,
) -> Result<PreparedExecutablePlan, String> {
    let validated = build_prepared_validated_plan(comp, executable)?;
    let plan = validated.artifact();
    let boundedness = analyze_read_boundedness(plan);
    let (mut graph_summary, mut review) = graph_summary(plan, &boundedness);
    review.unused_seeds = unused_seed_hints(es, plan);
    review.unused_bindings = unused_binding_hints(plan);
    if !review.unused_seeds.is_empty() {
        graph_summary["unused_seeds"] = serde_json::json!(review.unused_seeds.clone());
    }
    if !review.unused_bindings.is_empty() {
        graph_summary["unused_bindings"] = serde_json::json!(review.unused_bindings.clone());
    }
    crate::plasm_plan_run::enrich_graph_summary_auth_scoped_reads(es, plan, &mut graph_summary);
    Ok(PreparedExecutablePlan {
        validated,
        review,
        boundedness,
        graph_summary,
    })
}

/// Build validated plan from executable comp and push `.limit` / filter+limit budgets upstream.
pub(crate) fn build_prepared_validated_plan(
    comp: &PlasmComp,
    executable: &ExecutablePlasmComp,
) -> Result<ValidatedPlan, String> {
    let mut validated = build_validated_plan_from_executable(comp, executable)?;
    apply_read_budgets(&mut validated);
    Ok(validated)
}

/// Analyze return-reachable list reads on a prepared plan.
#[must_use]
pub fn analyze_read_boundedness(plan: &Plan<ValidatedPlanState>) -> ReadBoundedness {
    let mut out = ReadBoundedness::default();
    let reachable = crate::plan_node_graph::nodes_reachable_from_return(plan);
    for n in &plan.nodes {
        if !reachable.contains(n.id().as_str()) {
            continue;
        }
        if let ValidatedPlanNode::RelationTraversal(rel) = n {
            if rel.relation.source_cardinality == crate::plasm_plan::RelationSourceCardinality::Many
                && crate::plan_read_bounds::effective_relation_read_cap(rel).is_none()
            {
                out.has_relation_many_source_fanout = true;
            }
        }
        if let ValidatedPlanNode::ForEach(fe) = n {
            // D1: a provably-singleton source runs the effect exactly once — not a fanout.
            if crate::plasm_plan_run::for_each_body_mutates_remote(
                fe.effect_template.kind,
                fe.effect_template.effect_class,
            ) && !crate::plasm_plan::validated_source_is_static_singleton(
                plan,
                fe.source.as_str(),
            ) {
                out.has_foreach_fanout_risk = true;
            }
        }
        let ValidatedPlanNode::Surface(surface) = n else {
            continue;
        };
        if surface.effect_class != crate::plasm_plan::EffectClass::Read {
            continue;
        }
        if !matches!(
            surface.result_shape,
            crate::plasm_plan::ResultShape::List | crate::plasm_plan::ResultShape::Page
        ) {
            continue;
        }
        if !matches!(surface.kind, PlanNodeKind::Query | PlanNodeKind::Search) {
            continue;
        }
        if surface_is_read_bounded(surface) {
            continue;
        }
        // Unnarrowed root → structural advisory (`needs_review`) even when the default host
        // page caps the first fetch. Default page alone is not "expensive" live work.
        if crate::plan_node_graph::node_dependencies(n).is_empty() {
            out.has_unbounded_read_root = true;
        }
        if crate::plan_read_bounds::effective_host_page_size(surface).is_none() {
            out.has_paginated_list_fetch_all_default = true;
        }
    }
    out
}

/// Agent-declared read bounds only (explicit page/limit budget, search text, or API predicates).
///
/// The default host page ([`crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE`]) caps live fetch cost
/// but does **not** clear structural advisory review — see
/// [`return_path_node_is_unprojected_multi_row_read`].
#[must_use]
fn surface_is_read_bounded(surface: &ValidatedSurfaceNode) -> bool {
    if surface.page_size.is_some() || surface.pushed_read_budget.is_some() {
        return true;
    }
    if surface.kind == PlanNodeKind::Search {
        return true;
    }
    if !surface.predicates.is_empty() {
        return true;
    }
    false
}

/// Aggregate/group_by/sort/dedupe over row sets — not project/filter/limit/render.
#[must_use]
pub(crate) fn compute_op_is_full_collection(op: &ComputeOp) -> bool {
    matches!(
        op,
        ComputeOp::Aggregate { .. }
            | ComputeOp::GroupBy { .. }
            | ComputeOp::Sort { .. }
            | ComputeOp::DedupeBy { .. }
    )
}

/// List/page read on the return path without `[field,…]` projection.
///
/// This is **advisory structural review** (full-row materialization). The default host page does
/// not clear it — MCP must return a `run_ref` plan rather than fusing auto-execute.
/// An explicit project compute between the read and the return clears the advisory.
#[must_use]
pub(crate) fn return_path_node_is_unprojected_multi_row_read(n: &ValidatedPlanNode) -> bool {
    use crate::plasm_plan::EffectClass;

    match n {
        ValidatedPlanNode::Surface(s)
            if s.effect_class == EffectClass::Read
                && s.projection.is_empty()
                && matches!(
                    s.result_shape,
                    crate::plasm_plan::ResultShape::List | crate::plasm_plan::ResultShape::Page
                ) =>
        {
            true
        }
        ValidatedPlanNode::ForEach(fe)
            if fe.effect_class == EffectClass::Read
                && fe.projection.is_empty()
                && matches!(
                    fe.result_shape,
                    crate::plasm_plan::ResultShape::List | crate::plasm_plan::ResultShape::Page
                ) =>
        {
            true
        }
        _ => false,
    }
}

/// List/page read on the return path without projection or a downstream project step.
#[must_use]
pub(crate) fn return_path_has_unprojected_multi_row_read(plan: &Plan<ValidatedPlanState>) -> bool {
    let reachable = crate::plan_node_graph::nodes_reachable_from_return(plan);
    plan.nodes.iter().any(|n| {
        reachable.contains(n.id().as_str())
            && return_path_node_is_unprojected_multi_row_read(n)
            && !project_compute_downstream_of_node(plan, n.id().as_str())
    })
}

#[must_use]
fn relation_materialize_is_embed(materialize: &plasm_core::RelationMaterialization) -> bool {
    matches!(
        materialize,
        plasm_core::RelationMaterialization::FromParentGet { .. }
            | plasm_core::RelationMaterialization::PreferFromParentGet { .. }
    )
}

#[must_use]
pub(crate) fn limit_compute_downstream_of_node(
    plan: &Plan<ValidatedPlanState>,
    node_id: &str,
) -> bool {
    let by_id: HashMap<String, usize> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().as_str().to_string(), i))
        .collect();
    plan.nodes.iter().any(|n| {
        let ValidatedPlanNode::Compute(c) = n else {
            return false;
        };
        if !matches!(c.compute.op, ComputeOp::Limit { .. }) {
            return false;
        }
        let mut current = c.compute.source.clone();
        loop {
            if current == node_id {
                return true;
            }
            let Some(idx) = by_id.get(current.as_str()) else {
                return false;
            };
            match &plan.nodes[*idx] {
                ValidatedPlanNode::Compute(inner) => current = inner.compute.source.clone(),
                ValidatedPlanNode::RelationTraversal(r) if r.id.as_str() == node_id => return true,
                _ => return false,
            }
        }
    })
}

/// True when a `project` compute sits on a chain from `node_id` toward a return consumer.
#[must_use]
pub(crate) fn project_compute_downstream_of_node(
    plan: &Plan<ValidatedPlanState>,
    node_id: &str,
) -> bool {
    let by_id: HashMap<String, usize> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().as_str().to_string(), i))
        .collect();
    plan.nodes.iter().any(|n| {
        let ValidatedPlanNode::Compute(c) = n else {
            return false;
        };
        if !matches!(c.compute.op, ComputeOp::Project { .. }) {
            return false;
        }
        let mut current = c.compute.source.clone();
        loop {
            if current == node_id {
                return true;
            }
            let Some(idx) = by_id.get(current.as_str()) else {
                return false;
            };
            match &plan.nodes[*idx] {
                ValidatedPlanNode::Compute(inner) => current = inner.compute.source.clone(),
                ValidatedPlanNode::RelationTraversal(r) if r.id.as_str() == node_id => return true,
                ValidatedPlanNode::Derive(d) => current = d.source.as_str().to_string(),
                _ => return false,
            }
        }
    })
}

/// Embed-style relation on the return path with downstream `.limit` but no pushed relation budget.
#[must_use]
pub(crate) fn return_path_has_unbounded_relation_embed_hydrate(
    plan: &Plan<ValidatedPlanState>,
) -> bool {
    let reachable = crate::plan_node_graph::nodes_reachable_from_return(plan);
    plan.nodes.iter().any(|n| {
        let ValidatedPlanNode::RelationTraversal(rel) = n else {
            return false;
        };
        if !reachable.contains(rel.id.as_str()) {
            return false;
        }
        if crate::plan_read_bounds::effective_relation_read_cap(rel).is_some() {
            return false;
        }
        if !relation_materialize_is_embed(&rel.relation.materialize) {
            return false;
        }
        limit_compute_downstream_of_node(plan, rel.id.as_str())
    })
}

/// Entity wire names referenced anywhere in the plan (surfaces, IR, plan predicates).
#[must_use]
pub fn collect_plan_entity_names(plan: &Plan<ValidatedPlanState>) -> HashSet<String> {
    let mut out = HashSet::new();
    for n in &plan.nodes {
        if let ValidatedPlanNode::Surface(s) = n {
            if let Some(q) = &s.qualified_entity {
                out.insert(q.entity.clone());
            }
            if let Some(ir) = &s.ir {
                collect_entities_from_expr(&ir.expr, &mut out);
            }
            for pred in &s.predicates {
                collect_entities_from_plan_value(&pred.value, &mut out);
            }
        }
        if let ValidatedPlanNode::RelationTraversal(r) = n {
            out.insert(r.relation.target.entity.clone());
            collect_entities_from_expr(&r.relation.ir.expr, &mut out);
        }
    }
    out
}

fn collect_entities_from_plan_value(value: &PlanValue, out: &mut HashSet<String>) {
    match value {
        PlanValue::EntityRefKey { entity, key, .. } => {
            out.insert(entity.clone());
            collect_entities_from_plan_value(key, out);
        }
        PlanValue::Array { items } => {
            for item in items {
                collect_entities_from_plan_value(item, out);
            }
        }
        PlanValue::Object { fields } => {
            for v in fields.values() {
                collect_entities_from_plan_value(v, out);
            }
        }
        _ => {}
    }
}

fn collect_entities_from_expr(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Query(q) => {
            out.insert(q.entity.as_str().to_string());
            if let Some(pred) = &q.predicate {
                collect_entities_from_predicate(pred, out);
            }
        }
        Expr::Get(g) => {
            out.insert(g.reference.entity_type.as_str().to_string());
        }
        Expr::Create(c) => {
            out.insert(c.entity.as_str().to_string());
        }
        Expr::Delete(d) => {
            out.insert(d.target.entity_type.as_str().to_string());
        }
        Expr::Invoke(i) => {
            out.insert(i.target.entity_type.as_str().to_string());
        }
        Expr::Chain(c) => {
            collect_entities_from_expr(&c.source, out);
            if let ChainStep::Explicit { expr: inner } = &c.step {
                collect_entities_from_expr(inner, out);
            }
        }
        Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) | Expr::TeachingValue { .. } => {}
    }
}

fn collect_entities_from_predicate(pred: &Predicate, out: &mut HashSet<String>) {
    match pred {
        Predicate::True | Predicate::False => {}
        Predicate::Comparison { value, .. } => collect_entities_from_comparison_value(value, out),
        Predicate::And { args } | Predicate::Or { args } => {
            for a in args {
                collect_entities_from_predicate(a, out);
            }
        }
        Predicate::Not { predicate } => collect_entities_from_predicate(predicate, out),
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                collect_entities_from_predicate(inner, out);
            }
        }
    }
}

fn collect_entities_from_comparison_value(value: &TypedComparisonValue, out: &mut HashSet<String>) {
    if let Some(lit) = value.typed_literal() {
        use plasm_core::TypedLiteral;
        if let TypedLiteral::EntityRef(_) = lit {
            // Compound entity_ref keys do not carry a separate entity name; plan predicates use PlanValue::EntityRefKey instead.
        }
    } else {
        let v = value.to_value();
        if let plasm_core::Value::Object(map) = v {
            if let Some(plasm_core::Value::String(entity)) = map.get("entity_type") {
                out.insert(entity.clone());
            }
            if let Some(plasm_core::Value::String(entity)) = map.get("entity") {
                out.insert(entity.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_read_bounds::{
        effective_host_page_size, effective_relation_read_cap, PushedReadBudget,
    };
    use crate::plasm_plan_run::evaluate_plasm_plan_dry;
    use indexmap::IndexMap;
    use plasm_core::load_schema;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_session(entities: Vec<&str>) -> crate::execute_session::ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            load_schema(&root.join("tests/fixtures/execute_tiny")).expect("load execute_tiny"),
        );
        let mut ctxs = IndexMap::new();
        ctxs.insert(
            "acme".into(),
            Arc::new(plasm_core::CgsContext::entry("acme", cgs.clone())),
        );
        let exp = plasm_core::TeachingExposureSession::new(cgs.as_ref(), "acme", &entities);
        crate::execute_session::ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "acme".into(),
            String::new(),
            String::new(),
            None,
            entities.into_iter().map(str::to_string).collect(),
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    fn limit_compute_json(source: &str, count: u32) -> serde_json::Value {
        serde_json::json!({
            "id": "limited",
            "kind": "compute",
            "effect_class": "read",
            "result_shape": "list",
            "depends_on": [source],
            "compute": {
                "source": source,
                "op": { "kind": "limit", "count": count },
                "schema": {
                    "entity": "PlanLimit",
                    "fields": [{ "name": "id", "value_kind": "string", "source": ["id"] }]
                }
            }
        })
    }

    #[test]
    fn prepared_plan_applies_limit_pushdown() {
        let mut nodes = vec![serde_json::json!({
            "id": "r1",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        })];
        nodes.push(limit_compute_json("r1", 3));
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "limited-query",
            "nodes": nodes,
            "return": { "kind": "node", "node": "limited" }
        });
        let mut validated =
            crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        apply_read_budgets(&mut validated);
        let bounded = analyze_read_boundedness(validated.artifact());
        assert!(
            !bounded.execution_is_expensive(),
            "limit pushdown should bound query: {bounded:?}"
        );
        let surface = match &validated.nodes()[0] {
            ValidatedPlanNode::Surface(s) => s,
            _ => panic!("expected surface"),
        };
        assert_eq!(
            effective_host_page_size(surface),
            Some(3),
            "expected pushed limit on query surface"
        );
    }

    #[test]
    fn prepared_plan_applies_relation_limit_pushdown() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "get-relation-limit",
            "nodes": [
                {
                    "id": "product",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "category",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "list",
                    "relation": {
                        "source": "product",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Category" },
                        "cardinality": "one",
                        "source_cardinality": "single",
                        "materialize": { "kind": "from_parent_get", "path": [{ "key": "category" }] },
                        "expr": "Product(\"p1\").category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["product"],
                    "uses_result": [{ "node": "product", "as": "source" }]
                },
                {
                    "id": "limited",
                    "kind": "compute",
                    "effect_class": "read",
                    "result_shape": "list",
                    "depends_on": ["category"],
                    "compute": {
                        "source": "category",
                        "op": { "kind": "limit", "count": 3 },
                        "schema": {
                            "entity": "Category",
                            "fields": [{ "name": "id", "value_kind": "string", "source": ["id"] }]
                        }
                    }
                }
            ],
            "return": { "kind": "node", "node": "limited" }
        });
        let mut validated =
            crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        apply_read_budgets(&mut validated);
        let relation = validated
            .nodes()
            .iter()
            .find_map(|n| {
                let ValidatedPlanNode::RelationTraversal(r) = n else {
                    return None;
                };
                Some(r)
            })
            .expect("relation");
        assert_eq!(effective_relation_read_cap(relation), Some(3));
        assert_eq!(
            relation.pushed_read_budget,
            Some(PushedReadBudget::Limit(3))
        );
        assert!(
            !crate::plan_prepare::return_path_has_unbounded_relation_embed_hydrate(
                validated.artifact()
            )
        );
    }

    #[test]
    fn unbounded_query_gets_default_host_page_after_prepare() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "unbounded",
            "nodes": [{
                "id": "products",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "products" }
        });
        let mut validated =
            crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        apply_read_budgets(&mut validated);
        let bounded = analyze_read_boundedness(validated.artifact());
        assert!(
            !bounded.execution_is_expensive(),
            "default host page should keep bare query sync (not expensive): {bounded:?}"
        );
        assert!(
            bounded.has_unbounded_read_root,
            "unnarrowed root remains advisory even with default host page: {bounded:?}"
        );
        assert!(
            return_path_has_unprojected_multi_row_read(validated.artifact()),
            "unprojected list is advisory"
        );
        let surface = match &validated.nodes()[0] {
            ValidatedPlanNode::Surface(s) => s,
            _ => panic!("expected surface"),
        };
        assert_eq!(
            effective_host_page_size(surface),
            Some(crate::plan_read_bounds::DEFAULT_HOST_PAGE_SIZE)
        );
        let review = PlanDryReview {
            has_unprojected_multi_row_read: true,
            has_unbounded_read_root: true,
            ..Default::default()
        };
        assert!(
            review.needs_review(true),
            "advisory unprojected/unnarrowed must need review (MCP returns plan, no fuse)"
        );
        assert_eq!(
            crate::plan_gate::merged_gate_verdict(
                &crate::plan_flow::PlanFlowAnalysis {
                    policy_revision: None,
                    verdict: crate::plan_flow::FlowVerdict::Clean,
                    node_facts: Default::default(),
                    node_dispositions: Default::default(),
                    sink_proofs: Default::default(),
                    violations: Vec::new(),
                },
                &review,
                true,
            ),
            crate::plan_dry_display::PlanDryVerdict::Review
        );
    }

    #[test]
    fn limited_projected_list_clears_unprojected_advisory() {
        let s = test_session(vec!["Product"]);
        let mut nodes = vec![serde_json::json!({
            "id": "r1",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        })];
        nodes.push(limit_compute_json("r1", 3));
        nodes.push(project_compute_json("limited", "projected"));
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "projected",
            "nodes": nodes,
            "return": { "kind": "node", "node": "projected" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert!(
            !dry.review.has_unbounded_read_root,
            "limit pushdown clears unnarrowed: {:?}",
            dry.review
        );
        assert!(
            !dry.review.has_unprojected_multi_row_read,
            "downstream project clears unprojected advisory: {:?}",
            dry.review
        );
        assert!(!dry.review.needs_review(false));
    }

    #[test]
    fn collect_plan_entity_names_includes_relation_target() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "relation-target",
            "nodes": [
                {
                    "id": "product",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "category",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "single",
                    "relation": {
                        "source": "product",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Category" },
                        "cardinality": "one",
                        "source_cardinality": "single",
                        "expr": "Product(\"p1\").category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["product"],
                    "uses_result": [{ "node": "product", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "category" }
        });
        let validated = crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        let names = collect_plan_entity_names(validated.artifact());
        assert!(names.contains("Product"));
        assert!(names.contains("Category"));
    }

    #[test]
    fn relation_target_not_unused_seed_when_only_traversed() {
        let s = test_session(vec!["Product", "Category"]);
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "relation-only",
            "nodes": [
                {
                    "id": "product",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "category",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "single",
                    "relation": {
                        "source": "product",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Category" },
                        "cardinality": "one",
                        "source_cardinality": "single",
                        "expr": "Product(\"p1\").category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["product"],
                    "uses_result": [{ "node": "product", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "category" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert!(
            !dry.review
                .unused_seeds
                .iter()
                .any(|seed| seed.contains("Category")),
            "relation target should count as used: {:?}",
            dry.review.unused_seeds
        );
    }

    #[test]
    fn get_expr_entity_counts_as_used_for_unused_seed_walk() {
        use plasm_core::{EntityKey, GetExpr, Ref};
        let expr = Expr::Get(GetExpr {
            reference: Ref {
                entity_type: "Repository".into(),
                key: EntityKey::Compound(
                    [("owner".into(), "octocat".into())].into_iter().collect(),
                ),
            },
            path_vars: None,
            catalog_entry_id: plasm_core::CatalogEntryStamp::none(),
            capability_name: None,
        });
        let mut used = HashSet::new();
        collect_entities_from_expr(&expr, &mut used);
        assert!(used.contains("Repository"));
    }

    #[test]
    fn entity_ref_key_in_plan_predicate_counts_as_used() {
        let mut used = HashSet::new();
        collect_entities_from_plan_value(
            &PlanValue::EntityRefKey {
                api: "github".into(),
                entity: "Repository".into(),
                key: Box::new(PlanValue::Literal {
                    value: serde_json::json!({"owner": "octocat"}),
                }),
            },
            &mut used,
        );
        assert!(used.contains("Repository"));
    }

    #[test]
    fn dry_review_not_expensive_for_query_limit_program() {
        let s = test_session(vec!["Product"]);
        let mut nodes = vec![serde_json::json!({
            "id": "r1",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        })];
        nodes.push(limit_compute_json("r1", 3));
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "limited",
            "nodes": nodes,
            "return": { "kind": "node", "node": "limited" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert!(
            !dry.review.execution_is_expensive(),
            "review: {:?}",
            dry.review
        );
        assert!(
            !dry.review.has_unbounded_read_root,
            "should not warn unbounded after limit pushdown"
        );
    }

    #[test]
    fn dry_live_boundedness_isomorphism() {
        let s = test_session(vec!["Product", "Category"]);
        let mut nodes = vec![serde_json::json!({
            "id": "r1",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        })];
        nodes.push(limit_compute_json("r1", 3));
        let bounded_plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "bounded",
            "nodes": nodes,
            "return": { "kind": "node", "node": "limited" }
        });
        let dry_bounded = evaluate_plasm_plan_dry(&s, &bounded_plan).expect("bounded dry");
        assert_eq!(
            dry_bounded.review.execution_is_expensive(),
            analyze_read_boundedness(dry_bounded.validated_plan()).execution_is_expensive(),
        );
        assert!(!dry_bounded.review.execution_is_expensive());

        let unbounded_plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "unbounded",
            "nodes": [{
                "id": "products",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "products" }
        });
        let dry_unbounded = evaluate_plasm_plan_dry(&s, &unbounded_plan).expect("unbounded dry");
        assert_eq!(
            dry_unbounded.review.execution_is_expensive(),
            analyze_read_boundedness(dry_unbounded.validated_plan()).execution_is_expensive(),
        );
        assert!(!dry_unbounded.review.execution_is_expensive());
        assert!(
            dry_unbounded.review.has_unbounded_read_root,
            "unnarrowed advisory: {:?}",
            dry_unbounded.review
        );
        assert!(
            dry_unbounded.review.needs_review(true),
            "MCP fuse must not auto-execute advisory lists: {:?}",
            dry_unbounded.review
        );
    }

    fn project_compute_json(source: &str, id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "kind": "compute",
            "effect_class": "artifact_read",
            "result_shape": "list",
            "depends_on": [source],
            "compute": {
                "source": source,
                "op": { "kind": "project", "fields": { "id": ["id"], "name": ["name"] } },
                "schema": {
                    "entity": "PlanProject",
                    "fields": [
                        { "name": "id", "value_kind": "unknown", "source": ["id"] },
                        { "name": "name", "value_kind": "unknown", "source": ["name"] }
                    ]
                }
            }
        })
    }

    #[test]
    fn dry_review_ok_for_query_limit_project_without_plan_warnings() {
        use crate::plan_dry_display::{build_plan_dry_compact_view, PlanDryVerdict};

        let s = test_session(vec!["Product", "Category"]);
        let mut nodes = vec![serde_json::json!({
            "id": "berries",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        })];
        nodes.push(limit_compute_json("berries", 10));
        nodes.push(project_compute_json("limited", "c1"));
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "berries-limited",
            "nodes": nodes,
            "return": { "kind": "node", "node": "c1" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        let compact = build_plan_dry_compact_view(
            dry.validated_plan(),
            &dry.topological_order,
            &dry.review,
            &dry.graph_summary,
            Some(&s),
            None,
        );
        assert_eq!(
            compact.verdict,
            PlanDryVerdict::Ok,
            "review: {:?}",
            dry.review
        );
        assert!(
            compact.warnings.is_none(),
            "warnings: {:?}",
            compact.warnings
        );
        assert!(
            !dry.review.unused_seeds.is_empty(),
            "unused seeds remain session advisory"
        );
    }

    #[test]
    fn dry_review_default_page_bounds_bare_list_query() {
        use crate::plan_dry_display::{build_plan_dry_compact_view, PlanDryVerdict};

        let s = test_session(vec!["Product"]);
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "unbounded",
            "nodes": [{
                "id": "products",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "products" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        let compact = build_plan_dry_compact_view(
            dry.validated_plan(),
            &dry.topological_order,
            &dry.review,
            &dry.graph_summary,
            Some(&s),
            None,
        );
        assert_eq!(compact.verdict, PlanDryVerdict::Ok);
        assert!(
            compact.warnings.is_none()
                || !compact
                    .warnings
                    .as_deref()
                    .is_some_and(|w| w.contains("unbounded")),
            "default host page should avoid unbounded warning: {:?}",
            compact.warnings
        );
    }
}
