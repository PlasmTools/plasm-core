//! Python Program compilation into a typed, reviewed rowset DAG.

mod binding_continuation;
mod binding_contract;
pub(crate) mod error;
mod invoke_cardinality;
pub(crate) mod plan_serialize;
mod prelude;
mod prerequisite_seats;
pub(crate) mod relation;
pub(crate) mod row_suffix;
mod scalar_extract;
pub(crate) mod schema_validate;
mod types;
pub(crate) mod view_embed_proof;

#[path = "../plasm_render_dag.rs"]
mod render_dag;

mod pipeline;
mod python;
pub use python::admission::DeclarationKind as PythonDeclaration;
pub use python::build_statements::BuildStatementKind as PythonBuildStatement;
pub use python::catalog_operations::CatalogOperation as PythonCatalogOperation;
pub(crate) use python::compile_python_program_checked;
pub use python::quantifiers::QuantifierOperation as PythonQuantifierOperation;
pub use python::reductions::AggregateDescriptor as PythonAggregateDescriptor;
pub use python::relation_operations::RelationOperation as PythonRelationOperation;
pub use python::row_operations::RowOperation as PythonRowOperation;

// --- crate-visible entrypoints ---
#[allow(unused_imports)]
pub(crate) use pipeline::{
    compile_plasm_dag_to_plan_inner, compile_plasm_surface_line_to_plan, is_plasm_dag_candidate,
    is_plasm_dag_source,
};

#[cfg(test)]
pub(crate) use pipeline::{compile_plasm_dag_to_plan, compile_surface_fixture_json};

// --- in-module re-exports for submodules + integration tests (`use super::*`) ---
#[allow(unused_imports)]
pub(crate) use crate::execute_session::ExecuteSession;
#[allow(unused_imports)]
pub(in crate::plasm_dag) use crate::plasm_plan::QualifiedEntityKey;
#[allow(unused_imports)]
pub(in crate::plasm_dag) use binding_continuation::dispatch_binding_continuation;
#[allow(unused_imports)]
pub(in crate::plasm_dag) use binding_contract::binding_contract;
#[allow(unused_imports)]
pub(in crate::plasm_dag) use pipeline::{
    compile_node_expr, longest_matching_bound_prefix, relation_wire_names_for_source, require_node,
    split_return_list,
};
#[allow(unused_imports)]
pub(in crate::plasm_dag) use plan_serialize::parse_aggregates;
#[allow(unused_imports)]
pub(in crate::plasm_dag) use plasm_core::expr_parser::{
    collect_program_statement_lines, split_top_level,
};
#[allow(unused_imports)]
pub(in crate::plasm_dag) use relation::{
    lookup_relation_chain_meta, resolve_relation_segment_for_continuation,
    resolve_relation_wire_on_entity,
};
#[allow(unused_imports)]
pub(in crate::plasm_dag) use row_suffix::row_suffix_to_compute;
pub(crate) use row_suffix::RowSuffixLoweringError;
#[allow(unused_imports)]
pub(in crate::plasm_dag) use types::{
    CompileState, DagNode, DagNodeSource, ExpandedProgramSurface,
};

#[cfg(test)]
mod tests {
    include!("tests/integration.rs");
}
