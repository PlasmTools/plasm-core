//! Row-suffix transform lowering (`| where`, `| take`, typed [`RowSuffix`] streams).

mod stream;
mod to_compute;

#[allow(unused_imports)] // barrel re-exports for sibling `plasm_dag` modules
pub(in crate::plasm_dag) use stream::{
    coalesce_group_by_aggregate_suffixes, compile_state_with_nodes, decompose_row_suffix_stream,
    lower_suffix_stream, try_lower_row_suffix_expression,
};
pub(in crate::plasm_dag) use to_compute::row_suffix_to_compute;
