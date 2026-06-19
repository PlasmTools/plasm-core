//! Post-materialize compute and template rendering.

mod compute_ops;
mod eval;
mod for_each;
mod relation;

pub(crate) use compute_ops::*;
pub(crate) use eval::*;
pub(crate) use for_each::*;
pub(crate) use relation::*;
