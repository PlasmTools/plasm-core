//! Push `.limit(n)` / filter+limit / sort+limit read budgets onto surface nodes before execute.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    ComputeOp, EffectClass, FieldPath, PlanNodeKind, PlanNodeKind as SurfaceKind, PlanPredicate,
    ResultShape, ValidatedComputeNode, ValidatedPlanArtifact, ValidatedPlanNode,
    ValidatedPlanReturn, ValidatedRelationTraversalNode, ValidatedSurfaceNode,
};
use plasm_runtime::row_predicate::{JsonRowPredicate, JsonRowPredicateOp};
use plasm_runtime::{CachedEntity, ExecutionResult, RowMatchBudget, TopKSpec};

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
        predicates: Vec<PlanPredicate>,
    },
    TopK {
        count: usize,
        key: FieldPath,
        descending: bool,
        filter: Option<Vec<PlanPredicate>>,
    },
}

/// Shared cost gate: true when live execute should spawn async / MCP server-await.
#[must_use]
pub fn read_execution_is_expensive(
    has_unbounded_read_root: bool,
    has_paginated_list_fetch_all_default: bool,
    has_relation_many_source_fanout: bool,
    has_foreach_fanout_risk: bool,
) -> bool {
    has_unbounded_read_root
        || has_paginated_list_fetch_all_default
        || has_relation_many_source_fanout
        || has_foreach_fanout_risk
}

