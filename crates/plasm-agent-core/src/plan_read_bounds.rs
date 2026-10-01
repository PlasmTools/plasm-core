//! Push `.limit(n)` / filter+limit / sort+limit read budgets onto surface nodes before execute.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    ComputeOp, EffectClass, FieldPath, PlanNodeKind, PlanNodeKind as SurfaceKind, PlanPredicate,
    ResultShape, ValidatedComputeNode, ValidatedPlanArtifact, ValidatedPlanNode,
    ValidatedRelationTraversalNode, ValidatedSurfaceNode,
};
use plasm_runtime::row_predicate::BoundRowPredicate;
use plasm_runtime::{ExecutionResult, RowMatchBudget, TopKSpec};

/// Canonical host page size for unbounded list/page read roots: the first page is materialized
/// in-band, with continuation via `page(...)`. The MCP inline row cap
/// ([`crate::mcp_run_markdown::MCP_IN_BAND_ENTITY_ROW_CAP`]) derives from this constant so the first
/// host page always fits a single MCP tool response.
pub const DEFAULT_HOST_PAGE_SIZE: usize = 25;

/// Host-only read budget applied to a surface node after plan validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushedReadBudget {
    Limit(usize),
    FilterLimit {
        count: usize,
        predicates: Vec<BoundRowPredicate>,
    },
    TopK {
        count: usize,
        key: FieldPath,
        descending: bool,
        filter: Option<Vec<BoundRowPredicate>>,
    },
    /// Full-collection demand from aggregate / group / global sort / dedupe consumers.
    /// Upstream pagination must run to an authoritative terminal condition.
    Complete,
}

/// Shared cost gate: true when live execute should spawn async / MCP server-await.
///
/// Default host page (first page only) is **not** expensive. Unnarrowed roots still set
/// [`ReadBoundedness::has_unbounded_read_root`] for advisory `needs_review` / MCP plan return.
#[must_use]
pub fn read_execution_is_expensive(
    _has_unbounded_read_root: bool,
    has_paginated_list_fetch_all_default: bool,
    has_relation_many_source_fanout: bool,
    has_foreach_fanout_risk: bool,
) -> bool {
    has_paginated_list_fetch_all_default
        || has_relation_many_source_fanout
        || has_foreach_fanout_risk
}

/// Pushed `.limit(n)` / filter+limit cap on a relation traversal node (host-only overlay).
#[must_use]
pub fn effective_relation_read_cap(relation: &ValidatedRelationTraversalNode) -> Option<usize> {
    relation.pushed_read_budget.as_ref().and_then(|b| match b {
        PushedReadBudget::Limit(n) | PushedReadBudget::FilterLimit { count: n, .. } => Some(*n),
        PushedReadBudget::TopK { .. } | PushedReadBudget::Complete => None,
    })
}

/// Truncate materialized rows/entities when a read budget cap applies.
pub fn truncate_to_read_cap<T>(items: &mut Vec<T>, cap: Option<usize>) {
    if let Some(n) = cap {
        items.truncate(n);
    }
}

/// Explicit `.page_size(n)` on the surface node merged with any pushed budget, else a positive default
/// for unbounded list/page read surfaces.
///
/// [`PushedReadBudget::Complete`] clears the host page so pagination fetches the full collection.
#[must_use]
pub fn effective_host_page_size(surface: &ValidatedSurfaceNode) -> Option<usize> {
    if matches!(
        surface.pushed_read_budget.as_ref(),
        Some(PushedReadBudget::Complete)
    ) {
        return None;
    }
    let pushed = surface.pushed_read_budget.as_ref().and_then(|b| match b {
        PushedReadBudget::Limit(n) => Some(*n),
        PushedReadBudget::FilterLimit { count, .. } => Some(*count),
        PushedReadBudget::TopK { .. } | PushedReadBudget::Complete => None,
    });
    let explicit = match (surface.page_size, pushed) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if explicit.is_some() {
        return explicit;
    }
    if surface.effect_class == EffectClass::Read
        && matches!(surface.result_shape, ResultShape::List | ResultShape::Page)
        && matches!(surface.kind, SurfaceKind::Query | SurfaceKind::Search)
    {
        Some(DEFAULT_HOST_PAGE_SIZE)
    } else {
        None
    }
}

/// Truncate an execution result to the first `cap` rows and mint a synthetic `page(...)` continuation
/// when additional rows were materialized.
pub fn cap_execution_result_page(
    sess: &ExecuteSession,
    result: &mut ExecutionResult,
    cap: usize,
    node_id: &str,
    qualified_entity: &crate::plasm_plan::QualifiedEntityKey,
    logical_session_ref: Option<&str>,
) -> Result<(), plasm_core::collection_codec::CollectionFault> {
    // Keep the acquired backend page and its continuation intact. A presentation
    // cursor must never replace the handle that fetches the next backend page.
    if cap == 0 || result.entities().len() <= cap || result.paging_handle.is_some() {
        return Ok(());
    }
    let all = result.collection.clone();
    result.collection = all.delivery(0..cap)?;
    result.has_more = true;
    let cursor = crate::execute_session::SyntheticPageCursor {
        node_id: node_id.to_string(),
        qualified_entity: qualified_entity.clone(),
        collection: all,
        offset: cap,
        page_size: cap,
        request_fingerprints: result.request_fingerprints.clone(),
    };
    result.paging_handle =
        Some(sess.register_synthetic_paging_continuation(cursor, logical_session_ref));
    Ok(())
}

