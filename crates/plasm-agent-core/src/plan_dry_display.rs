//! Typed compact dry-run plan display — built from [`ValidatedPlanNode`] / [`ComputeOp`], not parsed text.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{
    AggregateFunction, AggregateSpec, ComputeOp, ComputeTemplate, EffectClass, EffectTemplate,
    FieldPath, Plan, PlanNodeKind, PlanPredicate, PlanPredicateOp, PlanValue, ValidatedPlanExprIr,
    ValidatedPlanExprTemplate, ValidatedPlanNode, ValidatedPlanReturn, ValidatedPlanState,
    ValidatedSurfaceNode,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanDryVerdict {
    Ok,
    Review,
    Deny,
}

impl PlanDryVerdict {
    /// Canonical agent/control-plane wire string (`ok` | `review` | `deny`).
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Review => "review",
            Self::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDryReview {
    pub has_unprojected_multi_row_read: bool,
    pub has_unbounded_read_root: bool,
    pub has_full_collection_compute: bool,
    pub has_foreach_fanout_risk: bool,
    /// `RelationTraversal` with `source_cardinality: many` (per upstream row).
    pub has_relation_many_source_fanout: bool,
    /// `query` surface → `.limit` → `.filter` on materialized rows (fetch vs row filter nudge).
    pub has_query_limit_row_filter: bool,
    /// Paginated list/search surface without pushed read budget or explicit page_size.
    pub has_paginated_list_fetch_all_default: bool,
    /// Embed-style relation with downstream `.limit` but no pushed relation read budget.
    pub has_unbounded_relation_embed_hydrate: bool,
    pub unused_seeds: Vec<String>,
    /// Binding labels that execute but are neither consumed downstream nor returned.
    pub unused_bindings: Vec<String>,
}

impl PlanDryReview {
    /// True when live execute should auto-async (fanout / true fetch-all), not advisory review alone.
    /// Unnarrowed roots with a default host page stay sync but still `needs_review` for MCP plan return.
    pub fn execution_is_expensive(&self) -> bool {
        crate::plan_read_bounds::read_execution_is_expensive(
            self.has_unbounded_read_root,
            self.has_paginated_list_fetch_all_default,
            self.has_relation_many_source_fanout,
            self.has_foreach_fanout_risk,
        )
    }

    pub fn needs_review(&self, return_unbounded_root: bool) -> bool {
        self.has_unprojected_multi_row_read
            || self.has_unbounded_read_root
            || return_unbounded_root
            || self.has_full_collection_compute
            || self.has_foreach_fanout_risk
            || self.has_relation_many_source_fanout
    }

