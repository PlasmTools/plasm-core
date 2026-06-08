//! Push `.limit(n)` / filter+limit / sort+limit read budgets onto surface nodes before execute.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::plasm_plan::{
    ComputeOp, FieldPath, PlanNodeKind, PlanPredicate, ValidatedComputeNode, ValidatedPlanArtifact,
    ValidatedPlanNode, ValidatedPlanReturn, ValidatedSurfaceNode,
};
use plasm_runtime::row_predicate::{JsonRowPredicate, JsonRowPredicateOp};
use plasm_runtime::{RowMatchBudget, TopKSpec};

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

/// Explicit `.page_size(n)` on the surface node merged with any pushed budget.
#[must_use]
pub fn effective_host_page_size(surface: &ValidatedSurfaceNode) -> Option<usize> {
    let pushed = surface.pushed_read_budget.as_ref().and_then(|b| match b {
        PushedReadBudget::Limit(n) => Some(*n),
        PushedReadBudget::FilterLimit { count, .. } => Some(*count),
        PushedReadBudget::TopK { .. } => None,
    });
    match (surface.page_size, pushed) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
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
        let Some((surface_idx, budget)) = classify_limit_chain(plan.nodes(), &by_id, compute_idx)
        else {
            continue;
        };
        let ValidatedPlanNode::Surface(surface) = &mut plan.nodes_mut()[surface_idx] else {
            continue;
        };
        merge_budget_into_surface(surface, budget);
    }
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
    match &mut surface.pushed_read_budget {
        None => surface.pushed_read_budget = Some(budget),
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
) -> Option<(usize, PushedReadBudget)> {
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
                return budget_from_chain(&chain).map(|budget| (idx, budget));
            }
            ValidatedPlanNode::Surface(surface) if surface.kind == PlanNodeKind::Get => {
                return None;
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
    use crate::plasm_plan::ComputeOp;

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
