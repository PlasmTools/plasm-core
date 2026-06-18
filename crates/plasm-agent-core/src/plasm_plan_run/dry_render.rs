//! Human-readable plan node operation strings for dry-run display and evidence source lines.

use crate::plasm_plan::{
    AggregateFunction, ComputeOp, ComputeTemplate, FieldPath, PlanNodeKind, PlanValue,
    ValidatedDeriveNode, ValidatedPlanDataInput, ValidatedPlanNode, ValidatedSurfaceNode,
};

pub fn render_node_operation(node: &ValidatedPlanNode) -> String {
    match node {
        ValidatedPlanNode::Surface(n) => render_surface_operation(n),
        ValidatedPlanNode::Data(n) => format!("data {}", render_plan_value(&n.data)),
        ValidatedPlanNode::Derive(n) => render_derive_template(n),
        ValidatedPlanNode::Compute(n) => render_compute_template(&n.compute),
        ValidatedPlanNode::RelationTraversal(n) => {
            let source = n.relation.source.as_str();
            let relation = n.relation.relation.as_str();
            let target = format!(
                "{}.{}",
                n.relation.target.entry_id, n.relation.target.entity
            );
            format!(
                "relation {source}.{relation} -> {target} <= {}",
                render_plan_expr_ir(&n.relation.ir)
            )
        }
        ValidatedPlanNode::ForEach(n) => {
            let source = n.source.as_str();
            let binding = n.item_binding.as_str();
            let template = render_effect_template_expr(&n.effect_template);
            format!("for_each {source} as {binding} => {template}")
        }
    }
}

pub(crate) fn render_surface_operation(node: &ValidatedSurfaceNode) -> String {
    let entity = node
        .qualified_entity
        .as_ref()
        .map(|q| format!("{}.{}", q.entry_id, q.entity))
        .unwrap_or_else(|| "<unqualified>".to_string());
    let expr = node
        .ir
        .as_ref()
        .map(render_plan_expr_ir)
        .or_else(|| node.ir_template.as_ref().map(render_plan_expr_template))
        .or_else(|| node.display_expr.clone())
        .unwrap_or_else(|| "<typed Plasm IR>".to_string());
    format!("{} {} <= {}", render_kind(node.kind), entity, expr)
}

pub(crate) fn render_plan_expr_ir(ir: &crate::plasm_plan::ValidatedPlanExprIr) -> String {
    ir.display_expr
        .clone()
        .unwrap_or_else(|| crate::expr_display::expr_display(&ir.expr))
}

pub(crate) fn render_plan_expr_template(
    template: &crate::plasm_plan::ValidatedPlanExprTemplate,
) -> String {
    template
        .display_expr
        .clone()
        .unwrap_or_else(|| "<typed Plasm IR template>".to_string())
}

pub(crate) fn render_effect_template_expr(template: &crate::plasm_plan::EffectTemplate) -> String {
    if !template.expr_template.trim().is_empty() {
        template.expr_template.clone()
    } else {
        template
            .ir_template
            .display_expr
            .clone()
            .unwrap_or_else(|| "<typed Plasm IR template>".to_string())
    }
}

pub(crate) fn render_derive_template(template: &ValidatedDeriveNode) -> String {
    let source = template.source.as_str();
    let binding = template.item_binding.as_str();
    let inputs = render_data_inputs(&template.inputs);
    let input_suffix = if inputs.is_empty() {
        String::new()
    } else {
        format!(" with {}", inputs.join(", "))
    };
    format!(
        "derive map {source} as {binding}{input_suffix} → {}",
        render_plan_value(&template.value)
    )
}

pub(crate) fn render_data_inputs(inputs: &[ValidatedPlanDataInput]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| {
            format!(
                "{} as {} {}",
                input.node.as_str(),
                input.alias.as_str(),
                render_input_cardinality(input.proof)
            )
        })
        .collect()
}

pub(crate) fn render_input_cardinality(
    proof: crate::plasm_plan::InputCardinalityProof,
) -> &'static str {
    match proof {
        crate::plasm_plan::InputCardinalityProof::StaticSingleton => "static-singleton",
        crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton => {
            "runtime-checked-singleton"
        }
    }
}

