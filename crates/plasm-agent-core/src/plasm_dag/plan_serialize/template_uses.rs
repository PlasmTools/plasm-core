//! Template-ref collection for plan `uses_result` / `ir_template`.

use super::super::prelude::*;

pub(in crate::plasm_dag) fn collect_template_uses_from_expr(expr: &Expr) -> Vec<serde_json::Value> {
    let ctx = plasm_core::TemplateRefContext::for_row_scope("_");
    let mut acc = Vec::new();
    collect_expr_for_template_uses(&mut acc, expr, &ctx);
    dedupe_uses(acc)
}

/// `uses_result` for relation plan nodes: per-row `source` plus any `node_input` aliases (e.g. `repo`).
pub(in crate::plasm_dag) fn relation_plan_uses_result(
    source_label: &str,
    parsed: &plasm_core::expr_parser::ParsedExpr,
) -> Vec<serde_json::Value> {
    let mut uses = vec![serde_json::json!({
        "node": source_label,
        "as": "source",
    })];
    for u in collect_template_uses_from_expr(&parsed.expr) {
        let node = u.get("node").and_then(|v| v.as_str()).unwrap_or("");
        let alias = u.get("as").and_then(|v| v.as_str()).unwrap_or(node);
        if node == source_label || (node == "source" && alias == "source") {
            continue;
        }
        uses.push(if node == "source" {
            serde_json::json!({ "node": source_label, "as": alias })
        } else {
            u
        });
    }
    dedupe_uses(uses)
}

/// Records upstream plan nodes so `node_input` holes become `uses_result` → `ir_template` + instantiation
/// before compile.
///
/// Surfaces covered: query predicates; **get**/**delete**/**invoke** `path_vars`; invoke/create payloads (values
/// recurse into objects/arrays). [`Expr::Get`] compound identity literals live on `reference`; program bindings
/// in compound slots are lowered to `path_vars` and collected here. [`PlasmInputRef::RowBinding`] is skipped on
/// purpose (`for_each` row scope).
pub(in crate::plasm_dag) fn collect_expr_for_template_uses(
    acc: &mut Vec<serde_json::Value>,
    expr: &Expr,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    match expr {
        Expr::Query(q) => {
            if let Some(pred) = &q.predicate {
                collect_predicate_for_template_uses(acc, pred, ctx);
            }
        }
        Expr::Get(g) => {
            if let Some(pv) = &g.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v, ctx);
                }
            }
        }
        Expr::Create(c) => {
            let v = c.input.to_value();
            collect_value_for_template_uses(acc, &v, ctx);
        }
        Expr::Delete(d) => {
            if let Some(pv) = &d.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v, ctx);
                }
            }
        }
        Expr::Invoke(i) => {
            if let Some(input) = &i.input {
                let v = input.to_value();
                collect_value_for_template_uses(acc, &v, ctx);
            }
            if let Some(pv) = &i.path_vars {
                for v in pv.values() {
                    collect_value_for_template_uses(acc, v, ctx);
                }
            }
        }
        Expr::Chain(ch) => {
            collect_expr_for_template_uses(acc, &ch.source, ctx);
            if let ChainStep::Explicit { expr } = &ch.step {
                collect_expr_for_template_uses(acc, expr.as_ref(), ctx);
            }
        }
        Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) => {}
        Expr::TeachingValue { value } => {
            collect_value_for_template_uses(acc, value, ctx);
        }
    }
}

pub(in crate::plasm_dag) fn collect_predicate_for_template_uses(
    acc: &mut Vec<serde_json::Value>,
    pred: &Predicate,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    match pred {
        Predicate::Comparison { value, .. } => {
            let v = value.to_value();
            collect_value_for_template_uses(acc, &v, ctx);
        }
        Predicate::And { args } | Predicate::Or { args } => {
            for a in args {
                collect_predicate_for_template_uses(acc, a, ctx);
            }
        }
        Predicate::Not { predicate } => {
            collect_predicate_for_template_uses(acc, predicate.as_ref(), ctx)
        }
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                collect_predicate_for_template_uses(acc, inner.as_ref(), ctx);
            }
        }
        Predicate::True | Predicate::False => {}
    }
}

pub(in crate::plasm_dag) fn collect_value_for_template_uses(
    acc: &mut Vec<serde_json::Value>,
    v: &Value,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    match v {
        Value::PlasmInputRef(PlasmInputRef::NodeInput { node, .. }) => {
            acc.push(json!({
                "node": node,
                "as": node,
            }));
        }
        Value::PlasmInputRef(PlasmInputRef::RowBinding { .. }) => {}
        Value::Object(m) => {
            for x in m.values() {
                collect_value_for_template_uses(acc, x, ctx);
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_value_for_template_uses(acc, x, ctx);
            }
        }
        Value::String(s) => {
            for (node, alias) in ctx.plan_node_roots_from_string(s) {
                acc.push(json!({
                    "node": node,
                    "as": alias,
                }));
            }
        }
        _ => {}
    }
}

pub(in crate::plasm_dag) fn dedupe_uses(uses: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut seen = BTreeSet::new();
    uses.into_iter()
        .filter(|u| {
            let key = format!(
                "{}:{}",
                u.get("node").and_then(|v| v.as_str()).unwrap_or_default(),
                u.get("as").and_then(|v| v.as_str()).unwrap_or_default()
            );
            seen.insert(key)
        })
        .collect()
}

pub(in crate::plasm_dag) fn dedupe_inputs(
    inputs: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let mut seen = BTreeSet::new();
    inputs
        .into_iter()
        .filter(|u| {
            let key = format!(
                "{}:{}",
                u.get("node").and_then(|v| v.as_str()).unwrap_or_default(),
                u.get("alias").and_then(|v| v.as_str()).unwrap_or_default()
            );
            seen.insert(key)
        })
        .collect()
}
