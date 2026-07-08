//! External crate imports shared by `plasm_dag` submodules.

pub(in crate::plasm_dag) use crate::execute_session::ExecuteSession;
pub(in crate::plasm_dag) use crate::plasm_dag_surface_guards::{
    content_reference_error, looks_like_data_literal, reject_bare_literal_noop_root,
    reject_derive_map_invalid_rhs, reject_relation_arrow_trap, ContentReferenceSite,
};
pub(in crate::plasm_dag) use crate::plasm_plan::{
    AggregateFunction, ComputeOp, EffectClass, FieldPath, OutputName, PlanExprIr, PlanNodeKind,
    PlanRelationTraversal, PlanValue, QualifiedEntityKey, RelationCardinality,
    RelationSourceCardinality, SyntheticFieldSchema, SyntheticResultSchema, SyntheticValueKind,
};
pub(in crate::plasm_dag) use crate::plasm_plan_run::{
    parse_plasm_program_surface_for_dag, symbol_map_for_plasm_surface_parse,
};
pub(in crate::plasm_dag) use crate::plasm_render_compile::{
    parse_field_list_with_tokens, render_plan_graph_edges,
};
pub(in crate::plasm_dag) use crate::program_binding::{
    BoundedSingletonKind, ContinuationAnchor, ContinuationCapability, ProgramBindingContract,
    RowCardinalityProof, SegmentPolicy,
};
pub(in crate::plasm_dag) use plasm_core::expr_parser::{
    collect_program_statement_lines, expand_flattened_program_statements,
    missing_program_roots_error, peel_postfix_suffixes, program_duplicate_return_node_error,
    program_empty_error, program_return_keyword_error, split_assignment_at_top_level,
    split_token_top_level, split_top_level, strip_line_comment, try_parse_render_tail,
    validate_program_label, validate_program_statement_order, PlasmPostfixOp,
};
pub(in crate::plasm_dag) use plasm_core::query_resolve;
pub(in crate::plasm_dag) use plasm_core::row_composition::RowSuffix;
pub(in crate::plasm_dag) use plasm_core::schema::{CapabilitySchema, EntityDef, InputType};
pub(in crate::plasm_dag) use plasm_core::{
    CapabilityKind, ChainExpr, ChainStep, EntityKey, Expr, GetExpr, PlasmInputRef, Predicate,
    PromptPipelineConfig, Ref, SymbolMapCrossRequestCache, Value,
};
pub(in crate::plasm_dag) use serde_json::json;
pub(in crate::plasm_dag) use std::cell::RefCell;
pub(in crate::plasm_dag) use std::collections::{BTreeMap, BTreeSet};
pub(in crate::plasm_dag) use std::ops::Deref;
pub(in crate::plasm_dag) use std::sync::Arc;
