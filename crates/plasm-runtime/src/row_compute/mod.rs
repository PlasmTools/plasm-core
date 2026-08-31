//! Polars adapter for fused [`plasm_core::RowPlan`] execute.
//!
//! Public types here do not re-export `polars::*`. [`ComputeOp`] stays the hashed constructor;
//! this module is the only physical engine.

mod adapter;
mod aggregates;
mod eval;
mod json_frame;
mod plan_apply;
mod predicates;
mod with_expr;

pub use adapter::PolarsAdapter;
pub use eval::{eval_compute_ops, ComputeEvalOutcome};
