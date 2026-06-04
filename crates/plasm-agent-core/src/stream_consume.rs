//! Host policy for [`plasm_runtime::StreamConsumeOpts`] on catalog reads.
//!
//! Paginated list/search reads in programs default to **all pages** unless the plan node sets
//! [`page_size`](crate::plasm_plan::ValidatedSurfaceNode::page_size) (first-page + cap) or the
//! agent uses MCP `page(pgN)` continuations (explicit resume path in [`crate::http_execute`]).
//! Row-level `.limit(n)` runs after materialization in the plan compute chain.

use plasm_compile::{parse_capability_template, template_pagination};
use plasm_core::{resolve_query_capability, ChainStep, Expr, QueryExpr, CGS};
use plasm_runtime::StreamConsumeOpts;

/// Consumption policy for a catalog read expression executed by the host.
pub(crate) fn stream_consume_for_read(
    cgs: &CGS,
    expr: &Expr,
    host_page_size: Option<usize>,
) -> StreamConsumeOpts {
    if host_page_size.is_some() {
        return StreamConsumeOpts::default();
    }
    if expr_has_paginated_query(cgs, expr) {
        StreamConsumeOpts {
            fetch_all: true,
            max_items: None,
            one_page: false,
        }
    } else {
        StreamConsumeOpts::default()
    }
}

fn expr_has_paginated_query(cgs: &CGS, expr: &Expr) -> bool {
    match expr {
        Expr::Query(q) => query_has_pagination(cgs, q),
        Expr::Chain(ch) => {
            expr_has_paginated_query(cgs, &ch.source)
                || matches!(
                    &ch.step,
                    ChainStep::Explicit { expr: step } if expr_has_paginated_query(cgs, step)
                )
        }
        _ => false,
    }
}

fn query_has_pagination(cgs: &CGS, q: &QueryExpr) -> bool {
    let capability = match resolve_query_capability(q, cgs) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let template = match parse_capability_template(&capability.mapping.template) {
        Ok(t) => t,
        Err(_) => return false,
    };
    template_pagination(&template).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{EntityName, QueryExpr};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn paginated_query_defaults_to_fetch_all_without_page_size_cap() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = plasm_core::load_schema(&root.join("../../fixtures/schemas/pokeapi_mini"))
            .expect("pokeapi_mini cgs");
        let cgs = Arc::new(cgs);
        let q = QueryExpr {
            entity: EntityName::new("Berry"),
            predicate: None,
            projection: None,
            pagination: None,
            hydrate: None,
            capability_name: None,
            catalog_entry_id: None,
        };
        let consume = stream_consume_for_read(cgs.as_ref(), &Expr::Query(q), None);
        assert!(consume.fetch_all);
        assert!(!consume.one_page);
    }

    #[test]
    fn host_page_size_disables_fetch_all() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = plasm_core::load_schema(&root.join("../../fixtures/schemas/pokeapi_mini"))
            .expect("pokeapi_mini cgs");
        let cgs = Arc::new(cgs);
        let q = QueryExpr {
            entity: EntityName::new("Berry"),
            predicate: None,
            projection: None,
            pagination: None,
            hydrate: None,
            capability_name: None,
            catalog_entry_id: None,
        };
        let consume = stream_consume_for_read(cgs.as_ref(), &Expr::Query(q), Some(10));
        assert!(!consume.fetch_all);
    }
}
