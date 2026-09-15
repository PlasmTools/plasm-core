//! Host policy for [`plasm_runtime::StreamConsumeOpts`] on catalog reads.

use crate::plan_read_bounds::{pushed_budget_to_stream_fields, PushedReadBudget};
use plasm_compile::{parse_capability_template, template_pagination};
use plasm_core::{resolve_query_capability, ChainStep, Expr, QueryExpr, CGS};
use plasm_runtime::StreamConsumeOpts;

/// Consumption policy when a validated surface may carry a pushed read budget.
pub(crate) fn stream_consume_for_surface_read(
    cgs: &CGS,
    expr: &Expr,
    host_page_size: Option<usize>,
    pushed_budget: Option<&PushedReadBudget>,
    graph_page_spill: bool,
) -> Result<StreamConsumeOpts, String> {
    if let Some(budget) = pushed_budget {
        if matches!(budget, PushedReadBudget::Complete) {
            return Ok(StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: graph_page_spill,
                row_match_budget: None,
                top_k: None,
                bound_kind: plasm_runtime::ConsumeBoundKind::None,
            });
        }
        let (row_match_budget, top_k) = pushed_budget_to_stream_fields(budget)?;
        if top_k.is_some() {
            return Ok(StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: graph_page_spill,
                row_match_budget,
                top_k,
                bound_kind: plasm_runtime::ConsumeBoundKind::ExpressionTake,
            });
        }
        if let Some(row_match_budget) = row_match_budget {
            return Ok(StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: graph_page_spill,
                row_match_budget: Some(row_match_budget),
                top_k: None,
                bound_kind: plasm_runtime::ConsumeBoundKind::ExpressionTake,
            });
        }
        if let PushedReadBudget::Limit(n) = budget {
            if host_page_size.is_some() || expr_has_paginated_query(cgs, expr) {
                return Ok(StreamConsumeOpts {
                    fetch_all: false,
                    max_items: Some(*n),
                    one_page: false,
                    graph_backed_result: graph_page_spill,
                    row_match_budget: None,
                    top_k: None,
                    bound_kind: plasm_runtime::ConsumeBoundKind::ExpressionTake,
                });
            }
        }
    }
    if host_page_size.is_some() {
        return Ok(StreamConsumeOpts {
            fetch_all: false,
            max_items: host_page_size,
            one_page: false,
            graph_backed_result: graph_page_spill,
            bound_kind: plasm_runtime::ConsumeBoundKind::HostPage,
            ..Default::default()
        });
    }
    Ok(if expr_has_paginated_query(cgs, expr) {
        StreamConsumeOpts {
            fetch_all: true,
            max_items: None,
            one_page: false,
            graph_backed_result: graph_page_spill,
            ..Default::default()
        }
    } else {
        StreamConsumeOpts::default()
    })
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
    let template = match capability
        .mapping
        .as_ref()
        .and_then(|m| parse_capability_template(&m.template).ok())
    {
        Some(t) => t,
        None => return false,
    };
    template_pagination(&template).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{EntityName, QueryExpr};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn pokeapi_cgs() -> Arc<CGS> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Arc::new(
            plasm_core::load_schema(&root.join("../../fixtures/schemas/pokeapi_mini"))
                .expect("pokeapi_mini cgs"),
        )
    }

    fn berry_query() -> Expr {
        Expr::Query(QueryExpr {
            entity: EntityName::new("Berry"),
            predicate: None,
            projection: None,
            pagination: None,
            hydrate: None,
            capability_name: None,
            catalog_entry_id: plasm_core::CatalogEntryStamp::none(),
        })
    }

    #[test]
    fn paginated_query_defaults_to_fetch_all_without_page_size_cap() {
        let cgs = pokeapi_cgs();
        let consume =
            stream_consume_for_surface_read(cgs.as_ref(), &berry_query(), None, None, false)
                .expect("consume");
        assert!(consume.fetch_all);
        assert!(!consume.one_page);
    }

    #[test]
    fn host_page_size_disables_fetch_all() {
        let cgs = pokeapi_cgs();
        let consume =
            stream_consume_for_surface_read(cgs.as_ref(), &berry_query(), Some(10), None, false)
                .expect("consume");
        assert!(!consume.fetch_all);
        assert_eq!(consume.max_items, Some(10));
        assert_eq!(
            consume.bound_kind,
            plasm_runtime::ConsumeBoundKind::HostPage
        );
    }

    #[test]
    fn pushed_limit_bounds_paginated_read() {
        let cgs = pokeapi_cgs();
        let budget = PushedReadBudget::Limit(5);
        let consume = stream_consume_for_surface_read(
            cgs.as_ref(),
            &berry_query(),
            None,
            Some(&budget),
            false,
        )
        .expect("consume");
        assert!(!consume.fetch_all);
        assert_eq!(consume.max_items, Some(5));
        assert_eq!(
            consume.bound_kind,
            plasm_runtime::ConsumeBoundKind::ExpressionTake
        );
    }
}
