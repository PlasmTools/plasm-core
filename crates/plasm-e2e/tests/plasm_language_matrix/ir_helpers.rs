//! Dry-plan IR extraction helpers for matrix planning asserts.

use plasm_agent::plasm_plan::{ComputeTemplate};
use plasm_agent::plasm_plan_run::DryPlasmPlanEvaluation;
use plasm_core::{
    ChainStep, EntityKey, Expr, GetExpr, QueryExpr,
    TypedComparisonValue, Value,
};

pub(crate) fn surface_exprs(dry: &DryPlasmPlanEvaluation) -> Vec<Expr> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            let ev = nr.get("ir")?.get("expr")?;
            serde_json::from_value(ev.clone()).ok()
        })
        .collect()
}

pub(crate) fn relation_exprs(dry: &DryPlasmPlanEvaluation) -> Vec<Expr> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            let ev = nr.get("execution_contract")?.get("ir")?;
            serde_json::from_value(ev.clone()).ok()
        })
        .collect()
}

pub(crate) fn compute_templates(dry: &DryPlasmPlanEvaluation) -> Vec<ComputeTemplate> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            nr.get("compute")
                .and_then(|c| serde_json::from_value::<ComputeTemplate>(c.clone()).ok())
        })
        .collect()
}

pub(crate) fn tcv_string(v: &TypedComparisonValue) -> Option<String> {
    match v.to_value() {
        Value::String(s) => Some(s),
        Value::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(crate) fn json_contains_selector_field(v: &serde_json::Value, want: &str) -> bool {
    match v {
        serde_json::Value::Object(map) => {
            if map.get("selector").and_then(|s| s.as_str()) == Some(want) {
                return true;
            }
            map.values().any(|x| json_contains_selector_field(x, want))
        }
        serde_json::Value::Array(a) => a.iter().any(|x| json_contains_selector_field(x, want)),
        _ => false,
    }
}

pub(crate) fn comp_has_relation_named(comp: &serde_json::Value, relation: &str) -> bool {
    comp_relation_named(comp, relation).is_some()
}

pub(crate) fn comp_relation_named<'a>(
    comp: &'a serde_json::Value,
    relation: &str,
) -> Option<&'a serde_json::Value> {
    let steps = comp.get("steps")?.as_object()?;
    for step in steps.values() {
        if step.get("kind").and_then(|k| k.as_str()) == Some("flat_map_relation")
            && step.pointer("/relation/relation").and_then(|x| x.as_str()) == Some(relation)
        {
            return step.get("relation");
        }
    }
    None
}

pub(crate) fn comp_ir_contains_selector(comp: &serde_json::Value, want: &str) -> bool {
    let Some(steps) = comp.get("steps").and_then(|s| s.as_object()) else {
        return false;
    };
    steps.values().any(|n| {
        [n.get("ir"), n.get("ir_template")]
            .into_iter()
            .flatten()
            .any(|ir| json_contains_selector_field(ir, want))
    })
}

pub(crate) fn json_value_contains_substring(v: &serde_json::Value, needle: &str) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains(needle),
        serde_json::Value::Array(a) => a.iter().any(|x| json_value_contains_substring(x, needle)),
        serde_json::Value::Object(o) => {
            o.values().any(|x| json_value_contains_substring(x, needle))
        }
        _ => false,
    }
}

pub(crate) fn first_query(exprs: &[Expr]) -> Result<&QueryExpr, String> {
    for e in exprs {
        if let Expr::Query(q) = e {
            return Ok(q);
        }
    }
    Err("expected a Query IR node".into())
}

pub(crate) fn comp_surface_page_size(comp: &serde_json::Value) -> Option<u64> {
    let steps = comp.get("steps")?.as_object()?;
    for step in steps.values() {
        if step.get("kind").and_then(|k| k.as_str()) == Some("invoke") {
            return step.get("page_size")?.as_u64();
        }
    }
    None
}

pub(crate) fn comp_steps_values(comp: &serde_json::Value) -> Vec<&serde_json::Value> {
    comp.get("steps")
        .and_then(|s| s.as_object())
        .map(|m| m.values().collect())
        .unwrap_or_default()
}

pub(crate) fn comp_first_invoke_qualified_entity(comp: &serde_json::Value) -> Option<&serde_json::Value> {
    comp_steps_values(comp)
        .into_iter()
        .find(|s| s.get("kind").and_then(|k| k.as_str()) == Some("invoke"))
        .and_then(|s| s.get("qualified_entity"))
}

pub(crate) fn comp_has_invoke_plan_kind(comp: &serde_json::Value, plan_kind: &str) -> bool {
    comp_steps_values(comp).iter().any(|n| {
        n.get("kind").and_then(|k| k.as_str()) == Some("invoke")
            && n.get("plan_kind").and_then(|k| k.as_str()) == Some(plan_kind)
    })
}

pub(crate) fn matches_for_each_action_node(nr: &serde_json::Value) -> bool {
    (nr.get("kind").and_then(|k| k.as_str()) == Some("for_each")
        || nr.get("kind").and_then(|k| k.as_str()) == Some("flat_map_effect"))
        && nr.pointer("/effect_template/kind").and_then(|k| k.as_str()) == Some("action")
}

pub(crate) fn assert_for_each_action_node(
    dry: &DryPlasmPlanEvaluation,
    comp: &serde_json::Value,
) -> Result<(), String> {
    let dry_ok = dry.node_results.iter().any(matches_for_each_action_node);
    let plan_ok = comp_steps_values(comp)
        .iter()
        .any(|n| matches_for_each_action_node(n));
    if !dry_ok && !plan_ok {
        return Err(
            "expected `for_each` with effect_template.kind action (invoke/update/action surface)"
                .into(),
        );
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]

pub(crate) fn expr_chain_selects_lines(e: &Expr) -> bool {
    chain_selector_matches(e, "lines")
}

pub(crate) fn expr_chain_selects_tags(e: &Expr) -> bool {
    chain_selector_matches(e, "tags")
}

pub(crate) fn chain_selector_matches(e: &Expr, want_sel: &str) -> bool {
    match e {
        Expr::Chain(c) if c.selector == want_sel => true,
        Expr::Chain(c) => {
            chain_selector_matches(&c.source, want_sel)
                || matches!(
                    &c.step,
                    ChainStep::Explicit { expr } if chain_selector_matches(expr, want_sel)
                )
        }
        _ => false,
    }
}

pub(crate) fn get_simple_id(g: &GetExpr) -> Option<&str> {
    match &g.reference.key {
        EntityKey::Simple(id) => id.as_lit_str(),
        EntityKey::Compound(_) => None,
    }
}

pub(crate) fn expr_contains_get_langitem(e: &Expr, want_id: Option<&str>) -> bool {
    match e {
        Expr::Get(g) if g.reference.entity_type == "LangItem" => {
            want_id.is_none_or(|id| get_simple_id(g) == Some(id))
        }
        Expr::Chain(c) => expr_contains_get_langitem(&c.source, want_id),
        _ => false,
    }
}

pub(crate) fn expr_mentions_langline(e: &Expr) -> bool {
    match e {
        Expr::Query(q) => q.entity == "LangLine",
        Expr::Chain(c) => {
            expr_mentions_langline(&c.source)
                || matches!(
                    &c.step,
                    ChainStep::Explicit { expr } if expr_mentions_langline(expr)
                )
        }
        _ => false,
    }
}

