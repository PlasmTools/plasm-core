//! Polars adapter for fused [`plasm_core::RowPlan`] execute.
//!
//! Public types here do not re-export `polars::*`. [`ComputeOp`] stays the hashed constructor;
//! this module is the only physical engine.

mod adapter;
mod eval;
mod expressions;
mod json_frame;
mod money;
mod nodes;

pub use adapter::PolarsAdapter;
pub use eval::{eval_compute_ops, ComputeEvalOutcome};
