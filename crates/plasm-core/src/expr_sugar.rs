//! Agent-ergonomics rewrites on parsed [`Expr`] trees.

use crate::expr::{Expr, GetExpr, QueryExpr, Ref};
use crate::predicate::Predicate;
use crate::schema::{CapabilityKind, CGS};
use crate::typed_literal::TypedComparisonValue;
use crate::value::Value;
use crate::CompOp;

/// When `Entity{id_field=value}` has only a single equality on the entity's `id_field` and the
/// catalog exposes Get but brace filters would route to Search with invalid filter keys, rewrite
/// to `Entity(value)` (Get).
pub fn rewrite_id_field_brace_query_to_get(expr: Expr, cgs: &CGS) -> Expr {
    match expr {
        Expr::Query(q) => {
            if let Some(get) = try_brace_query_to_get(&q, cgs) {
                Expr::Get(get)
            } else {
                Expr::Query(q)
            }
        }
        Expr::Chain(ch) => {
            let source = rewrite_id_field_brace_query_to_get(*ch.source, cgs);
            Expr::Chain(crate::expr::ChainExpr {
                source: Box::new(source),
                selector: ch.selector,
                catalog_entry_id: ch.catalog_entry_id,
                step: ch.step,
            })
        }
        other => other,
    }
}

fn try_brace_query_to_get(q: &QueryExpr, cgs: &CGS) -> Option<GetExpr> {
    if q.capability_name.is_some() {
        return None;
    }
    let ent = cgs.get_entity(q.entity.as_str())?;
    let pred = q.predicate.as_ref()?;
    let (field, value) = single_eq_field(pred)?;
    if field != ent.id_field.as_str() {
        return None;
    }
    if cgs
        .find_capabilities(&q.entity, CapabilityKind::Get)
        .is_empty()
    {
        return None;
    }
    if !pred_references_only_field(pred, ent.id_field.as_str()) {
        return None;
    }
    let id_str = predicate_value_to_string(&value)?;
    Some(GetExpr::from_ref(Ref::new(q.entity.clone(), id_str)))
}

fn pred_references_only_field(pred: &Predicate, field: &str) -> bool {
    match pred {
        Predicate::Comparison { field: f, .. } => f == field,
        Predicate::And { args } => args.iter().all(|p| pred_references_only_field(p, field)),
        Predicate::Or { args } => args.len() == 1 && pred_references_only_field(&args[0], field),
        Predicate::Not { .. } => false,
        Predicate::True | Predicate::False => false,
        Predicate::ExistsRelation { .. } => false,
    }
}

fn single_eq_field(pred: &Predicate) -> Option<(&str, Value)> {
    match pred {
        Predicate::Comparison {
            field,
            op: CompOp::Eq,
            value,
        } => Some((field.as_str(), typed_to_value(value))),
        Predicate::And { args } if args.len() == 1 => single_eq_field(&args[0]),
        _ => None,
    }
}

fn typed_to_value(v: &TypedComparisonValue) -> Value {
    v.to_value()
}

fn predicate_value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;

    #[test]
    fn rewrite_issue_identifier_brace_to_get() {
        let dir = std::path::Path::new("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered("Issue", Predicate::eq("identifier", "EVA-60"));
        let expr = rewrite_id_field_brace_query_to_get(Expr::Query(q), &cgs);
        match expr {
            Expr::Get(g) => assert_eq!(g.reference.primary_slot_str(), "EVA-60"),
            other => panic!("expected Get, got {other:?}"),
        }
    }
}
