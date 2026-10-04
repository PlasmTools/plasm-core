//! Serializable effect **`Plan`** contract (JSON DAG).
//!
//! Plasm programs and other hosts compile to this shape; the runtime validates the DAG, runs dry
//! reviews, and executes when requested.

mod cardinality;
mod compute_transfer;
mod types;
mod validate;

pub(crate) use cardinality::{
    scoped_capture_permits_singleton, validated_source_is_static_singleton,
};
pub(crate) use compute_transfer::{
    compute_cardinality_transfer, compute_result_shape, map_body_cardinality_transfer,
};
pub use types::*;
pub use validate::*;
