//! Plasm **program** compiler: multi-line bindings, postfix transforms (`.limit`, `.sort`, …),
//! and final roots, lowered to the serialized program [`Plan`](crate::plasm_plan::Plan) IR consumed by [`crate::plasm_plan_run`].
//!
//! Surface path expressions ([`plasm_core::expr_parser`]) remain the leaf language; this module
//! stitches labels, postfix transforms, and `=>` derives into a single coherent program surface.

mod prelude;
mod types;
mod schema_validate;
mod plan_serialize;
mod postfix;
mod relation;
mod binding_contract;
mod binding_continuation;

#[path = "../plasm_render_dag.rs"]
mod render_dag;

mod pipeline;

// --- crate-visible entrypoints ---
pub(crate) use pipeline::{
    compile_plasm_dag_to_plan, compile_plasm_dag_to_plan_inner, compile_plasm_expression_to_plan,
    compile_plasm_surface_line_to_plan, is_plasm_dag_candidate, is_plasm_dag_source,
};

// --- in-module re-exports for submodules + integration tests (`use super::*`) ---
pub(in crate::plasm_dag) use binding_contract::binding_contract;
pub(in crate::plasm_dag) use binding_continuation::dispatch_binding_continuation;
pub(in crate::plasm_dag) use pipeline::{
    compile_node_expr, longest_matching_bound_prefix, relation_wire_names_for_source, require_node,
    rewrite_binding_field_projection_root, split_return_list,
};
pub(in crate::plasm_dag) use plan_serialize::{node_to_json, parse_aggregates};
pub(in crate::plasm_dag) use postfix::postfix_op_to_compute;
pub(in crate::plasm_dag) use relation::{
    lookup_relation_chain_meta, resolve_relation_segment_for_continuation, resolve_relation_wire_on_entity,
};
pub(in crate::plasm_dag) use types::{CompileState, DagNode, DagNodeSource, ExpandedProgramSurface};
pub(in crate::plasm_dag) use crate::plasm_plan::QualifiedEntityKey;
pub(in crate::plasm_dag) use plasm_core::expr_parser::{collect_program_statement_lines, split_top_level};
pub(crate) use crate::execute_session::ExecuteSession;

#[cfg(test)]
mod tests {
    include!("tests/integration.rs");
}