/// Pushed `.limit(n)` / filter+limit cap on a relation traversal node (host-only overlay).
#[must_use]
pub fn effective_relation_read_cap(relation: &ValidatedRelationTraversalNode) -> Option<usize> {
    relation.pushed_read_budget.as_ref().and_then(|b| match b {
        PushedReadBudget::Limit(n) | PushedReadBudget::FilterLimit { count: n, .. } => Some(*n),
        PushedReadBudget::TopK { .. } => None,
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
#[must_use]
pub fn effective_host_page_size(surface: &ValidatedSurfaceNode) -> Option<usize> {
    let pushed = surface.pushed_read_budget.as_ref().and_then(|b| match b {
        PushedReadBudget::Limit(n) => Some(*n),
        PushedReadBudget::FilterLimit { count, .. } => Some(*count),
        PushedReadBudget::TopK { .. } => None,
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
    entity_type: &str,
    logical_session_ref: Option<&str>,
) {
    if cap == 0 || result.entities.len() <= cap {
        result.count = result.entities.len();
        return;
    }
    let all: Vec<CachedEntity> = std::mem::take(&mut result.entities);
    result.entities = all[..cap].to_vec();
    result.count = result.entities.len();
    result.has_more = true;
    let cursor = crate::execute_session::SyntheticPageCursor {
        node_id: node_id.to_string(),
        entity_type: entity_type.to_string(),
        rows: all,
        offset: cap,
        page_size: cap,
        request_fingerprints: result.request_fingerprints.clone(),
    };
    result.paging_handle =
        Some(sess.register_synthetic_paging_continuation(cursor, logical_session_ref));
}

/// Walk return-reachable limit chains and push row budgets onto upstream surface reads.
pub fn apply_read_budgets(plan: &mut ValidatedPlanArtifact) {
    let by_id: HashMap<String, usize> = plan
        .nodes()
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().as_str().to_string(), i))
        .collect();
    let reachable = return_reachable_node_ids(plan, &by_id);
    for compute_idx in 0..plan.nodes().len() {
        if !reachable.contains(plan.nodes()[compute_idx].id().as_str()) {
            continue;
        }
        let Some((target, budget)) = classify_limit_chain(plan.nodes(), &by_id, compute_idx) else {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitChainTarget {
    Surface(usize),
    Relation(usize),
}

fn return_reachable_node_ids(
    plan: &ValidatedPlanArtifact,
    by_id: &HashMap<String, usize>,
) -> HashSet<String> {
    let mut seeds: VecDeque<String> = match plan.return_value() {
        ValidatedPlanReturn::Node(id) => VecDeque::from([id.as_str().to_string()]),
        ValidatedPlanReturn::Parallel { parallel } => {
            parallel.iter().map(|id| id.as_str().to_string()).collect()
        }
    };
    let mut reachable = HashSet::new();
    while let Some(id) = seeds.pop_front() {
        if !reachable.insert(id.clone()) {
            continue;
        }
        let Some(idx) = by_id.get(id.as_str()) else {
            continue;
        };
        for upstream in upstream_node_ids(&plan.nodes()[*idx]) {
            if reachable.contains(upstream.as_str()) {
                continue;
            }
            seeds.push_back(upstream);
        }
    }
    reachable
}

fn upstream_node_ids(node: &ValidatedPlanNode) -> Vec<String> {
    match node {
        ValidatedPlanNode::Compute(c) => vec![c.compute.source.clone()],
        ValidatedPlanNode::Derive(d) => vec![d.source.as_str().to_string()],
        ValidatedPlanNode::ForEach(f) => vec![f.source.as_str().to_string()],
        ValidatedPlanNode::RelationTraversal(r) => vec![r.relation.source.as_str().to_string()],
        ValidatedPlanNode::Surface(s) => s
            .depends_on
            .iter()
            .map(|d| d.as_str().to_string())
            .collect(),
        ValidatedPlanNode::Data(d) => d
            .depends_on
            .iter()
            .map(|dep| dep.as_str().to_string())
            .collect(),
    }
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
    let middle: Vec<&ComputeOp> = chain
        .iter()
        .skip(1)
        .filter(|op| !matches!(op, ComputeOp::Project { .. }))
        .collect();
    match middle.as_slice() {
        [] => Some(PushedReadBudget::Limit(*count)),
        [ComputeOp::Filter { predicates }] => Some(PushedReadBudget::FilterLimit {
            count: *count,
            predicates: predicates.clone(),
        }),
        [ComputeOp::Sort { key, descending }] => Some(PushedReadBudget::TopK {
            count: *count,
            key: key.clone(),
            descending: *descending,
            filter: None,
        }),
        [ComputeOp::Filter { predicates }, ComputeOp::Sort { key, descending }] => {
            Some(PushedReadBudget::TopK {
                count: *count,
                key: key.clone(),
                descending: *descending,
                filter: Some(predicates.clone()),
            })
        }
        _ => None,
    }
}

pub fn lower_plan_predicates(
    predicates: &[PlanPredicate],
) -> Result<Vec<JsonRowPredicate>, String> {
    predicates
        .iter()
        .map(plan_predicate_to_json)
        .collect::<Result<Vec<_>, _>>()
}

pub fn plan_predicate_to_json(pred: &PlanPredicate) -> Result<JsonRowPredicate, String> {
    let rhs = match &pred.value {
        crate::plasm_plan::PlanValue::Literal { value } => value.clone(),
        crate::plasm_plan::PlanValue::EntityRefKey { key, .. } => match key.as_ref() {
            crate::plasm_plan::PlanValue::Literal { value } => value.clone(),
            other => {
                return Err(format!(
                    "unsupported plan predicate value for pushdown: {other:?}"
                ));
            }
        },
        other => {
            return Err(format!(
                "unsupported plan predicate value for pushdown: {other:?}"
            ));
        }
    };
    Ok(JsonRowPredicate {
        field_path: pred.field_path.clone(),
        op: match pred.op {
            crate::plasm_plan::PlanPredicateOp::Eq => JsonRowPredicateOp::Eq,
            crate::plasm_plan::PlanPredicateOp::Ne => JsonRowPredicateOp::Ne,
            crate::plasm_plan::PlanPredicateOp::Lt => JsonRowPredicateOp::Lt,
            crate::plasm_plan::PlanPredicateOp::Lte => JsonRowPredicateOp::Lte,
            crate::plasm_plan::PlanPredicateOp::Gt => JsonRowPredicateOp::Gt,
            crate::plasm_plan::PlanPredicateOp::Gte => JsonRowPredicateOp::Gte,
            crate::plasm_plan::PlanPredicateOp::Contains => JsonRowPredicateOp::Contains,
            crate::plasm_plan::PlanPredicateOp::In => JsonRowPredicateOp::In,
            crate::plasm_plan::PlanPredicateOp::Exists => JsonRowPredicateOp::Exists,
        },
        value: rhs,
    })
}

pub fn pushed_budget_to_stream_fields(
    budget: &PushedReadBudget,
) -> Result<(Option<RowMatchBudget>, Option<TopKSpec>), String> {
    match budget {
        PushedReadBudget::Limit(_) => Ok((None, None)),
        PushedReadBudget::FilterLimit { count, predicates } => {
            let preds = lower_plan_predicates(predicates)?;
            Ok((
                Some(RowMatchBudget {
                    count: *count,
                    predicates: preds,
                }),
                None,
            ))
        }
        PushedReadBudget::TopK {
            count,
            key,
            descending,
            filter,
        } => {
            let row_filter = filter
                .as_ref()
                .map(|ps| lower_plan_predicates(ps))
                .transpose()?
                .unwrap_or_default();
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
                predicates: Vec::new(),
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
            count: entities.len(),
            entities,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: plasm_runtime::ExecutionSource::Cache,
            stats: Default::default(),
            request_fingerprints: vec![],
        };
        cap_execution_result_page(&sess, &mut result, 2, "rows", "LangItem", Some("l_test"));
        assert_eq!(result.entities.len(), 2);
        assert!(result.has_more);
        assert!(result.paging_handle.is_some());
    }

    #[test]
    fn lower_plan_predicates_rejects_non_literal() {
        let preds = vec![PlanPredicate {
            field_path: vec!["x".into()],
            op: crate::plasm_plan::PlanPredicateOp::Eq,
            value: crate::plasm_plan::PlanValue::Helper {
                name: "nope".into(),
                args: vec![],
                display: None,
            },
        }];
        assert!(lower_plan_predicates(&preds).is_err());
    }
}
