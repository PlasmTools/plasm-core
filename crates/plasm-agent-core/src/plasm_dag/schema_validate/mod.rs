//! Field-path validation and compute schema guards.

mod catalog;
mod compute_schema;
mod dag_lookup;
mod path_validate;

#[allow(unused_imports)] // barrel re-exports for sibling `plasm_dag` modules
pub(in crate::plasm_dag) use catalog::{
    agent_program_error, capability_for_surface_expr, capability_input_param_wires,
    cgs_for_qualified_entity, infer_entity_row_columns, is_opaque_passthrough_compute_schema,
    logical_row_field_paths_for_entity, logical_row_field_paths_from_names,
    resolve_compute_field_path, resolve_sort_field_path, row_contract_field_error,
    single_segment_teaching_field_hint,
};
#[allow(unused_imports)]
pub(in crate::plasm_dag) use compute_schema::{
    compute_passthrough_or_fallback_schema, infer_render_columns_for_node,
    passthrough_identity_projection_fields, synthetic_schema_passthrough_rows,
};
#[allow(unused_imports)]
pub(in crate::plasm_dag) use dag_lookup::{
    logical_row_field_paths_for_surface_node, lookup_dag_node, resolve_immediate_compute_schema,
    resolve_qualified_entity_for_dag_source, resolve_surface_dag_node,
};
#[allow(unused_imports)]
pub(in crate::plasm_dag) use path_validate::{
    validate_compute_paths_for_allowed_set, validate_compute_paths_for_dag_source,
    validate_compute_paths_for_entity, validate_compute_paths_for_schema,
    validate_surface_inline_projection,
};
