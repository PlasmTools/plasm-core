//! Short human-readable [`Expr`] line (REPL `→ …` hint, MCP tool text).

use plasm_core::cgs_federation::FederationDispatch;
use plasm_core::resolve_query_capability;
use plasm_core::Expr;
use plasm_core::CGS;

/// Compact one-line `il` (no `cap=` for queries that depend on resolution).
pub fn expr_display(expr: &Expr) -> String {
    match expr {
        Expr::Get(g) => format!("Get({})", g.reference),
        Expr::Query(q) => {
            let pred = if q.predicate.is_some() {
                "filtered"
            } else {
                "all"
            };
            let cap = q
                .capability_name
                .as_deref()
                .map(|c| format!(" cap={c}"))
                .unwrap_or_default();
            format!("Query({} {}){cap}", q.entity, pred)
        }
        Expr::Chain(c) => {
            format!("Chain({} .{})", expr_display(&c.source), c.selector)
        }
        Expr::Create(c) => format!("Create({})", c.entity),
        Expr::Delete(d) => format!("Delete({})", d.target),
        Expr::Invoke(i) => format!("Invoke({} {})", i.capability, i.target.entity_type),
        Expr::Page(p) => match p.limit {
            Some(l) => format!("page({} limit={l})", p.handle),
            None => format!("page({})", p.handle),
        },
        Expr::Wait(w) => format!("wait({})", w.handle),
        Expr::Cancel(c) => format!("cancel({})", c.handle),
        Expr::TeachingValue { value } => match value {
            plasm_core::Value::UnionCtor { ctor_label, .. } => {
                format!("TeachingValue({ctor_label}{{…}})")
            }
            _ => "TeachingValue".to_string(),
        },
    }
}

/// Like [`expr_display`], but **`Query(… )` always includes `cap=…`** when the schema
/// resolves a primary query/search (`resolve_query_capability`), or when `capability_name`
/// is already set.
pub fn expr_display_resolved(expr: &Expr, cgs: &CGS) -> String {
    match expr {
        Expr::Get(g) => format!("Get({})", g.reference),
        Expr::Query(q) => {
            let pred = if q.predicate.is_some() {
                "filtered"
            } else {
                "all"
            };
            let cap = cap_suffix_for_query(q, cgs);
            format!("Query({} {}){cap}", q.entity, pred)
        }
        Expr::Chain(c) => format!(
            "Chain({} .{})",
            expr_display_resolved(&c.source, cgs),
            c.selector
        ),
        Expr::Create(c) => format!("Create({})", c.entity),
        Expr::Delete(d) => format!("Delete({})", d.target),
        Expr::Invoke(i) => format!("Invoke({} {})", i.capability, i.target.entity_type),
        Expr::Page(p) => match p.limit {
            Some(l) => format!("page({} limit={l})", p.handle),
            None => format!("page({})", p.handle),
        },
        Expr::Wait(w) => format!("wait({})", w.handle),
        Expr::Cancel(c) => format!("cancel({})", c.handle),
        Expr::TeachingValue { value } => match value {
            plasm_core::Value::UnionCtor { ctor_label, .. } => {
                format!("TeachingValue({ctor_label}{{…}})")
            }
            _ => "TeachingValue".to_string(),
        },
    }
}

/// Per-entity [`CGS`] (federation) for resolving the same `cap=` rule as
/// [`plasm_core::summary_render::render_intent_federated`].
pub fn expr_display_resolved_federated(
    expr: &Expr,
    fed: &FederationDispatch,
    fallback: &CGS,
) -> String {
    match expr {
        Expr::Get(g) => format!("Get({})", g.reference),
        Expr::Query(q) => {
            let pred = if q.predicate.is_some() {
                "filtered"
            } else {
                "all"
            };
            let cgs = fed
                .resolve_entity(
                    q.entity.as_str(),
                    plasm_core::ResolutionHint::default(),
                    fallback,
                )
                .unwrap_or(fallback);
            let cap = cap_suffix_for_query(q, cgs);
            format!("Query({} {}){cap}", q.entity, pred)
        }
        Expr::Chain(c) => format!(
            "Chain({} .{})",
            expr_display_resolved_federated(&c.source, fed, fallback),
            c.selector
        ),
        Expr::Create(c) => format!("Create({})", c.entity),
        Expr::Delete(d) => format!("Delete({})", d.target),
        Expr::Invoke(i) => format!("Invoke({} {})", i.capability, i.target.entity_type),
        Expr::Page(p) => match p.limit {
            Some(l) => format!("page({} limit={l})", p.handle),
            None => format!("page({})", p.handle),
        },
        Expr::Wait(w) => format!("wait({})", w.handle),
        Expr::Cancel(c) => format!("cancel({})", c.handle),
        Expr::TeachingValue { value } => match value {
            plasm_core::Value::UnionCtor { ctor_label, .. } => {
                format!("TeachingValue({ctor_label}{{…}})")
            }
            _ => "TeachingValue".to_string(),
        },
    }
}

fn cap_suffix_for_query(q: &plasm_core::QueryExpr, cgs: &CGS) -> String {
    if let Some(ref n) = q.capability_name {
        return format!(" cap={n}");
    }
    resolve_query_capability(q, cgs)
        .ok()
        .map(|c| format!(" cap={}", c.name))
        .unwrap_or_default()
}

#[cfg(test)]
mod il_snapshots {
    use super::expr_display;
    use super::expr_display_resolved;
    use super::expr_display_resolved_federated;
    use plasm_core::expr_parser::parse;
    use plasm_core::load_schema;
    use std::path::PathBuf;

    fn tiny_cgs() -> plasm_core::CGS {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        load_schema(&root.join("tests/fixtures/execute_tiny")).expect("load execute_tiny")
    }

    #[test]
    fn il_query_product_resolved_includes_cap() {
        let cgs = tiny_cgs();
        let pe = parse("Product", &cgs).expect("parse");
        let raw = expr_display(&pe.expr);
        let res = expr_display_resolved(&pe.expr, &cgs);
        assert_eq!(raw, "Query(Product all)");
        assert_eq!(res, "Query(Product all) cap=product_list");
        assert!(res.contains("cap=product_list"), "{res}");
    }

    #[test]
    fn il_get_product() {
        let cgs = tiny_cgs();
        let pe = parse(r#"Product("p1")"#, &cgs).expect("parse");
        assert_eq!(expr_display_resolved(&pe.expr, &cgs), "Get(Product:p1)");
    }

    #[test]
    fn il_query_product_federated_matches_single_catalog() {
        use plasm_core::CgsContext;
        use plasm_core::FederationDispatch;
        use plasm_core::TeachingExposureSession;
        use std::sync::Arc;

        let cgs = std::sync::Arc::new(tiny_cgs());
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "acme".to_string(),
            Arc::new(CgsContext::entry("acme", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "acme", &["Product"]);
        let fed = FederationDispatch::from_contexts_and_exposure(ctxs, &exp);
        let pe = parse("Product", cgs.as_ref()).expect("parse");
        let s = expr_display_resolved_federated(&pe.expr, &fed, cgs.as_ref());
        assert_eq!(s, "Query(Product all) cap=product_list");
    }
}
