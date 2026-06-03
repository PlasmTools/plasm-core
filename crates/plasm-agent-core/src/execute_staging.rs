//! Staged execute scheduling for HTTP/MCP: only consecutive pure [`plasm_core::Expr::Query`]
//! lines without top-level projection enrichment may share a parallel stage (fork snapshot +
//! `join_all` + ordered `merge_from_graph`). Everything else runs sequentially so
//! in-request cache dependencies remain observable.

#![allow(dead_code)] // parallel stage helpers wired when HTTP execute adopts staging

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::Expr;

/// A line may share a parallel query stage iff it is a root `Query` and does not request
/// post-hoc projection enrichment (`ParsedExpr.projection`), which consults the session graph.
#[must_use]
pub fn line_may_share_parallel_query_stage(parsed: &ParsedExpr) -> bool {
    if parsed.projection.as_ref().is_some_and(|p| !p.is_empty()) {
        return false;
    }
    matches!(parsed.expr, Expr::Query(_))
}

/// Group consecutive parallel-safe line indices into [`ExecuteStage::Parallel`] when the group has
/// at least two lines; single parallel-safe lines use [`ExecuteStage::Sequential`] (same semantics,
/// simpler execution path).
#[must_use]
pub fn build_execute_stages(parallel_safe: &[bool]) -> Vec<ExecuteStage> {
    let n = parallel_safe.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if parallel_safe[i] {
            let start = i;
            while i < n && parallel_safe[i] {
                i += 1;
            }
            let idxs: Vec<usize> = (start..i).collect();
            if idxs.len() >= 2 {
                out.push(ExecuteStage::Parallel(idxs));
            } else {
                out.push(ExecuteStage::Sequential(idxs[0]));
            }
        } else {
            out.push(ExecuteStage::Sequential(i));
            i += 1;
        }
    }
    out
}

/// One execution unit: either a single line index or a parallel group (fork-merge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteStage {
    Sequential(usize),
    Parallel(Vec<usize>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_stages_all_sequential_flags_yield_sequential_stages() {
        let s = build_execute_stages(&[false, false, false]);
        assert_eq!(
            s,
            vec![
                ExecuteStage::Sequential(0),
                ExecuteStage::Sequential(1),
                ExecuteStage::Sequential(2),
            ]
        );
    }

    #[test]
    fn build_stages_two_parallel_merges() {
        let s = build_execute_stages(&[true, true]);
        assert_eq!(s, vec![ExecuteStage::Parallel(vec![0, 1])]);
    }

    #[test]
    fn build_stages_single_parallel_safe_is_sequential() {
        let s = build_execute_stages(&[true]);
        assert_eq!(s, vec![ExecuteStage::Sequential(0)]);
    }

    #[test]
    fn build_stages_mixed_parallel_runs_and_barriers() {
        let s = build_execute_stages(&[true, true, false, true, true]);
        assert_eq!(
            s,
            vec![
                ExecuteStage::Parallel(vec![0, 1]),
                ExecuteStage::Sequential(2),
                ExecuteStage::Parallel(vec![3, 4]),
            ]
        );
    }

    #[test]
    fn parallel_safe_root_query_without_projection() {
        use plasm_core::expr_parser::ParsedExpr;
        use plasm_core::{Expr, QueryExpr};
        let p = ParsedExpr {
            expr: Expr::Query(QueryExpr::all("Pet")),
            projection: None,
        };
        assert!(line_may_share_parallel_query_stage(&p));
    }

    #[test]
    fn not_parallel_safe_with_top_level_projection_enrichment() {
        use plasm_core::expr_parser::ParsedExpr;
        use plasm_core::{Expr, QueryExpr};
        let p = ParsedExpr {
            expr: Expr::Query(QueryExpr::all("Pet")),
            projection: Some(vec!["name".into()]),
        };
        assert!(!line_may_share_parallel_query_stage(&p));
    }

    #[test]
    fn not_parallel_safe_non_query_root() {
        use plasm_core::expr_parser::ParsedExpr;
        use plasm_core::{Expr, GetExpr, Ref};
        let p = ParsedExpr {
            expr: Expr::Get(GetExpr {
                reference: Ref::new("Pet", "1"),
                path_vars: None,
                catalog_entry_id: None,
            }),
            projection: None,
        };
        assert!(!line_may_share_parallel_query_stage(&p));
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    fn flatten_stage_indices_in_order(stages: &[ExecuteStage]) -> Vec<usize> {
        let mut out = Vec::new();
        for s in stages {
            match s {
                ExecuteStage::Sequential(i) => out.push(*i),
                ExecuteStage::Parallel(v) => out.extend_from_slice(v),
            }
        }
        out
    }

    proptest! {
        #[test]
        fn build_execute_stages_partition_ordered_and_parallel_arity(
            flags in prop::collection::vec(any::<bool>(), 0..=128)
        ) {
            let stages = build_execute_stages(&flags);
            let n = flags.len();
            if n == 0 {
                prop_assert!(stages.is_empty());
                return Ok(());
            }
            let ordered = flatten_stage_indices_in_order(&stages);
            let expected: Vec<usize> = (0..n).collect();
            prop_assert_eq!(ordered, expected);

            for s in &stages {
                if let ExecuteStage::Parallel(v) = s {
                    prop_assert!(v.len() >= 2);
                }
            }
        }

        #[test]
        fn line_may_share_expected(case in 0usize..3) {
            use plasm_core::expr_parser::ParsedExpr;
            use plasm_core::{Expr, GetExpr, QueryExpr, Ref};
            let (expected, p) = match case {
                0 => (
                    true,
                    ParsedExpr {
                        expr: Expr::Query(QueryExpr::all("Pet")),
                        projection: None,
                    },
                ),
                1 => (
                    false,
                    ParsedExpr {
                        expr: Expr::Query(QueryExpr::all("Pet")),
                        projection: Some(vec!["name".into()]),
                    },
                ),
                2 => (
                    false,
                    ParsedExpr {
                        expr: Expr::Get(GetExpr {
                            reference: Ref::new("Pet", "1"),
                            path_vars: None,
                            catalog_entry_id: None,
                        }),
                        projection: None,
                    },
                ),
                _ => unreachable!(),
            };
            prop_assert_eq!(line_may_share_parallel_query_stage(&p), expected);
        }
    }
}
