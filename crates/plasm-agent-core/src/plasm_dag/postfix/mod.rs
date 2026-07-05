//! Postfix transform lowering (`.limit`, `.filter`, row suffix streams).

mod postfix_op;
mod row_suffix;

pub(in crate::plasm_dag) use postfix_op::postfix_op_to_compute;
pub(in crate::plasm_dag) use row_suffix::{
    coalesce_group_by_aggregate_suffixes, compile_state_with_nodes, decompose_row_suffix_stream,
    lower_row_expression, lower_suffix_stream, row_suffix_to_postfix, try_lower_row_suffix_expression,
};