    pub fn warning_line(&self, return_unbounded_root: bool) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.has_unprojected_multi_row_read {
            parts.push("project list reads".to_string());
        }
        if (self.has_unbounded_read_root || return_unbounded_root)
            && !parts.iter().any(|p| p.contains("unbounded"))
        {
            parts.push("unbounded read".to_string());
        }
        if self.has_full_collection_compute && !parts.iter().any(|p| p.contains("project")) {
            parts.push("narrow before aggregate/limit".to_string());
        }
        if self.has_foreach_fanout_risk {
            parts.push("for_each fanout".to_string());
        }
        if self.has_relation_many_source_fanout {
            parts.push("relation per-row fanout".to_string());
        }
        if self.has_unbounded_relation_embed_hydrate {
            parts.push("relation embed hydrate unbounded".to_string());
        }
        if self.has_query_limit_row_filter && !parts.iter().any(|p| p.contains("fetch filter")) {
            parts.push("fetch filter: e1{…} at HTTP, binding.filter{…} on rows".to_string());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("; "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDryCompactView {
    pub verdict: PlanDryVerdict,
    pub node_count: usize,
    pub read_count: usize,
    pub write_count: usize,
    pub return_label: String,
    /// When true, compact dry-run text includes the bind-ordered execution footer.
    pub show_execution_order_footer: bool,
    pub deny_line: Option<String>,
    pub warnings: Option<String>,
    pub steps: Vec<PlanDryStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDryStep {
    pub ordinal: u8,
    pub id: String,
    pub op: PlanDryOp,
    pub uses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDryOp {
    Surface {
        kind: PlanNodeKind,
        expr: String,
    },
    Project {
        fields: Vec<String>,
    },
    Filter {
        predicates: Vec<String>,
    },
    GroupBy {
        keys: Vec<String>,
        aggregates: String,
    },
    Aggregate {
        aggregates: String,
    },
    Sort {
        key: String,
        descending: bool,
    },
    Limit {
        count: usize,
    },
    Dedupe {
        keys: Vec<String>,
    },
    Render {
        columns: Vec<String>,
        template_chars: usize,
    },
    ForEach {
        source: String,
        binding: String,
        body: String,
    },
    Relation {
        relation: String,
        target: String,
        expr: String,
    },
    Data {
        summary: String,
    },
    Derive {
        source: String,
        binding: String,
        summary: String,
    },
}

pub fn build_plan_dry_compact_view(
    plan: &Plan<ValidatedPlanState>,
    topological_order: &[String],
    review: &PlanDryReview,
    graph_summary: &serde_json::Value,
    es: Option<&ExecuteSession>,
    flow_verdict_override: Option<PlanDryVerdict>,
) -> PlanDryCompactView {
    let display_map = build_plan_node_display_map(plan, topological_order);
    let return_unbounded = return_roots_include_unbounded_list_surface(plan);
    let flow_verdict = flow_verdict_override.or_else(|| {
        graph_summary
            .get("security_verdict")
            .and_then(|v| v.as_str())
            .and_then(|v| match v {
                "denied" | "deny" => Some(PlanDryVerdict::Deny),
                "needs_review" | "review" => Some(PlanDryVerdict::Review),
                "clean" | "ok" => Some(PlanDryVerdict::Ok),
                _ => None,
            })
    });
    let inferred_review = if review.needs_review(return_unbounded) {
        PlanDryVerdict::Review
    } else {
        PlanDryVerdict::Ok
    };
    let verdict = std::cmp::max(flow_verdict.unwrap_or(PlanDryVerdict::Ok), inferred_review);
    let deny_line = if verdict == PlanDryVerdict::Deny {
        let violations = graph_summary
            .get("flow_summary")
            .and_then(|v| v.get("violation_count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Some(format!("flow denied: {violations} policy violation(s)"))
    } else {
        None
    };
    let read_count = json_string_array(graph_summary.get("read_nodes")).len();
    let write_count = json_string_array(graph_summary.get("write_or_side_effect_nodes")).len();
    let show_execution_order_footer = matches!(
        &plan.return_value,
        ValidatedPlanReturn::Parallel { parallel } if parallel.len() > 1
    ) && write_count > 1;
    let steps = topological_order
        .iter()
        .enumerate()
        .filter_map(|(ordinal, id)| {
            let node = plan.nodes.iter().find(|n| n.id().as_str() == id)?;
            let display_id = display_map
                .get(id.as_str())
                .cloned()
                .unwrap_or_else(|| id.clone());
            let op = compact_op_from_node(node, es, &display_map);
            let uses = step_upstream_labels(node, &display_map);
            Some(PlanDryStep {
                ordinal: (ordinal + 1).min(u8::MAX as usize) as u8,
                id: display_id,
                op,
                uses,
            })
        })
        .collect();
    PlanDryCompactView {
        verdict,
        node_count: plan.nodes.len(),
        read_count,
        write_count,
        return_label: primary_return_label(plan, &display_map),
        show_execution_order_footer,
        deny_line,
        warnings: review.warning_line(return_unbounded),
        steps,
    }
}

pub fn render_plan_dry_compact_text(
    view: &PlanDryCompactView,
    plan_handle: Option<&str>,
) -> String {
    let mut out = String::new();
    let verdict = match view.verdict {
        PlanDryVerdict::Ok => "ok",
        PlanDryVerdict::Review => "review",
        PlanDryVerdict::Deny => "deny",
    };
    let mut header = format!("plan {verdict} · {}n {}r", view.node_count, view.read_count,);
    if view.write_count > 0 {
        let _ = write!(header, " {}w", view.write_count);
    }
    let _ = write!(header, " → {}", view.return_label);
    if let Some(handle) = plan_handle {
        let _ = write!(header, " · {handle}");
    }
    let _ = writeln!(out, "{header}");
    if let Some(deny) = view.deny_line.as_ref() {
        let _ = writeln!(out, "deny: {deny}");
    }
    if let Some(warn) = view.warnings.as_ref() {
        let _ = writeln!(out, "warn: {warn}");
    }
    let _ = writeln!(out);
    for step in &view.steps {
        let op = render_plan_dry_op(&step.op);
        if step.uses.is_empty() {
            let _ = writeln!(out, "{:02} {:<12} {}", step.ordinal, step.id, op);
        } else {
            let _ = writeln!(
                out,
                "{:02} {:<12} {} ← {}",
                step.ordinal,
                step.id,
                op,
                step.uses.join(", ")
            );
        }
    }
    if view.show_execution_order_footer {
        let _ = writeln!(
            out,
            "execution: bind-ordered (writes are sequential; parallel return is shape only)"
        );
    }
    out
}

/// Operator-facing step title for synthetic IR nodes (not tuned `r1`/`c2` labels).
pub(crate) fn human_ux_headline_for_op(op: &PlanDryOp) -> String {
    match op {
        PlanDryOp::Surface { kind, .. } => match kind {
            PlanNodeKind::Query | PlanNodeKind::Search | PlanNodeKind::Get => "Read list".into(),
            PlanNodeKind::Create => "Create".into(),
            PlanNodeKind::Update => "Update".into(),
            PlanNodeKind::Delete => "Delete".into(),
            PlanNodeKind::Action => "Write".into(),
            _ => render_kind(*kind).to_string(),
        },
        PlanDryOp::Project { fields } => {
            if fields.len() <= 2 {
                format!("Keep {}", fields.join(", "))
            } else {
                format!("Keep {} fields", fields.len())
            }
        }
        PlanDryOp::Filter { .. } => "Filter rows".into(),
        PlanDryOp::GroupBy { keys, .. } => format!("Group by {}", keys.join(", ")),
        PlanDryOp::Aggregate { .. } => "Summarize".into(),
        PlanDryOp::Sort { key, descending } => {
            if *descending {
                format!("Sort by {key} (descending)")
            } else {
                format!("Sort by {key}")
            }
        }
        PlanDryOp::Limit { count } => format!("Take first {count}"),
        PlanDryOp::Dedupe { keys } if keys.is_empty() => "Distinct rows".into(),
        PlanDryOp::Dedupe { keys } => format!("Dedupe on {}", keys.join(", ")),
        PlanDryOp::Render { .. } => "Render text".into(),
        PlanDryOp::ForEach { .. } => "For each row".into(),
        PlanDryOp::Relation { .. } => "Follow relation".into(),
        PlanDryOp::Data { .. } => "Static data".into(),
        PlanDryOp::Derive { .. } => "Derive rows".into(),
    }
}

/// Secondary line for plan UX — resolved wire names in predicate/field text.
pub(crate) fn human_ux_summary_for_op(op: &PlanDryOp) -> String {
    match op {
        PlanDryOp::Filter { predicates } if !predicates.is_empty() => {
            format!("Where {}", predicates.join(", "))
        }
        PlanDryOp::Filter { .. } => "Filter rows".into(),
        PlanDryOp::Project { fields } => format!("Fields: {}", fields.join(", ")),
        PlanDryOp::Surface { kind, expr } => match kind {
            PlanNodeKind::Search => format!("Search · {expr}"),
            PlanNodeKind::Get => format!("Get · {expr}"),
            PlanNodeKind::Query => format!("Read · {expr}"),
            PlanNodeKind::Create => format!("Create · {expr}"),
            PlanNodeKind::Update => format!("Update · {expr}"),
            PlanNodeKind::Delete => format!("Delete · {expr}"),
            PlanNodeKind::Action => format!("Write · {expr}"),
            _ => format!("{} · {expr}", render_kind(*kind)),
        },
        PlanDryOp::Sort { key, descending } => {
            if *descending {
                format!("Sort by {key} (descending)")
            } else {
                format!("Sort by {key}")
            }
        }
        PlanDryOp::Limit { count } => format!("Take first {count}"),
        PlanDryOp::GroupBy { keys, .. } => format!("Group by {}", keys.join(", ")),
        PlanDryOp::Aggregate { .. } => "Summarize".into(),
        PlanDryOp::Dedupe { keys } if keys.is_empty() => "Distinct rows".into(),
        PlanDryOp::Dedupe { keys } => format!("Dedupe on {}", keys.join(", ")),
        PlanDryOp::Render { columns, .. } => format!("Render {}", columns.join(", ")),
        PlanDryOp::Relation {
            relation, target, ..
        } => format!("Via {relation} → {target}"),
        PlanDryOp::ForEach {
            source, binding, ..
        } => format!("For each row in {source} as {binding}"),
        PlanDryOp::Derive {
            source, binding, ..
        } => format!("Derive from {source} as {binding}"),
        PlanDryOp::Data { summary } => format!("Data · {summary}"),
    }
}

pub(crate) fn render_plan_dry_op(op: &PlanDryOp) -> String {
    match op {
        PlanDryOp::Surface { kind, expr } => format!("{} {expr}", render_kind(*kind)),
        PlanDryOp::Project { fields } => format!("project {}", fields.join(", ")),
        PlanDryOp::Filter { predicates } => format!("filter {}", predicates.join(", ")),
        PlanDryOp::GroupBy { keys, aggregates } => {
            format!("group_by {} → {{{aggregates}}}", keys.join(", "))
        }
        PlanDryOp::Aggregate { aggregates } => format!("aggregate → {{{aggregates}}}"),
        PlanDryOp::Sort { key, descending } => {
            format!("sort {key} {}", if *descending { "desc" } else { "asc" })
        }
        PlanDryOp::Limit { count } => format!("limit {count}"),
        PlanDryOp::Dedupe { keys } => {
            if keys.is_empty() {
                "distinct *".to_string()
            } else {
                format!("dedupe {}", keys.join(", "))
            }
        }
        PlanDryOp::Render {
            columns,
            template_chars,
        } => format!("render [{}] ({} chars)", columns.join(", "), template_chars),
        PlanDryOp::ForEach {
            source,
            binding,
            body,
        } => {
            format!("for_each {source} as {binding} => {body}")
        }
        PlanDryOp::Relation {
            relation,
            target,
            expr,
        } => format!("relation {relation} → {target} {expr}"),
        PlanDryOp::Data { summary } => format!("data {summary}"),
        PlanDryOp::Derive {
            source,
            binding,
            summary,
        } => format!("derive map {source} as {binding} → {summary}"),
    }
}

fn compact_op_from_node(
    node: &ValidatedPlanNode,
    es: Option<&ExecuteSession>,
    display_map: &HashMap<String, String>,
) -> PlanDryOp {
    match node {
        ValidatedPlanNode::Surface(s) => PlanDryOp::Surface {
            kind: s.kind,
            expr: surface_compact_expr(s, es),
        },
        ValidatedPlanNode::Data(n) => PlanDryOp::Data {
            summary: data_value_summary(&n.data),
        },
        ValidatedPlanNode::Derive(n) => PlanDryOp::Derive {
            source: map_display_id(n.source.as_str(), display_map),
            binding: n.item_binding.as_str().to_string(),
            summary: plan_value_summary(&n.value),
        },
        ValidatedPlanNode::Compute(n) => compact_op_from_compute(&n.compute, display_map),
        ValidatedPlanNode::RelationTraversal(n) => PlanDryOp::Relation {
            relation: format!(
                "{}.{}",
                map_display_id(n.relation.source.as_str(), display_map),
                n.relation.relation.as_str()
            ),
            target: format!(
                "{}.{}",
                n.relation.target.entry_id, n.relation.target.entity
            ),
            expr: render_plan_expr_ir_for_session(&n.relation.ir, es),
        },
        ValidatedPlanNode::ForEach(n) => PlanDryOp::ForEach {
            source: map_display_id(n.source.as_str(), display_map),
            binding: n.item_binding.as_str().to_string(),
            body: effect_template_body(&n.effect_template, es),
        },
    }
}

fn compact_op_from_compute(
    compute: &ComputeTemplate,
    display_map: &HashMap<String, String>,
) -> PlanDryOp {
    let _ = display_map;
    match &compute.op {
        ComputeOp::Project { fields } => PlanDryOp::Project {
            fields: fields.keys().map(|k| k.as_str().to_string()).collect(),
        },
        ComputeOp::Filter { predicates } => PlanDryOp::Filter {
            predicates: predicates.iter().map(render_predicate_compact).collect(),
        },
        ComputeOp::GroupBy { keys, aggregates } => PlanDryOp::GroupBy {
            keys: keys.iter().map(|k| k.dotted()).collect(),
            aggregates: render_aggregates_compact(aggregates),
        },
        ComputeOp::Aggregate { aggregates } => PlanDryOp::Aggregate {
            aggregates: render_aggregates_compact(aggregates),
        },
        ComputeOp::Sort { key, descending } => PlanDryOp::Sort {
            key: key.dotted(),
            descending: *descending,
        },
        ComputeOp::Limit { count } => PlanDryOp::Limit { count: *count },
        ComputeOp::DedupeBy { keys } => PlanDryOp::Dedupe {
            keys: keys.iter().map(|k| k.dotted()).collect(),
        },
        ComputeOp::Render {
            columns, template, ..
        } => PlanDryOp::Render {
            columns: columns.iter().map(|c| c.as_str().to_string()).collect(),
            template_chars: template.chars().count(),
        },
    }
}

fn surface_compact_expr(surface: &ValidatedSurfaceNode, es: Option<&ExecuteSession>) -> String {
    let raw = surface
        .ir
        .as_ref()
        .map(|ir| render_plan_expr_ir_for_session(ir, es))
        .or_else(|| surface.ir_template.as_ref().map(render_plan_expr_template))
        .or_else(|| surface.display_expr.clone())
        .unwrap_or_else(|| "<typed Plasm IR>".to_string());
    crate::plan_dry_compact::compact_agent_surface_expr(&raw)
}

fn render_plan_expr_ir_for_session(
    ir: &ValidatedPlanExprIr,
    es: Option<&ExecuteSession>,
) -> String {
    if let Some(display) = ir.display_expr.as_ref() {
        return display.clone();
    }
    render_expr_wire_for_execute_session(&ir.expr, es)
}

/// Canonical wire-surface renderer for typed [`Expr`] in an execute session (dry plan, artifacts).
/// Compact IL summaries ([`crate::expr_display::expr_display_resolved`]) are a separate hint surface.
pub(crate) fn render_expr_wire_for_execute_session(
    expr: &plasm_core::Expr,
    es: Option<&ExecuteSession>,
) -> String {
    match es {
        None => crate::expr_display::expr_display(expr),
        Some(es) => {
            if es.contexts_by_entry.len() > 1 {
                if let Some(exposure) = es.teaching_exposure.as_ref() {
                    let fed = plasm_core::FederationDispatch::from_contexts_and_exposure(
                        es.contexts_by_entry.clone(),
                        exposure,
                    );
                    return plasm_core::render_expr_surface_federated(expr, &fed, es.cgs.as_ref());
                }
            }
            plasm_core::render_expr_surface(expr, es.cgs.as_ref())
        }
    }
}

fn render_plan_expr_template(template: &ValidatedPlanExprTemplate) -> String {
    template
        .display_expr
        .clone()
        .unwrap_or_else(|| "<typed Plasm IR template>".to_string())
}

fn effect_template_body(template: &EffectTemplate, _es: Option<&ExecuteSession>) -> String {
    if !template.expr_template.trim().is_empty() {
        return template.expr_template.clone();
    }
    template
        .ir_template
        .display_expr
        .clone()
        .unwrap_or_else(|| "<typed Plasm IR template>".to_string())
}

fn step_upstream_labels(
    node: &ValidatedPlanNode,
    display_map: &HashMap<String, String>,
) -> Vec<String> {
    let mut ids: Vec<String> = node
        .uses_result()
        .iter()
        .map(|u| map_display_id(&u.node, display_map))
        .collect();
    if ids.is_empty() {
        match node {
            ValidatedPlanNode::Compute(n) => {
                ids.push(map_display_id(&n.compute.source, display_map));
            }
            ValidatedPlanNode::Derive(n) => {
                ids.push(map_display_id(n.source.as_str(), display_map));
            }
            ValidatedPlanNode::ForEach(n) => {
                ids.push(map_display_id(n.source.as_str(), display_map));
            }
            ValidatedPlanNode::RelationTraversal(n) => {
                ids.push(map_display_id(n.relation.source.as_str(), display_map));
            }
            _ => {}
        }
    }
    ids
}

fn map_display_id(id: &str, display_map: &HashMap<String, String>) -> String {
    display_map
        .get(id)
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

fn primary_return_label(
    plan: &Plan<ValidatedPlanState>,
    display_map: &HashMap<String, String>,
) -> String {
    match &plan.return_value {
        ValidatedPlanReturn::Node(id) => map_display_id(id.as_str(), display_map),
        ValidatedPlanReturn::Parallel { parallel } => {
            if parallel.len() == 1 {
                map_display_id(parallel[0].as_str(), display_map)
            } else if parallel.len() <= 3 {
                let names: Vec<String> = parallel
                    .iter()
                    .map(|id| map_display_id(id.as_str(), display_map))
                    .collect();
                format!("returns: {}", names.join(", "))
            } else {
                format!("returns({})", parallel.len())
            }
        }
    }
}

fn render_predicate_compact(predicate: &PlanPredicate) -> String {
    format!(
        "{}{}{}",
        predicate.field_path.dotted(),
        render_predicate_op(predicate.op),
        render_plan_value_compact(&predicate.value)
    )
}

fn render_aggregates_compact(aggregates: &[AggregateSpec]) -> String {
    aggregates
        .iter()
        .map(|agg| {
            let field = agg
                .field
                .as_ref()
                .map(FieldPath::dotted)
                .unwrap_or_else(|| "*".to_string());
            format!(
                "{}={}({field})",
                agg.name.as_str(),
                render_aggregate_function(agg.function)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_predicate_op(op: PlanPredicateOp) -> &'static str {
    match op {
        PlanPredicateOp::Eq => "=",
        PlanPredicateOp::Ne => "!=",
        PlanPredicateOp::Lt => "<",
        PlanPredicateOp::Lte => "<=",
        PlanPredicateOp::Gt => ">",
        PlanPredicateOp::Gte => ">=",
        PlanPredicateOp::Contains => "~",
        PlanPredicateOp::In => " in ",
        PlanPredicateOp::Exists => " exists ",
    }
}

fn render_aggregate_function(function: AggregateFunction) -> &'static str {
    match function {
        AggregateFunction::Count => "count",
        AggregateFunction::Sum => "sum",
        AggregateFunction::Avg => "avg",
        AggregateFunction::Min => "min",
        AggregateFunction::Max => "max",
        AggregateFunction::First => "first",
        AggregateFunction::Last => "last",
    }
}

fn render_plan_value_compact(value: &PlanValue) -> String {
    match value {
        PlanValue::Literal { value } => render_json_value(value),
        PlanValue::Helper {
            name,
            args,
            display,
        } => display.clone().unwrap_or_else(|| {
            format!(
                "{}({})",
                name,
                args.iter()
                    .map(render_json_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }),
        PlanValue::Object { fields } => format!("{{{}}}", fields.len()),
        PlanValue::Array { items } => format!("[{}]", items.len()),
        PlanValue::Template { .. } => "template".to_string(),
        PlanValue::NodeSymbol { alias, path, .. } => {
            if path.is_empty() {
                alias.clone()
            } else {
                format!("{alias}.{}", path.join("."))
            }
        }
        PlanValue::BindingSymbol { binding, path } => {
            if path.is_empty() {
                binding.clone()
            } else {
                format!("{binding}.{}", path.join("."))
            }
        }
        PlanValue::Symbol { path } => path.clone(),
        PlanValue::EntityRefKey { key, .. } => render_plan_value_compact(key),
    }
}

fn data_value_summary(value: &PlanValue) -> String {
    match value {
        PlanValue::Object { fields } => format!("{{{}}}", fields.len()),
        _ => plan_value_summary(value),
    }
}

fn plan_value_summary(value: &PlanValue) -> String {
    match value {
        PlanValue::Object { fields } => format!("{{{}}}", fields.len()),
        PlanValue::Array { items } => format!("[{}]", items.len()),
        PlanValue::Literal { value } => render_json_value(value),
        PlanValue::Template { .. } => "template".to_string(),
        _ => render_plan_value_compact(value),
    }
}

fn render_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => format!("\"{s}\""),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(items) => format!("[{}]", items.len()),
        serde_json::Value::Object(map) => format!("{{{}}}", map.len()),
    }
}

fn render_kind(kind: PlanNodeKind) -> &'static str {
    match kind {
        PlanNodeKind::Query => "query",
        PlanNodeKind::Search => "search",
        PlanNodeKind::Get => "get",
        PlanNodeKind::Create => "create",
        PlanNodeKind::Update => "update",
        PlanNodeKind::Delete => "delete",
        PlanNodeKind::Action => "action",
        PlanNodeKind::Data => "data",
        PlanNodeKind::Derive => "derive",
        PlanNodeKind::Compute => "compute",
        PlanNodeKind::ForEach => "for_each",
        PlanNodeKind::Relation => "relation",
    }
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn surface_read_list_root_unbounded(s: &ValidatedSurfaceNode) -> bool {
    // Default host page caps fetch cost but is not an agent-declared bound — still advisory.
    matches!(s.result_shape, crate::plasm_plan::ResultShape::List)
        && s.effect_class == EffectClass::Read
        && s.depends_on.is_empty()
        && s.page_size.is_none()
        && s.pushed_read_budget.is_none()
        && s.kind != PlanNodeKind::Search
        && s.predicates.is_empty()
}

pub(crate) fn return_roots_include_unbounded_list_surface(plan: &Plan<ValidatedPlanState>) -> bool {
    for id in plan.return_value.refs() {
        let Some(node) = plan.nodes.iter().find(|n| n.id() == id) else {
            continue;
        };
        if let ValidatedPlanNode::Surface(s) = node {
            if surface_read_list_root_unbounded(s) {
                return true;
            }
        }
    }
    false
}

fn is_synthetic_plan_node_id(id: &str) -> bool {
    id.starts_with("__plasm_")
        || id
            .strip_prefix("return_")
            .and_then(|rest| rest.parse::<u32>().ok())
            .is_some()
}

pub(crate) fn is_synthetic_plan_node_id_public(id: &str) -> bool {
    is_synthetic_plan_node_id(id)
}

#[derive(Default)]
struct SyntheticPlanLabelCounters {
    r: usize,
    w: usize,
    c: usize,
    d: usize,
    f: usize,
    l: usize,
    x: usize,
}

fn next_synthetic_plan_label(
    node: &ValidatedPlanNode,
    counters: &mut SyntheticPlanLabelCounters,
) -> String {
    match node {
        ValidatedPlanNode::Surface(surface) => match surface.effect_class {
            EffectClass::Read => {
                counters.r += 1;
                format!("r{}", counters.r)
            }
            EffectClass::Write | EffectClass::SideEffect => {
                counters.w += 1;
                format!("w{}", counters.w)
            }
            EffectClass::ArtifactRead => {
                counters.x += 1;
                format!("x{}", counters.x)
            }
        },
        ValidatedPlanNode::Compute(_) => {
            counters.c += 1;
            format!("c{}", counters.c)
        }
        ValidatedPlanNode::Derive(_) => {
            counters.d += 1;
            format!("d{}", counters.d)
        }
        ValidatedPlanNode::ForEach(_) => {
            counters.f += 1;
            format!("f{}", counters.f)
        }
        ValidatedPlanNode::RelationTraversal(_) => {
            counters.l += 1;
            format!("l{}", counters.l)
        }
        ValidatedPlanNode::Data(_) => {
            counters.x += 1;
            format!("x{}", counters.x)
        }
    }
}

/// Node id → compact display label (`r1`, `c1`, …) for async operation progress lines.
pub(crate) fn plan_node_display_map(
    plan: &Plan<ValidatedPlanState>,
    topological_order: &[String],
) -> HashMap<String, String> {
    build_plan_node_display_map(plan, topological_order)
}

fn build_plan_node_display_map(
    plan: &Plan<ValidatedPlanState>,
    topological_order: &[String],
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut counters = SyntheticPlanLabelCounters::default();
    for id in topological_order {
        let Some(node) = plan.nodes.iter().find(|n| n.id().as_str() == id) else {
            continue;
        };
        let label = if is_synthetic_plan_node_id(id.as_str()) {
            next_synthetic_plan_label(node, &mut counters)
        } else {
            id.clone()
        };
        map.insert(id.clone(), label);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::{OutputName, SyntheticResultSchema};
    use std::collections::BTreeMap;

    #[test]
    fn project_op_uses_field_names_only() {
        let mut fields = BTreeMap::new();
        fields.insert(
            OutputName::new("identifier").expect("name"),
            FieldPath::new(vec!["identifier".to_string()]).expect("path"),
        );
        fields.insert(
            OutputName::new("title").expect("name"),
            FieldPath::new(vec!["title".to_string()]).expect("path"),
        );
        let op = compact_op_from_compute(
            &ComputeTemplate {
                source: "open_auth".to_string(),
                op: ComputeOp::Project { fields },
                schema: SyntheticResultSchema {
                    entity: None,
                    fields: Vec::new(),
                },
                page_size: None,
                collection_alias: None,
            },
            &HashMap::new(),
        );
        assert_eq!(
            op,
            PlanDryOp::Project {
                fields: vec!["identifier".to_string(), "title".to_string()],
            }
        );
        assert_eq!(render_plan_dry_op(&op), "project identifier, title");
    }

    #[test]
    fn plan_expr_wire_surface_is_not_il_summary() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/petstore_minimal");
        if !dir.exists() {
            return;
        }
        let cgs = plasm_core::load_schema_dir(&dir).expect("petstore_minimal");
        let pe = plasm_core::expr_parser::parse("Pet(1)", &cgs).expect("parse");
        let wire = plasm_core::render_expr_surface(&pe.expr, &cgs);
        assert_eq!(wire, "Pet(1)");
        assert!(
            !wire.starts_with("Get("),
            "wire surface must not be compact IL: {wire}"
        );
        let ir = ValidatedPlanExprIr {
            expr: pe.expr,
            projection: pe.projection,
            display_expr: Some(wire.clone()),
        };
        assert_eq!(render_plan_expr_ir_for_session(&ir, None), wire);
    }
}