/// Walk return-reachable limit chains and push row budgets onto upstream surface reads.
pub fn apply_read_budgets(plan: &mut ValidatedPlanArtifact) {
    let by_id: HashMap<String, usize> = plan
        .nodes()
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().as_str().to_string(), i))
        .collect();
    let reachable = crate::plan_node_graph::nodes_reachable_from_return(plan.artifact());
    let shared = shared_collection_bindings(plan);
    for compute_idx in 0..plan.nodes().len() {
        if !reachable.contains(plan.nodes()[compute_idx].id().as_str()) {
            continue;
        }
        let Some((target, budget)) =
            classify_limit_chain(plan.nodes(), &by_id, &shared, compute_idx)
        else {
            continue;
        };
        match target {
            LimitChainTarget::Surface(idx) => {
                let ValidatedPlanNode::Surface(surface) = &mut plan.nodes_mut()[idx] else {
                    continue;
                };
                merge_budget_into_surface(surface, budget);
            }
            LimitChainTarget::Relation(idx) => {
                let ValidatedPlanNode::RelationTraversal(relation) = &mut plan.nodes_mut()[idx]
                else {
                    continue;
                };
                merge_budget_into_relation(relation, budget);
            }
        }
    }
    apply_complete_demands(plan);
}

/// A pushed budget belongs to one consumer chain. A shared binding is a semantic
/// boundary: no consumer may narrow the rows seen by its siblings (including a
/// direct program return). Materialize that binding before applying branch budgets.
fn shared_collection_bindings(plan: &ValidatedPlanArtifact) -> HashSet<String> {
    let mut consumers: HashMap<String, usize> = HashMap::new();
    // Non-returned bindings still execute, including mutations consuming rows.
    for node in plan.nodes() {
        for source in crate::plan_node_graph::node_dependencies(node) {
            *consumers.entry(source).or_default() += 1;
        }
    }
    let returns = match &plan.artifact().return_value {
        crate::plasm_plan::ValidatedPlanReturn::Node(node) => vec![node.as_str()],
        crate::plasm_plan::ValidatedPlanReturn::Parallel { parallel } => {
            parallel.iter().map(|n| n.as_str()).collect()
        }
    };
    for source in returns {
        *consumers.entry(source.to_owned()).or_default() += 1;
    }
    consumers
        .into_iter()
        .filter_map(|(id, count)| (count > 1).then_some(id))
        .collect()
}

/// Relational algebra consumes its source expression, not an implicit host page.
/// Explicit limits remain bounded; presentation paging belongs after evaluation.
#[must_use]
pub(crate) fn compute_op_is_full_collection(op: &ComputeOp) -> bool {
    matches!(
        op,
        ComputeOp::Python { .. }
            | ComputeOp::Aggregate { .. }
            | ComputeOp::Filter { .. }
            | ComputeOp::Project { .. }
            | ComputeOp::GroupBy { .. }
            | ComputeOp::Sort { .. }
            | ComputeOp::DedupeBy { .. }
    )
}

