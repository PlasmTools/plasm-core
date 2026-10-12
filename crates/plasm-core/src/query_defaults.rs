//! RA-15: CGS-authored selection/scope defaults flow into query scope; omission is diagnosed.

use crate::query_resolve::resolve_query_capability;
use crate::{
    CompOp, InputFieldSchema, Predicate, QueryExpr, TypeError, TypedComparisonValue, Value, CGS,
};

/// Inject authored CGS defaults into `query` selection/scope, or reject an omission.
///
/// `expression` is the surface text (or a reconstructed hint) so the diagnostic can
/// point at the query the agent wrote.
pub fn apply_required_selection_defaults(
    query: &mut QueryExpr,
    cgs: &CGS,
    expression: &str,
) -> Result<(), TypeError> {
    let Ok(cap) = resolve_query_capability(query, cgs) else {
        return Ok(());
    };
    let present = selection_fields_present(query.predicate.as_ref());
    let mut injected = Vec::new();
    for field in cap
        .selection_params()
        .iter()
        .chain(cap.scope_params().iter())
    {
        if !field.required || present.contains(field.name.as_str()) {
            continue;
        }
        match &field.default {
            Some(default) => injected.push(comparison_from_default(field, default.clone())?),
            None => {
                return Err(TypeError::RequiredParameterOmitted {
                    parameter: field.name.clone(),
                    expression: expression.trim().to_string(),
                });
            }
        }
    }
    if injected.is_empty() {
        return Ok(());
    }
    query.predicate = Some(merge_predicates(query.predicate.take(), injected));
    Ok(())
}

/// Walk an expression tree and apply RA-15 defaults (or reject omission).
pub fn apply_required_selection_defaults_in_expr(
    expr: &mut crate::Expr,
    cgs: &CGS,
    expression: &str,
) -> Result<(), TypeError> {
    expr.try_for_each_leaf_mut(|leaf| {
        if let crate::Expr::Query(query) = leaf {
            apply_required_selection_defaults(query, cgs, expression)
        } else {
            Ok(())
        }
    })
}

fn selection_fields_present(pred: Option<&Predicate>) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    collect_comparison_fields(pred, &mut out);
    out
}

fn collect_comparison_fields(
    pred: Option<&Predicate>,
    out: &mut std::collections::BTreeSet<String>,
) {
    if let Some(pred) = pred {
        for node in pred.walk() {
            if let Predicate::Comparison { field, .. } = node {
                out.insert(field.clone());
            }
        }
    }
}

fn comparison_from_default(
    field: &InputFieldSchema,
    default: Value,
) -> Result<Predicate, TypeError> {
    Ok(Predicate::Comparison {
        field: field.name.clone(),
        op: CompOp::Eq,
        value: TypedComparisonValue::from_value(default),
    })
}

fn merge_predicates(existing: Option<Predicate>, mut injected: Vec<Predicate>) -> Predicate {
    match existing {
        None if injected.len() == 1 => injected.pop().expect("one injected"),
        None => Predicate::and(injected),
        Some(mut other) => {
            if let Predicate::And { args } = &mut other {
                args.extend(injected);
                return other;
            }
            let mut args = vec![other];
            args.extend(injected);
            Predicate::and(args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use std::path::Path;

    fn matrix_cgs() -> CGS {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        load_schema_dir(&dir).expect("language matrix cgs")
    }

    #[test]
    fn omits_required_shelf_without_inventing_default() {
        let cgs = matrix_cgs();
        assert!(
            cgs.get_entity("LangLane").is_some(),
            "LangLane must exist on the language-matrix fixture"
        );
        let mut q = QueryExpr::all("LangLane");
        let err = apply_required_selection_defaults(&mut q, &cgs, "LangLane")
            .expect_err("omission must diagnose");
        let msg = err.to_string();
        assert!(msg.contains("shelf"), "{msg}");
        assert!(msg.contains("LangLane"), "{msg}");
    }

    #[test]
    fn authored_default_fills_omitted_shelf() {
        let cgs = matrix_cgs();
        assert!(
            cgs.get_entity("LangLaneStock").is_some(),
            "LangLaneStock must exist on the language-matrix fixture"
        );
        let mut q = QueryExpr::all("LangLaneStock");
        apply_required_selection_defaults(&mut q, &cgs, "LangLaneStock").expect("default fills");
        let pred = q.predicate.expect("injected");
        let text = format!("{pred:?}");
        assert!(text.contains("shelf"), "{text}");
        assert!(text.contains("mine"), "{text}");
    }
}
