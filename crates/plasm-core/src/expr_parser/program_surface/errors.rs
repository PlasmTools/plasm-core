//! Stable program-surface error messages.

use super::labels::{looks_like_domain_symbol, reserved_program_label_pattern};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceSyntaxError {
    #[error("invalid UTF-8 boundary")]
    InvalidUtf8Boundary,
    #[error("unbalanced delimiters in `{expression}`")]
    UnbalancedDelimiters { expression: String },
    #[error("physical newline inside a quoted Plasm string parameter; use a tagged heredoc for multiline parameters (PLP-3)")]
    QuotedPhysicalNewline,
    #[error("tagged heredoc `<<` requires a tag ([A-Za-z_][A-Za-z0-9_]*) followed by a newline")]
    InvalidHeredocTag,
    #[error("tagged heredoc `<<{tag}` opener cannot contain text after the tag")]
    HeredocOpenerTrailingText { tag: String },
    #[error("incomplete tagged heredoc opener (missing newline after <<TAG)")]
    HeredocOpenerMissingNewline,
    #[error("unterminated tagged heredoc <<{tag}")]
    HeredocUnterminated { tag: String },
    #[error("heredoc close line `{line}` left pending tag `{tag}` in program staging")]
    HeredocCloseNotStaged { tag: String, line: String },
    #[error("heredoc close tag `{tag}` requires a close delimiter before trailing call arguments")]
    HeredocCloseDelimiterMissing { tag: String },
    #[error("heredoc body contains close tag `{tag}` before the real close; use an opaque tag")]
    HeredocTagCollision { tag: String },
    #[error("unterminated program statement (unbalanced delimiters after heredoc close)")]
    StatementDelimitersUnterminated,
    #[error("unterminated program statement (unexpected trailing fragment)")]
    StatementTrailingFragment,
    #[error("binding names must be identifiers like `issue`, not `{label}`")]
    InvalidBindingLabel { label: String },
    #[error("Return must be last — move bindings above the return line, or bind intermediate steps before returning.")]
    BindingAfterRoots,
    #[error("Only one return line allowed — put every root on one comma-separated line, not one bare label per line.")]
    MultipleRootLines,
    #[error("Only one return line allowed — bind intermediate steps (`done = rows => e#.m#(…, _.f)` or `label = …`), then end with one return line of roots.")]
    IntermediateRoots,
    #[error("Intermediate step must be a binding — write `{binding} = …`, then return on the last line.")]
    IntermediateStepRequiresBinding { binding: String },
    #[error("pipe head must not be empty")]
    EmptyPipeHead,
    #[error("unknown pipe head `{head}`; use a catalog source or binding label")]
    UnknownPipeHead { head: String },
    #[error("pipe stages must not be empty; use `head | stage`")]
    EmptyPipeStage,
    #[error("`from` is not Plasm syntax; write a catalog head before pipe stages")]
    FromKeyword,
    #[error("pipe stage requires {part}")]
    MissingStageArgument { part: String },
    #[error("`{stage}` requires a positive integer")]
    InvalidStageBound { stage: String },
    #[error("unknown pipe stage `{stage}`; use where, select, summarize, order by, take, distinct, or union")]
    UnknownPipeStage { stage: String },
    #[error("unknown pipe stage `{stage}`: this is a Minijinja filter, not row algebra; write it inside a template (`source => <<TAG` with `{{{{ value | filter }}}}`)")]
    TemplateFilterAsPipeStage { stage: String },
    #[error("select assignment `{item}` must be `name = expression`")]
    InvalidSelectAssignment { item: String },
    #[error("select items must not be empty")]
    EmptySelectItem,
    #[error("`select *` alone is redundant; remove the stage")]
    RedundantSelectAll,
    #[error("summarize requires named aggregates, e.g. `summarize n=count()`")]
    UnnamedAggregates,
    #[error("summarize by requires at least one key")]
    EmptySummarizeKeys,
    #[error("order by requires a field")]
    MissingOrderField,
    #[error("unknown order direction `{direction}`; use asc or desc")]
    UnknownOrderDirection { direction: String },
    #[error("invalid order term `{term}`")]
    InvalidOrderTerm { term: String },
    #[error("`=>` requires an applicator")]
    MissingApplicator,
    #[error("`=>` must be the final stage; `{expression}` has a row pipeline after application; bind `{head}` first, then apply `{tail}`")]
    PipelineAfterApplication {
        expression: String,
        head: String,
        tail: String,
    },
    #[error("relation applicator must be `_.r#` or `_.wire`, got `{expression}`")]
    InvalidRelationApplicator { expression: String },
    #[error("unsupported `=>` applicator `{expression}`; use a relation applicator `_.r#`, method application, or a render template")]
    UnsupportedApplicator { expression: String },
    #[error("row-to-text rendering requires `source => <<TAG`, not `source <<TAG`")]
    MissingRenderArrow,
    #[error("`=>` requires a left-hand rowset")]
    MissingApplicationSource,
    #[error(
        "unexpected trailing syntax `{tail}` after render applicator; `=>` must be the final stage"
    )]
    RenderTrailingSyntax { tail: String },
    #[error("row-to-text template heredoc `<<{tag}` has a malformed line terminator")]
    RenderLineTerminator { tag: String },
}

/// Agent-facing hint when a program has bindings but no executable return roots.
pub fn missing_program_roots_error() -> String {
    "Add a final return line (e.g. `limited[p2,p14]`), or omit only when every line is `label = …` (last binding is returned)."
        .to_string()
}

pub fn program_empty_error() -> String {
    "Program is empty.".to_string()
}

pub fn program_return_keyword_error() -> String {
    "Remove `return` — write bare roots on the last line (e.g. `limited` or `a, b`).".to_string()
}

pub fn program_invalid_binding_label_error(label: &str) -> String {
    if looks_like_domain_symbol(label) {
        format!("Binding names must be labels like `items_1`; whole names matching `{}` are reserved (`{label}`). Rename these bindings and their references.", reserved_program_label_pattern())
    } else {
        format!("Binding names must be identifiers like `issue`, not `{label}`.")
    }
}

pub fn program_binding_after_return_error() -> String {
    "Return must be last — move bindings above the return line, or bind intermediate steps before returning."
        .to_string()
}

pub fn program_duplicate_return_node_error() -> String {
    "Program has multiple return expressions — bind each step (`filtered = e# | where …`), then one final return line."
        .to_string()
}