/// Complete demand is a graph property across lexical scopes. A path contains
/// enclosing map node indices followed by the local node index. Walking this
/// flattened graph avoids recursive re-analysis and keeps each node single-visit.
fn apply_complete_demands(plan: &mut ValidatedPlanArtifact) {
    let mut pending = VecDeque::new();
    let mut scopes = vec![Vec::new()];
    while let Some(scope) = scopes.pop() {
        let local = scoped_plan_mut(plan, &scope);
        let reachable = crate::plan_node_graph::nodes_reachable_from_return(local.artifact());
        let mut sources = shared_collection_bindings(local);
        for (idx, node) in local.nodes().iter().enumerate() {
            if matches!(node, ValidatedPlanNode::MapBody(_)) {
                let mut child = scope.clone();
                child.push(idx);
                scopes.push(child);
            }
            if !reachable.contains(node.id().as_str()) {
                continue;
            }
            match node {
                ValidatedPlanNode::Compute(c) if compute_op_is_full_collection(&c.compute.op) => {
                    sources.insert(c.compute.source.clone());
                }
                ValidatedPlanNode::ForEach(f) => {
                    sources.insert(f.source.to_string());
                }
                ValidatedPlanNode::MapBody(m) => {
                    sources.insert(m.body.parent.source.to_string());
                }
                ValidatedPlanNode::Derive(d) => {
                    sources.insert(d.source.to_string());
                    sources.extend(
                        d.inputs
                            .iter()
                            .filter(|input| {
                                input.proof == crate::plasm_plan::InputCardinalityProof::Collection
                            })
                            .map(|input| input.node.to_string()),
                    );
                }
                _ => {}
            }
        }
        enqueue_local_demands(local, &scope, sources, &mut pending);
    }
    let mut seen = HashSet::new();
    while let Some(mut path) = pending.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let idx = path.pop().expect("demand always names a node");
        let local = scoped_plan_mut(plan, &path);
        let upstream = crate::plan_node_graph::node_dependencies(&local.nodes()[idx]);
        let mut propagate_upstream = false;
        let mut captured = None;
        match &mut local.nodes_mut()[idx] {
            ValidatedPlanNode::Surface(surface)
                if matches!(surface.kind, PlanNodeKind::Query | PlanNodeKind::Search) =>
            {
                merge_budget_into_surface(surface, PushedReadBudget::Complete);
            }
            // take defines a bounded expression; do not enlarge it.
            ValidatedPlanNode::Compute(c) if matches!(c.compute.op, ComputeOp::Limit { .. }) => {}
            ValidatedPlanNode::MapBody(map) => {
                let returns = map
                    .plan
                    .return_value()
                    .refs()
                    .into_iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                let mut child = path.clone();
                child.push(idx);
                enqueue_local_demands(&map.plan, &child, returns, &mut pending);
                propagate_upstream = true;
            }
            ValidatedPlanNode::Capture(capture) => captured = Some(capture.id.to_string()),
            ValidatedPlanNode::Compute(_)
            | ValidatedPlanNode::Derive(_)
            | ValidatedPlanNode::ForEach(_) => propagate_upstream = true,
            ValidatedPlanNode::RelationTraversal(relation) => {
                merge_budget_into_relation(relation, PushedReadBudget::Complete);
                propagate_upstream = true;
            }
            _ => {}
        }
        if propagate_upstream {
            enqueue_local_demands(local, &path, upstream, &mut pending);
        }
        if let Some(captured) = captured {
            if let Some(map_idx) = path.pop() {
                let parent = scoped_plan_mut(plan, &path);
                let ValidatedPlanNode::MapBody(map) = &parent.nodes()[map_idx] else {
                    unreachable!("scope path must name a map body")
                };
                let source = if map.body.parent.local.as_str() == captured {
                    Some(map.body.parent.source.to_string())
                } else {
                    map.body
                        .captures
                        .iter()
                        .find(|port| port.local.as_str() == captured)
                        .map(|port| port.source.to_string())
                };
                enqueue_local_demands(parent, &path, source, &mut pending);
            }
        }
    }
}

fn scoped_plan_mut<'a>(
    mut plan: &'a mut ValidatedPlanArtifact,
    scope: &[usize],
) -> &'a mut ValidatedPlanArtifact {
    for &idx in scope {
        let ValidatedPlanNode::MapBody(map) = &mut plan.nodes_mut()[idx] else {
            unreachable!("scope path must name a map body")
        };
        plan = &mut map.plan;
    }
    plan
}