pub(crate) fn render_compute_template(compute: &ComputeTemplate) -> String {
    match &compute.op {
        ComputeOp::Project { fields } => {
            let fields = fields
                .iter()
                .map(|(name, path)| format!("{}={}", name.as_str(), path.dotted()))
                .collect::<Vec<_>>()
                .join(", ");
            format!("project {} -> {{{fields}}}", compute.source)
        }
        ComputeOp::Filter { predicates } => {
            let predicates = predicates
                .iter()
                .map(render_predicate)
                .collect::<Vec<_>>()
                .join(", ");
            format!("filter {} where {predicates}", compute.source)
        }
        ComputeOp::GroupBy { keys, aggregates } => {
            let key_list = keys
                .iter()
                .map(|k| k.dotted())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "group_by {} keys=[{key_list}] -> {{{}}}",
                compute.source,
                render_aggregates(aggregates)
            )
        }
        ComputeOp::Aggregate { aggregates } => {
            format!(
                "aggregate {} -> {{{}}}",
                compute.source,
                render_aggregates(aggregates)
            )
        }
        ComputeOp::Sort { key, descending } => format!(
            "sort {} by {} {}",
            compute.source,
            key.dotted(),
            if *descending { "desc" } else { "asc" }
        ),
        ComputeOp::Limit { count } => format!("limit {} count={count}", compute.source),
        ComputeOp::DedupeBy { keys } => {
            if keys.is_empty() {
                format!("distinct {} *", compute.source)
            } else {
                format!(
                    "dedupe {} keys={}",
                    compute.source,
                    keys.iter()
                        .map(|k| k.dotted())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
        }
        ComputeOp::Render { columns, template } => format!(
            "render {} columns=[{}] template_chars={}",
            compute.source,
            columns
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            template.chars().count()
        ),
    }
}

pub(crate) fn render_aggregates(aggregates: &[crate::plasm_plan::AggregateSpec]) -> String {
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

pub(crate) fn render_predicate(predicate: &crate::plasm_plan::PlanPredicate) -> String {
    format!(
        "{}{}{}",
        predicate.field_path.join("."),
        render_predicate_op(predicate.op),
        render_plan_value(&predicate.value)
    )
}

pub(crate) fn render_plan_value(value: &PlanValue) -> String {
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
        PlanValue::Symbol { path } => format!("${path}"),
        PlanValue::BindingSymbol { binding, path } => {
            let suffix = if path.is_empty() {
                String::new()
            } else {
                format!(".{}", path.join("."))
            };
            format!("${binding}{suffix}")
        }
        PlanValue::NodeSymbol { alias, path, .. } => {
            let suffix = if path.is_empty() {
                String::new()
            } else {
                format!(".{}", path.join("."))
            };
            format!("${alias}{suffix}")
        }
        PlanValue::Template { template, .. } => format!("template`{template}`"),
        PlanValue::EntityRefKey { key, .. } => render_plan_value(key),
        PlanValue::Array { items } => {
            if items.is_empty() {
                return "[0 items]".to_string();
            }
            let mut rendered = items
                .iter()
                .take(5)
                .map(render_plan_value)
                .collect::<Vec<_>>();
            if items.len() > 5 {
                rendered.push("...".to_string());
            }
            format!("[{}]", rendered.join(", "))
        }
        PlanValue::Object { fields } => {
            if fields.is_empty() {
                return "{0 fields}".to_string();
            }
            let mut rendered = fields
                .iter()
                .take(8)
                .map(|(name, value)| format!("{name}: {}", render_plan_value(value)))
                .collect::<Vec<_>>();
            if fields.len() > 8 {
                rendered.push("...".to_string());
            }
            format!("{{{}}}", rendered.join(", "))
        }
    }
}

pub(crate) fn render_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => format!("{s:?}"),
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                return "[0 items]".to_string();
            }
            let mut rendered = items
                .iter()
                .take(5)
                .map(render_json_value)
                .collect::<Vec<_>>();
            if items.len() > 5 {
                rendered.push("...".to_string());
            }
            format!("[{}]", rendered.join(", "))
        }
        serde_json::Value::Object(obj) => {
            if obj.is_empty() {
                return "{0 fields}".to_string();
            }
            let mut rendered = obj
                .iter()
                .take(8)
                .map(|(name, value)| format!("{name}: {}", render_json_value(value)))
                .collect::<Vec<_>>();
            if obj.len() > 8 {
                rendered.push("...".to_string());
            }
            format!("{{{}}}", rendered.join(", "))
        }
        other => other.to_string(),
    }
}

pub(crate) fn render_kind(kind: PlanNodeKind) -> &'static str {
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

pub(crate) fn render_aggregate_function(function: AggregateFunction) -> &'static str {
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

pub(crate) fn render_predicate_op(op: crate::plasm_plan::PlanPredicateOp) -> &'static str {
    match op {
        crate::plasm_plan::PlanPredicateOp::Eq => "=",
        crate::plasm_plan::PlanPredicateOp::Ne => "!=",
        crate::plasm_plan::PlanPredicateOp::Lt => "<",
        crate::plasm_plan::PlanPredicateOp::Lte => "<=",
        crate::plasm_plan::PlanPredicateOp::Gt => ">",
        crate::plasm_plan::PlanPredicateOp::Gte => ">=",
        crate::plasm_plan::PlanPredicateOp::Contains => "~",
        crate::plasm_plan::PlanPredicateOp::In => " in ",
        crate::plasm_plan::PlanPredicateOp::Exists => " exists ",
    }
}
