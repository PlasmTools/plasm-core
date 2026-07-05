//! Plan JSON serialization, template uses, surface contract inference.

mod template_uses;
mod parse_helpers;
mod surface_infer;
mod plan_emit;

pub(in crate::plasm_dag) use template_uses::{
    collect_expr_for_template_uses, collect_predicate_for_template_uses,
    collect_template_uses_from_expr, collect_value_for_template_uses, dedupe_inputs, dedupe_uses,
    relation_plan_uses_result,
};
pub(in crate::plasm_dag) use parse_helpers::{
    parse_aggregates, parse_dedupe_key_paths, parse_field_list, parse_group_by_key_and_aggregate_tail,
    parse_literal, parse_one_aggregate_spec, parse_plan_value_expr, parse_sort_direction_token,
    parse_sort_field_and_direction,
};
pub(in crate::plasm_dag) use surface_infer::{
    infer_surface_contract, infer_surface_contract_from_expr, looks_like_plasm_effect_template,
    schema_from_aggregates, schema_from_group_by, schema_from_output_fields, single_unknown_schema,
};
pub(in crate::plasm_dag) use plan_emit::{emit_plan_json_for_source, expr_template_json, node_to_json};
