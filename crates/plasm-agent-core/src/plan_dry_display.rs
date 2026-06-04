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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDryVerdict {
    Ok,
    Review,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanDryReview {
    pub has_unprojected_multi_row_read: bool,
    pub has_unbounded_read_root: bool,
    pub has_full_collection_compute: bool,
    pub has_foreach_fanout_risk: bool,
    /// `RelationTraversal` with `source_cardinality: many` (per upstream row).
    pub has_relation_many_source_fanout: bool,
    /// `query` surface → `.limit` → `.filter` on materialized rows (fetch vs row filter nudge).
    pub has_query_limit_row_filter: bool,
    pub unused_seeds: Vec<String>,
}

impl PlanDryReview {
    pub fn needs_review(&self, return_unbounded_root: bool) -> bool {
        self.has_unprojected_multi_row_read
            || self.has_unbounded_read_root
            || return_unbounded_root
            || self.has_full_collection_compute
            || self.has_foreach_fanout_risk
            || self.has_relation_many_source_fanout
            || !self.unused_seeds.is_empty()
    }

    pub fn warning_line(&self, return_unbounded_root: bool) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.has_unprojected_multi_row_read {
            parts.push("project list reads".to_string());
        }
        if !self.unused_seeds.is_empty() {
            parts.push(format!("unused seed {}", self.unused_seeds.join(", ")));
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
) -> PlanDryCompactView {
    let display_map = build_plan_node_display_map(plan, topological_order);
    let return_unbounded = return_roots_include_unbounded_list_surface(plan);
    let verdict = if review.needs_review(return_unbounded) {
        PlanDryVerdict::Review
    } else {
        PlanDryVerdict::Ok
    };
    let read_count = json_string_array(graph_summary.get("read_nodes")).len();
    let write_count = json_string_array(graph_summary.get("write_or_side_effect_nodes")).len();
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
    out
}

fn render_plan_dry_op(op: &PlanDryOp) -> String {
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
        ComputeOp::Render { columns, template } => PlanDryOp::Render {
            columns: columns.iter().map(|c| c.as_str().to_string()).collect(),
            template_chars: template.chars().count(),
        },
    }
}

fn surface_compact_expr(surface: &ValidatedSurfaceNode, es: Option<&ExecuteSession>) -> String {
    surface
        .ir
        .as_ref()
        .map(|ir| render_plan_expr_ir_for_session(ir, es))
        .or_else(|| surface.ir_template.as_ref().map(render_plan_expr_template))
        .or_else(|| surface.display_expr.clone())
        .unwrap_or_else(|| "<typed Plasm IR>".to_string())
}

fn render_plan_expr_ir_for_session(
    ir: &ValidatedPlanExprIr,
    es: Option<&ExecuteSession>,
) -> String {
    if let Some(display) = ir.display_expr.as_ref() {
        return display.clone();
    }
    let Some(es) = es else {
        return crate::expr_display::expr_display(&ir.expr);
    };
    let exp = match es.domain_exposure.as_ref() {
        Some(e) => e.clone(),
        None => return crate::expr_display::expr_display_resolved(&ir.expr, es.cgs.as_ref()),
    };
    let fed = plasm_core::FederationDispatch::from_contexts_and_exposure(
        es.contexts_by_entry.clone(),
        &exp,
    );
    crate::expr_display::expr_display_resolved_federated(&ir.expr, &fed, es.cgs.as_ref())
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
            } else {
                format!("parallel({})", parallel.len())
            }
        }
    }
}

fn render_predicate_compact(predicate: &PlanPredicate) -> String {
    format!(
        "{}{}{}",
        predicate.field_path.join("."),
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
    matches!(s.result_shape, crate::plasm_plan::ResultShape::List)
        && s.effect_class == EffectClass::Read
        && s.depends_on.is_empty()
        && s.page_size.is_none()
        && s.kind != PlanNodeKind::Search
        && s.predicates.is_empty()
}

fn return_roots_include_unbounded_list_surface(plan: &Plan<ValidatedPlanState>) -> bool {
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
}
