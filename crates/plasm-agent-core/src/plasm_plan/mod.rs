//! Serializable effect **`Plan`** contract (JSON DAG).
//!
//! Plasm programs and other hosts compile to this shape; the runtime validates the DAG, runs dry
//! reviews, and executes when requested.

mod cardinality;
mod types;
mod validate;

pub(crate) use cardinality::validated_source_is_static_singleton;
pub use types::*;
pub use validate::*;
