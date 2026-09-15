//! Physical-line program staging and delimiter-aware splitting shared with DAG lowering.
//!
//! Multi-line Plasm programs must join tagged heredocs across physical lines before binding/root
//! splitting — same rules as structured parameter heredocs ([`super::heredoc_surface`]).

mod errors;
mod flatten;
mod labels;
mod order;
mod physical_lines;
mod split;

#[cfg(test)]
mod tests;

pub use errors::{
    missing_program_roots_error, program_binding_after_return_error,
    program_duplicate_return_node_error, program_empty_error, program_invalid_binding_label_error,
    program_return_keyword_error,
};
// Intermediate-return helpers stay crate-visible for order validation + unit tests.
#[allow(unused_imports)]
pub use errors::{
    program_intermediate_return_error, program_intermediate_return_must_be_binding_error,
    program_multiple_return_lines_error,
};
pub use flatten::{
    expand_flattened_program_statements, split_flattened_program_line, FlattenedProgram,
    FlattenedProgramLine,
};
pub use labels::{
    is_valid_program_label, looks_like_domain_symbol, pipe_head_has_catalog_surface_syntax,
    validate_pipe_head_syntax, validate_program_label,
};
pub use order::validate_program_statement_order;
pub use physical_lines::{
    collect_program_statement_lines, scan_physical_line_stmt_state, strip_line_comment,
    PhysicalLineStmtState,
};
pub use split::{
    classify_top_level_assignment, split_assignment_at_top_level, split_assignment_for_binding,
    split_token_top_level, split_top_level, TopLevelAssignment,
};
