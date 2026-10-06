//! External crate imports shared by `plasm_dag` submodules.

pub(in crate::plasm_dag) use crate::execute_session::ExecuteSession;
pub(in crate::plasm_dag) use crate::plasm_dag_surface_guards::{
    looks_like_data_literal, reject_bare_literal_noop_root, reject_derive_map_invalid_rhs,
    reject_relation_arrow_trap,
};
pub(in crate::plasm_dag) use crate::plasm_plan::{
    AggregateFunction, ComputeOp, EffectClass, FieldPath, OutputName, PlanExprIr, PlanNodeKind,
    PlanPredicate, PlanPredicateOp, PlanRelationTraversal, PlanValue, QualifiedEntityKey,
    RelationCardinality, RelationSourceCardinality, SyntheticFieldSchema, SyntheticResultSchema,
    SyntheticValueKind,
};
pub(in crate::plasm_dag) use crate::plasm_plan_run::{
    parse_plasm_program_surface_for_dag, symbol_map_for_plasm_surface_parse,
};
pub(in crate::plasm_dag) use crate::plasm_render_compile::{
    parse_field_list_with_tokens, render_plan_graph_edges,
};
pub(in crate::plasm_dag) use crate::program_binding::{
    BindingValueKind, BoundedSingletonKind, ContinuationAnchor, ContinuationCapability,
    ProgramBindingContract, RowCardinalityProof, SegmentPolicy,
};
pub(in crate::plasm_dag) use plasm_core::expr_parser::{
    classify_top_level_assignment, collect_program_statement_lines,
    expand_flattened_program_statements, is_valid_program_label, parse_expr_node, parse_pipe_expr,
    peel_collect_meta, pipe_head_has_catalog_surface_syntax, split_assignment_at_top_level,
    split_top_level, strip_line_comment, validate_program_statement_order, Applicator, ExprNode,
    PipeExpr, RenderApplicator, RowExpr, TopLevelAssignment,
};
pub(in crate::plasm_dag) use plasm_core::query_resolve;
pub(in crate::plasm_dag) use plasm_core::row_composition::RowSuffix;
pub(in crate::plasm_dag) use plasm_core::schema::{CapabilitySchema, EntityDef};
pub(in crate::plasm_dag) use plasm_core::{
    CapabilityKind, ChainExpr, Expr, GetExpr, PlasmInputRef, PromptPipelineConfig, Ref,
    SymbolMapCrossRequestCache,
};
pub(in crate::plasm_dag) use std::cell::RefCell;
pub(in crate::plasm_dag) use std::collections::{BTreeMap, BTreeSet};
pub(in crate::plasm_dag) use std::ops::Deref;
pub(in crate::plasm_dag) use std::sync::Arc;