fn enqueue_local_demands(
    plan: &ValidatedPlanArtifact,
    scope: &[usize],
    sources: impl IntoIterator<Item = String>,
    pending: &mut VecDeque<Vec<usize>>,
) {
    for source in sources {
        let source = crate::plasm_plan::PlanNodeId::new(source)
            .expect("complete demand source is a validated node id");
        if let Some(idx) = plan.node_index(&source) {
            let mut path = scope.to_vec();
            path.push(idx);
            pending.push_back(path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitChainTarget {
    Surface(usize),
    Relation(usize),
}

fn merge_budget_into_surface(surface: &mut ValidatedSurfaceNode, budget: PushedReadBudget) {
    merge_pushed_budget_into(&mut surface.pushed_read_budget, budget);
}

fn merge_budget_into_relation(
    relation: &mut ValidatedRelationTraversalNode,
    budget: PushedReadBudget,
) {
    merge_pushed_budget_into(&mut relation.pushed_read_budget, budget);
}

fn merge_pushed_budget_into(slot: &mut Option<PushedReadBudget>, budget: PushedReadBudget) {
    match slot {
        None => *slot = Some(budget),
        Some(existing) => {
            let merged = merge_pushed_budget(existing.clone(), budget);
            *existing = merged;
        }
    }
}

fn merge_pushed_budget(a: PushedReadBudget, b: PushedReadBudget) -> PushedReadBudget {
    match (a, b) {
        (PushedReadBudget::Complete, other) | (other, PushedReadBudget::Complete) => {
            // Complete yields to an explicit bounded budget (limit/top-k); otherwise stays Complete.
            match other {
                PushedReadBudget::Complete => PushedReadBudget::Complete,
                bounded => bounded,
            }
        }
        (PushedReadBudget::Limit(x), PushedReadBudget::Limit(y)) => {
            PushedReadBudget::Limit(x.min(y))
        }
        (
            PushedReadBudget::FilterLimit { count: x, .. },
            PushedReadBudget::FilterLimit {
                count: y,
                predicates: py,
            },
        ) => PushedReadBudget::FilterLimit {
            count: x.min(y),
            predicates: py,
        },
        (
            PushedReadBudget::TopK { count: x, .. },
            PushedReadBudget::TopK {
                count: y,
                key,
                descending,
                filter,
            },
        ) => PushedReadBudget::TopK {
            count: x.min(y),
            key,
            descending,
            filter,
        },
        (_, b) => b,
    }
}

fn classify_limit_chain(
    nodes: &[ValidatedPlanNode],
    by_id: &HashMap<String, usize>,
    shared: &HashSet<String>,
    compute_idx: usize,
) -> Option<(LimitChainTarget, PushedReadBudget)> {
    let ValidatedPlanNode::Compute(compute) = &nodes[compute_idx] else {
        return None;
    };
    let ComputeOp::Limit { count } = compute.compute.op else {
        return None;
    };
    let mut chain = vec![ComputeOp::Limit { count }];
    let mut current = compute.compute.source.clone();
    loop {
        if shared.contains(&current) {
            return None;
        }
        let idx = *by_id.get(current.as_str())?;
        match &nodes[idx] {
            ValidatedPlanNode::Surface(surface)
                if matches!(surface.kind, PlanNodeKind::Query | PlanNodeKind::Search) =>
            {
                return budget_from_chain(&chain)
                    .map(|budget| (LimitChainTarget::Surface(idx), budget));
            }
            ValidatedPlanNode::Surface(surface) if surface.kind == PlanNodeKind::Get => {
                return None;
            }
            ValidatedPlanNode::RelationTraversal(_) => {
                return budget_from_chain(&chain)
                    .map(|budget| (LimitChainTarget::Relation(idx), budget));
            }
            ValidatedPlanNode::Compute(ValidatedComputeNode { compute: tpl, .. }) => {
                chain.push(tpl.op.clone());
                current = tpl.source.clone();
            }
            _ => return None,
        }
    }
}

fn budget_from_chain(chain: &[ComputeOp]) -> Option<PushedReadBudget> {
    let ComputeOp::Limit { count } = chain.first()? else {
        return None;
    };
    let mut count = *count;
    let mut rewritten: Vec<ComputeOp> = Vec::new();
    for op in chain.iter().skip(1) {
        if let ComputeOp::Limit { count: upstream } = op {
            // Positional prefixes compose only before crossing a selection/order barrier.
            if !rewritten.is_empty() {
                return None;
            }
            count = count.min(*upstream);
        } else if let ComputeOp::Project { fields } = op {
            // Walk consumers back through each projection. Aliases are not source fields.
            let rebase = |path: &FieldPath| -> Option<FieldPath> {
                if let Some((_, source)) = fields
                    .iter()
                    .find(|(name, _)| name.as_str() == path.dotted())
                {
                    return Some(source.clone());
                }
                let (first, rest) = path.segments().split_first()?;
                let (_, source) = fields.iter().find(|(name, _)| name.as_str() == first)?;
                let mut segments = source.segments().to_vec();
                segments.extend_from_slice(rest);
                FieldPath::from_dotted(&segments.join(".")).ok()
            };
            for consumer in &mut rewritten {
                match consumer {
                    ComputeOp::Sort { key, .. } => *key = rebase(key)?,
                    ComputeOp::Filter { predicates } => {
                        *predicates = predicates
                            .try_map(&mut |predicate| {
                                let mut result = predicate.clone();
                                result.field_path = rebase(&result.field_path).ok_or(())?;
                                Ok::<_, ()>(result)
                            })
                            .ok()?;
                    }
                    _ => return None,
                }
            }
        } else {
            rewritten.push(op.clone());
        }
    }
    let middle: Vec<_> = rewritten.iter().collect();
    match middle.as_slice() {
        [] => Some(PushedReadBudget::Limit(count)),
        [ComputeOp::Filter { predicates }] => Some(PushedReadBudget::FilterLimit {
            count,
            predicates: lower_plan_predicates(&predicates.conjunction()?).ok()?,
        }),
        [ComputeOp::Sort { key, descending }] => Some(PushedReadBudget::TopK {
            count,
            key: key.clone(),
            descending: *descending,
            filter: None,
        }),
        [ComputeOp::Filter { predicates }, ComputeOp::Sort { key, descending }] => {
            Some(PushedReadBudget::TopK {
                count,
                key: key.clone(),
                descending: *descending,
                filter: Some(lower_plan_predicates(&predicates.conjunction()?).ok()?),
            })
        }
        _ => None,
    }
}

pub fn lower_plan_predicates(
    predicates: &[PlanPredicate],
) -> Result<Vec<BoundRowPredicate>, String> {
    predicates
        .iter()
        .map(bind_row_predicate)
        .collect::<Result<Vec<_>, _>>()
}

pub fn bind_row_predicate(pred: &PlanPredicate) -> Result<BoundRowPredicate, String> {
    Ok(BoundRowPredicate {
        field_path: pred.field_path.clone(),
        op: pred.op,
        value: pred.value.clone().into_resolved()?,
    })
}

pub fn pushed_budget_to_stream_fields(
    budget: &PushedReadBudget,
) -> Result<(Option<RowMatchBudget>, Option<TopKSpec>), String> {
    match budget {
        PushedReadBudget::Limit(_) | PushedReadBudget::Complete => Ok((None, None)),
        PushedReadBudget::FilterLimit { count, predicates } => Ok((
            Some(RowMatchBudget {
                count: *count,
                predicates: predicates.clone(),
            }),
            None,
        )),
        PushedReadBudget::TopK {
            count,
            key,
            descending,
            filter,
        } => {
            let row_filter = filter.clone().unwrap_or_default();
            Ok((
                None,
                Some(TopKSpec {
                    count: *count,
                    sort_key: key.segments().to_vec(),
                    descending: *descending,
                    row_filter,
                }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::{ComputeOp, ValidatedPlanNode};
    use plasm_runtime::ResultCoverage;

    #[test]
    fn bounded_sibling_must_not_truncate_returned_source() {
        let plan = serde_json::json!({
            "version":1,"kind":"program","name":"shared-source",
            "nodes":[
                {"id":"rows","kind":"query","qualified_entity":{"entry_id":"matrix","entity":"Product"},
                 "expr":"Product","ir":{"expr":{"op":"query","entity":"Product"}},"effect_class":"read","result_shape":"list"},
                {"id":"sample","kind":"compute","effect_class":"read","result_shape":"list","depends_on":["rows"],
                 "compute":{"source":"rows","op":{"kind":"limit","count":1},
                 "schema":{"entity":"Product","fields":[{"name":"id","value_kind":"string","source":["id"]}]}}}
            ],"return":{"kind":"parallel","nodes":["rows","sample"]}
        });
        let wire = serde_json::to_vec(&plan).unwrap();
        let mut plan = crate::plasm_plan::parse_and_validate_plan_json(
            &serde_json::from_slice(&wire).unwrap(),
        )
        .unwrap();
        apply_read_budgets(&mut plan);
        let ValidatedPlanNode::Surface(source) = &plan.nodes()[0] else {
            panic!("source")
        };
        assert_eq!(source.pushed_read_budget, Some(PushedReadBudget::Complete));
    }

    proptest::proptest! {
        #[test]
        fn sibling_limits_preserve_full_source_across_wire(
            left in 1usize..40, right in 1usize..40, reverse in proptest::bool::ANY,
            return_source in proptest::bool::ANY,
        ) {
            use serde_json::json;
            let schema=json!({"entity":"Product","fields":[{"name":"id","value_kind":"string","source":["id"]}]});
            let mut nodes=vec![json!({"id":"rows","kind":"query","qualified_entity":{"entry_id":"matrix","entity":"Product"},
                "expr":"Product","ir":{"expr":{"op":"query","entity":"Product"}},"effect_class":"read","result_shape":"list"})];
            let mut branches=vec![("left",left),("right",right)];
            if reverse { branches.reverse(); }
            for (id,count) in branches {
                nodes.push(json!({"id":id,"kind":"compute","effect_class":"read","result_shape":"list","depends_on":["rows"],
                    "compute":{"source":"rows","op":{"kind":"limit","count":count},"schema":schema}}));
            }
            let returns=if return_source {vec!["rows","left","right"]} else {vec!["left"]};
            let wire=serde_json::to_vec(&json!({"version":1,"kind":"program","name":"siblings","nodes":nodes,
                "return":{"kind":"parallel","nodes":returns}})).unwrap();
            let mut plan=crate::plasm_plan::parse_and_validate_plan_json(&serde_json::from_slice(&wire).unwrap()).unwrap();
            apply_read_budgets(&mut plan);
            apply_read_budgets(&mut plan);
            let ValidatedPlanNode::Surface(source)=&plan.nodes()[0] else {panic!("source")};
            proptest::prop_assert_eq!(&source.pushed_read_budget,&Some(PushedReadBudget::Complete));
        }
    }

    proptest::proptest! {
        #[test]
        fn serial_prefixes_compose(counts in proptest::collection::vec(0usize..100, 1..12)) {
            let chain = counts.iter().map(|count| ComputeOp::Limit {count: *count}).collect::<Vec<_>>();
            proptest::prop_assert_eq!(budget_from_chain(&chain), Some(PushedReadBudget::Limit(*counts.iter().min().unwrap())));
        }
    }

    #[test]
    fn serial_prefixes_do_not_cross_selection_or_ordering() {
        for barrier in [
            ComputeOp::Filter {
                predicates: Vec::new().into(),
            },
            ComputeOp::Sort {
                key: FieldPath::from_dotted("score").unwrap(),
                descending: false,
            },
        ] {
            assert_eq!(
                budget_from_chain(&[
                    ComputeOp::Limit { count: 1 },
                    barrier,
                    ComputeOp::Limit { count: 25 },
                ]),
                None
            );
        }
    }

    #[test]
    fn limit_only_chain_budget() {
        let chain = vec![ComputeOp::Limit { count: 5 }];
        assert!(matches!(
            budget_from_chain(&chain),
            Some(PushedReadBudget::Limit(5))
        ));
    }

    #[test]
    fn filter_limit_chain_budget() {
        let chain = vec![
            ComputeOp::Limit { count: 3 },
            ComputeOp::Filter {
                predicates: Vec::new().into(),
            },
        ];
        assert!(matches!(
            budget_from_chain(&chain),
            Some(PushedReadBudget::FilterLimit { count: 3, .. })
        ));
    }

    #[test]
    fn unsupported_chain_returns_none() {
        let chain = vec![
            ComputeOp::Limit { count: 3 },
            ComputeOp::GroupBy {
                keys: vec![],
                aggregates: vec![],
            },
        ];
        assert!(budget_from_chain(&chain).is_none());
    }

    #[test]
    fn apply_read_budgets_pushes_limit_onto_relation_traversal() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "relation-limit",
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
            .expect("relation node");
        assert_eq!(
            relation.pushed_read_budget,
            Some(PushedReadBudget::Limit(3))
        );
    }

    #[test]
    fn default_host_page_size_for_unbounded_search_surface() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "unbounded-search",
            "nodes": [{
                "id": "hits",
                "kind": "search",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product~\"q\"",
                "ir": { "expr": { "op": "query", "entity": "Product", "capability_name": "product_search" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "hits" }
        });
        let validated = crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        let surface = match &validated.nodes()[0] {
            ValidatedPlanNode::Surface(s) => s,
            _ => panic!("expected surface"),
        };
        assert_eq!(
            effective_host_page_size(surface),
            Some(DEFAULT_HOST_PAGE_SIZE),
            "unbounded Search uses the same first-page cap as Query"
        );
    }

    #[test]
    fn cap_execution_result_page_at_cap_does_not_claim_remainder() {
        use indexmap::IndexMap;
        use plasm_core::{EntityKey, Ref, Value};
        use plasm_runtime::{CachedEntity, EntityCompleteness};

        let cgs = std::sync::Arc::new(
            plasm_core::loader::load_schema_dir(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("matrix"),
        );
        let sess = crate::test_support::graph_fixtures::test_execute_session(
            cgs.clone(),
            "cap-page-eq-test",
        );
        let mut entities = Vec::new();
        for i in 0..DEFAULT_HOST_PAGE_SIZE {
            let mut fields = IndexMap::new();
            fields.insert("id".into(), Value::String(format!("i{i}")));
            entities.push(CachedEntity::from_decoded(
                Ref {
                    entity_type: "LangItem".into(),
                    key: EntityKey::Simple(format!("i{i}").into()),
                },
                fields,
                IndexMap::new(),
                0,
                EntityCompleteness::Complete,
            ));
        }
        let mut result = ExecutionResult {
            collection: crate::test_support::execution_fixtures::collection(
                entities,
                ResultCoverage::Unknown,
            ),
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: plasm_runtime::ExecutionSource::Cache,
            stats: Default::default(),
            request_fingerprints: vec![],
            operations: plasm_runtime::OperationLedger::empty(),
        };
        cap_execution_result_page(
            &sess,
            &mut result,
            DEFAULT_HOST_PAGE_SIZE,
            "rows",
            &crate::plasm_plan::QualifiedEntityKey {
                entry_id: sess.entry_id.clone(),
                entity: "LangItem".into(),
            },
            Some("l_test"),
        )
        .expect("valid recorded page");
        assert_eq!(result.entities().len(), DEFAULT_HOST_PAGE_SIZE);
        assert!(
            !result.has_more,
            "len==cap is an honest page, not a silent drop"
        );
        assert!(result.paging_handle.is_none());
    }

    #[test]
    fn default_host_page_size_for_unbounded_query_surface() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "unbounded-query",
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
        let validated = crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        let surface = match &validated.nodes()[0] {
            ValidatedPlanNode::Surface(s) => s,
            _ => panic!("expected surface"),
        };
        assert_eq!(
            effective_host_page_size(surface),
            Some(DEFAULT_HOST_PAGE_SIZE)
        );
    }

    proptest::proptest! {
        #[test]
        fn global_collection_demand_reaches_every_union_branch(
            branches in 2usize..8, bounded_first in proptest::bool::ANY,
        ) {
            use serde_json::json;
            let schema = json!({"entity":"Product","fields":[{"name":"id","value_kind":"string","source":["id"]}]});
            let mut nodes = Vec::new();
            for n in 0..branches {
                nodes.push(json!({"id":format!("q{n}"),"kind":"query",
                    "qualified_entity":{"entry_id":"matrix","entity":"Product"},
                    "expr":"Product","ir":{"expr":{"op":"query","entity":"Product"}},
                    "effect_class":"read","result_shape":"list"}));
            }
            let mut source = "q0".to_string();
            if bounded_first {
                nodes.push(json!({"id":"bounded","kind":"compute","effect_class":"read","result_shape":"list",
                    "depends_on":["q0"],"compute":{"source":"q0","op":{"kind":"limit","count":3},"schema":schema}}));
                source = "bounded".into();
            }
            for n in 1..branches {
                let id = format!("u{n}"); let other=format!("q{n}");
                nodes.push(json!({"id":id,"kind":"compute","effect_class":"read","result_shape":"list",
                    "depends_on":[source,other],"compute":{"source":source,"op":{"kind":"union","other":other},"schema":schema}}));
                source=id;
            }
            nodes.push(json!({"id":"total","kind":"compute","effect_class":"read","result_shape":"single",
                "depends_on":[source],"compute":{"source":source,"op":{"kind":"aggregate","aggregates":[{"name":"n","function":"count"}]},
                    "schema":{"entity":"Product","fields":[{"name":"n","value_kind":"number","source":["n"]}]}}}));
            let json=json!({"version":1,"kind":"program","name":"union-demand","nodes":nodes,"return":{"kind":"node","node":"total"}});
            let mut plan=crate::plasm_plan::parse_and_validate_plan_json(&json).expect("abstract union plan");
            apply_read_budgets(&mut plan);
            for (n,node) in plan.nodes().iter().take(branches).enumerate() {
                let ValidatedPlanNode::Surface(surface)=node else {panic!("query surface")};
                let expected=if n==0 && bounded_first {PushedReadBudget::Limit(3)} else {PushedReadBudget::Complete};
                proptest::prop_assert_eq!(&surface.pushed_read_budget,&Some(expected));
            }
        }
    }

    #[test]
    fn filter_and_projection_consume_the_requested_collection() {
        use serde_json::json;
        for op in [
            json!({"kind":"filter", "predicates":{"kind":"atom","args":{"field_path":["id"],"op":"exists","value":{"kind":"literal","value":true}}}}),
            json!({"kind":"project", "fields":{"id":["id"]}}),
        ] {
            let plan = json!({
                "version":1,"kind":"program","name":"row-algebra",
                "nodes":[
                    {"id":"rows","kind":"query","qualified_entity":{"entry_id":"matrix","entity":"Product"},
                     "expr":"Product","ir":{"expr":{"op":"query","entity":"Product"}},"effect_class":"read","result_shape":"list"},
                    {"id":"out","kind":"compute","effect_class":"read","result_shape":"list","depends_on":["rows"],
                     "compute":{"source":"rows","op":op,"schema":{"entity":"Product","fields":[{"name":"id","value_kind":"string","source":["id"]}]}}}
                ],"return":{"kind":"node","node":"out"}
            });
            let mut validated =
                crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("abstract algebra");
            apply_read_budgets(&mut validated);
            let ValidatedPlanNode::Surface(surface) = &validated.nodes()[0] else {
                panic!("surface")
            };
            assert_eq!(
                surface.pushed_read_budget,
                Some(PushedReadBudget::Complete),
                "{op}"
            );
            assert_eq!(effective_host_page_size(surface), None);
        }
    }

    #[test]
    fn aggregate_consumer_pushes_complete_and_clears_host_page() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "sum-query",
            "nodes": [
                {
                    "id": "payments",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "filtered",
                    "kind": "compute",
                    "effect_class": "read",
                    "result_shape": "list",
                    "depends_on": ["payments"],
                    "compute": {
                        "source": "payments",
                        "op": {
                            "kind": "filter",
                            "predicates": {"kind":"atom","args":{"field_path":["id"],"op":"exists","value":{"kind":"literal","value":true}}}
                        },
                        "schema": {
                            "entity": "Product",
                            "fields": [{ "name": "id", "value_kind": "string", "source": ["id"] }]
                        }
                    }
                },
                {
                    "id": "total",
                    "kind": "compute",
                    "effect_class": "read",
                    "result_shape": "single",
                    "depends_on": ["filtered"],
                    "compute": {
                        "source": "filtered",
                        "op": {
                            "kind": "aggregate",
                            "aggregates": [{ "name": "n", "function": "count" }]
                        },
                        "schema": {
                            "entity": "Product",
                            "fields": [{ "name": "n", "value_kind": "number", "source": ["n"] }]
                        }
                    }
                }
            ],
            "return": { "kind": "node", "node": "total" }
        });
        let mut validated =
            crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        apply_read_budgets(&mut validated);
        let surface = match &validated.nodes()[0] {
            ValidatedPlanNode::Surface(s) => s,
            _ => panic!("expected surface"),
        };
        assert_eq!(
            surface.pushed_read_budget,
            Some(PushedReadBudget::Complete),
            "aggregate must demand Complete collection, not a host page"
        );
        assert_eq!(
            effective_host_page_size(surface),
            None,
            "Complete demand must clear DEFAULT_HOST_PAGE_SIZE"
        );
    }

    #[test]
    fn for_each_consumer_demands_full_collection() {
        let plan = serde_json::json!({
            "version": 1, "kind": "program", "name": "export-all",
            "nodes": [
                {"id":"items", "kind":"query", "qualified_entity":{"entry_id":"acme","entity":"Product"},
                 "expr":"Product", "ir":{"expr":{"op":"query","entity":"Product"}},
                 "effect_class":"read", "result_shape":"list"},
                {"id":"writes", "kind":"for_each", "source":"items", "item_binding":"item",
                 "depends_on":["items"], "uses_result":[{"node":"items","as":"item"}],
                 "effect_class":"side_effect", "result_shape":"side_effect_ack",
                 "effect_template":{"kind":"action", "qualified_entity":{"entry_id":"acme","entity":"Product"},
                     "expr_template":"Product.create(title=\"copy\")",
                     "ir_template":{"expr":{"op":"create","capability":"product_create","entity":"Product","input":{"title":"copy"}},"input_bindings":[]},
                     "effect_class":"side_effect", "result_shape":"side_effect_ack"}}
            ], "return":{"kind":"node","node":"writes"}
        });
        let mut validated =
            crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
        apply_read_budgets(&mut validated);
        let ValidatedPlanNode::Surface(surface) = &validated.nodes()[0] else {
            panic!("surface")
        };
        assert_eq!(surface.pushed_read_budget, Some(PushedReadBudget::Complete));
        assert_eq!(effective_host_page_size(surface), None);

        let mut bounded = plan.clone();
        bounded["nodes"].as_array_mut().unwrap().insert(1, serde_json::json!({
            "id":"selected", "kind":"compute", "effect_class":"read", "result_shape":"list",
            "depends_on":["items"], "compute":{"source":"items", "op":{"kind":"limit","count":3},
                "schema":{"entity":"Product","fields":[{"name":"id","value_kind":"string","source":["id"]}]}}
        }));
        bounded["nodes"][2]["source"] = serde_json::json!("selected");
        bounded["nodes"][2]["depends_on"] = serde_json::json!(["selected"]);
        bounded["nodes"][2]["uses_result"] = serde_json::json!([{"node":"selected","as":"item"}]);
        let mut validated =
            crate::plasm_plan::parse_and_validate_plan_json(&bounded).expect("bounded validate");
        apply_read_budgets(&mut validated);
        let ValidatedPlanNode::Surface(surface) = &validated.nodes()[0] else {
            panic!("surface")
        };
        assert_eq!(
            surface.pushed_read_budget,
            Some(PushedReadBudget::Limit(3)),
            "explicit take remains bounded under fanout"
        );
    }

    #[test]
    fn default_host_page_size_matches_mcp_in_band_cap() {
        assert_eq!(
            DEFAULT_HOST_PAGE_SIZE,
            crate::mcp_run_markdown::MCP_IN_BAND_ENTITY_ROW_CAP
        );
    }

    #[test]
    fn cap_execution_result_page_mints_continuation() {
        use indexmap::IndexMap;
        use plasm_core::{EntityKey, Ref, Value};
        use plasm_runtime::{CachedEntity, EntityCompleteness};

        let cgs = std::sync::Arc::new(
            plasm_core::loader::load_schema_dir(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .expect("matrix"),
        );
        let sess =
            crate::test_support::graph_fixtures::test_execute_session(cgs.clone(), "cap-page-test");
        let mut entities = Vec::new();
        for i in 0..5 {
            let mut fields = IndexMap::new();
            fields.insert("id".into(), Value::String(format!("i{i}")));
            entities.push(CachedEntity::from_decoded(
                Ref {
                    entity_type: "LangItem".into(),
                    key: EntityKey::Simple(format!("i{i}").into()),
                },
                fields,
                IndexMap::new(),
                0,
                EntityCompleteness::Complete,
            ));
        }
        let mut result = ExecutionResult {
            collection: crate::test_support::execution_fixtures::collection(
                entities,
                ResultCoverage::Unknown,
            ),
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: plasm_runtime::ExecutionSource::Cache,
            stats: Default::default(),
            request_fingerprints: vec![],
            operations: plasm_runtime::OperationLedger::empty(),
        };
        result.collection = crate::test_support::execution_fixtures::collection(
            result.entities().iter().cloned().collect(),
            plasm_runtime::ResultCoverage::Complete,
        );
        cap_execution_result_page(
            &sess,
            &mut result,
            2,
            "rows",
            &crate::plasm_plan::QualifiedEntityKey {
                entry_id: sess.entry_id.clone(),
                entity: "LangItem".into(),
            },
            Some("l_test"),
        )
        .expect("valid recorded page");
        assert_eq!(result.entities().len(), 2);
        assert!(result.has_more);
        assert!(result.paging_handle.is_some());
        assert_eq!(
            result.coverage(),
            plasm_runtime::ResultCoverage::Complete,
            "presentation paging must not rewrite expression coverage"
        );
    }

    #[test]
    fn lower_plan_predicates_rejects_non_literal() {
        let preds = vec![PlanPredicate {
            field_path: FieldPath::from_dotted("x").expect("field path"),
            op: crate::plasm_plan::PlanPredicateOp::Eq,
            value: crate::plasm_plan::PlanValue::NodeSymbol {
                node: "nope".into(),
                alias: "nope".into(),
                path: vec![],
            },
        }];
        assert!(lower_plan_predicates(&preds).is_err());
    }
}

#[cfg(test)]
mod projection_pushdown_laws {
    use super::*;
    use plasm_core::OutputName;
    #[test]
    fn pushed_sort_keys_are_rebased_through_composed_projections() {
        let path = |s| FieldPath::from_dotted(s).unwrap();
        let projection = |a, b| ComputeOp::Project {
            fields: [(OutputName::new(a).unwrap(), path(b))].into(),
        };
        let chain = [
            ComputeOp::Limit { count: 2 },
            ComputeOp::Sort {
                key: path("shown"),
                descending: true,
            },
            projection("shown", "renamed"),
            projection("renamed", "timestamp"),
        ];
        let Some(PushedReadBudget::TopK { key, .. }) = budget_from_chain(&chain) else {
            panic!("typed top-k budget")
        };
        assert_eq!(key, path("timestamp"));
        let invalid = [
            ComputeOp::Limit { count: 2 },
            ComputeOp::Sort {
                key: path("missing"),
                descending: true,
            },
            projection("shown", "timestamp"),
        ];
        assert!(budget_from_chain(&invalid).is_none());
    }
}
