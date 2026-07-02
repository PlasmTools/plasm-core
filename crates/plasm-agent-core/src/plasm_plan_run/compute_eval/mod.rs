//! Post-materialize compute and template rendering.

mod compute_ops;
mod dry_staging;
mod eval;
mod for_each;
mod hole_paths;
mod input_rows;
mod relation;

pub(crate) use compute_ops::*;
pub(crate) use dry_staging::*;
pub(crate) use eval::*;
pub(crate) use for_each::*;
pub(crate) use hole_paths::NodeInputHoleIndex;
pub(crate) use input_rows::*;
pub(crate) use relation::*;
