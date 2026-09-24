//! Path expression parser — parses the Plasm micro-syntax into typed [`Expr`] values.
//!
//! The grammar is LL(1); each token unambiguously determines the parse path:
//!
//! ```text
//! expr       = source pipeline* projection?
//! source     = Entity "(" id ")"               — GetExpr
//!            | Entity "(" id_field "=" value ")" — GetExpr (shadow/repair sugar; normative, deliberately untaught)
//!            | Entity "{" pred ("," pred)* "}"  — QueryExpr with filters
//!            | Entity "~" quoted_or_bare         — Search QueryExpr
//!            | Entity                            — QueryExpr::all
//!            | `v` DIGITS `{` union_ctor_fields `}` — [`Expr::TeachingValue`] (teaching table union constructor literal)
//!
//! **Not valid:** `Entity:id` or `Get(Entity, id)` — there is no `Get(` wrapper; use **`Entity(id)`** only.
//! A typo like `Pokemon:pikachu` is rejected (otherwise it would parse as `Pokemon` + ignored tail → wrong query).
//!
//! pipeline   = "." field_name                  — ChainExpr (EntityRef follow) or relation nav
//!            | "." method "()"                  — zero-arity pipeline: DeleteExpr or InvokeExpr (by capability kind)
//!            | "." method "(" method_args ")"   — dotted-call alias: Create / Update / Action / Delete with args (see teaching table)
//!              method_args = dotted_call_args | union_ctor_payload
//!              dotted_call_args = ε | ".." | key=value ("," key=value)* ["," ".."]
//!              union_ctor_payload = `v` DIGITS `{` … `}` (sole contents of `( )` when capability root input is `InputType::Union`)
//!            | "." method                       — same as `method()` when schema disallows name clashes
//!            | ".^" Entity                      — reverse traversal query
//!            | ".^" Entity "{" preds "}"        — reverse traversal with filter
//!
//! pred       = field op value?
//!            | foreign_entity "." field op value?  — cross-entity filter (value may be omitted after `op` for teaching placeholders)
//!
//! op         = "=" | "!=" | ">" | "<" | ">=" | "<=" | "~"
//! value      = quoted_string | structured_heredoc | uuid | number | bare_word (bare allows `\\` before delimiters)
//!            | `[` value ( `,` value )* `]` — array literal (predicate / dotted-call arg RHS only)
//!            | in `{{…}}` predicates and `method(k=v,…)` args: unquoted **phrase** (spaces OK) to `,` or closing `}}` / `)`
//!
//! structured_heredoc = `<<` TAG `\n` payload `\n` TAG (tagged heredoc only; TAG matches `[A-Za-z_][A-Za-z0-9_]*`).
//! The close line may be exactly `TAG`, or `TAG` immediately followed by optional ASCII whitespace and
//! parser-owned delimiters such as `)`, `}`, `,`, or `})` on that line (e.g. `TAG})` after the body line).
//! teaching gloss and diagnostics emphasize heredocs for non-`short` string semantics.
//!
//! projection = "[" field ("," field)* "]"
//! ```
//!
//! # Layout and leniency
//!
//! Value parsing lives in submodule `value` (file `value.rs` next to this one): strict `Entity(id)` / search
//! vs lenient predicate and dotted-call arg RHS; see that file for scannerless / fault-tolerant parsing references.
//!
//! teaching prompts may show bare `$` as a teaching placeholder; it parses as the string `$`.
//! Unary `Entity($)` in `{…}` filters, dotted-call arguments, and array elements matches scalar teaching — a fill-in
//! for that entity’s identity (the renderer emits `e#($)` the same as other witness placeholders).
//!
//! # Examples
//!
//! ```text
//! parse("Pet(10)", &cgs)        → Ok(ParsedExpr { expr: Get(Pet,10), .. })
//! parse("Order{quantity>3}", &cgs) → Ok(ParsedExpr { expr: Query(...), .. })
//! parse("Order(5).petId", &cgs) → Ok(ParsedExpr { expr: Chain(..), .. })
//! ```

mod entity_ref_parse;
pub(crate) mod heredoc_surface;
pub(crate) mod predicate_surface;
pub(crate) mod program_surface;
mod value;

pub mod applicator;
pub mod collect_meta;
pub mod iterate_until;
pub mod pipe;
pub mod program;
pub mod value_expr;

pub use applicator::{parse_applicator, split_apply_expr, Applicator, RenderApplicator};
pub use collect_meta::{normalize_nested_projection_field, peel_collect_meta, CollectMeta};
pub use heredoc_surface::{
    parse_tagged_heredoc_literal, tagged_heredoc_close_kind, HeredocCloseLineKind,
};
pub use iterate_until::{
    iterate_seed_is_label, iterate_seed_must_be_get_identity, try_parse_iterate_until,
    IterateUntilExpr, ITERATE_SEED_GET_FAMILY,
};
pub use pipe::{parse_pipe_expr, PipeExpr, PipeStage};
pub use program::{
    parse_expr_node, parse_program_shape, ExprNode, ParsedProgram, RowExpr, Statement,
};
pub use program_surface::{
    classify_top_level_assignment, collect_program_statement_lines,
    expand_flattened_program_statements, is_valid_program_label, looks_like_domain_symbol,
    missing_program_roots_error, pipe_head_has_catalog_surface_syntax,
    program_binding_after_return_error, program_duplicate_return_node_error, program_empty_error,
    program_invalid_binding_label_error, program_return_keyword_error,
    scan_physical_line_stmt_state, split_assignment_at_top_level, split_assignment_for_binding,
    split_flattened_program_line, split_token_top_level, split_top_level, strip_line_comment,
    validate_pipe_head_syntax, validate_program_label, validate_program_statement_order,
    FlattenedProgram, FlattenedProgramLine, PhysicalLineStmtState, TopLevelAssignment,
};
pub use value_expr::{RenderExpr, ValueExpr};

pub mod data;
mod quoted;

use crate::cgs_federation::CgsLayer;
use crate::schema::{
    capability_is_zero_arity_invoke, capability_path_method_segment,
    resolve_capability_input_param_field,
};
use crate::symbol_tuning::{
    entity_slices_for_render, CatalogScope, FocusSpec, SymbolMap, SymbolSession,
};
use crate::{
    catalog_id::CatalogEntryStamp, coerce_value_for_field_type,
    coerce_value_for_field_type_with_policy, ArrayFieldCoercionPolicy, ArrayItemsSchema,
    CapabilityKind, CapabilityName, ChainExpr, CompOp, CreateExpr, DeleteExpr, EntityDef,
    EntityName, Expr, FieldType, GetExpr, IdentitySlot, InputType, InvokeExpr, InvokeInputPayload,
    PageExpr, Predicate, QueryExpr, Ref, SymbolResolveError, Value, ValueWireFormat, CGS,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

/// Structured reason for a [`ParseError`] — drives [`crate::error_render::render_parse_error`]
/// without substring matching on ad hoc messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    ExpectedChar {
        expected: char,
        got: Option<char>,
    },
    ExpectedIdentifier,
    ExpectedOperator,
    ExpectedValue,
    UnterminatedString,
    UnterminatedEscape,
    /// Unknown `\` escape in a quoted string (e.g. `\q`); not a JSON-style escape.
    UnknownEscape {
        escape: char,
    },
    InvalidFloat {
        raw: String,
    },
    InvalidInteger {
        raw: String,
    },
    UnknownEntity {
        name: String,
        /// Span of the entity identifier in the source, when known (e.g. first token of the expression).
        span_opt: Option<(usize, usize)>,
    },
    IdMustBeStringOrNumber,
    /// `Entity()` — no id before `)`; singleton GET uses `Entity.method()` from teaching table.
    EmptyGetParens {
        entity: String,
    },
    /// `Entity:…` looks like a mistaken get-by-id; Plasm uses `Entity(id)` only.
    ColonAfterEntityName {
        entity: String,
    },
    SearchTextMustBeString,
    /// `Entity~text` but the CGS has no [`CapabilityKind::Search`] for that entity.
    SearchNotSupported {
        entity: String,
    },
    PredicateFieldNotFound {
        field: String,
        entity: String,
        /// Byte range in the source covering the predicate field identifier.
        span_start: usize,
        span_end: usize,
    },
    /// `| where` after a concrete compute schema (RA-2 projection grain).
    RowSchemaFieldNotFound {
        field: String,
        available: Vec<String>,
        span_start: usize,
        span_end: usize,
    },
    NotNavigable {
        field: String,
        entity: String,
        span_start: usize,
        span_end: usize,
    },
    /// `.p#` used where a relation hop was expected (`r#` or wire name required).
    RelationSegmentWrongRole {
        sym: String,
        wire: String,
        entity: String,
        span_start: usize,
        span_end: usize,
    },
    NotFieldOrRelation {
        field: String,
        entity: String,
        span_start: usize,
        span_end: usize,
    },
    /// `Get(…).relation` on a cardinality-many edge with no `materialize` / `unavailable` in CGS.
    ManyRelationUnmaterialized {
        entity: String,
        relation: String,
        target: String,
        span_start: usize,
        span_end: usize,
    },
    NoEntityRefBridge {
        target_entity: String,
        source_entity: String,
    },
    NoZeroArityMethod {
        entity: String,
        label: String,
    },
    /// Same kebab `label` resolves to multiple zero-arity capabilities (schema overlap).
    AmbiguousZeroArityMethod {
        entity: String,
        label: String,
        capability_names: Vec<String>,
    },
    /// `Get(anchor).label(…)` — no matching Create/Update/Delete/Action for this label on `anchor`.
    DottedCallNoMatch {
        anchor_entity: String,
        label: String,
    },
    /// Same `label` matches more than one same-domain dotted-call capability on `anchor`.
    DottedCallAmbiguous {
        anchor_entity: String,
        label: String,
    },
    /// Same create label matches more than one cross-domain Create.
    DottedCreateAmbiguous {
        anchor_entity: String,
        label: String,
    },
    /// Same wire entity name exists in multiple loaded catalogs without `catalog_entry_id` / `e#` stamp.
    AmbiguousEntityCatalog {
        entity: String,
    },
    /// Exact `id_field` brace could not lower to Get (no Get / ambiguous / unresolved catalog).
    IdentityBraceGetFailed {
        message: String,
    },
    CapabilityMissingInternal {
        name: String,
    },
    InvokeRequiresTargetId {
        entity: String,
        label: String,
        /// Taught left-hand seat (`eN(<id>).mK(...)` or token-brace equivalent).
        taught_seat: String,
    },
    UnexpectedTrailingInput {
        tail: String,
    },
    InvalidProgramString {
        message: String,
    },
    InvalidTemporalValue {
        message: String,
    },
    /// Angle-bracket teaching hole used as a program value (PLP-10).
    UnfilledTeachingHole {
        hole: String,
    },
    /// Prefer adding a variant above.
    Other {
        message: String,
    },
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseErrorKind::InvalidProgramString { message } => write!(f, "invalid program string: {message}"),
            ParseErrorKind::ExpectedChar { expected, got } => match got {
                Some(g) => write!(f, "expected '{expected}', got '{g}'"),
                None => write!(f, "expected '{expected}', got end of input"),
            },
            ParseErrorKind::ExpectedIdentifier => write!(f, "expected identifier"),
            ParseErrorKind::ExpectedOperator => {
                write!(f, "expected operator (= != > < >= <= ~)")
            }
            ParseErrorKind::ExpectedValue => write!(f, "expected value"),
            ParseErrorKind::UnterminatedString => write!(f, "unterminated string"),
            ParseErrorKind::UnterminatedEscape => write!(f, "unterminated escape"),
            ParseErrorKind::UnknownEscape { escape } => write!(
                f,
                "unknown escape `\\{escape}` in quoted string; use JSON-style `\\n`/`\\t`/`\\\\`/`\\\"`/`\\uXXXX`, or a tagged heredoc (`<<TAG` … `TAG`) for multiline bodies"
            ),
            ParseErrorKind::InvalidFloat { raw } => write!(f, "invalid float: {raw}"),
            ParseErrorKind::InvalidInteger { raw } => write!(f, "invalid integer: {raw}"),
            ParseErrorKind::UnknownEntity { name, .. } => write!(f, "unknown entity '{name}'"),
            ParseErrorKind::IdMustBeStringOrNumber => write!(f, "ID must be a string or number"),
            ParseErrorKind::EmptyGetParens { entity } => write!(
                f,
                "empty parentheses after entity '{entity}' (use `{entity}(id)` or a pathless method from the prompt)"
            ),
            ParseErrorKind::ColonAfterEntityName { entity } => write!(
                f,
                "unexpected ':' after `{entity}`; use `{entity}(id)` for get-by-id, not `{entity}:…`"
            ),
            ParseErrorKind::SearchTextMustBeString => write!(f, "search text must be a string"),
            ParseErrorKind::SearchNotSupported { entity } => write!(
                f,
                "full-text `~` search is not available for entity '{entity}' (no Search capability); use query {{…}} or Get(id) per that entity's teaching block"
            ),
            ParseErrorKind::PredicateFieldNotFound { field, entity, .. } => write!(
                f,
                "field '{field}' not found on entity '{entity}' (not an entity field or capability param)"
            ),
            ParseErrorKind::RowSchemaFieldNotFound {
                field, available, ..
            } => write!(
                f,
                "RA-2: field '{field}' is not a column on the current row (available: {})",
                available.join(", ")
            ),
            ParseErrorKind::NotNavigable { field, entity, .. } => write!(
                f,
                "'{field}' on '{entity}' is not navigable (not an EntityRef or relation)"
            ),
            ParseErrorKind::RelationSegmentWrongRole { sym, wire, entity, .. } => write!(
                f,
                "'{sym}' is a query parameter or field ('{wire}'), not a relation on '{entity}'; use '.{wire}' or an r# symbol from the teaching table"
            ),
            ParseErrorKind::NotFieldOrRelation { field, entity, .. } => write!(
                f,
                "'{field}' not found on entity '{entity}' (not a field or relation)"
            ),
            ParseErrorKind::ManyRelationUnmaterialized {
                entity,
                relation,
                target,
                ..
            } => write!(
                f,
                "cardinality-many relation '{relation}' on '{entity}' (target '{target}') has no chain materialization (declare materialize: from_parent_get, query_scoped with capability+param, or query_scoped_bindings with capability+bindings in the schema)"
            ),
            ParseErrorKind::NoEntityRefBridge {
                target_entity,
                source_entity,
            } => write!(
                f,
                "no EntityRef from '{target_entity}' to '{source_entity}' found"
            ),
            ParseErrorKind::NoZeroArityMethod { entity, label } => {
                write!(f, "no zero-arity method `{label}` on entity `{entity}`")
            }
            ParseErrorKind::AmbiguousZeroArityMethod {
                entity,
                label,
                capability_names,
            } => write!(
                f,
                "ambiguous zero-arity method `{label}` on entity `{entity}`: capabilities {}",
                capability_names.join(", ")
            ),
            ParseErrorKind::DottedCallNoMatch { label, .. } => write!(
                f,
                "no `{label}(…)` create/update/delete/action matches this expression (check capability names in the prompt)"
            ),
            ParseErrorKind::DottedCallAmbiguous {
                anchor_entity,
                label,
            } => write!(
                f,
                "ambiguous capability label `{label}` for entity `{anchor_entity}`"
            ),
            ParseErrorKind::DottedCreateAmbiguous { label, .. } => {
                write!(f, "ambiguous create label `{label}`")
            }
            ParseErrorKind::AmbiguousEntityCatalog { entity } => write!(
                f,
                "ambiguous entity `{entity}` across loaded catalogs — use session `e#` (catalog ownership stamp), not bare wire entity name"
            ),
            ParseErrorKind::IdentityBraceGetFailed { message } => f.write_str(message),
            ParseErrorKind::CapabilityMissingInternal { name } => {
                write!(f, "internal: capability '{name}' missing")
            }
            ParseErrorKind::InvokeRequiresTargetId { taught_seat, .. } => write!(
                f,
                "invoke requires `{taught_seat}` on the left"
            ),
            ParseErrorKind::UnexpectedTrailingInput { tail } => {
                write!(f, "unexpected input after expression: '{tail}'")
            }
            ParseErrorKind::InvalidTemporalValue { message } => {
                write!(f, "invalid date/time value: {message}")
            }
            ParseErrorKind::UnfilledTeachingHole { hole } => write!(
                f,
                "PLP-10: unfilled teaching hole `{hole}` is not an identifier or literal; fill it with a bound value, a row field, or a string of that sort"
            ),
            ParseErrorKind::Other { message } => f.write_str(message),
        }
    }
}

/// The result of parsing a path expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedExpr {
    /// The composed expression tree.
    pub expr: Expr,
    /// Optional **row** projection from explicit `[f,f,…]` (preserves entity/row shape).
    pub projection: Option<Vec<String>>,
    /// Single-segment `.wire` sugar on a catalog source (PLP-1 scalar extract when the
    /// source is a proven StaticSingleton Get; never silent `| select`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_dot_extract: Option<String>,
}

impl ParsedExpr {
    /// Expression with neither row projection nor PLP-1 field-dot extract.
    #[must_use]
    pub fn from_expr(expr: Expr) -> Self {
        Self {
            expr,
            projection: None,
            field_dot_extract: None,
        }
    }

    /// Attach an explicit `[…]` row projection (clears any field-dot extract).
    #[must_use]
    pub fn with_projection(mut self, projection: Option<Vec<String>>) -> Self {
        self.projection = projection;
        self.field_dot_extract = None;
        self
    }
}

/// A structured parse error with position information.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    /// Byte offset in the input where parsing failed.
    pub offset: usize,
}

impl ParseError {
    /// Same text as [`ParseErrorKind`] (what used to live in `message`).
    #[must_use]
    pub fn message(&self) -> String {
        self.kind.to_string()
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at offset {}: {}", self.offset, self.kind)
    }
}

impl std::error::Error for ParseError {}

/// Top-level `v` + ASCII digits — union constructor surface (`v101{…}`), not a CGS entity name.
fn is_root_union_ctor_surface_label(name: &str) -> bool {
    let b = name.as_bytes();
    if b.first() != Some(&b'v') {
        return false;
    }
    let tail = &name[1..];
    !tail.is_empty() && tail.bytes().all(|x| x.is_ascii_digit())
}

/// End index (exclusive) of a comparison RHS inside `{…}` / row-filter bodies.
fn scan_top_level_pred_rhs_end(input: &str, start: usize) -> usize {
    let bytes = input.as_bytes();
    let mut i = start;
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut depth_bracket = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'{' => depth_brace += 1,
            b'}' if depth_brace == 0 && depth_paren == 0 && depth_bracket == 0 => break,
            b'}' => depth_brace -= 1,
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            b',' if depth_paren == 0 && depth_brace == 0 && depth_bracket == 0 => break,
            _ => {}
        }
        i += 1;
    }
    i
}

/// Classification of unparsed input tail after a prefix parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseRemainder {
    Empty,
    Prose(String),
    Syntax { at: usize, head: char },
}

impl ParseRemainder {
    #[must_use]
    pub fn acceptable_for_program_line(&self) -> bool {
        matches!(self, Self::Empty | Self::Prose(_))
    }
}

/// Like [`parse`], but also returns how the unconsumed tail is classified.
pub fn parse_with_remainder(
    input: &str,
    cgs: &CGS,
) -> Result<(ParsedExpr, ParseRemainder), ParseError> {
    let span = crate::spans::parse_program(input.len());
    let _guard = span.enter();
    let mut p = Parser::new(input, cgs);
    let mut parsed = p.parse_expr()?;
    parsed.expr =
        crate::expr_sugar::lower_id_field_brace_to_get(parsed.expr, cgs).map_err(|e| {
            ParseError {
                kind: ParseErrorKind::IdentityBraceGetFailed {
                    message: e.to_string(),
                },
                offset: 0,
            }
        })?;
    Ok((parsed, p.classify_remainder()))
}

/// Honest Create-as-filter note: do not name a Search wire when the entity has none.
fn create_only_predicate_entity_note(cgs: &CGS, entity_name: &str, pred_wire: &str) -> String {
    let has_search = !cgs
        .find_capabilities(entity_name, CapabilityKind::Search)
        .is_empty();
    let has_query = !cgs
        .find_capabilities(entity_name, CapabilityKind::Query)
        .is_empty();
    if has_search {
        format!(
            "{entity_name} (field `{pred_wire}` is a Create-capability param; in search filters `e#~\"…\"{{…}}` use the Search-capability param wire, not a Create-only homograph)"
        )
    } else if has_query {
        format!(
            "{entity_name} (field `{pred_wire}` is a Create-capability param, not a query filter; use the Query teaching-row wires in `e#{{…}}`, not a Create-only homograph)"
        )
    } else if crate::query_resolve::sole_nullary_singleton_get(cgs, entity_name).is_some() {
        format!(
            "{entity_name} (field `{pred_wire}` is a Create-capability param; this entity has no Query or Search filter surface — use the pathless singleton `{entity_name}` / `e#`, not Create params as `{{…}}` filters)"
        )
    } else {
        format!(
            "{entity_name} (field `{pred_wire}` is a Create-capability param; this entity has no Query or Search filter surface — use Get `e#(<id>)` when the id is known, not Create params as `{{…}}` filters)"
        )
    }
}

/// Opaque `m#` bound to a read kind: name the taught seat, never "find a mutator".
fn opaque_read_kind_invoke_note(
    raw: &str,
    cap: &crate::CapabilitySchema,
    entity_sym: &str,
    cgs: Option<&CGS>,
) -> String {
    let kind = cap.kind.as_str();
    let fq = format!("{}.{}", cap.domain, cap.name);
    match cap.kind {
        CapabilityKind::Get => {
            let pathless = cgs
                .and_then(|g| crate::query_resolve::sole_nullary_singleton_get(g, &cap.domain))
                .is_some();
            if pathless {
                format!(
                    "`{raw}` is {kind} `{fq}` — fetch is pathless `{entity_sym}` / `{entity_sym}[…]`, not `{entity_sym}.{raw}(…)`"
                )
            } else {
                format!(
                    "`{raw}` is {kind} `{fq}` — fetch is `{entity_sym}(<id>)`, not `{entity_sym}.{raw}(…)`"
                )
            }
        }
        CapabilityKind::Query => format!(
            "`{raw}` is {kind} `{fq}` — list with `{entity_sym}{{…}}`, not `{entity_sym}.{raw}(…)`"
        ),
        CapabilityKind::Search => format!(
            "`{raw}` is {kind} `{fq}` — search with `{entity_sym}~\"<query>\"`, not `{entity_sym}.{raw}(…)`"
        ),
        _ => format!("`{raw}` is {kind} `{fq}`, not a mutator"),
    }
}

/// Identity-required mutator/Get seat the teaching table already showed.
fn taught_explicit_anchor_seat(
    source: &Expr,
    cap: &crate::CapabilitySchema,
    sym_map: &dyn crate::symbol_tuning::SymbolSession,
    cgs: Option<&CGS>,
) -> String {
    let entity = source.primary_entity();
    let eid = source
        .session_catalog_entry_id()
        .map(|s| s.as_str())
        .unwrap_or("");
    let es = sym_map.entity_sym_for(eid, entity);
    let ent = cgs.and_then(|g| g.get_entity(entity));
    let id_wire = match ent {
        Some(e) if !e.id_field.as_str().is_empty() => {
            sym_map.ident_sym_entity_field_for(eid, entity, e.id_field.as_str())
        }
        _ => String::new(),
    };
    match cap.kind {
        CapabilityKind::Get => {
            crate::taught_seat::taught_get_identity_receiver(&es, &id_wire, ent, cgs)
        }
        _ => {
            let ms = sym_map.method_sym_for(eid, entity, cap.name.as_str());
            crate::taught_seat::taught_identity_mutator_invoke_seat(&es, &ms, &id_wire, ent, cgs)
        }
    }
}

/// Parse a Plasm path expression string against a CGS for validation.
///
/// Returns a [`ParsedExpr`] containing the resolved [`Expr`] tree and optional
/// projection. Returns a [`ParseError`] if the syntax is invalid or references
/// unknown entities/fields.
///
/// **Prefix parse:** reads **one** expression from the start of `input`, then stops.
/// Trailing text (after whitespace) is **ignored** so callers can paste noisy LLM output
/// without failing the whole line.
pub fn parse(input: &str, cgs: &CGS) -> Result<ParsedExpr, ParseError> {
    let span = crate::spans::parse_program(input.len());
    let _guard = span.enter();
    let mut p = Parser::new(input, cgs);
    let mut parsed = p.parse_expr()?;
    parsed.expr =
        crate::expr_sugar::lower_id_field_brace_to_get(parsed.expr, cgs).map_err(|e| {
            ParseError {
                kind: ParseErrorKind::IdentityBraceGetFailed {
                    message: e.to_string(),
                },
                offset: 0,
            }
        })?;
    Ok(parsed)
}

/// Parse against multiple disjoint [`CGS`] graphs (federated execute). Caller supplies the session
/// [`crate::symbol_tuning::SymbolMap`] behind [`Arc`] (e.g. [`TeachingExposureSession::symbol_map_arc`](crate::symbol_tuning::TeachingExposureSession::symbol_map_arc)).
pub fn parse_with_cgs_layers(
    input: &str,
    layers: &[CgsLayer<'_>],
    sym_map: Arc<dyn SymbolSession>,
) -> Result<ParsedExpr, ParseError> {
    parse_with_cgs_layers_program(input, layers, sym_map, None, false)
}

/// Parse one Plasm line against `cgs`, using in-grammar session symbol resolution when `sym_map` is set.
pub fn parse_session_line(
    input: &str,
    cgs: &CGS,
    sym_map: Option<Arc<dyn SymbolSession>>,
) -> Result<ParsedExpr, ParseError> {
    match sym_map {
        Some(map) => {
            let sole_entry = map.sole_registry_entry_id().map(str::to_string);
            let layer = match (
                cgs.entry_id.as_deref().filter(|id| !id.is_empty()),
                sole_entry.as_deref(),
            ) {
                (Some(entry_id), _) => CgsLayer::qualified(entry_id, cgs),
                (None, Some(entry_id)) if !entry_id.is_empty() => {
                    CgsLayer::qualified(entry_id, cgs)
                }
                (None, Some(_)) | (None, None) => CgsLayer::unset(cgs),
            };
            parse_with_cgs_layers(input, std::slice::from_ref(&layer), map)
        }
        None => parse(input, cgs),
    }
}

/// Like [`parse_with_cgs_layers`], but when compiling a **Plasm program**, supply the set of
/// in-scope program node ids so `method(p=report)` and `report.field` lower to
/// [`crate::value::PlasmInputRef`] instead of string literals. `for_each_row_context` enables
/// `_.field` row holes on the right-hand side of `=>`.
pub fn parse_with_cgs_layers_program(
    input: &str,
    layers: &[CgsLayer<'_>],
    sym_map: Arc<dyn SymbolSession>,
    program_nodes: Option<&BTreeSet<String>>,
    for_each_row_context: bool,
) -> Result<ParsedExpr, ParseError> {
    parse_with_cgs_layers_program_opts(
        input,
        layers,
        sym_map,
        program_nodes,
        for_each_row_context,
        true,
    )
}

/// Parse a **row-local filter body** (`Entity{field=…}` synthesized by `.filter{…}`) without the
/// `id_field → Get` sugar. That rewrite is correct for surface queries, but inside a row predicate
/// it would fold `{id_field=value}` into an `Expr::Get` and trip the row-predicate contract with a
/// misleading "no list to filter" diagnostic. Row filters are always predicate `Expr::Query`.
///
/// `row_schema_fields` is RA-2 `current_row_schema` (prior `| select` aliases / projected columns).
/// When non-empty, predicate LHS names are judged against that grain, not the catalog entity.
pub fn parse_row_filter_body(
    input: &str,
    layers: &[CgsLayer<'_>],
    sym_map: Arc<dyn SymbolSession>,
    row_schema_fields: &[String],
    program_nodes: &std::collections::BTreeSet<String>,
) -> Result<ParsedExpr, ParseError> {
    let span = crate::spans::parse_program(input.len());
    let _guard = span.enter();
    if layers.is_empty() {
        return Err(ParseError {
            kind: ParseErrorKind::Other {
                message: "parse_with_cgs_layers: empty CGS layer list".into(),
            },
            offset: 0,
        });
    }
    let mut p = Parser::new_with_sym_map(input, LayerStack::borrowed(layers), sym_map);
    p.program_nodes = Some(program_nodes);
    p.row_schema_fields = row_schema_fields.to_vec();
    let parsed = p.parse_expr()?;
    let remainder = p.classify_remainder();
    if !remainder.acceptable_for_program_line() {
        return Err(ParseError {
            kind: ParseErrorKind::Other {
                message: format!(
                    "unexpected trailing syntax starting with `{}`",
                    match &remainder {
                        ParseRemainder::Syntax { head, .. } => *head,
                        _ => ' ',
                    }
                ),
            },
            offset: match remainder {
                ParseRemainder::Syntax { at, .. } => at,
                _ => p.pos,
            },
        });
    }
    Ok(parsed)
}

fn parse_with_cgs_layers_program_opts(
    input: &str,
    layers: &[CgsLayer<'_>],
    sym_map: Arc<dyn SymbolSession>,
    program_nodes: Option<&BTreeSet<String>>,
    for_each_row_context: bool,
    apply_id_field_get_rewrite: bool,
) -> Result<ParsedExpr, ParseError> {
    let span = crate::spans::parse_program(input.len());
    let _guard = span.enter();
    if layers.is_empty() {
        return Err(ParseError {
            kind: ParseErrorKind::Other {
                message: "parse_with_cgs_layers: empty CGS layer list".into(),
            },
            offset: 0,
        });
    }
    let mut p = Parser::new_with_sym_map(input, LayerStack::borrowed(layers), sym_map);
    p.program_nodes = program_nodes;
    p.for_each_row_context = for_each_row_context;
    let mut parsed = p.parse_expr()?;
    if apply_id_field_get_rewrite {
        parsed.expr = crate::expr_sugar::lower_id_field_brace_to_get_federated(parsed.expr, layers)
            .map_err(|e| ParseError {
                kind: ParseErrorKind::IdentityBraceGetFailed {
                    message: e.to_string(),
                },
                offset: 0,
            })?;
    }
    let remainder = p.classify_remainder();
    if !remainder.acceptable_for_program_line() {
        if matches!(&parsed.expr, Expr::Get(_))
            && matches!(&remainder, ParseRemainder::Syntax { head: '{', .. })
        {
            return Err(ParseError {
                kind: ParseErrorKind::Other { message: format!(
                    "unexpected trailing query braces after Get `{}`: Get accepts identity only. Bind required provisions through their declared capabilities before this Get; use `| where` for row selection. Do not discard selection criteria during repair.",
                    input[..p.pos].trim()
                ) },
                offset: p.pos,
            });
        }
        return Err(ParseError {
            kind: ParseErrorKind::Other {
                message: format!(
                    "unexpected trailing syntax starting with `{}`",
                    match &remainder {
                        ParseRemainder::Syntax { head, .. } => *head,
                        _ => ' ',
                    }
                ),
            },
            offset: match remainder {
                ParseRemainder::Syntax { at, .. } => at,
                _ => p.pos,
            },
        });
    }
    Ok(parsed)
}

// ── Internal parser ────────────────────────────────────────────────────────

pub(super) enum LayerStack<'a> {
    Borrowed(&'a [CgsLayer<'a>]),
    Single(CgsLayer<'a>),
}

impl<'a> LayerStack<'a> {
    fn borrowed(layers: &'a [CgsLayer<'a>]) -> Self {
        Self::Borrowed(layers)
    }

    fn single(layer: CgsLayer<'a>) -> Self {
        Self::Single(layer)
    }

    fn as_slice(&self) -> &[CgsLayer<'a>] {
        match self {
            Self::Borrowed(layers) => layers,
            Self::Single(layer) => std::slice::from_ref(layer),
        }
    }
}

pub(super) struct Parser<'a> {
    pub(super) input: &'a str,
    pub(super) pos: usize,
    stack: LayerStack<'a>,
    /// Same `m#` → kebab table as the SYMBOL MAP bundle (forgiving when expansion did not run).
    sym_map: Arc<dyn SymbolSession>,
    /// When set, bare `id` / `id.path` in dotted-call args, predicates, and array literals refer
    /// to program nodes with those ids (typed [`crate::value::PlasmInputRef`]).
    pub(super) program_nodes: Option<&'a BTreeSet<String>>,
    /// Enables `_.path` row references (for `source => …` templates).
    pub(super) for_each_row_context: bool,
    /// RA-2 `current_row_schema` column names when parsing a row-local `| where` body.
    /// Empty means catalog-entity grain (query braces / iterate-until).
    row_schema_fields: Vec<String>,
    /// When the surface entity token was an opaque `e#`, owning catalog stamped on built [`Expr`].
    pending_session_catalog_entry_id: Option<String>,
    /// Deferred field-dot extract candidate (PLP-1 scalar on StaticSingleton; `[…]` remains row projection).
    pending_field_dot_extract: Option<String>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, cgs: &'a CGS) -> Self {
        let (full, _) = entity_slices_for_render(cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(cgs, &full));
        let layer = match cgs.entry_id.as_deref().filter(|id| !id.is_empty()) {
            Some(entry_id) => CgsLayer::qualified(entry_id, cgs),
            None => CgsLayer::unset(cgs),
        };
        Self::new_with_sym_map(input, LayerStack::single(layer), sym_map)
    }

    fn new_with_sym_map(
        input: &'a str,
        stack: LayerStack<'a>,
        sym_map: Arc<dyn SymbolSession>,
    ) -> Self {
        let mut p = Self {
            input,
            pos: 0,
            stack,
            sym_map,
            program_nodes: None,
            for_each_row_context: false,
            row_schema_fields: Vec::new(),
            pending_session_catalog_entry_id: None,
            pending_field_dot_extract: None,
        };
        p.skip_ws();
        p
    }

    fn ok_stamped(&self, expr: Expr) -> Result<Expr, ParseError> {
        Ok(expr.with_session_catalog_entry_id(self.active_catalog_entry_id(None)))
    }

    fn layers_stack(&self) -> &[CgsLayer<'a>] {
        self.stack.as_slice()
    }

    fn cgs_layers(&self) -> impl Iterator<Item = &'a CGS> + '_ {
        self.layers_stack().iter().map(CgsLayer::cgs)
    }

    fn primary_cgs(&self) -> &CGS {
        self.layers_stack()[0].cgs()
    }

    fn sole_layer_catalog_entry_id(&self) -> Option<&str> {
        let stack = self.layers_stack();
        if stack.len() == 1 {
            stack[0].entry_id()
        } else {
            None
        }
    }

    fn catalog_entry_id_for_entity(&self, entity: &str) -> Result<Option<&str>, ParseError> {
        if let Some(eid) = self
            .pending_session_catalog_entry_id
            .as_deref()
            .or_else(|| self.sole_layer_catalog_entry_id())
        {
            for layer in self.layers_stack() {
                if layer.matches_forward_entry_id(eid) && layer.cgs().get_entity(entity).is_some() {
                    return Ok(Some(layer.forward_map_entry_id()));
                }
            }
        }
        let hits: Vec<_> = self
            .layers_stack()
            .iter()
            .filter(|layer| layer.cgs().get_entity(entity).is_some())
            .collect();
        match hits.len() {
            0 => Err(self.err(ParseErrorKind::UnknownEntity {
                name: entity.to_string(),
                span_opt: None,
            })),
            1 => Ok(Some(hits[0].forward_map_entry_id())),
            _ => Err(self.err(ParseErrorKind::AmbiguousEntityCatalog {
                entity: entity.to_string(),
            })),
        }
    }

    fn catalog_scope_for_entity(&self, entity: &str) -> Result<CatalogScope<'_>, ParseError> {
        if let Some(eid) = self
            .pending_session_catalog_entry_id
            .as_deref()
            .or_else(|| self.sole_layer_catalog_entry_id())
        {
            if self.cgs_for_catalog_entry_id(eid, entity).is_some() {
                return Ok(CatalogScope::qualified(eid));
            }
        }
        let hits: Vec<_> = self
            .layers_stack()
            .iter()
            .filter(|layer| layer.cgs().get_entity(entity).is_some())
            .collect();
        match hits.len() {
            0 => Err(self.err(ParseErrorKind::UnknownEntity {
                name: entity.to_string(),
                span_opt: None,
            })),
            1 => Ok(hits[0].catalog_scope()),
            _ => Err(self.err(ParseErrorKind::AmbiguousEntityCatalog {
                entity: entity.to_string(),
            })),
        }
    }

    fn active_catalog_entry_id(&self, source: Option<&Expr>) -> Option<String> {
        source
            .and_then(|e| {
                e.session_catalog_entry_id()
                    .map(|id| id.as_str().to_string())
            })
            .or_else(|| self.pending_session_catalog_entry_id.clone())
            .or_else(|| self.sole_layer_catalog_entry_id().map(str::to_string))
    }

    fn cgs_for_catalog_entry_id(&self, entry_id: &str, entity: &str) -> Option<&'a CGS> {
        for layer in self.layers_stack() {
            if layer.matches_forward_entry_id(entry_id) && layer.cgs().get_entity(entity).is_some()
            {
                return Some(layer.cgs());
            }
        }
        None
    }

    fn cgs_for_entity(&self, entity: &str) -> Option<&CGS> {
        let matches: Vec<_> = self
            .layers_stack()
            .iter()
            .map(CgsLayer::cgs)
            .filter(|c| c.get_entity(entity).is_some())
            .collect();
        if matches.len() == 1 {
            return Some(matches[0]);
        }
        None
    }

    fn cgs_for_entity_required(&self, entity: &str) -> Result<&CGS, ParseError> {
        if let Some(eid) = self.active_catalog_entry_id(None) {
            if let Some(cgs) = self.cgs_for_catalog_entry_id(eid.as_str(), entity) {
                return Ok(cgs);
            }
        }
        match self.cgs_for_entity(entity) {
            Some(cgs) => Ok(cgs),
            None if self.layers_stack().len() > 1 => {
                Err(self.err(ParseErrorKind::AmbiguousEntityCatalog {
                    entity: entity.to_string(),
                }))
            }
            None => Ok(self.primary_cgs()),
        }
    }

    fn cgs_for_entity_with_preferred_catalog(
        &self,
        entity: &str,
        preferred_entry_id: Option<&str>,
    ) -> Result<&CGS, ParseError> {
        if let Some(eid) = preferred_entry_id {
            if let Some(cgs) = self.cgs_for_catalog_entry_id(eid, entity) {
                return Ok(cgs);
            }
        }
        self.cgs_for_entity_required(entity)
    }

    fn cgs_for_expr_source(&self, source: &Expr) -> Result<&CGS, ParseError> {
        let entity = source.primary_entity();
        if let Some(eid) = source.session_catalog_entry_id() {
            if let Some(cgs) = self.cgs_for_catalog_entry_id(eid.as_str(), entity) {
                return Ok(cgs);
            }
        }
        self.cgs_for_entity_required(entity)
    }

    fn canonical_entity_name_in_layers(&self, raw: &str) -> String {
        for c in self.cgs_layers() {
            if let Some(can) = c.canonical_entity_name(raw) {
                if c.get_entity(&can).is_some() {
                    return can;
                }
            }
        }
        for c in self.cgs_layers() {
            if c.get_entity(raw).is_some() {
                return raw.to_string();
            }
        }
        self.primary_cgs()
            .canonical_entity_name(raw)
            .unwrap_or_else(|| raw.to_string())
    }

    /// Resolve a method label for kebab / wire-name lookup. Opaque session `m#` passes through
    /// unchanged; invoke resolution must use [`Self::try_resolve_opaque_invoke_capability`] first.
    fn normalize_method_symbol_label(&self, label: &str) -> String {
        if crate::symbol_tuning::SymbolMap::is_opaque_m_sym(label) {
            return label.to_string();
        }
        self.sym_map
            .resolve_method_symbol_token(label)
            .map(str::to_string)
            .unwrap_or_else(|| label.to_string())
    }

    /// Single entry for session `m#` → catalog capability (invoke-anchor validated).
    fn try_resolve_opaque_invoke_capability<'b>(
        &'b self,
        source: &Expr,
        raw: &str,
    ) -> Option<Result<&'b crate::CapabilitySchema, ParseError>> {
        if !crate::symbol_tuning::SymbolMap::is_opaque_m_sym(raw) {
            return None;
        }
        Some(
            match self.sym_map.resolve_opaque_session_method_capability(
                self.layers_stack(),
                raw,
                source.primary_entity(),
            ) {
                Ok(cap) => Ok(cap),
                Err(SymbolResolveError::MethodAnchorMismatch { .. }) => {
                    let binding = match self.sym_map.resolve_session_method(raw) {
                        Ok(b) => b,
                        Err(e) => {
                            return Some(Err(self.err(ParseErrorKind::Other {
                                message: e.to_agent_program_error(),
                            })));
                        }
                    };
                    let anchor = source.primary_entity();
                    for layer in self.layers_stack() {
                        if !layer.matches_forward_entry_id(binding.entry_id.as_str()) {
                            continue;
                        }
                        let cgs = layer.cgs();
                        let Some(cap) = cgs.capabilities.get(binding.capability.as_str()) else {
                            continue;
                        };
                        if cap.domain.as_str() != binding.domain.as_str() {
                            continue;
                        }
                        if Self::scoped_query_bridge_matches_anchor(cap, cgs, anchor)
                            || (cap.kind == CapabilityKind::Create
                                && self.can_bind_create_receiver(cap, source))
                        {
                            return Some(Ok(cap));
                        }
                    }
                    Err(self.err(ParseErrorKind::Other {
                        message: SymbolResolveError::MethodAnchorMismatch {
                            token: raw.trim().to_string(),
                            bound_domain: binding.domain.to_string(),
                            anchor_entity: anchor.to_string(),
                        }
                        .to_agent_program_error(),
                    }))
                }
                Err(e) => Err(self.err(ParseErrorKind::Other {
                    message: e.to_agent_program_error(),
                })),
            },
        )
    }

    fn scoped_query_bridge_matches_anchor(
        cap: &crate::CapabilitySchema,
        cgs: &CGS,
        anchor_entity: &str,
    ) -> bool {
        if cap.kind != CapabilityKind::Query {
            return false;
        }
        let scope_fields: Vec<_> = cap.scope_params().iter().filter(|f| f.required).collect();
        if scope_fields.len() != 1 {
            return false;
        }
        let sf = scope_fields[0];
        let Ok(nv) = sf.named_value(cgs) else {
            return false;
        };
        let FieldType::EntityRef { target, .. } = &nv.field_type else {
            return false;
        };
        target.as_str() == anchor_entity
    }

    fn finish_zero_arity_invoke_from_cap(
        &self,
        source: &Expr,
        raw: &str,
        cap: &crate::CapabilitySchema,
    ) -> Result<Expr, ParseError> {
        let entity = source.primary_entity().to_string();
        self.validate_entity_preferring(
            &entity,
            source.session_catalog_entry_id().map(|id| id.as_str()),
        )?;
        let cap_name = cap.name.clone();
        let needs_anchor_id = cap.requires_receiver();

        let expr = if cap.kind == CapabilityKind::Create {
            Expr::Create(CreateExpr::new(
                cap_name,
                entity,
                Value::Object(Default::default()),
            ))
        } else if cap.kind == CapabilityKind::Delete {
            if let Expr::Get(g) = source {
                Expr::Delete(DeleteExpr::with_target(cap_name, g.reference.clone()))
            } else if !needs_anchor_id {
                let g = self.pathless_mutator_anchor_from_receiver(source, false, cap, raw)?;
                Expr::Delete(DeleteExpr::with_target(cap_name, g.reference.clone()))
            } else {
                return Err(self.err_invoke_requires_taught_seat(source, cap, raw));
            }
        } else if cap.kind == CapabilityKind::Get {
            let mut g = if !needs_anchor_id {
                let mut g = GetExpr::pathless_nullary(entity);
                g.capability_name = Some(cap_name);
                g
            } else {
                match source {
                    Expr::Get(src) => {
                        let mut g = GetExpr::from_ref(src.reference.clone());
                        g.capability_name = Some(cap_name);
                        g
                    }
                    _ => {
                        return Err(self.err_invoke_requires_taught_seat(source, cap, raw));
                    }
                }
            };
            if let Some(id) = source.session_catalog_entry_id() {
                g.catalog_entry_id = CatalogEntryStamp::some(id.clone());
            }
            Expr::Get(g)
        } else if !needs_anchor_id {
            let g = self.pathless_mutator_anchor_from_receiver(source, false, cap, raw)?;
            Expr::Invoke(InvokeExpr::with_target(cap_name, g.reference.clone(), None))
        } else {
            let Expr::Get(g) = source else {
                return Err(self.err_invoke_requires_taught_seat(source, cap, raw));
            };
            Expr::Invoke(InvokeExpr::with_target(cap_name, g.reference.clone(), None))
        };
        Ok(Self::stamp_session_catalog_from_source(source, expr))
    }

    fn parse_zero_arity_invoke(&mut self, source: Expr, label: String) -> Result<Expr, ParseError> {
        let raw = label.clone();
        if let Some(res) = self.try_resolve_opaque_invoke_capability(&source, &raw) {
            return self.finish_zero_arity_invoke_from_cap(&source, &raw, res?);
        }
        let label = self.normalize_method_symbol_label(&label);
        if let Some(expr) =
            self.try_scoped_query_bridge_from_get(&source, &label, Some(raw.as_str()))?
        {
            return Ok(expr);
        }
        let entity = source.primary_entity().to_string();
        self.validate_entity_preferring(
            &entity,
            source.session_catalog_entry_id().map(|id| id.as_str()),
        )?;
        let resolved_name = self.resolve_zero_arity_pipeline_cap(&entity, &label)?;
        let cap = self
            .cgs_for_expr_source(&source)?
            .get_capability(&resolved_name)
            .ok_or_else(|| {
                self.err(ParseErrorKind::CapabilityMissingInternal {
                    name: resolved_name.clone(),
                })
            })?;
        self.finish_zero_arity_invoke_from_cap(&source, &raw, cap)
    }

    fn invoke_arg_catalog_entry_id(
        &self,
        source: &Expr,
        raw_method_label: Option<&str>,
    ) -> Result<String, ParseError> {
        crate::catalog_ownership::catalog_entry_id_for_invoke(
            source,
            raw_method_label,
            self.sym_map.as_ref(),
            crate::catalog_ownership::InvokeCatalogResolutionContext {
                pending_session_catalog_entry_id: self.pending_session_catalog_entry_id.as_deref(),
            },
        )
        .map_err(|e| {
            self.err(ParseErrorKind::Other {
                message: e.to_string(),
            })
        })
    }

    /// Resolve invoke `key=value` map keys at capability materialization (cap-qualified only).
    ///
    /// Field names come from [`CapabilitySchema::invocation_object_fields`] (`payload` ∪
    /// `arguments`) — the same body-object set as object-body InvokeArg coerce.
    fn normalize_invoke_arg_keys_for_cap(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        raw_method_label: Option<&str>,
        raw_map: IndexMap<String, Value>,
    ) -> Result<IndexMap<String, Value>, ParseError> {
        if cap.primary_invocation_schema().is_none() {
            return Ok(raw_map);
        }
        let fields: Vec<_> = cap.invocation_object_fields().cloned().collect();
        self.normalize_invoke_arg_keys_for_fields(
            cap,
            source,
            raw_method_label,
            fields.as_slice(),
            raw_map,
        )
    }

    fn normalize_invoke_arg_keys_for_fields(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        raw_method_label: Option<&str>,
        fields: &[crate::InputFieldSchema],
        raw_map: IndexMap<String, Value>,
    ) -> Result<IndexMap<String, Value>, ParseError> {
        let entry_id = self.invoke_arg_catalog_entry_id(source, raw_method_label)?;
        let cap_label = format!("{}/{}.{}", entry_id, cap.domain, cap.name);
        let hint = self.sym_map.cap_param_syms_hint(
            entry_id.as_str(),
            cap.domain.as_str(),
            cap.name.as_str(),
        );
        let mut out = IndexMap::new();
        for (raw_key, val) in raw_map {
            let resolved = if fields.iter().any(|f| f.name == raw_key)
                || resolve_capability_input_param_field(cap, raw_key.as_str()).is_some()
            {
                raw_key
            } else if crate::symbol_tuning::SymbolMap::is_opaque_p_sym(&raw_key) {
                self.sym_map
                    .resolve_cap_param(
                        CatalogScope::qualified(entry_id.as_str()),
                        cap.domain.as_str(),
                        cap.name.as_str(),
                        &raw_key,
                        cap,
                    )
                    .map_err(|e| {
                        self.err(ParseErrorKind::Other {
                            message: format!("{e} on `{cap_label}`{hint}"),
                        })
                    })?
            } else {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "unknown argument `{raw_key}` for capability `{cap_label}`{hint}"
                    ),
                }));
            };
            if out.insert(resolved.clone(), val).is_some() {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "duplicate invoke argument `{resolved}` on capability `{cap_label}`"
                    ),
                }));
            }
        }
        Ok(out)
    }

    fn union_ctor_leaf_field_key(
        resolved: &str,
        variant: &crate::schema::InputVariantSchema,
        parent_path: &str,
    ) -> String {
        if variant.fields.iter().any(|f| f.name.as_str() == resolved) {
            return resolved.to_string();
        }
        if !parent_path.is_empty() {
            let prefix = format!("{parent_path}.");
            if let Some(rest) = resolved.strip_prefix(&prefix) {
                if variant.fields.iter().any(|f| f.name.as_str() == rest) {
                    return rest.to_string();
                }
            }
        }
        resolved.rsplit('.').next().unwrap_or(resolved).to_string()
    }

    fn normalize_invoke_ctor_fields_for_variant(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        raw_method_label: Option<&str>,
        parent_path: &str,
        variant: &crate::schema::InputVariantSchema,
        raw_map: indexmap::IndexMap<String, Value>,
    ) -> Result<indexmap::IndexMap<String, Value>, ParseError> {
        let resolved_map = self.normalize_invoke_arg_keys_for_fields(
            cap,
            source,
            raw_method_label,
            variant.fields.as_slice(),
            raw_map,
        )?;
        let mut out = indexmap::IndexMap::new();
        for (k, v) in resolved_map {
            let leaf = Self::union_ctor_leaf_field_key(k.as_str(), variant, parent_path);
            if out.insert(leaf.clone(), v).is_some() {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "duplicate union constructor field `{leaf}` under `{parent_path}`"
                    ),
                }));
            }
        }
        Ok(out)
    }

    fn normalize_invoke_union_ctor_element(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        raw_method_label: Option<&str>,
        array_field: &str,
        variants: &[crate::schema::InputVariantSchema],
        el: Value,
    ) -> Result<Value, ParseError> {
        let Value::UnionCtor {
            ctor_label,
            ctor_fields,
        } = el
        else {
            return Ok(el);
        };
        let Some(variant) = variants.iter().find(|v| {
            crate::schema::union_variant_constructor_symbol(v) == Some(ctor_label.as_str())
        }) else {
            return Ok(Value::UnionCtor {
                ctor_label,
                ctor_fields,
            });
        };
        let parent_path = format!("{array_field}.{}", variant.name);
        let normalized = self.normalize_invoke_ctor_fields_for_variant(
            cap,
            source,
            raw_method_label,
            parent_path.as_str(),
            variant,
            ctor_fields,
        )?;
        let mut ctor_fields = normalized;
        self.coerce_registry_input_fields(
            cap.domain.as_str(),
            self.active_catalog_entry_id(Some(source)).as_deref(),
            variant.fields.iter(),
            &mut ctor_fields,
        )?;
        Ok(Value::UnionCtor {
            ctor_label,
            ctor_fields,
        })
    }

    fn normalize_invoke_nested_input_value(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        raw_method_label: Option<&str>,
        f: &crate::InputFieldSchema,
        val: Value,
    ) -> Result<Value, ParseError> {
        let crate::InputFieldWire::Inline(ty) = &f.wire else {
            return Ok(val);
        };
        match ty.as_ref() {
            crate::InputType::Array { element_type, .. } => {
                let Value::Array(items) = val else {
                    return Ok(val);
                };
                let crate::InputType::Union { variants } = element_type.as_ref() else {
                    return Ok(Value::Array(items));
                };
                let mut out = Vec::with_capacity(items.len());
                for el in items {
                    out.push(self.normalize_invoke_union_ctor_element(
                        cap,
                        source,
                        raw_method_label,
                        f.name.as_str(),
                        variants,
                        el,
                    )?);
                }
                Ok(Value::Array(out))
            }
            _ => Ok(val),
        }
    }

    fn normalize_invoke_union_ctor_args_in_object(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        raw_method_label: Option<&str>,
        mut map: indexmap::IndexMap<String, Value>,
    ) -> Result<indexmap::IndexMap<String, Value>, ParseError> {
        // Same body-object field set as coerce — payload ∪ arguments (not arguments-only).
        let fields: Vec<_> = cap.invocation_object_fields().cloned().collect();
        for f in &fields {
            if let Some(v) = map.get_mut(&f.name) {
                let taken = std::mem::replace(v, Value::Null);
                *v = self.normalize_invoke_nested_input_value(
                    cap,
                    source,
                    raw_method_label,
                    f,
                    taken,
                )?;
            }
        }
        Ok(map)
    }

    fn find_union_variant_for_ctor_label(
        &self,
        ctor_label: &str,
    ) -> Option<(&crate::CapabilitySchema, &crate::schema::InputVariantSchema)> {
        for cgs in self.cgs_layers() {
            for cap in cgs.capabilities.values() {
                for is in cap.invocation_input_schemas() {
                    let variants = match &is.input_type {
                        crate::InputType::Union { variants } => variants,
                        crate::InputType::Object { fields, .. } => {
                            let mut found = None;
                            for f in fields {
                                let crate::InputFieldWire::Inline(ty) = &f.wire else {
                                    continue;
                                };
                                if let crate::InputType::Array { element_type, .. } = ty.as_ref() {
                                    if let crate::InputType::Union { variants } =
                                        element_type.as_ref()
                                    {
                                        if let Some(v) = variants.iter().find(|v| {
                                            crate::schema::union_variant_constructor_symbol(v)
                                                == Some(ctor_label)
                                        }) {
                                            found = Some(v);
                                            break;
                                        }
                                    }
                                }
                            }
                            match found {
                                Some(v) => {
                                    return Some((cap, v));
                                }
                                None => continue,
                            }
                        }
                        _ => continue,
                    };
                    if let Some(v) = variants.iter().find(|v| {
                        crate::schema::union_variant_constructor_symbol(v) == Some(ctor_label)
                    }) {
                        return Some((cap, v));
                    }
                }
            }
        }
        None
    }

    fn normalize_standalone_union_ctor_fields(
        &self,
        ctor_label: &str,
        raw_map: indexmap::IndexMap<String, Value>,
    ) -> Result<indexmap::IndexMap<String, Value>, ParseError> {
        let Some((cap, variant)) = self.find_union_variant_for_ctor_label(ctor_label) else {
            return Ok(raw_map);
        };
        let entry_id = self
            .pending_session_catalog_entry_id
            .as_deref()
            .unwrap_or("");
        let cap_label = format!("{entry_id}/{}.{}", cap.domain, cap.name);
        let hint =
            self.sym_map
                .cap_param_syms_hint(entry_id, cap.domain.as_str(), cap.name.as_str());
        let mut out = indexmap::IndexMap::new();
        for (raw_key, val) in raw_map {
            let resolved = if variant.fields.iter().any(|f| f.name == raw_key) {
                raw_key
            } else if crate::symbol_tuning::SymbolMap::is_opaque_p_sym(&raw_key) {
                self.sym_map
                    .resolve_cap_param(
                        CatalogScope::qualified(entry_id),
                        cap.domain.as_str(),
                        cap.name.as_str(),
                        &raw_key,
                        cap,
                    )
                    .map_err(|e| {
                        self.err(ParseErrorKind::Other {
                            message: format!("{e} on `{cap_label}`{hint}"),
                        })
                    })?
            } else {
                raw_key
            };
            let leaf =
                Self::union_ctor_leaf_field_key(resolved.as_str(), variant, variant.name.as_str());
            if out.insert(leaf.clone(), val).is_some() {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "duplicate union constructor field `{leaf}` in `{ctor_label}`"
                    ),
                }));
            }
        }
        Ok(out)
    }

    fn err(&self, kind: ParseErrorKind) -> ParseError {
        ParseError {
            kind,
            offset: self.pos,
        }
    }

    fn remaining(&self) -> &str {
        &self.input[self.pos..]
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let ch = self.remaining().chars().next()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn expect_char(&mut self, c: char) -> Result<(), ParseError> {
        self.skip_ws();
        match self.consume_char() {
            Some(got) if got == c => Ok(()),
            Some(got) => Err(self.err(ParseErrorKind::ExpectedChar {
                expected: c,
                got: Some(got),
            })),
            None => Err(self.err(ParseErrorKind::ExpectedChar {
                expected: c,
                got: None,
            })),
        }
    }

    fn try_consume(&mut self, s: &str) -> bool {
        self.skip_ws();
        if self.remaining().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    /// Parse an identifier: ASCII alphanumeric + underscore, starting with letter or underscore.
    fn parse_ident(&mut self) -> Result<String, ParseError> {
        Ok(self.parse_ident_with_span()?.0)
    }

    /// Same as [`Self::parse_ident`], plus byte span `(start, end)` of the identifier in `input`.
    fn parse_ident_with_span(&mut self) -> Result<(String, usize, usize), ParseError> {
        self.skip_ws();
        let start = self.pos;
        let bytes = self.input.as_bytes();
        if self.pos >= bytes.len()
            || (!bytes[self.pos].is_ascii_alphabetic() && bytes[self.pos] != b'_')
        {
            return Err(self.err(ParseErrorKind::ExpectedIdentifier));
        }
        while self.pos < bytes.len()
            && (bytes[self.pos].is_ascii_alphanumeric() || bytes[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let end = self.pos;
        Ok((self.input[start..end].to_string(), start, end))
    }

    /// True when the next tokens look like `ident =` (compound `k=v` map vs unary inner value).
    fn peek_compound_key_value_form(&mut self) -> bool {
        let save = self.pos;
        let looks = (|| {
            self.skip_ws();
            let bytes = self.input.as_bytes();
            if self.pos >= bytes.len()
                || !(bytes[self.pos].is_ascii_alphabetic() || bytes[self.pos] == b'_')
            {
                return false;
            }
            if self.parse_ident_with_span().is_err() {
                return false;
            }
            self.skip_ws();
            self.peek_char() == Some('=')
                && self.input.as_bytes().get(self.pos + 1).copied() != Some(b'=')
        })();
        self.pos = save;
        looks
    }

    /// Parse `k=v, ...` until closing `)`; consumes `)`. Values use full [`Self::parse_value`] so nested
    /// entity constructors are accepted. Keys must match `ent.key_vars` exactly (no extras, no omissions).
    fn parse_strict_compound_key_value_map(
        &mut self,
        head: &crate::expr_parser::entity_ref_parse::EntityCtorHead,
        ent: &EntityDef,
    ) -> Result<IndexMap<String, Value>, ParseError> {
        let display_entity = head.canonical.as_str();
        let mut parts: IndexMap<String, Value> = IndexMap::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some(')') {
                break;
            }
            let (raw_key, _, _) = self.parse_ident_with_span()?;
            let key = self.normalize_compound_ctor_key(head, ent, &raw_key)?;
            if parts.contains_key(&key) {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "duplicate key `{raw_key}` in compound constructor for `{display_entity}`"
                    ),
                }));
            }
            self.skip_ws();
            if self.peek_char() != Some('=') {
                return Err(self.err(ParseErrorKind::ExpectedChar {
                    expected: '=',
                    got: self.peek_char(),
                }));
            }
            self.pos += 1;
            let val = self.parse_compound_key_value_rhs()?;
            parts.insert(key, val);
            self.skip_ws();
            if self.peek_char() == Some(')') {
                break;
            }
            if self.peek_char() == Some(',') {
                self.pos += 1;
                continue;
            }
            return Err(self.err(ParseErrorKind::Other {
                message: "expected `,` or `)` after key=value in compound constructor".into(),
            }));
        }
        self.expect_char(')')?;
        let expected: BTreeSet<String> = ent
            .key_vars
            .iter()
            .map(|k| k.as_str().to_string())
            .collect();
        let got: BTreeSet<String> = parts.keys().cloned().collect();
        if expected != got {
            return Err(self.err(ParseErrorKind::Other {
                message: format!(
                    "compound constructor for `{display_entity}` must supply exactly keys {:?}, got {:?}",
                    ent.key_vars, got
                ),
            }));
        }
        Ok(parts)
    }

    /// Shadow/repair sugar (normative, deliberately untaught): `Entity(id_field=value)` on simple-id
    /// entities ≡ canonical `Entity(value)`.
    ///
    /// Rejects wrong keys and multi-key maps (compound entities use [`Self::parse_strict_compound_key_value_map`]).
    fn try_parse_simple_id_field_get_sugar(
        &mut self,
        entity: &str,
        ent: &EntityDef,
    ) -> Result<Option<Expr>, ParseError> {
        if !ent.key_vars.is_empty() {
            return Ok(None);
        }
        let save = self.pos;
        if !self.peek_compound_key_value_form() {
            return Ok(None);
        }
        let (key, _, _) = self.parse_ident_with_span()?;
        self.skip_ws();
        if self.peek_char() != Some('=') {
            self.pos = save;
            return Ok(None);
        }
        self.pos += 1;
        let id_val = if self.program_nodes.is_some() {
            self.parse_dotted_call_arg_value_rhs()?
        } else {
            self.parse_compound_key_value_rhs()?
        };
        self.skip_ws();
        if self.peek_char() == Some(',') {
            self.pos = save;
            return Ok(None);
        }
        if self.peek_char() != Some(')') {
            self.pos = save;
            return Ok(None);
        }
        self.expect_char(')')?;
        let wire_key = self
            .sym_map
            .resolve_entity_field(
                self.catalog_scope_for_entity(entity)?,
                entity,
                ent,
                key.as_str(),
            )
            .unwrap_or_else(|_| key.clone());
        if wire_key != ent.id_field.as_str() {
            return Err(self.err(ParseErrorKind::Other {
                message: format!(
                    "entity `{entity}` uses a simple id; `{key}=…` is only accepted when `{key}` is the identity field `{}` — otherwise use `{entity}(value)`",
                    ent.id_field
                ),
            }));
        }
        if let Value::PlasmInputRef(r) = id_val {
            return Ok(Some(Expr::Get(GetExpr::from_ref(Ref::simple_binding(
                entity, r,
            )))));
        }
        let id_str = self.identity_literal_from_value(&id_val)?;
        Ok(Some(Expr::Get(GetExpr::new(entity, id_str))))
    }

    /// `Entity(id_field == value)` or `Entity(p# == value)` when `p#` resolves to `id_field`.
    fn try_parse_id_field_eq_get_in_parens(
        &mut self,
        entity: &str,
        ent: &EntityDef,
    ) -> Result<Option<Expr>, ParseError> {
        if !ent.key_vars.is_empty() {
            return Ok(None);
        }
        let save = self.pos;
        let (field, _, _) = match self.parse_ident_with_span() {
            Ok(v) => v,
            Err(_) => {
                self.pos = save;
                return Ok(None);
            }
        };
        self.skip_ws();
        if self.peek_char() != Some('=')
            || self.input.as_bytes().get(self.pos + 1).copied() != Some(b'=')
        {
            self.pos = save;
            return Ok(None);
        }
        self.pos += 2;
        self.skip_ws();
        let id_val = if self.program_nodes.is_some() {
            self.parse_predicate_rhs_after_op()?
        } else {
            self.parse_value()?
        };
        self.skip_ws();
        if self.peek_char() != Some(')') {
            self.pos = save;
            return Ok(None);
        }
        self.expect_char(')')?;
        let wire_field = self
            .sym_map
            .resolve_entity_field(
                self.catalog_scope_for_entity(entity)?,
                entity,
                ent,
                field.as_str(),
            )
            .unwrap_or(field);
        if wire_field != ent.id_field.as_str() {
            self.pos = save;
            return Ok(None);
        }
        if let Value::PlasmInputRef(r) = id_val {
            return Ok(Some(Expr::Get(GetExpr::from_ref(Ref::simple_binding(
                entity, r,
            )))));
        }
        let id_str = self.identity_literal_from_value(&id_val)?;
        Ok(Some(Expr::Get(GetExpr::new(entity, id_str))))
    }

    /// Like [`Self::try_parse_simple_id_field_get_sugar`] but returns the inner id [`Value`] for nested constructors.
    fn try_parse_simple_id_field_constructor_sugar(
        &mut self,
        entity_canon: &str,
        ent: &EntityDef,
    ) -> Result<Option<Value>, ParseError> {
        if !ent.key_vars.is_empty() {
            return Ok(None);
        }
        let save = self.pos;
        if !self.peek_compound_key_value_form() {
            return Ok(None);
        }
        let (key, _, _) = self.parse_ident_with_span()?;
        self.skip_ws();
        if self.peek_char() != Some('=') {
            self.pos = save;
            return Ok(None);
        }
        self.pos += 1;
        let id_val = self.parse_compound_key_value_rhs()?;
        self.skip_ws();
        if self.peek_char() == Some(',') {
            self.pos = save;
            return Ok(None);
        }
        if self.peek_char() != Some(')') {
            self.pos = save;
            return Ok(None);
        }
        self.expect_char(')')?;
        // Thread the session symbol map exactly like the top-level get sugar
        // (`try_parse_simple_id_field_get_sugar`): a `p#` token that resolves to the entity's
        // identity field is a legal keyed identity get. Comparing the raw `key` here rejected
        // `Team(p76=key)` even though `p76` resolves to `key`.
        let wire_key = self
            .sym_map
            .resolve_entity_field(
                self.catalog_scope_for_entity(entity_canon)?,
                entity_canon,
                ent,
                key.as_str(),
            )
            .unwrap_or_else(|_| key.clone());
        if wire_key != ent.id_field.as_str() {
            return Err(self.err(ParseErrorKind::Other {
                message: format!(
                    "entity `{entity_canon}` uses a simple id; `{key}=…` is only accepted when `{key}` is the identity field `{}` — otherwise use `{entity_canon}(value)`",
                    ent.id_field
                ),
            }));
        }
        Ok(Some(id_val))
    }

    /// Serialize one compound-get path slot for [`EntityKey::Compound`] (string map); nested
    /// [`Value::Object`] constructors become deterministic JSON text.
    fn compound_get_slot_string_from_value(&self, v: &Value) -> Result<String, ParseError> {
        match v {
            Value::String(s) => Ok(s.clone()),
            Value::Integer(n) => Ok(n.to_string()),
            Value::Float(_) => Err(self.err(ParseErrorKind::Other {
                message:
                    "IEEE float is not an identity literal; use quoted digits or an exact integer"
                        .into(),
            })),
            Value::Object(_) => serde_json::to_string(v).map_err(|e| {
                self.err(ParseErrorKind::Other {
                    message: format!("compound get slot must be JSON-serializable: {e}"),
                })
            }),
            _ => Err(self.err(ParseErrorKind::IdMustBeStringOrNumber)),
        }
    }

    /// Identity GET / entity-ref constructor slot: accept program-mode [`Value::PhraseIdent`]
    /// as a literal id unless it shadows an in-scope program binding.
    fn identity_literal_from_value(&self, v: &Value) -> Result<String, ParseError> {
        if let Value::PhraseIdent(ident) = v {
            if let Some(labels) = self.program_nodes {
                if labels.contains(ident.as_str()) {
                    return Err(self.err(ParseErrorKind::Other {
                        message: format!(
                            "`{ident}` names a program binding in this plan — use `{ident}` or `{ident}.<field>` as a binding reference, not an unquoted literal"
                        ),
                    }));
                }
            }
            return Ok(ident.clone());
        }
        self.compound_get_slot_string_from_value(v)
    }

    /// Lower [`Value::PhraseIdent`] leaves to [`Value::String`] for nested entity-ref constructors.
    pub(super) fn normalize_identity_constructor_value(
        &self,
        v: Value,
    ) -> Result<Value, ParseError> {
        match v {
            Value::PhraseIdent(ident) => {
                if let Some(labels) = self.program_nodes {
                    if labels.contains(ident.as_str()) {
                        return Err(self.err(ParseErrorKind::Other {
                            message: format!(
                                "`{ident}` names a program binding in this plan — use `{ident}` or `{ident}.<field>` as a binding reference, not an unquoted literal"
                            ),
                        }));
                    }
                }
                Ok(Value::String(ident))
            }
            Value::Object(map) => {
                let mut out = indexmap::IndexMap::new();
                for (k, val) in map {
                    out.insert(k, self.normalize_identity_constructor_value(val)?);
                }
                Ok(Value::Object(out))
            }
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.normalize_identity_constructor_value(item)?);
                }
                Ok(Value::Array(out))
            }
            other => Ok(other),
        }
    }

    /// Wire-only legacy entry — prefer [`Self::try_parse_entity_ref_value`] for session `e#` heads.
    pub(super) fn parse_entity_constructor_value_after_open_paren(
        &mut self,
        entity_canon: &str,
    ) -> Result<Value, ParseError> {
        use entity_ref_parse::{EntityCtorHead, EntityRefRhsMode};
        let head = EntityCtorHead::new(
            entity_canon,
            self.catalog_entry_id_for_entity(entity_canon)
                .ok()
                .flatten()
                .filter(|id| !id.is_empty())
                .map(|s| s.to_string()),
        );
        self.parse_entity_ref_value_after_open_paren(&head, EntityRefRhsMode::Strict)
    }

    /// Parse method / nav segment after `.` — ASCII alnum + `_` + `-` (hyphens allowed for `get-me` style).
    fn parse_method_label_with_span(&mut self) -> Result<(String, usize, usize), ParseError> {
        self.skip_ws();
        let start = self.pos;
        let bytes = self.input.as_bytes();
        if self.pos >= bytes.len()
            || (!bytes[self.pos].is_ascii_alphabetic() && bytes[self.pos] != b'_')
        {
            return Err(self.err(ParseErrorKind::ExpectedIdentifier));
        }
        self.pos += 1;
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let end = self.pos;
        Ok((self.input[start..end].to_string(), start, end))
    }

    fn resolve_zero_arity_pipeline_cap(
        &self,
        entity: &str,
        label: &str,
    ) -> Result<String, ParseError> {
        let mut matches: Vec<String> = Vec::new();
        for kind in [
            CapabilityKind::Action,
            CapabilityKind::Update,
            CapabilityKind::Delete,
            CapabilityKind::Get,
            CapabilityKind::Create,
        ] {
            for cap in self
                .cgs_for_entity_required(entity)?
                .find_capabilities(entity, kind)
            {
                if matches!(kind, CapabilityKind::Get) && cap.requires_receiver() {
                    continue;
                }
                if !capability_is_zero_arity_invoke(cap) {
                    continue;
                }
                if capability_path_method_segment(cap).as_str() == label {
                    matches.push(cap.name.to_string());
                }
            }
        }
        matches.sort();
        matches.dedup();
        match matches.len() {
            0 => Err(self.err(ParseErrorKind::NoZeroArityMethod {
                entity: entity.to_string(),
                label: label.to_string(),
            })),
            1 => Ok(matches.into_iter().next().unwrap()),
            _ => Err(self.err(ParseErrorKind::AmbiguousZeroArityMethod {
                entity: entity.to_string(),
                label: label.to_string(),
                capability_names: matches,
            })),
        }
    }

    /// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (hex digits), case-insensitive.
    fn try_consume_standard_uuid(&mut self) -> Option<String> {
        let start = self.pos;
        let bytes = self.input.as_bytes();
        let mut i = start;
        let groups = [8usize, 4, 4, 4, 12];
        for (gi, glen) in groups.iter().enumerate() {
            for _ in 0..*glen {
                if i >= bytes.len() || !bytes[i].is_ascii_hexdigit() {
                    self.pos = start;
                    return None;
                }
                i += 1;
            }
            if gi + 1 < groups.len() {
                if i >= bytes.len() || bytes[i] != b'-' {
                    self.pos = start;
                    return None;
                }
                i += 1;
            }
        }
        let s = self.input[start..i].to_string();
        self.pos = i;
        Some(s)
    }

    /// Resolve entity field or capability param typing (including [`ValueWireFormat`] for dates).
    fn lookup_field_typing(
        &self,
        entity_name: &str,
        field: &str,
    ) -> Option<(FieldType, Option<ValueWireFormat>, Option<ArrayItemsSchema>)> {
        let ec = self.cgs_for_entity(entity_name).or_else(|| {
            if self.layers_stack().len() == 1 {
                Some(self.primary_cgs())
            } else {
                None
            }
        })?;
        for kind in [CapabilityKind::Query, CapabilityKind::Search] {
            for cap in ec.find_capabilities(entity_name, kind) {
                if let Some(f) = cap.query_surface_fields().find(|f| f.name == field) {
                    let nv = f.named_value(ec).ok()?;
                    return Some((
                        nv.field_type.clone(),
                        nv.value_format,
                        nv.array_items.clone(),
                    ));
                }
            }
        }
        if let Some(ent) = ec.get_entity(entity_name) {
            if let Some(fs) = ent.fields.get(field) {
                let nv = fs.named_value(ec).ok()?;
                return Some((
                    nv.field_type.clone(),
                    nv.value_format,
                    nv.array_items.clone(),
                ));
            }
        }
        None
    }

    /// Comma-separated `key=value` inside `(` … `)` for dotted-call create/update/invoke (see module `//!`).
    /// Optional-parameter teaching form: `(..)` or `(k=v,..)` — `..` adds no keys.
    fn parse_paren_object_arg_list(&mut self) -> Result<IndexMap<String, Value>, ParseError> {
        let mut map = IndexMap::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some(')') {
                break;
            }
            if self.try_consume_double_dot() {
                self.skip_ws();
                if self.peek_char() != Some(')') {
                    return Err(self.err(ParseErrorKind::Other {
                        message: "expected `)` after `..` in argument list".into(),
                    }));
                }
                break;
            }
            let (key, _, _) = self.parse_ident_with_span()?;
            self.skip_ws();
            if self.peek_char() != Some('=') {
                return Err(self.err(ParseErrorKind::ExpectedChar {
                    expected: '=',
                    got: self.peek_char(),
                }));
            }
            self.pos += 1;
            let val = self.parse_dotted_call_arg_value_rhs()?;
            map.insert(key, val);
            self.skip_ws();
            if self.peek_char() == Some(')') {
                break;
            }
            if self.peek_char() == Some(',') {
                self.pos += 1;
                self.skip_ws();
                if self.try_consume_double_dot() {
                    self.skip_ws();
                    if self.peek_char() != Some(')') {
                        return Err(self.err(ParseErrorKind::Other {
                            message: "expected `)` after `,..` in argument list".into(),
                        }));
                    }
                    break;
                }
                continue;
            }
            return Err(self.err(ParseErrorKind::Other {
                message: "expected `,` or `)` after `key=value` in argument list".into(),
            }));
        }
        Ok(map)
    }

    /// Consumes `..` if present (teaching table optional-parameter ellipsis).
    fn try_consume_double_dot(&mut self) -> bool {
        let b = self.input.as_bytes();
        if b.get(self.pos) == Some(&b'.') && b.get(self.pos + 1) == Some(&b'.') {
            self.pos += 2;
            true
        } else {
            false
        }
    }

    /// Shared InvokeArg coerce over a concrete field list (object body lanes or union variant).
    ///
    /// Call with [`CapabilitySchema::invocation_object_fields`] for create/invoke object bodies
    /// (`payload` ∪ `arguments`), or with a union variant's fields for ctor coerce. Scalar /
    /// array / temporal / money / boolean / stringish PhraseIdent rules match query filters;
    /// only array scalar-wrap differs by [`ArrayFieldCoercionPolicy::InvokeArg`]. Inline nested
    /// object wires are skipped (nested coerce is not this boundary).
    fn coerce_registry_input_fields<'f, I>(
        &self,
        domain: &str,
        catalog_entry_id: Option<&str>,
        fields: I,
        map: &mut IndexMap<String, Value>,
    ) -> Result<(), ParseError>
    where
        I: IntoIterator<Item = &'f crate::InputFieldSchema>,
    {
        let ec = self.cgs_for_entity_with_preferred_catalog(domain, catalog_entry_id)?;
        for f in fields {
            if let Some(v) = map.get_mut(&f.name) {
                if matches!(&f.wire, crate::InputFieldWire::Inline(_)) {
                    continue;
                }
                let old = std::mem::replace(v, Value::Null);
                let nv = f.named_value(ec).map_err(|_| {
                    self.err(ParseErrorKind::Other {
                        message: format!("internal: unknown value_ref for parameter `{}`", f.name),
                    })
                })?;
                // Preserve program binding diagnostics before string coercion erases the
                // distinction between an unquoted identifier and a quoted literal.
                if let (Some(labels), Value::PhraseIdent(ident)) = (self.program_nodes, &old) {
                    if matches!(
                        nv.field_type,
                        FieldType::String
                            | FieldType::Blob
                            | FieldType::Uuid
                            | FieldType::DigitId
                            | FieldType::Json
                    ) {
                        crate::phrase_ident::validate_identifier_phrase(
                            ident,
                            labels,
                            Some(&crate::phrase_ident::PhraseIdentFieldContext {
                                field_type: &nv.field_type,
                                allowed_values: nv.allowed_values.as_deref(),
                            }),
                        )
                        .map_err(|message| self.err(ParseErrorKind::Other { message }))?;
                    }
                }
                let array_ref = nv.array_items.as_ref();
                *v = coerce_value_for_field_type_with_policy(
                    &nv.field_type,
                    nv.value_format,
                    array_ref,
                    old,
                    ArrayFieldCoercionPolicy::InvokeArg,
                )
                .map_err(|m| {
                    if matches!(nv.field_type, FieldType::Date) {
                        self.err(ParseErrorKind::InvalidTemporalValue { message: m })
                    } else {
                        self.err(ParseErrorKind::Other { message: m })
                    }
                })?;
            }
        }
        Ok(())
    }

    /// Catalog-scoped CGS layers for mutator resolution under federation (`e#` stamp or `m#` triple).
    fn layers_for_dotted_call_source(
        &self,
        source: &Expr,
        raw_label: Option<&str>,
    ) -> Vec<&'a CGS> {
        let primary = source.primary_entity();
        if let Ok(eid) = crate::catalog_ownership::catalog_entry_id_for_invoke(
            source,
            raw_label,
            self.sym_map.as_ref(),
            crate::catalog_ownership::InvokeCatalogResolutionContext {
                pending_session_catalog_entry_id: self.pending_session_catalog_entry_id.as_deref(),
            },
        ) {
            if let Some(cgs) = self.cgs_for_catalog_entry_id(eid.as_str(), primary) {
                return vec![cgs];
            }
        }
        self.cgs_layers().collect::<Vec<_>>()
    }

    fn stamp_session_catalog_from_source(source: &Expr, expr: Expr) -> Expr {
        let stamped = expr
            .with_session_catalog_entry_id(source.session_catalog_entry_id().map(|id| id.as_str()));
        if let Expr::Create(mut c) = stamped {
            if c.catalog_entry_id.is_none() {
                if let Some(id) = source.session_catalog_entry_id() {
                    c.catalog_entry_id = CatalogEntryStamp::some(id.clone());
                }
            }
            Expr::Create(c)
        } else {
            stamped
        }
    }

    /// Resolve Create / Update / Action / Delete with object input.
    /// Opaque session `m#` resolves only via the forward method binding table — no CGS scan fallback.
    ///
    /// Read kinds (Query / Search / Get) are rejected here so a wrong `m#` names the
    /// taught read seat instead of a param error on the bound query. Zero-arity
    /// `e#.m#()` Gets still resolve via [`Self::parse_zero_arity_invoke`] (pathless gets).
    fn resolve_dotted_call_capability(
        &self,
        label: &str,
        raw_label: Option<&str>,
        source: &Expr,
    ) -> Result<&crate::CapabilitySchema, ParseError> {
        if let Some(raw) = raw_label {
            if crate::symbol_tuning::SymbolMap::is_opaque_m_sym(raw) {
                let cap = self
                    .try_resolve_opaque_invoke_capability(source, raw)
                    .expect("opaque m# always attempts session binding")?;
                if matches!(
                    cap.kind,
                    CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
                ) {
                    let entity = source.primary_entity();
                    let eid = source
                        .session_catalog_entry_id()
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let es = self.sym_map.entity_sym_for(eid, entity);
                    let cgs = self.cgs_for_entity_required(entity).ok();
                    return Err(self.err(ParseErrorKind::Other {
                        message: opaque_read_kind_invoke_note(raw, cap, &es, cgs),
                    }));
                }
                return Ok(cap);
            }
            if let Some(res) = self.try_resolve_opaque_invoke_capability(source, raw) {
                return res;
            }
        }
        let primary = source.primary_entity();
        let scoped_layers = self.layers_for_dotted_call_source(source, raw_label);
        let same_domain: Vec<_> = scoped_layers
            .iter()
            .flat_map(|c| c.capabilities.values())
            .filter(|cap| {
                capability_path_method_segment(cap).as_str() == label
                    && matches!(
                        cap.kind,
                        CapabilityKind::Create
                            | CapabilityKind::Update
                            | CapabilityKind::Action
                            | CapabilityKind::Delete
                    )
                    && cap.domain.as_str() == primary
            })
            .collect();
        if same_domain.len() == 1 {
            return Ok(same_domain[0]);
        }
        if same_domain.len() > 1 {
            return Err(self.err(ParseErrorKind::DottedCallAmbiguous {
                anchor_entity: primary.to_string(),
                label: label.to_string(),
            }));
        }

        let cross: Vec<_> = scoped_layers
            .iter()
            .flat_map(|c| c.capabilities.values())
            .filter(|cap| {
                capability_path_method_segment(cap).as_str() == label
                    && cap.kind == CapabilityKind::Create
            })
            .collect();
        if cross.len() == 1 && self.can_bind_create_receiver(cross[0], source) {
            return Ok(cross[0]);
        }
        if cross.len() > 1 {
            return Err(self.err(ParseErrorKind::DottedCreateAmbiguous {
                anchor_entity: primary.to_string(),
                label: label.to_string(),
            }));
        }

        Err(self.err(ParseErrorKind::DottedCallNoMatch {
            anchor_entity: primary.to_string(),
            label: label.to_string(),
        }))
    }

    fn can_bind_create_receiver(&self, cap: &crate::CapabilitySchema, source: &Expr) -> bool {
        let Expr::Get(g) = source else {
            return false;
        };
        let Ok(cgs) = self.cgs_for_expr_source(source) else {
            return false;
        };
        let Some(anchor) = cgs.get_entity(g.reference.entity_type.as_str()) else {
            return false;
        };
        crate::create_binds_from_anchor_identity(cap, anchor, Some(&g.reference))
    }

    /// Build Create / Update / Action / Delete from dotted-call args after `resolve_dotted_call_capability`.
    fn finish_dotted_call_with_payload(
        &mut self,
        source: Expr,
        field_raw: String,
        mut map: IndexMap<String, Value>,
    ) -> Result<Expr, ParseError> {
        let label = self.normalize_method_symbol_label(&field_raw);
        let cap = self.resolve_dotted_call_capability(&label, Some(field_raw.as_str()), &source)?;
        map =
            self.normalize_invoke_arg_keys_for_cap(cap, &source, Some(field_raw.as_str()), map)?;
        map = self.normalize_invoke_union_ctor_args_in_object(
            cap,
            &source,
            Some(field_raw.as_str()),
            map,
        )?;
        self.coerce_registry_input_fields(
            cap.domain.as_str(),
            self.active_catalog_entry_id(Some(&source)).as_deref(),
            cap.invocation_object_fields(),
            &mut map,
        )?;
        let needs_explicit_anchor = cap.requires_receiver();
        let taught_seat = self.taught_explicit_anchor_seat_for(&source, cap);
        let input = Value::Object(map);
        self.finish_dotted_call_with_payload_value_inner(
            source,
            needs_explicit_anchor,
            cap.name.clone(),
            cap.domain.clone(),
            cap.kind,
            field_raw.as_str(),
            taught_seat,
            input,
        )
    }

    /// Dotted call whose payload is already a single [`Value`] (e.g. root `input_schema` union `v#{{…}}`).
    fn finish_dotted_call_with_payload_value(
        &mut self,
        source: Expr,
        field_raw: String,
        mut value: Value,
    ) -> Result<Expr, ParseError> {
        let label = self.normalize_method_symbol_label(&field_raw);
        let cap = self.resolve_dotted_call_capability(&label, Some(field_raw.as_str()), &source)?;
        let Some(is) = &cap.inputs.payload else {
            return Err(self.err(ParseErrorKind::Other {
                message: "this capability has no payload schema — use `key=value` arguments".into(),
            }));
        };
        if !matches!(&is.input_type, crate::InputType::Union { .. }) {
            return Err(self.err(ParseErrorKind::Other {
                message: "this capability expects `key=value` arguments, not a sole `v#{{…}}` constructor"
                    .into(),
            }));
        }
        if let Value::UnionCtor {
            ctor_label,
            ctor_fields,
            ..
        } = &mut value
        {
            if let Some(is) = &cap.inputs.payload {
                if let InputType::Union { variants } = &is.input_type {
                    if let Some(variant) = variants.iter().find(|v| {
                        crate::schema::union_variant_constructor_symbol(v)
                            == Some(ctor_label.as_str())
                    }) {
                        let normalized = self.normalize_invoke_ctor_fields_for_variant(
                            cap,
                            &source,
                            Some(field_raw.as_str()),
                            variant.name.as_str(),
                            variant,
                            std::mem::take(ctor_fields),
                        )?;
                        *ctor_fields = normalized;
                        self.coerce_registry_input_fields(
                            cap.domain.as_str(),
                            self.active_catalog_entry_id(Some(&source)).as_deref(),
                            variant.fields.iter(),
                            ctor_fields,
                        )?;
                    }
                }
            }
        } else {
            return Err(self.err(ParseErrorKind::Other {
                message: "expected `v#{{…}}` union constructor for this invoke".into(),
            }));
        }
        let needs_explicit_anchor = cap.requires_receiver();
        let taught_seat = self.taught_explicit_anchor_seat_for(&source, cap);
        self.finish_dotted_call_with_payload_value_inner(
            source,
            needs_explicit_anchor,
            cap.name.clone(),
            cap.domain.clone(),
            cap.kind,
            field_raw.as_str(),
            taught_seat,
            value,
        )
    }

    fn taught_explicit_anchor_seat_for(
        &self,
        source: &Expr,
        cap: &crate::CapabilitySchema,
    ) -> String {
        let cgs = self.cgs_for_entity_required(source.primary_entity()).ok();
        taught_explicit_anchor_seat(source, cap, self.sym_map.as_ref(), cgs)
    }

    fn err_invoke_requires_taught_seat(
        &self,
        source: &Expr,
        cap: &crate::CapabilitySchema,
        label: &str,
    ) -> ParseError {
        self.err(ParseErrorKind::InvokeRequiresTargetId {
            entity: source.primary_entity().to_string(),
            label: label.to_string(),
            taught_seat: self.taught_explicit_anchor_seat_for(source, cap),
        })
    }

    /// Pathless mutator/get anchor: explicit identity Get or, when the CML template has no path
    /// variables, a synthetic `"0"` Get that **preserves** the receiver's federated catalog stamp.
    ///
    /// Bare teaching heads (`e3.m7(…)`) must not re-validate the wire entity name unqualified —
    /// under homographs that would drop ownership and lie as [`ParseErrorKind::UnknownEntity`].
    fn pathless_mutator_anchor_from_receiver(
        &self,
        source: &Expr,
        needs_explicit_anchor: bool,
        cap: &crate::CapabilitySchema,
        label: &str,
    ) -> Result<GetExpr, ParseError> {
        let taught_seat = self.taught_explicit_anchor_seat_for(source, cap);
        self.coerce_dotted_call_get_anchor_with_seat(
            source,
            needs_explicit_anchor,
            label,
            &taught_seat,
        )
    }

    fn coerce_dotted_call_get_anchor_with_seat(
        &self,
        source: &Expr,
        needs_explicit_anchor: bool,
        label: &str,
        taught_seat: &str,
    ) -> Result<GetExpr, ParseError> {
        if let Expr::Get(g) = source {
            return Ok(g.clone());
        }
        if needs_explicit_anchor {
            return Err(self.err(ParseErrorKind::InvokeRequiresTargetId {
                entity: source.primary_entity().to_string(),
                label: label.to_string(),
                taught_seat: taught_seat.to_string(),
            }));
        }
        let entity = source.primary_entity().to_string();
        let stamp = source.session_catalog_entry_id();
        self.validate_entity_preferring(&entity, stamp.map(|id| id.as_str()))?;
        let mut g = GetExpr::pathless_nullary(entity);
        if let Some(id) = stamp {
            g.catalog_entry_id = CatalogEntryStamp::some(id.clone());
        }
        Ok(g)
    }

    fn finish_dotted_call_with_payload_value_inner(
        &mut self,
        source: Expr,
        needs_explicit_anchor: bool,
        cap_name: CapabilityName,
        cap_domain: EntityName,
        cap_kind: CapabilityKind,
        label: &str,
        taught_seat: String,
        input: Value,
    ) -> Result<Expr, ParseError> {
        match cap_kind {
            CapabilityKind::Create => Ok(Self::stamp_session_catalog_from_source(
                &source,
                Expr::Create(CreateExpr {
                    capability: cap_name,
                    entity: cap_domain,
                    input: InvokeInputPayload::from(input),
                    catalog_entry_id: CatalogEntryStamp::none(),
                    dotted_receiver: Some(Box::new(source.clone())),
                }),
            )),
            CapabilityKind::Delete => {
                let g = self.coerce_dotted_call_get_anchor_with_seat(
                    &source,
                    needs_explicit_anchor,
                    label,
                    &taught_seat,
                )?;
                let mut delete = DeleteExpr::with_target(cap_name, g.reference.clone());
                delete.input = Some(input.into());
                Ok(Self::stamp_session_catalog_from_source(
                    &source,
                    Expr::Delete(delete),
                ))
            }
            CapabilityKind::Update | CapabilityKind::Action => {
                let g = self.coerce_dotted_call_get_anchor_with_seat(
                    &source,
                    needs_explicit_anchor,
                    label,
                    &taught_seat,
                )?;
                Ok(Self::stamp_session_catalog_from_source(
                    &source,
                    Expr::Invoke(InvokeExpr::with_target(
                        cap_name,
                        g.reference.clone(),
                        Some(input),
                    )),
                ))
            }
            _ => Err(self.err(ParseErrorKind::Other {
                message: "internal: dotted-call alias not supported for this capability kind"
                    .into(),
            })),
        }
    }

    fn parse_dotted_call_with_payload(
        &mut self,
        source: Expr,
        label: String,
    ) -> Result<Expr, ParseError> {
        self.skip_ws();
        let label_norm = self.normalize_method_symbol_label(&label);
        let cap =
            self.resolve_dotted_call_capability(&label_norm, Some(label.as_str()), &source)?;
        let root_union = cap
            .inputs
            .payload
            .as_ref()
            .is_some_and(|is| matches!(&is.input_type, InputType::Union { .. }));
        let starts_union_ctor = value::peek_starts_v_numeric_union_ctor_arg(self.input, self.pos);

        if starts_union_ctor {
            if !root_union {
                return Err(self.err(ParseErrorKind::Other {
                    message: "`v` + digits + `{…}` is only allowed as the sole `(…)` payload when the capability root input is a tagged union — use `key=value` arguments"
                        .into(),
                }));
            }
            let val = self.parse_dotted_call_arg_value_rhs()?;
            self.skip_ws();
            if self.peek_char() == Some(',') {
                return Err(self.err(ParseErrorKind::Other {
                    message: "union constructor payload must be the only parenthesized argument (no `,` after `v#{{…}}`)"
                        .into(),
                }));
            }
            self.expect_char(')')?;
            return self.finish_dotted_call_with_payload_value(source, label, val);
        }

        let map = self.parse_paren_object_arg_list()?;
        self.expect_char(')')?;
        self.finish_dotted_call_with_payload(source, label, map)
    }

    /// Parse a comparison operator.
    fn parse_op(&mut self) -> Result<CompOp, ParseError> {
        self.skip_ws();
        if self.try_consume("!=") {
            return Ok(CompOp::Neq);
        }
        if self.try_consume(">=") {
            return Ok(CompOp::Gte);
        }
        if self.try_consume("<=") {
            return Ok(CompOp::Lte);
        }
        if self.try_consume(">") {
            return Ok(CompOp::Gt);
        }
        if self.try_consume("<") {
            return Ok(CompOp::Lt);
        }
        if self.try_consume("=") {
            return Ok(CompOp::Eq);
        }
        if self.try_consume("~") {
            return Ok(CompOp::Contains);
        }
        Err(self.err(ParseErrorKind::ExpectedOperator))
    }

    /// Ensure a query/search `{…}` predicate LHS resolves to an entity row field or a query/search cap param.
    fn validate_query_predicate_wire(
        &self,
        cgs: &CGS,
        entity_name: &str,
        pred_wire: &str,
        pred_field: &str,
        span_start: usize,
        span_end: usize,
    ) -> Result<(), ParseError> {
        if !self.row_schema_fields.is_empty() {
            if self.row_schema_fields.iter().any(|f| f == pred_wire) {
                return Ok(());
            }
            return Err(ParseError {
                kind: ParseErrorKind::RowSchemaFieldNotFound {
                    field: pred_field.to_string(),
                    available: self.row_schema_fields.clone(),
                    span_start,
                    span_end,
                },
                offset: span_start,
            });
        }
        let Some(ent) = cgs.get_entity(entity_name) else {
            return Ok(());
        };
        if ent.fields.contains_key(pred_wire) {
            return Ok(());
        }
        let is_query_or_search_param = [CapabilityKind::Query, CapabilityKind::Search]
            .into_iter()
            .flat_map(|kind| cgs.find_capabilities(entity_name, kind))
            .any(|cap| cap.query_surface_fields().any(|f| f.name == pred_wire));
        if is_query_or_search_param {
            return Ok(());
        }
        let create_only = cgs
            .find_capabilities(entity_name, CapabilityKind::Create)
            .iter()
            .any(|cap| cap.input_fields().any(|f| f.name == pred_wire));
        if create_only {
            return Err(ParseError {
                kind: ParseErrorKind::PredicateFieldNotFound {
                    field: pred_field.to_string(),
                    entity: create_only_predicate_entity_note(cgs, entity_name, pred_wire),
                    span_start,
                    span_end,
                },
                offset: span_start,
            });
        }
        Err(ParseError {
            kind: ParseErrorKind::PredicateFieldNotFound {
                field: pred_field.to_string(),
                entity: entity_name.to_string(),
                span_start,
                span_end,
            },
            offset: span_start,
        })
    }

    /// Parse a single predicate: `field op value` or `foreign.field op value`.
    fn parse_pred(&mut self, entity_name: &str) -> Result<Predicate, ParseError> {
        let (first, span_start, span_end) = self.parse_ident_with_span()?;
        self.skip_ws();

        // Check for dot — cross-entity: `foreign.field op value`
        if self.peek_char() == Some('.') {
            self.pos += 1;
            let field = self.parse_ident()?;
            let op = self.parse_op()?;
            let mut val = self.parse_predicate_rhs_after_op()?;
            let first_ty = self.canonical_entity_name_in_layers(&first);
            if let Some((ft, vf, arr)) = self.lookup_field_typing(&first_ty, &field) {
                if !matches!(val, Value::Null) {
                    val = coerce_value_for_field_type(&ft, vf, arr.as_ref(), val).map_err(|m| {
                        self.err(if matches!(ft, FieldType::Date) {
                            ParseErrorKind::InvalidTemporalValue { message: m }
                        } else {
                            ParseErrorKind::Other {
                                message: format!("invalid value for `{field}` ({ft:?}): {m}"),
                            }
                        })
                    })?;
                }
            }
            // Validate: first should be a foreign.entity prefix resolvable via EntityRef
            // We accept it here — cross_entity module validates at execution time
            return Ok(Predicate::comparison(format!("{first}.{field}"), op, val));
        }

        let pred_field = if entity_name == "List" && first == "list_id" {
            "id".to_string()
        } else {
            first
        };
        let ec = self.cgs_for_entity_required(entity_name)?;
        let pred_wire = if crate::symbol_tuning::SymbolMap::is_opaque_p_sym(pred_field.as_str()) {
            let ent = ec.get_entity(entity_name).ok_or_else(|| {
                self.err(ParseErrorKind::Other {
                    message: format!("entity `{entity_name}` is not defined in catalog"),
                })
            })?;
            self.sym_map
                .resolve_query_filter_field(
                    self.catalog_scope_for_entity(entity_name)?,
                    entity_name,
                    ent,
                    ec,
                    pred_field.as_str(),
                )
                .map_err(|e| {
                    self.err(ParseErrorKind::Other {
                        message: e.to_agent_program_error(),
                    })
                })?
        } else {
            pred_field.clone()
        };

        self.validate_query_predicate_wire(
            ec,
            entity_name,
            pred_wire.as_str(),
            pred_field.as_str(),
            span_start,
            span_end,
        )?;

        let op = self.parse_op()?;
        let mut val = self.parse_predicate_rhs_after_op()?;
        if let Some((ft, vf, arr)) = self.lookup_field_typing(entity_name, &pred_wire) {
            if !matches!(val, Value::Null) && !val.is_domain_example_placeholder() {
                val = coerce_value_for_field_type(&ft, vf, arr.as_ref(), val).map_err(|m| {
                    self.err(if matches!(ft, FieldType::Date) {
                        ParseErrorKind::InvalidTemporalValue { message: m }
                    } else {
                        ParseErrorKind::Other {
                            message: format!("invalid value for `{pred_wire}` ({ft:?}): {m}"),
                        }
                    })
                })?;
            }
        }
        Ok(Predicate::comparison(pred_wire, op, val))
    }

    /// Value after a comparison operator in `{…}` — may be omitted (comma or `}` next) for teaching slots.
    fn parse_predicate_rhs_after_op(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        if matches!(self.peek_char(), Some(',') | Some('}')) {
            Ok(Value::Null)
        } else if let Some(v) = self.try_rewrite_temporal_predicate_rhs()? {
            Ok(v)
        } else {
            self.parse_predicate_value_rhs()
        }
    }

    /// If the upcoming RHS is a Kusto/wire temporal expression, rewrite to a string literal.
    fn try_rewrite_temporal_predicate_rhs(&mut self) -> Result<Option<Value>, ParseError> {
        let start = self.pos;
        let end = scan_top_level_pred_rhs_end(self.input, start);
        if end <= start {
            return Ok(None);
        }
        let raw = self.input[start..end].trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let rewritten = crate::temporal::rewrite_temporal_aliases_in_predicate_body(raw);
        if rewritten.as_ref() == raw {
            return Ok(None);
        }
        self.pos = end;
        // Rewritten surface is either `now` or a quoted English phrase.
        let t = rewritten.trim();
        if t.eq_ignore_ascii_case("now") {
            return Ok(Some(Value::String("now".into())));
        }
        if let Some(inner) = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
            return Ok(Some(Value::String(
                inner.replace("\\\"", "\"").replace("\\\\", "\\"),
            )));
        }
        Ok(Some(Value::String(t.to_string())))
    }

    /// Parse comma-separated predicates inside `{ }`.
    fn parse_preds(&mut self, entity_name: &str) -> Result<Vec<Predicate>, ParseError> {
        let mut preds = Vec::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some('}') {
                break;
            }
            preds.push(self.parse_pred(entity_name)?);
            self.skip_ws();
            if self.peek_char() == Some(',') {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(preds)
    }

    /// Build a QueryExpr from a predicate list (one pred = Comparison, many = And).
    fn preds_to_query(entity: &str, preds: Vec<Predicate>) -> QueryExpr {
        match preds.len() {
            0 => QueryExpr::all(entity),
            1 => QueryExpr::filtered(entity, preds.into_iter().next().unwrap()),
            _ => QueryExpr::filtered(entity, Predicate::and(preds)),
        }
    }

    /// `Issue.search(team_key=ENG, q=auth)` — same capability as `Issue~"auth"{team_key=ENG}`.
    /// Also accepts named search capabilities: `Item.search-by-date(query=…, tags=…)`.
    fn try_parse_entity_dot_search(&mut self, entity: &str) -> Result<Option<Expr>, ParseError> {
        let mark = self.pos;
        if self.peek_char() != Some('.') {
            return Ok(None);
        }
        self.pos += 1;
        self.skip_ws();
        let method = self.parse_ident()?;
        let cap_name = self.resolve_entity_dot_search_cap(entity, &method)?;
        let Some(cap_name) = cap_name else {
            self.pos = mark;
            return Ok(None);
        };
        self.skip_ws();
        if self.peek_char() != Some('(') {
            self.pos = mark;
            return Ok(None);
        }
        self.pos += 1;
        self.skip_ws();
        let preds = if self.peek_char() == Some(')') {
            Vec::new()
        } else {
            self.parse_paren_preds(entity)?
        };
        self.skip_ws();
        self.expect_char(')')?;
        Ok(Some(self.build_search_query_expr(
            entity,
            preds,
            Some(cap_name),
        )?))
    }

    fn resolve_entity_dot_search_cap(
        &self,
        entity: &str,
        method: &str,
    ) -> Result<Option<CapabilityName>, ParseError> {
        let c = self.cgs_for_entity_required(entity)?;
        let caps = c.find_capabilities(entity, CapabilityKind::Search);
        if caps.is_empty() {
            return Ok(None);
        }
        if method == "search" {
            return Ok(c
                .primary_search_capability(entity)
                .map(|cap| cap.name.clone()));
        }
        Ok(caps
            .iter()
            .find(|cap| capability_path_method_segment(cap).as_str() == method)
            .map(|cap| cap.name.clone()))
    }

    fn parse_paren_preds(&mut self, entity_name: &str) -> Result<Vec<Predicate>, ParseError> {
        let mut preds = Vec::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some(')') {
                break;
            }
            preds.push(self.parse_pred(entity_name)?);
            self.skip_ws();
            if self.peek_char() == Some(',') {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(preds)
    }

    fn build_search_query_expr(
        &mut self,
        entity: &str,
        preds: Vec<Predicate>,
        capability_name: Option<CapabilityName>,
    ) -> Result<Expr, ParseError> {
        let c = self.cgs_for_entity_required(entity)?;
        if c.find_capabilities(entity, CapabilityKind::Search)
            .is_empty()
        {
            return Err(self.err(ParseErrorKind::SearchNotSupported {
                entity: entity.to_string(),
            }));
        }
        let cap_name = capability_name.or_else(|| {
            c.find_capabilities(entity, CapabilityKind::Search)
                .first()
                .map(|cap| cap.name.clone())
        });
        let mut query = Self::preds_to_query(entity, preds);
        query.capability_name = cap_name;
        Ok(Expr::Query(query))
    }

    #[allow(dead_code)] // Unqualified facade; stamped receivers use [`Self::validate_entity_preferring`].
    fn validate_entity(&self, name: &str) -> Result<(), ParseError> {
        self.validate_entity_preferring(name, None)
    }

    /// Entity membership check that distinguishes absent vs federated ambiguous wire names.
    ///
    /// When `preferred_entry_id` (or active session catalog) pins a layer, only that catalog is
    /// consulted. Otherwise: unique hit → ok; multi-hit → [`ParseErrorKind::AmbiguousEntityCatalog`];
    /// zero hits → [`ParseErrorKind::UnknownEntity`] (never collapse multi-hit into unknown).
    fn validate_entity_preferring(
        &self,
        name: &str,
        preferred_entry_id: Option<&str>,
    ) -> Result<(), ParseError> {
        if let Some(eid) = preferred_entry_id {
            if self.cgs_for_catalog_entry_id(eid, name).is_some() {
                return Ok(());
            }
        }
        if let Some(eid) = self.active_catalog_entry_id(None) {
            if self.cgs_for_catalog_entry_id(eid.as_str(), name).is_some() {
                return Ok(());
            }
        }
        let match_count = self
            .layers_stack()
            .iter()
            .filter(|layer| layer.cgs().get_entity(name).is_some())
            .count();
        match match_count {
            0 => Err(ParseError {
                kind: ParseErrorKind::UnknownEntity {
                    name: name.to_string(),
                    span_opt: None,
                },
                offset: self.pos,
            }),
            1 => Ok(()),
            _ => Err(self.err(ParseErrorKind::AmbiguousEntityCatalog {
                entity: name.to_string(),
            })),
        }
    }

    /// `Team(id).members` → `Member` query with `team_id` when CGS has no `members` relation on `Team`.
    fn expand_team_members_sugar(
        &mut self,
        source: &Expr,
        field: &str,
        source_entity: &str,
    ) -> Result<Option<Expr>, ParseError> {
        if field != "members" || source_entity != "Team" {
            return Ok(None);
        }
        let Some(team_cgs) = self.cgs_for_entity("Team") else {
            return Ok(None);
        };
        let Some(team_ent) = team_cgs.get_entity("Team") else {
            return Ok(None);
        };
        if team_ent.relations.contains_key("members") {
            return Ok(None);
        }
        let Some(team_id) = extract_primary_id(source) else {
            return Ok(None);
        };
        let member_has_team = team_cgs
            .find_capabilities("Member", CapabilityKind::Query)
            .iter()
            .any(|cap| cap.scope_params().iter().any(|f| f.name == "team_id"));
        if !member_has_team {
            return Ok(None);
        }
        self.skip_ws();
        let mut preds = vec![Predicate::eq("team_id", Value::String(team_id))];
        if self.peek_char() == Some('{') {
            self.pos += 1;
            preds.extend(self.parse_preds("Member")?);
            self.expect_char('}')?;
        }
        Ok(Some(Expr::Query(Self::preds_to_query("Member", preds))))
    }

    /// `Get(Anchor, id).<query-kebab>` when the query capability lives on another domain but scopes
    /// with a single `EntityRef` to `Anchor` (e.g. `Team(42).space-query` → query `Space` with `team_id`).
    ///
    /// When `raw_label` is session `m#`, resolve the catalog capability directly from the symbol map.
    fn try_scoped_query_bridge_from_get(
        &self,
        source: &Expr,
        label: &str,
        raw_label: Option<&str>,
    ) -> Result<Option<Expr>, ParseError> {
        let Expr::Get(g) = source else {
            return Ok(None);
        };
        let anchor_entity = g.reference.entity_type.as_str();
        let anchor_id = g.reference.primary_slot_str();

        let mut matches: Vec<&crate::CapabilitySchema> = Vec::new();
        if let Some(raw) = raw_label {
            if let Some(Ok(cap)) = self.try_resolve_opaque_invoke_capability(source, raw) {
                for c in self.cgs_layers() {
                    if c.get_capability(cap.name.as_str()).is_some()
                        && Self::scoped_query_bridge_matches_anchor(cap, c, anchor_entity)
                    {
                        matches.push(cap);
                        break;
                    }
                }
            }
        }
        if matches.is_empty() {
            for c in self.cgs_layers() {
                for cap in c.capabilities.values() {
                    if cap.kind != CapabilityKind::Query {
                        continue;
                    }
                    if cap.domain.as_str() == anchor_entity {
                        continue;
                    }
                    if capability_path_method_segment(cap).as_str() != label {
                        continue;
                    }
                    let scope_fields: Vec<_> =
                        cap.scope_params().iter().filter(|f| f.required).collect();
                    if scope_fields.len() != 1 {
                        continue;
                    }
                    let sf = scope_fields[0];
                    if let Ok(nv) = sf.named_value(c) {
                        if let FieldType::EntityRef { target, .. } = &nv.field_type {
                            if target.as_str() == anchor_entity {
                                matches.push(cap);
                            }
                        }
                    }
                }
            }
        }
        if matches.is_empty() {
            return Ok(None);
        }
        if matches.len() > 1 {
            return Err(self.err(ParseErrorKind::Other {
                message: format!("ambiguous scoped query `{label}` for anchor `{anchor_entity}`"),
            }));
        }
        let cap = matches[0];
        let scope_name = cap
            .scope_params()
            .iter()
            .find(|f| f.required)
            .map(|f| f.name.as_str())
            .ok_or_else(|| {
                self.err(ParseErrorKind::Other {
                    message: "internal: scoped query missing scope field".into(),
                })
            })?;
        let preds = vec![Predicate::eq(scope_name, Value::String(anchor_id))];
        let mut q = Self::preds_to_query(cap.domain.as_str(), preds);
        q.capability_name = Some(cap.name.clone());
        Ok(Some(Expr::Query(q)))
    }

    fn parse_decimal_usize(&mut self) -> Result<usize, ParseError> {
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        let raw = &self.input[start..self.pos];
        if raw.is_empty() {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        raw.parse::<usize>().map_err(|_| {
            self.err(ParseErrorKind::InvalidInteger {
                raw: raw.to_string(),
            })
        })
    }

    fn parse_page_invocation(&mut self) -> Result<Expr, ParseError> {
        self.expect_char('(')?;
        self.skip_ws();
        let handle_raw = self.parse_continuation_handle_operand()?;
        let handle = crate::PagingHandle::parse(&handle_raw).map_err(|e| {
            self.err(ParseErrorKind::Other {
                message: e.to_string(),
            })
        })?;
        self.skip_ws();
        let mut limit = None;
        if self.peek_char() == Some(',') {
            self.pos += 1;
            self.skip_ws();
            let key = self.parse_ident()?;
            if key != "limit" {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "page(...) only accepts optional `limit=N` (unexpected `{key}`)"
                    ),
                }));
            }
            self.expect_char('=')?;
            self.skip_ws();
            limit = Some(self.parse_decimal_usize()?);
        }
        self.skip_ws();
        self.expect_char(')')?;
        Ok(Expr::Page(PageExpr { handle, limit }))
    }

    /// Wire handle operand for `wait` / `cancel` / `page`: allows `-` in base64url `l_<token>_oN`.
    fn is_continuation_handle_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_' || c == '-'
    }

    fn parse_continuation_handle_operand(&mut self) -> Result<String, ParseError> {
        self.skip_ws();
        let start = self.pos;
        while let Some(c) = self.peek_char() {
            if !Self::is_continuation_handle_char(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        if self.pos == start {
            return Err(self.err(ParseErrorKind::ExpectedIdentifier));
        }
        Ok(self.input[start..self.pos].to_string())
    }

    fn parse_operation_handle_invocation(
        &mut self,
        kind: &'static str,
    ) -> Result<Expr, ParseError> {
        self.expect_char('(')?;
        self.skip_ws();
        let handle_raw = self.parse_continuation_handle_operand()?;
        let handle = crate::OperationHandle::parse(&handle_raw).map_err(|e| {
            self.err(ParseErrorKind::Other {
                message: e.to_string(),
            })
        })?;
        self.skip_ws();
        self.expect_char(')')?;
        if kind == "wait" {
            Ok(Expr::Wait(crate::WaitExpr { handle }))
        } else {
            Ok(Expr::Cancel(crate::CancelExpr { handle }))
        }
    }

    fn parse_wait_invocation(&mut self) -> Result<Expr, ParseError> {
        self.parse_operation_handle_invocation("wait")
    }

    fn parse_cancel_invocation(&mut self) -> Result<Expr, ParseError> {
        self.parse_operation_handle_invocation("cancel")
    }

    fn parse_source(&mut self) -> Result<Expr, ParseError> {
        let (raw, span_start, span_end) = self.parse_ident_with_span()?;
        if raw == "page" {
            self.skip_ws();
            if self.peek_char() == Some('(') {
                return self.parse_page_invocation();
            }
        }
        if raw == "wait" {
            self.skip_ws();
            if self.peek_char() == Some('(') {
                return self.parse_wait_invocation();
            }
        }
        if raw == "cancel" {
            self.skip_ws();
            if self.peek_char() == Some('(') {
                return self.parse_cancel_invocation();
            }
        }
        self.skip_ws();
        if is_root_union_ctor_surface_label(&raw) && self.peek_char() == Some('{') {
            self.pos += 1;
            let obj = self.parse_brace_kv_object_map()?;
            self.expect_char('}')?;
            let obj = self.normalize_standalone_union_ctor_fields(raw.as_str(), obj)?;
            return Ok(Expr::TeachingValue {
                value: Value::UnionCtor {
                    ctor_label: raw,
                    ctor_fields: obj,
                },
            });
        }
        let mut entity: Option<String> = self.sym_map.resolve_session_entity_symbol(&raw);
        let entity_from_sym = entity.is_some().then(|| raw.clone());
        if entity.is_none() {
            for c in self.cgs_layers() {
                let e = c.canonical_entity_name(&raw).unwrap_or_else(|| raw.clone());
                if c.get_entity(&e).is_some() {
                    entity = Some(e);
                    break;
                }
            }
        }
        if entity.is_none() {
            for c in self.cgs_layers() {
                if c.get_entity(&raw).is_some() {
                    entity = Some(raw.clone());
                    break;
                }
            }
        }
        let entity = match entity {
            Some(e) => e,
            None => {
                let entity_try = self
                    .primary_cgs()
                    .canonical_entity_name(&raw)
                    .unwrap_or_else(|| raw.clone());
                let kind = if raw == "Get" || entity_try == "Get" {
                    ParseErrorKind::Other {
                        message: "Plasm does not use a `Get(` wrapper; use `Entity(id)` for get-by-id (e.g. `Pokemon(pikachu)`)"
                            .to_string(),
                    }
                } else {
                    ParseErrorKind::UnknownEntity {
                        name: entity_try,
                        span_opt: Some((span_start, span_end)),
                    }
                };
                return Err(ParseError {
                    kind,
                    offset: span_start,
                });
            }
        };
        self.pending_session_catalog_entry_id = None;
        if let Some(sym) = entity_from_sym.as_deref() {
            self.pending_session_catalog_entry_id = self
                .sym_map
                .resolve_session_entity(sym)
                .ok()
                .map(|b| b.entry_id.to_string());
        }
        let ent = self
            .cgs_for_entity_with_preferred_catalog(
                &entity,
                self.pending_session_catalog_entry_id.as_deref(),
            )?
            .get_entity(&entity)
            .ok_or_else(|| ParseError {
                kind: ParseErrorKind::UnknownEntity {
                    name: entity.clone(),
                    span_opt: None,
                },
                offset: self.pos,
            })?
            .clone();

        if let Some(expr) = self.try_parse_entity_dot_search(&entity)? {
            return self.ok_stamped(expr);
        }

        self.skip_ws();
        let expr = match self.peek_char() {
            Some('(') => {
                // Get by ID: simple `Entity(value)` or compound `Entity(k=v,...)`.
                self.pos += 1;
                self.skip_ws();
                if self.peek_char() == Some(')') {
                    return Err(self.err(ParseErrorKind::EmptyGetParens {
                        entity: entity.clone(),
                    }));
                }
                let after_paren = self.pos;
                let looks_kv = self.peek_compound_key_value_form();
                if ent.key_vars.len() > 1 {
                    if !looks_kv {
                        return Err(self.err(ParseErrorKind::Other {
                            message: format!(
                                "entity `{}` has compound key {:?}; use `{}(key=value, ...)` with those keys",
                                entity, ent.key_vars, entity
                            ),
                        }));
                    }
                    let head = entity_ref_parse::EntityCtorHead::new(
                        entity.clone(),
                        self.pending_session_catalog_entry_id.clone(),
                    );
                    let map_values = self.parse_strict_compound_key_value_map(&head, &ent)?;
                    let mut slots: BTreeMap<String, IdentitySlot> = BTreeMap::new();
                    for k in &ent.key_vars {
                        let v = map_values.get(k.as_str()).expect("keys validated");
                        if let Value::PlasmInputRef(r) = v {
                            slots.insert(k.to_string(), IdentitySlot::binding(r.clone()));
                        } else {
                            let s = self.identity_literal_from_value(v)?;
                            slots.insert(k.to_string(), IdentitySlot::lit(s));
                        }
                    }
                    let get = GetExpr::from_ref(Ref::compound_slots(entity, slots));
                    Ok(Expr::Get(get))
                } else {
                    if looks_kv && ent.key_vars.is_empty() {
                        if let Some(get) =
                            self.try_parse_simple_id_field_get_sugar(&entity, &ent)?
                        {
                            return self.ok_stamped(get);
                        }
                        return Err(self.err(ParseErrorKind::Other {
                            message: format!(
                                "entity `{}` uses a simple id; use `{}(id)` not key=value form",
                                entity, entity
                            ),
                        }));
                    }
                    self.pos = after_paren;
                    if let Some(get) = self.try_parse_id_field_eq_get_in_parens(&entity, &ent)? {
                        return self.ok_stamped(get);
                    }
                    let id_val = if self.program_nodes.is_some() {
                        self.parse_dotted_call_arg_value_rhs()?
                    } else {
                        self.parse_value()?
                    };
                    self.expect_char(')')?;
                    if let Value::PlasmInputRef(r) = id_val {
                        Ok(Expr::Get(GetExpr::from_ref(Ref::simple_binding(entity, r))))
                    } else {
                        let id_str = self.identity_literal_from_value(&id_val)?;
                        Ok(Expr::Get(GetExpr::new(entity, id_str)))
                    }
                }
            }
            Some('{') => {
                // Query with predicates (chained `{…}{…}` groups are AND-conjoined)
                self.pos += 1;
                let mut preds = self.parse_preds(&entity)?;
                self.expect_char('}')?;
                while self.peek_char() == Some('{') {
                    self.pos += 1;
                    preds.extend(self.parse_preds(&entity)?);
                    self.expect_char('}')?;
                }
                Ok(Expr::Query(Self::preds_to_query(&entity, preds)))
            }
            Some('~') => {
                // Full-text search (requires Search capability on this entity)
                let search_empty = {
                    let c = self.cgs_for_entity_required(&entity)?;
                    c.find_capabilities(&entity, CapabilityKind::Search)
                        .is_empty()
                };
                if search_empty {
                    return Err(self.err(ParseErrorKind::SearchNotSupported {
                        entity: entity.to_string(),
                    }));
                }
                self.pos += 1;
                self.skip_ws();
                if matches!(self.peek_char(), None | Some('[')) {
                    return Err(self.err(ParseErrorKind::SearchTextMustBeString));
                }
                let text = self.parse_value()?;
                let text_str = match &text {
                    Value::String(s) => s.clone(),
                    Value::Integer(n) => n.to_string(),
                    _ => return Err(self.err(ParseErrorKind::SearchTextMustBeString)),
                };
                // The primary Search capability and its free-text selection lane identify the
                // search-text input (`query`/`q`/`search`, or a sole selection slot).
                let (cap_name, q_field) = {
                    let c = self.cgs_for_entity_required(&entity)?;
                    let cap = c.primary_search_capability(&entity).ok_or_else(|| {
                        self.err(ParseErrorKind::Other {
                            message: format!(
                                "search capability for `{entity}` is structurally ambiguous"
                            ),
                        })
                    })?;
                    let field = cap.search_text_selection_param().ok_or_else(|| {
                        self.err(ParseErrorKind::Other {
                            message: format!(
                                "search capability `{}` must declare a free-text selection parameter (query/q/search)",
                                cap.name
                            ),
                        })
                    })?;
                    (Some(cap.name.clone()), field.name.clone())
                };

                let mut preds = vec![Predicate::eq(q_field, text_str)];
                self.skip_ws();
                if self.peek_char() == Some('{') {
                    self.pos += 1;
                    preds.extend(self.parse_preds(&entity)?);
                    self.expect_char('}')?;
                }
                let mut query = Self::preds_to_query(&entity, preds);
                query.capability_name = cap_name;
                Ok(Expr::Query(query))
            }
            _ => {
                // `Entity:id` must not parse as bare `Entity` + ignored `:id` tail (that would run
                // QueryExpr::all and return the wrong first page). Reject explicitly.
                if self.peek_char() == Some(':') {
                    return Err(ParseError {
                        kind: ParseErrorKind::ColonAfterEntityName {
                            entity: entity.clone(),
                        },
                        offset: self.pos,
                    });
                }
                // Query all
                Ok(Expr::Query(QueryExpr::all(entity)))
            }
        }?;
        self.ok_stamped(expr)
    }

    fn parse_pipeline(&mut self, source: Expr) -> Result<Expr, ParseError> {
        self.skip_ws();
        // Check for reverse traversal: .^Entity or .^Entity{preds}
        if self.remaining().starts_with(".^") {
            self.pos += 2;
            let target_raw = self.parse_ident()?;
            let target_entity = self.canonical_entity_name_in_layers(&target_raw);
            self.validate_entity_preferring(
                &target_entity,
                source.session_catalog_entry_id().map(|id| id.as_str()),
            )?;

            // Extract the source entity ID to build the reverse query predicate
            let source_id = extract_primary_id(&source);

            // Find the FK param on the target entity that references source entity
            let source_entity = source.primary_entity();
            let fk_param = self.find_fk_param(&target_entity, source_entity)?;

            // Optional filter on the target
            self.skip_ws();
            let mut preds: Vec<Predicate> = Vec::new();
            if self.peek_char() == Some('{') {
                self.pos += 1;
                preds = self.parse_preds(&target_entity)?;
                self.expect_char('}')?;
            }

            // Inject the FK predicate
            let fk_pred = match source_id {
                Some(id) => Predicate::eq(&fk_param, id),
                None => {
                    // Source is a query — this becomes a cross-entity composition;
                    // for now we emit a ChainExpr-style but via a Query with the
                    // FK field as an EntityRef predicate placeholder.
                    // Full cross-entity push-left is handled at execution time.
                    Predicate::eq(&fk_param, "__source_id__")
                }
            };
            preds.insert(0, fk_pred);
            let query = Self::preds_to_query(&target_entity, preds);
            return Ok(Expr::Query(query));
        }

        // Forward navigation: .method() zero-arity invoke | .fieldName or .relationName
        if self.remaining().starts_with('.') {
            self.pos += 1;
            let (field_raw, span_start, span_end) = self.parse_method_label_with_span()?;
            let field = self.normalize_method_symbol_label(&field_raw);

            self.skip_ws();
            if self.peek_char() == Some('(') {
                self.pos += 1;
                self.skip_ws();
                if self.peek_char() == Some(')') {
                    self.pos += 1;
                    // Empty `()` must still run path-var injection when the capability has required
                    // inputs (e.g. scope copied from `Entity(k=$,…)`); zero-arity routing skips that.
                    let label_norm = self.normalize_method_symbol_label(&field_raw);
                    if let Ok(cap) = self.resolve_dotted_call_capability(
                        &label_norm,
                        Some(field_raw.as_str()),
                        &source,
                    ) {
                        if !capability_is_zero_arity_invoke(cap) {
                            return self.finish_dotted_call_with_payload(
                                source,
                                field_raw,
                                IndexMap::new(),
                            );
                        }
                    }
                    return self.parse_zero_arity_invoke(source, field_raw);
                }
                return self.parse_dotted_call_with_payload(source, field_raw);
            }

            let source_entity = {
                let cgs = self.cgs_for_expr_source(&source)?;
                source
                    .relation_navigation_entity(cgs)
                    .ok_or_else(|| ParseError {
                        kind: ParseErrorKind::NotNavigable {
                            field: field.clone(),
                            entity: source.primary_entity().to_string(),
                            span_start,
                            span_end,
                        },
                        offset: span_start,
                    })?
            };
            if let Some(expr) =
                self.expand_team_members_sugar(&source, &field, source_entity.as_str())?
            {
                return Ok(expr);
            }
            if let Some(expr) =
                self.try_scoped_query_bridge_from_get(&source, &field, Some(&field_raw))?
            {
                return Ok(expr);
            }
            let cgs_src = self.cgs_for_expr_source(&source)?;
            if let Some(ent) = cgs_src.get_entity(&source_entity).cloned() {
                let seg_ctx = crate::relation_segment::RelationSegmentContext {
                    map: self.sym_map.as_ref(),
                    entity: source_entity.as_str(),
                    relations: &ent.relations,
                    binding_label: None,
                    allow_lhs_coercion: false,
                };
                let relation_field = match crate::relation_segment::resolve_relation_segment(
                    &seg_ctx,
                    field.as_str(),
                ) {
                    crate::relation_segment::RelationSegmentOutcome::Wire(w) => w,
                    crate::relation_segment::RelationSegmentOutcome::WrongRole { sym, wire } => {
                        return Err(ParseError {
                            kind: ParseErrorKind::RelationSegmentWrongRole {
                                sym,
                                wire,
                                entity: source_entity.clone(),
                                span_start,
                                span_end,
                            },
                            offset: span_start,
                        });
                    }
                    crate::relation_segment::RelationSegmentOutcome::NotFound => field.clone(),
                };
                // Check declared relations (e.g. .species, .abilities, .moves)
                if let Some(rel) = ent.relations.get(relation_field.as_str()) {
                    let target = &rel.target_resource;
                    let cardinality = rel.cardinality;

                    // Braces belong to catalog sources, never relation hops (RA-1/RA-6).
                    // Replacing this chain with a target query erases its receiver.
                    self.skip_ws();
                    if self.peek_char() == Some('{') {
                        return Err(ParseError {
                            kind: ParseErrorKind::Other {
                                message: crate::relation_segment::relation_query_braces_message(
                                    &relation_field,
                                ),
                            },
                            offset: self.pos,
                        });
                    }

                    // Cardinality-one → ChainExpr, executed at runtime.
                    // The executor fetches the source entity, then looks up the decoded
                    // relation target ID from entity.fields[selector] (populated by the
                    // relation decoder) to dispatch Get(target, id).
                    if cardinality == crate::Cardinality::One {
                        let chain = ChainExpr::auto_get(source, relation_field);
                        return Ok(Expr::Chain(chain));
                    }

                    {
                        let mat = rel
                            .materialize
                            .as_ref()
                            .unwrap_or(&crate::RelationMaterialization::Unavailable);
                        match mat {
                            crate::RelationMaterialization::FromParentGet { .. }
                            | crate::RelationMaterialization::PreferFromParentGet { .. }
                            | crate::RelationMaterialization::ViewEmbed { .. }
                            | crate::RelationMaterialization::QueryScoped { .. }
                            | crate::RelationMaterialization::QueryScopedBindings { .. } => {
                                let chain = ChainExpr::auto_get(source, relation_field);
                                return Ok(Expr::Chain(chain));
                            }
                            crate::RelationMaterialization::GetScopedBindings { .. } => {
                                return Err(ParseError {
                                    kind: ParseErrorKind::ManyRelationUnmaterialized {
                                        entity: source_entity.clone(),
                                        relation: field.clone(),
                                        target: target.to_string(),
                                        span_start,
                                        span_end,
                                    },
                                    offset: span_start,
                                });
                            }
                            crate::RelationMaterialization::Unavailable => {
                                return Err(ParseError {
                                    kind: ParseErrorKind::ManyRelationUnmaterialized {
                                        entity: source_entity.clone(),
                                        relation: field.clone(),
                                        target: target.to_string(),
                                        span_start,
                                        span_end,
                                    },
                                    offset: span_start,
                                });
                            }
                        }
                    }
                }

                // Check EntityRef fields (e.g. .petId → ChainExpr). Non-ref fields are
                // field-dot extract candidates: `source.wire` (PLP-1 scalar on StaticSingleton;
                // explicit `source[wire]` remains row projection).
                match ent.fields.get(field.as_str()) {
                    Some(f) => {
                        let is_ref = f
                            .named_value(cgs_src)
                            .ok()
                            .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }));
                        if !is_ref {
                            self.skip_ws();
                            if self.remaining().starts_with('.') {
                                return Err(ParseError {
                                    kind: ParseErrorKind::NotNavigable {
                                        field: field.clone(),
                                        entity: source_entity.clone(),
                                        span_start,
                                        span_end,
                                    },
                                    offset: span_start,
                                });
                            }
                            if self.pending_field_dot_extract.is_some() {
                                return Err(ParseError {
                                    kind: ParseErrorKind::NotNavigable {
                                        field: field.clone(),
                                        entity: source_entity.clone(),
                                        span_start,
                                        span_end,
                                    },
                                    offset: span_start,
                                });
                            }
                            self.pending_field_dot_extract = Some(field);
                            return Ok(source);
                        }
                    }
                    None => {
                        return match self.parse_zero_arity_invoke(source, field.clone()) {
                            Ok(expr) => Ok(expr),
                            Err(e) => {
                                if matches!(e.kind, ParseErrorKind::NoZeroArityMethod { .. }) {
                                    Err(ParseError {
                                        kind: ParseErrorKind::NotFieldOrRelation {
                                            field: field.clone(),
                                            entity: source_entity.clone(),
                                            span_start,
                                            span_end,
                                        },
                                        offset: span_start,
                                    })
                                } else {
                                    Err(e)
                                }
                            }
                        };
                    }
                }
            }

            let chain = ChainExpr::auto_get(source, field);
            return Ok(Expr::Chain(chain));
        }

        Ok(source)
    }

    /// Find the FK parameter name on `target_entity` that accepts EntityRef(source_entity).
    fn find_fk_param(
        &self,
        target_entity: &str,
        source_entity: &str,
    ) -> Result<String, ParseError> {
        // First: check query capability parameters
        for c in self.cgs_layers() {
            for cap in c.find_capabilities(target_entity, CapabilityKind::Query) {
                for f in cap.scope_params() {
                    if let Ok(nv) = f.named_value(c) {
                        if let FieldType::EntityRef { target, .. } = &nv.field_type {
                            if target.as_str() == source_entity {
                                return Ok(f.name.clone());
                            }
                        }
                    }
                }
            }
        }
        // Second: check entity fields for EntityRef
        if let Some(ent) = self
            .cgs_for_entity_required(target_entity)?
            .get_entity(target_entity)
        {
            for (fname, field) in &ent.fields {
                if let Ok(nv) = field.named_value(self.cgs_for_entity_required(target_entity)?) {
                    if let FieldType::EntityRef { target, .. } = &nv.field_type {
                        if target.as_str() == source_entity {
                            return Ok(fname.as_str().to_string());
                        }
                    }
                }
            }
        }
        Err(self.err(ParseErrorKind::NoEntityRefBridge {
            target_entity: target_entity.to_string(),
            source_entity: source_entity.to_string(),
        }))
    }

    /// Parse a `[f,f,...]` projection list.
    fn parse_projection(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect_char('[')?;
        let mut fields = Vec::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some(']') {
                break;
            }
            fields.push(self.parse_ident()?);
            self.skip_ws();
            if self.peek_char() == Some(',') {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.expect_char(']')?;
        Ok(fields)
    }

    fn parse_expr(&mut self) -> Result<ParsedExpr, ParseError> {
        let mut expr = self.parse_source()?;
        if let Some(eid) = expr.session_catalog_entry_id() {
            self.pending_session_catalog_entry_id = Some(eid.to_string());
        }

        // Apply pipeline steps
        loop {
            self.skip_ws();
            if !self.remaining().starts_with('.') {
                break;
            }
            let prev_pos = self.pos;
            let next = self.parse_pipeline(expr)?;
            expr = next;
            if self.pos == prev_pos {
                break;
            }
        }

        // Optional projection — explicit `[…]` (row) vs deferred `.field` sugar (scalar-extract candidate).
        self.skip_ws();
        let (projection, field_dot_extract) = if self.peek_char() == Some('[') {
            if self.pending_field_dot_extract.is_some() {
                return Err(ParseError {
                    kind: ParseErrorKind::UnexpectedTrailingInput {
                        tail: self.remaining().to_string(),
                    },
                    offset: self.pos,
                });
            }
            (Some(self.parse_projection()?), None)
        } else if let Some(wire) = self.pending_field_dot_extract.take() {
            // PLP-1: `.wire` is scalar extract on StaticSingleton — not `| select` / `[wire]`.
            (None, Some(wire))
        } else {
            (None, None)
        };

        // One expression per call; ignore trailing noise (LLM markdown, prose, etc.).
        self.skip_ws();

        Ok(ParsedExpr {
            expr,
            projection,
            field_dot_extract,
        })
    }

    fn classify_remainder(&self) -> ParseRemainder {
        let tail = self.remaining().trim();
        if tail.is_empty() {
            return ParseRemainder::Empty;
        }
        let head = tail.chars().next().unwrap_or(' ');
        if matches!(head, '{' | '.' | '[' | '(' | '~' | '=' | ')' | ',' | '|') {
            ParseRemainder::Syntax { at: self.pos, head }
        } else {
            ParseRemainder::Prose(tail.to_string())
        }
    }
}

/// Extract the primary ID string from a source expression for use in reverse queries.
fn extract_primary_id(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Get(g) => Some(g.reference.primary_slot_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Micro **parser** tests: tokenization, postfix peel interaction, error paths, tiny AST shapes.
    //! Prefer **minimal** CGS (petstore when present). Anything that states “this **author program**
    //! means X” end-to-end should have a counterpart row in `plasm-e2e` `plasm_language_matrix`
    //! — cite the matrix row id on semantic parallels (e.g. `lang_query_all`).
    use super::*;
    use crate::cgs_federation::cgs_layer_stack;
    use crate::schema::capability_method_label_kebab;
    use crate::schema::registry_test_util;
    use crate::schema::NamedValueSchema;
    use crate::symbol_tuning::{
        entity_slices_for_render, teaching_exposure_session_from_focus, FocusSpec, SymbolMap,
    };
    use crate::{
        loader::load_schema_dir, CapabilityKind, CapabilityMapping, CapabilitySchema, Cardinality,
        EntityKey, FieldType, InputFieldSchema, InputFieldWire, InputSchema, InputType,
        InputVariantSchema, OutputSchema, OutputType, PlasmInputRef, RelationSchema,
        ResourceSchema, WireVariantDiscriminator, CGS,
    };
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    fn test_layer<'a>(cgs: &'a CGS) -> [CgsLayer<'a>; 1] {
        [CgsLayer::new(cgs.entry_id.as_deref().unwrap_or(""), cgs)]
    }

    fn seed_fx_str(cgs: &mut CGS) {
        cgs.values.insert(
            "fx_str".into(),
            NamedValueSchema {
                domain: Default::default(),
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                array_items: None,
                currency: None,
            },
        );
    }

    fn petstore_cgs() -> CGS {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if dir.exists() {
            load_schema_dir(dir).unwrap()
        } else {
            CGS::new()
        }
    }

    fn has_petstore() -> bool {
        std::path::Path::new("../../fixtures/schemas/petstore").exists()
    }

    #[test]
    fn parse_root_union_ctor_teaching_line() {
        let cgs = petstore_cgs();
        let r = parse("v101{a=$}", &cgs).unwrap();
        match &r.expr {
            Expr::TeachingValue { value } => match value {
                Value::UnionCtor {
                    ctor_label,
                    ctor_fields,
                } => {
                    assert_eq!(ctor_label, "v101");
                    assert_eq!(ctor_fields.len(), 1);
                }
                _ => panic!("expected UnionCtor"),
            },
            _ => panic!("expected TeachingValue"),
        }
        crate::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Path-var Get must keep a UUID identity slot (must not default id to "0").
    #[test]
    fn langitem_get_preserves_uuid() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let u = "d6f28392-a2a8-46ce-a1c5-b5ec81ed2396";
        let r = parse(&format!(r#"LangItem("{u}")"#), &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), u);
    }

    /// Semantic parallel: matrix `lang_get_by_id` (LangItem); petstore Get shape only.
    #[test]
    fn parse_get_by_id() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet(10)", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Get(_)));
        assert_eq!(r.projection, None);
        if let Expr::Get(g) = &r.expr {
            assert_eq!(g.reference.entity_type, "Pet");
            assert_eq!(g.reference.simple_id().map(|s| s.as_str()), Some("10"));
        }
    }

    /// Shadow/repair sugar (deliberately untaught): `Entity(id_field=value)` ≡ canonical `Entity(value)`.
    #[test]
    fn parse_get_simple_id_field_named_sugar() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse(r#"LangItem(id="i1")"#, &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert_eq!(g.reference.entity_type, "LangItem");
        assert_eq!(g.reference.simple_id().map(|s| s.as_str()), Some("i1"));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
        let r2 = parse(r#"LangItem("i1")"#, &cgs).unwrap();
        assert_eq!(
            serde_json::to_value(&r.expr).unwrap(),
            serde_json::to_value(&r2.expr).unwrap()
        );
    }

    #[test]
    fn parse_get_simple_id_field_named_sugar_rejects_non_identity_key() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let err = parse(r#"LangItem(headline="x")"#, &cgs).unwrap_err();
        assert!(
            err.message().contains("identity field"),
            "unexpected: {}",
            err.message()
        );
    }

    #[test]
    fn parse_get_rejects_positional_id_plus_extra_arguments() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        parse(r#"LangItem("i1")"#, &cgs).expect("unary Get");
        parse(
            r#"LangItem("i1").update(title="x", score=1, owner="alice")"#,
            &cgs,
        )
        .expect("method args belong after Get identity");
        let err = parse(r#"LangItem("i1", title="x")"#, &cgs)
            .expect_err("positional id plus extra Get args");
        assert!(
            matches!(
                err.kind,
                ParseErrorKind::ExpectedChar {
                    expected: ')',
                    got: Some(',')
                }
            ),
            "expected close after positional id, got {:?}",
            err.kind
        );
    }

    /// PLP-10: card glyphs (`<id>`, `<wire>`, `"<query>"`) are templates, not program values.
    /// Official T102901 copied `e4(<id>).m5(...)` — that shape must parse-reject.
    #[test]
    fn parse_rejects_unfilled_teaching_holes() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).unwrap();
        let assert_hole = |input: &str, hole: &str| {
            let err = parse(input, &cgs).expect_err(input);
            assert!(
                matches!(
                    &err.kind,
                    ParseErrorKind::UnfilledTeachingHole { hole: h } if h == hole
                ),
                "{input}: {:?}",
                err.kind
            );
            let msg = err.message();
            assert!(
                msg.contains("PLP-10")
                    && msg.contains("fill it with a bound value")
                    && msg.contains("row field")
                    && msg.contains("string of that sort"),
                "{input}: {msg}"
            );
            assert!(
                !msg.to_ascii_lowercase().contains("email"),
                "fill law must stay domain-general: {msg}"
            );
        };
        assert_hole("LangItem(<id>)", "<id>");
        assert_hole(r#"LangItem("<id>")"#, "<id>");
        assert_hole("LangItem{title=<wire>}", "<wire>");
        assert_hole(r#"LangItem~"<query>""#, "<query>");
        parse(r#"LangItem("i1")"#, &cgs).expect("filled Get identity");
        parse(r#"LangItem{tags="alpha"}"#, &cgs).expect("filled query string");
        parse("LangItem | where score < 10", &cgs).expect("comparison < is not a teaching hole");

        let exposure = teaching_exposure_session_from_focus(&cgs, FocusSpec::Single("LangItem"));
        let map = exposure.symbol_map_arc();
        let e = map.entity_sym_for("", "LangItem");
        let err = parse_session_line(&format!("{e}(<id>)"), &cgs, Some(map.clone()))
            .expect_err("e#(<id>) must reject");
        assert!(
            matches!(
                &err.kind,
                ParseErrorKind::UnfilledTeachingHole { hole } if hole == "<id>"
            ),
            "{:?}",
            err.kind
        );
        parse_session_line(&format!(r#"{e}("i1")"#), &cgs, Some(map))
            .expect("filled e# identity must parse");
    }

    #[test]
    fn parse_get_with_string_id() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse(r#"Pet("fluffy")"#, &cgs).unwrap();
        if let Expr::Get(g) = &r.expr {
            assert_eq!(g.reference.simple_id().map(|s| s.as_str()), Some("fluffy"));
        }
    }

    /// Semantic parallel: matrix `lang_query_all` (bare entity query).
    #[test]
    fn parse_query_all() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
        if let Expr::Query(q) = &r.expr {
            assert_eq!(q.entity, "Pet");
            assert!(q.predicate.is_none());
        }
    }

    /// Pagination IR micro-case; matrix touches paging via `lang_domain_symbol_page_size`.
    #[test]
    fn parse_page_continuation() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("page(pg1)", &cgs).unwrap();
        let Expr::Page(p) = &r.expr else {
            panic!("expected Page, got {:?}", r.expr);
        };
        assert_eq!(p.handle.as_str(), "pg1");
        assert_eq!(p.limit, None);
    }

    #[test]
    fn parse_page_continuation_with_limit() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("page(pg2, limit=50)", &cgs).unwrap();
        let Expr::Page(p) = &r.expr else {
            panic!("expected Page, got {:?}", r.expr);
        };
        assert_eq!(p.handle.as_str(), "pg2");
        assert_eq!(p.limit, Some(50));
    }

    #[test]
    fn parse_page_continuation_namespaced_slot() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let r = parse(&format!("page({wire}_pg1)"), &cgs).unwrap();
        let Expr::Page(p) = &r.expr else {
            panic!("expected Page, got {:?}", r.expr);
        };
        assert_eq!(p.handle.as_str(), format!("{wire}_pg1"));
        assert_eq!(p.limit, None);
    }

    #[test]
    fn parse_wait_operation_continuation() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let r = parse(&format!("wait({wire}_o1)"), &cgs).unwrap();
        let Expr::Wait(w) = &r.expr else {
            panic!("expected Wait, got {:?}", r.expr);
        };
        assert_eq!(w.handle.as_str(), format!("{wire}_o1"));
    }

    #[test]
    fn parse_wait_operation_continuation_base64url_hyphen() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let wire = "l_EPUj_tlFT76v-qOcMZXRAQ";
        let r = parse(&format!("wait({wire}_o1)"), &cgs).unwrap();
        let Expr::Wait(w) = &r.expr else {
            panic!("expected Wait, got {:?}", r.expr);
        };
        assert_eq!(w.handle.as_str(), format!("{wire}_o1"));
    }

    #[test]
    fn parse_page_continuation_base64url_hyphen() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let wire = "l_EPUj_tlFT76v-qOcMZXRAQ";
        let r = parse(&format!("page({wire}_pg1)"), &cgs).unwrap();
        let Expr::Page(p) = &r.expr else {
            panic!("expected Page, got {:?}", r.expr);
        };
        assert_eq!(p.handle.as_str(), format!("{wire}_pg1"));
    }

    #[test]
    fn parse_cancel_operation_continuation() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let wire = "l_AAAAAAAAQACAAAAAAAAAAQ";
        let r = parse(&format!("cancel({wire}_o2)"), &cgs).unwrap();
        let Expr::Cancel(c) = &r.expr else {
            panic!("expected Cancel, got {:?}", r.expr);
        };
        assert_eq!(c.handle.as_str(), format!("{wire}_o2"));
    }

    #[test]
    fn parse_query_with_filter() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=available}", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
        if let Expr::Query(q) = &r.expr {
            assert!(q.predicate.is_some());
        }
    }

    #[test]
    fn parse_structured_heredoc_predicate_tagged() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=<<S\navailable\nS\n}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String("available\n".into()));
    }

    #[test]
    fn parse_structured_heredoc_predicate_with_embedded_quote() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=<<S\na\"b\nS\n}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String("a\"b\n".into()));
    }

    #[test]
    fn parse_structured_heredoc_predicate_commas_inside() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=<<S\na, b, c\nS\n}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String("a, b, c\n".into()));
    }

    #[test]
    fn parse_structured_heredoc_tagged_when_body_has_triple_angle_line() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=<<T\n>>>\nstill inside\nT\n}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(
            value.to_value(),
            Value::String(">>>\nstill inside\n".into())
        );
    }

    /// Short `TAG` closes on the first matching line — an interior line equal to `TAG` truncates the body (RFC822/MIME hazard).
    #[test]
    fn parse_structured_heredoc_tag_collision_truncates_at_first_matching_line() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let input = "<<T\nbefore\nT\nafter\nT";
        let mut p = Parser::new(input, &cgs);
        assert!(p.structured_heredoc_starts_here());
        let v = p.parse_structured_heredoc().unwrap();
        assert_eq!(v, Value::String("before\n".into()));
        assert_eq!(&p.input[p.pos..], "\nafter\nT");
    }

    /// Long opaque tag: interior line `T` does not close — full payload until final close line.
    #[test]
    fn parse_structured_heredoc_opaque_tag_preserves_interior_close_like_line() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let input = "<<PLASM_T\nbefore\nT\nafter\nPLASM_T";
        let mut p = Parser::new(input, &cgs);
        assert!(p.structured_heredoc_starts_here());
        let v = p.parse_structured_heredoc().unwrap();
        assert_eq!(v, Value::String("before\nT\nafter\n".into()));
        assert!(p.input[p.pos..].is_empty());
    }

    #[test]
    fn parse_structured_heredoc_tagged_glued_close_paren_fragment() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let input = "<<X\nx\nX)";
        let mut p = Parser::new(input, &cgs);
        assert!(p.structured_heredoc_starts_here());
        let v = p.parse_structured_heredoc().unwrap();
        assert_eq!(v, Value::String("x\n".into()));
        assert_eq!(&p.input[p.pos..], ")");
    }

    #[test]
    fn parse_structured_heredoc_tagged_glued_close_ws_paren_fragment() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let input = "<<X\nx\nX )";
        let mut p = Parser::new(input, &cgs);
        assert!(p.structured_heredoc_starts_here());
        let v = p.parse_structured_heredoc().unwrap();
        assert_eq!(v, Value::String("x\n".into()));
        assert_eq!(&p.input[p.pos..], " )");
    }

    #[test]
    fn parse_structured_heredoc_tagged_glued_close_nested_delimiters_fragment() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let input = "<<X\nx\nX})";
        let mut p = Parser::new(input, &cgs);
        assert!(p.structured_heredoc_starts_here());
        let v = p.parse_structured_heredoc().unwrap();
        assert_eq!(v, Value::String("x\n".into()));
        assert_eq!(&p.input[p.pos..], "})");
    }

    #[test]
    fn parse_structured_heredoc_predicate_tagged_glued_close_brace() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=<<X\nx\nX}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String("x\n".into()));
    }

    #[test]
    fn parse_structured_heredoc_predicate_tagged_glued_close_comma() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=<<X\nx\nX,\nname=dog}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::And { args } = pred else {
            panic!("expected And");
        };
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn parse_structured_heredoc_tagged_glued_close_brace() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=<<T\nhi\nT}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String("hi\n".into()));
    }

    #[test]
    fn parse_structured_heredoc_predicate_tagged_rejects_junk_after_marker() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        assert!(parse("Pet{status=<<X\nx\nXfoo}", &cgs).is_err());
    }

    fn root_union_heredoc_fixture_cgs() -> CGS {
        let mut cgs = CGS::new();
        seed_fx_str(&mut cgs);
        cgs.add_resource(ResourceSchema {
            name: "Document".into(),
            description: String::new(),
            id_field: "slug".into(),
            id_format: None,
            id_from: None,
            fields: vec![registry_test_util::entity_field_from_values(
                &cgs, "fx_str", "slug", true, "",
            )],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "document_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Document".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": "documents"},
                        {"type": "var", "name": "slug"}
                    ]
                })
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec!["slug".into()],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "document_suggest".into(),
            description: String::new(),
            kind: CapabilityKind::Action,
            domain: "Document".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "POST",
                    "path": [
                        {"type": "literal", "value": "documents"},
                        {"type": "var", "name": "slug"},
                        {"type": "literal", "value": "ops"}
                    ],
                    "body": {"type": "var", "name": "input"}
                })
                .into(),
            }),
            derived: None,
            inputs: crate::schema::CapabilityInputs {
                receiver: Some(crate::CapabilityReceiver::Entity {
                    entity: "Document".into(),
                }),
                payload: Some(InputSchema {
                    input_type: InputType::Union {
                        variants: vec![InputVariantSchema {
                            name: "insert".into(),
                            description: None,
                            constructor_symbol: Some("v111".into()),
                            fields: vec![InputFieldSchema {
                                name: "content".into(),
                                wire: InputFieldWire::Registry(
                                    crate::schema::ValueDomainKey::new("fx_str").unwrap(),
                                ),
                                required: true,
                                description: None,
                                default: None,
                                wire_json_path: None,
                                wire_array_element_key: None,
                                sink_class: None,
                            }],
                            wire: WireVariantDiscriminator {
                                field: "kind".into(),
                                value: "insert".into(),
                            },
                        }],
                    },
                    validation: Default::default(),
                    description: None,
                    examples: vec![],
                }),
                ..Default::default()
            },
            output_schema: Some(OutputSchema {
                output_type: OutputType::SideEffect {
                    description: "adds a suggestion".into(),
                },
                decoder: serde_json::json!({}),
                idempotent: false,
                reconcile: None,
            }),
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs
    }

    #[test]
    fn parse_root_union_ctor_heredoc_with_nested_close_delimiters() {
        let cgs = root_union_heredoc_fixture_cgs();
        let cap = cgs.get_capability("document_suggest").unwrap();
        let label = capability_method_label_kebab(cap);
        let r = parse(
            &format!("Document(doc).{label}(v111{{content=<<TXT\nhello\nTXT}})"),
            &cgs,
        )
        .unwrap();
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        let inp = inv.input.as_ref().expect("input").to_value();
        let Value::UnionCtor { ctor_fields, .. } = inp else {
            panic!("expected union ctor, got {inp:?}");
        };
        assert_eq!(
            ctor_fields.get("content"),
            Some(&Value::String("hello\n".into()))
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_symbolic_union_ctor_resolves_opaque_field_keys_in_parse_tree() {
        use crate::symbol_tuning::TeachingExposureSession;

        let cgs = root_union_heredoc_fixture_cgs();
        let exposure = TeachingExposureSession::new(&cgs, "", &["Document"]);
        let map = exposure.symbol_map_arc();
        let cap = cgs.get_capability("document_suggest").unwrap();
        let label = capability_method_label_kebab(cap);
        let content_sym =
            map.ident_sym_cap_param_for("", cap.domain.as_str(), cap.name.as_str(), "content");
        let parsed = parse_with_cgs_layers(
            &format!("Document(doc).{label}(v111{{{content_sym}=$}})"),
            &test_layer(&cgs),
            map,
        )
        .expect("parse");
        let Expr::Invoke(inv) = &parsed.expr else {
            panic!("expected Invoke, got {:?}", parsed.expr);
        };
        let inp = inv.input.as_ref().expect("input").to_value();
        let Value::UnionCtor { ctor_fields, .. } = inp else {
            panic!("expected union ctor, got {inp:?}");
        };
        assert!(
            ctor_fields.contains_key("content"),
            "union ctor keys must be wire field names, got {ctor_fields:?}"
        );
        crate::type_check_expr(&parsed.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_structured_heredoc_rejects_untagged_opener() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let err = parse("Pet{status=<<\navailable\n>>>\n}", &cgs).unwrap_err();
        assert!(matches!(err.kind, ParseErrorKind::ExpectedValue));
    }

    #[test]
    fn parse_query_predicate_empty_rhs_is_null_and_typechecks() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, op, value } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "status");
        assert_eq!(*op, CompOp::Eq);
        assert_eq!(value.to_value(), Value::Null);
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_query_predicate_array_literal_typechecks() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse(r#"Pet{tags=["puppy","friendly"]}"#, &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, op, value } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "tags");
        assert_eq!(*op, CompOp::Eq);
        assert_eq!(
            value.to_value(),
            Value::Array(vec![
                Value::String("puppy".into()),
                Value::String("friendly".into()),
            ])
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_query_predicate_array_single_value_wraps_and_typechecks() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse(r#"Pet{tags=puppy}"#, &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "tags");
        assert_eq!(
            value.to_value(),
            Value::Array(vec![Value::String("puppy".into())])
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_query_filter_value_does_not_include_trailing_ws_after_bare_word() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=available }", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, op, value } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "status");
        assert_eq!(*op, CompOp::Eq);
        assert_eq!(value.to_value(), Value::String("available".to_string()));
    }

    #[test]
    fn parse_chain_entity_ref() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        // Order has petId: EntityRef(Pet) in petstore
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        // Try parsing with a CGS that has Order.petId as EntityRef
        // petstore fixture should have this after EntityRef backfill
        let r = parse("Order(5).petId", &cgs);
        // If petId is not EntityRef in this fixture, it will error — that's ok
        if let Ok(parsed) = r {
            assert!(matches!(parsed.expr, Expr::Chain(_)));
        }
    }

    #[test]
    fn parse_zero_arity_invoke_petstore_no_parens() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("User.login", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Invoke(_)));
        assert_eq!(r.expr.primary_entity(), "User");
    }

    #[test]
    fn parse_projection() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet(10)[name,status]", &cgs).unwrap();
        assert_eq!(
            r.projection,
            Some(vec!["name".to_string(), "status".to_string()])
        );
    }

    #[test]
    fn parse_field_dot_extract_is_not_bracket_projection() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("language matrix cgs");
        let bracket = parse(r#"LangItem("i1")[title]"#, &cgs).expect("bracket project");
        let sugar = parse(r#"LangItem("i1").title"#, &cgs).expect("field-dot scalar extract");
        assert!(matches!(bracket.expr, Expr::Get(_)));
        assert!(matches!(sugar.expr, Expr::Get(_)));
        assert_eq!(bracket.projection, Some(vec!["title".to_string()]));
        assert_eq!(bracket.field_dot_extract, None);
        // PLP-1: `.wire` is scalar-extract candidate — never silent `| select` / `[wire]`.
        assert_eq!(sugar.projection, None);
        assert_eq!(sugar.field_dot_extract, Some("title".to_string()));
        // Relation still wins over field when both exist (tags).
        let rel = parse(r#"LangItem("i1").tags"#, &cgs).expect("relation hop");
        assert!(matches!(rel.expr, Expr::Chain(_)));
        assert_eq!(rel.projection, None);
        assert_eq!(rel.field_dot_extract, None);
    }

    #[test]
    fn parse_field_dot_extract_rejects_further_nav() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("language matrix cgs");
        let err = parse(r#"LangItem("i1").title.tags"#, &cgs).expect_err("no chain after sugar");
        assert!(
            matches!(err.kind, ParseErrorKind::NotNavigable { .. }),
            "expected NotNavigable, got {err:?}"
        );
    }

    #[test]
    fn parse_query_with_projection() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet{status=available}[name]", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
        assert_eq!(r.projection, Some(vec!["name".to_string()]));
    }

    #[test]
    fn parse_rejects_unknown_entity() {
        let cgs = CGS::new();
        let e = parse("Bogus(1)", &cgs).unwrap_err();
        assert!(e.message().contains("unknown entity"));
    }

    #[test]
    fn parse_rejects_unknown_field_in_pred() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let e = parse("Pet{bogusfield=x}", &cgs).unwrap_err();
        assert!(e.message().contains("not found"));
    }

    #[test]
    fn parse_multi_pred_becomes_and() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        // Two fields that exist on Order
        let r = parse("Order{quantity>1,status=placed}", &cgs).unwrap();
        if let Expr::Query(q) = &r.expr {
            assert!(matches!(q.predicate, Some(crate::Predicate::And { .. })));
        }
    }

    #[test]
    fn parse_cross_entity_pred_dot_path() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        // pet.status is a cross-entity path — parser accepts it even though
        // 'pet' is not a field on Order (validated at exec time by cross_entity module)
        let r = parse("Order{pet.status=available}", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
    }

    #[test]
    fn parse_declared_relation_navigation() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        // Pet has declared relation: category → Category (cardinality one)
        // One-cardinality relations from a Get source → ChainExpr (executor resolves at runtime)
        let r = parse("Pet(10).category", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Chain(_)));
        if let Expr::Chain(c) = &r.expr {
            assert_eq!(c.selector, "category");
        }
    }

    #[test]
    fn parse_nested_one_cardinality_relation_chain() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse(r#"LangItem("i1").summary.detail"#, &cgs).unwrap();
        let Expr::Chain(outer) = &r.expr else {
            panic!("expected nested Chain");
        };
        assert_eq!(outer.selector, "detail");
        let Expr::Chain(inner) = outer.source.as_ref() else {
            panic!("expected inner Chain");
        };
        assert_eq!(inner.selector, "summary");
        assert!(matches!(inner.source.as_ref(), Expr::Get(_)));
        crate::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_relation_nav_with_filter() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let error = parse("Pet(10).tags{name=fluffy}", &cgs).unwrap_err();
        assert!(error.to_string().contains("does not accept query braces"));
    }

    #[test]
    fn parse_zero_arity_invoke_pathless_profile() {
        let dir = std::path::Path::new("../../fixtures/schemas/sole_nullary_get");
        let cgs = load_schema_dir(dir).expect("sole_nullary_get");
        let mut r = parse("Profile", &cgs).unwrap();
        crate::normalize_expr_query_capabilities(&mut r.expr, &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Get(_)));
        assert_eq!(r.expr.primary_entity(), "Profile");
    }

    /// Bare `true`/`false` coerce to [`Value::Bool`] on boolean payload fields.
    #[test]
    fn parse_langitem_create_active_false_typechecks() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse(r#"LangItem.create(title="t", active=false)"#, &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Create(_)));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_zero_arity_invoke_pathless_profile_no_parens() {
        let dir = std::path::Path::new("../../fixtures/schemas/sole_nullary_get");
        let cgs = load_schema_dir(dir).expect("sole_nullary_get");
        let mut r = parse("Profile", &cgs).unwrap();
        crate::normalize_expr_query_capabilities(&mut r.expr, &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Get(_)));
        assert_eq!(r.expr.primary_entity(), "Profile");
    }

    #[test]
    fn parse_zero_arity_invoke_path_langitem_tags() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangItem(i1).tags", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Chain(_)));
        assert_eq!(r.expr.primary_entity(), "LangItem");
    }

    /// Opaque session `m#` resolves to catalog capability for a query.
    #[test]
    fn parse_langitem_query_via_method_symbol() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        cgs.get_capability("langitem_query")
            .expect("langitem_query");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let sym = map.entity_sym_for("", "LangItem");
        let line = sym.to_string();
        let mut r = parse(&line, &cgs).unwrap();
        crate::normalize_expr_query_capabilities(&mut r.expr, &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected Query, got {:?}", r.expr);
        };
        assert_eq!(q.entity.as_str(), "LangItem");
        assert_eq!(q.capability_name.as_deref(), Some("langitem_query"));
    }

    /// Dotted-call alias grammar: non-empty `(key=value,…)` after kebab label → Create.
    #[test]
    fn parse_dotted_call_langitem_create_typechecks() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse(r#"LangItem.create(title="Sprint Sandbox")"#, &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Create(_)));
        if let Expr::Create(c) = &r.expr {
            assert_eq!(c.capability.as_str(), "langitem_create");
            assert_eq!(c.entity.as_str(), "LangItem");
        }
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Dotted-call args: `(..)` when all parameters are optional (teaching table ellipsis).
    #[test]
    fn parse_dotted_call_optional_only_double_dot_typechecks() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangItem(i1).update(..)", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Invoke(_)));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Dotted-call args: required bindings plus `,..` for optional tail.
    #[test]
    fn parse_dotted_call_required_plus_optional_ellipsis_typechecks() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse(r#"LangItem.create(title="example",..)"#, &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Create(_)));
        if let Expr::Create(c) = &r.expr {
            let wire = c.input.to_value();
            let obj = wire.as_object().expect("object input");
            assert!(obj.contains_key("title"));
            assert!(!obj.contains_key("recorded_at"));
        }
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_langitem_ping_invoke_typechecks() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let cap = cgs.get_capability("langitem_ping").expect("langitem_ping");
        let label = capability_method_label_kebab(cap);
        let line = format!("LangItem(demo-slug).{label}()");
        let r = parse(&line, &cgs).unwrap();
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        assert_eq!(inv.capability.as_str(), "langitem_ping");
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn langitem_get_capability_is_get_kind() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let cap = cgs.get_capability("langitem_get").expect("langitem_get");
        assert_eq!(cap.kind, CapabilityKind::Get);
    }

    /// Sole `vN{{…}}` is rejected when the capability root input is an object, not a union.
    #[test]
    fn parse_rejects_root_union_ctor_on_object_input_capability() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let cap = cgs
            .get_capability("langitem_create")
            .expect("langitem_create");
        let label = capability_method_label_kebab(cap);
        let line = format!("LangItem.{label}(v111{{title=$,score=$}})");
        let e = parse(&line, &cgs).expect_err("expected parse error");
        assert!(
            e.message().contains("tagged union") || e.message().contains("key=value"),
            "{}",
            e.message()
        );
    }

    #[test]
    fn parse_hex_item_id_delete() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        // Without hex continuation, `8` parses as an integer and `)` is expected at `b`.
        for line in [
            "LangItem(8badcafe00000001).delete",
            "LangItem(8badcafe00000001).delete()",
        ] {
            let r = parse(line, &cgs).unwrap();
            assert!(
                matches!(r.expr, Expr::Delete(_)),
                "expected Delete for {line:?}, got {:?}",
                r.expr
            );
            if let Expr::Delete(d) = &r.expr {
                assert_eq!(d.capability.as_str(), "langitem_delete");
                assert_eq!(
                    d.target.simple_id().map(|s| s.as_str()),
                    Some("8badcafe00000001")
                );
            }
            crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
        }
    }

    #[test]
    fn typecheck_langtag_item_id_entity_ctor_query() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse(r#"LangTag{item_id=LangItem(i1)}"#, &cgs).unwrap();
        assert!(crate::type_checker::type_check_expr(&r.expr, &cgs).is_ok());
    }

    #[test]
    fn parse_zero_arity_invoke_path_langitem_tags_no_parens() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangItem(i1).tags", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Chain(_)));
        assert_eq!(r.expr.primary_entity(), "LangItem");
    }

    #[test]
    fn parse_zero_arity_invoke_rejects_action_without_id_when_path_needs_it() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let e = parse("LangItem.ping()", &cgs).unwrap_err();
        let msg = e.message();
        assert!(
            msg.contains("(<id>).") && msg.contains("on the left"),
            "must name taught identity seat, got: {msg}"
        );
        assert!(
            !msg.contains("requires `LangItem.ping"),
            "must not advertise pathless wire form as required, got: {msg}"
        );
    }

    /// Compound-key Get must keep all identity parts (not flatten to a single slot).
    #[test]
    fn parse_compound_branch_get_preserves_compound_ref() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let line = "CompoundBranch(owner=alice,item_id=i1,name=main)";
        let r = parse(line, &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        let parts = g
            .reference
            .compound_parts()
            .expect("compound CompoundBranch ref");
        assert_eq!(parts.get("owner").map(String::as_str), Some("alice"));
        assert_eq!(parts.get("item_id").map(String::as_str), Some("i1"));
        assert_eq!(parts.get("name").map(String::as_str), Some("main"));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Simple-id Get must stay a single slot (not `primary_slot_str()` flattening).
    #[test]
    fn parse_langitem_ping_preserves_simple_ref() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangItem(sheet-id-9).ping()", &cgs).unwrap();
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        assert_eq!(inv.capability.as_str(), "langitem_ping");
        assert_eq!(
            inv.target.simple_id().map(|s| s.as_str()),
            Some("sheet-id-9")
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_ignores_trailing_noise_after_expression() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse("Pet(10) ```json extra", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Get(_)));
    }

    #[test]
    fn parse_langitem_string_id_preserves_integer_literal() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        // `LangItem{id=<bigint>}` is referentially `LangItem(<id>)`: the id-field brace sugar
        // rewrites it to a Get whose key preserves the string-typed id verbatim.
        let r = parse("LangItem{id=123456789012345678}", &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected get (id-field brace sugar), got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), "123456789012345678");
    }

    #[test]
    fn parse_langitem_recorded_at_now_normalizes_temporal() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse(r#"LangItem.create(title="t", recorded_at=now)"#, &cgs).unwrap();
        let Expr::Create(c) = &r.expr else {
            panic!("expected create");
        };
        let wire = c.input.to_value();
        let obj = wire.as_object().expect("object input");
        let value = obj.get("recorded_at").expect("recorded_at");
        assert!(
            !matches!(value, Value::String(s) if s == "now"),
            "recorded_at=now should normalize, got {value:?}"
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_langitem_recorded_at_next_week_normalizes_temporal() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse(r#"LangItem.create(title="t", recorded_at=next-week)"#, &cgs).unwrap();
        let Expr::Create(c) = &r.expr else {
            panic!("expected create");
        };
        let wire = c.input.to_value();
        let obj = wire.as_object().expect("object input");
        let value = obj.get("recorded_at").expect("recorded_at");
        assert!(
            !matches!(value, Value::String(s) if s == "next-week"),
            "recorded_at=next-week should normalize, got {value:?}"
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_uuid_value_in_id_brace() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let u = "550e8400-e29b-41d4-a716-446655440000";
        let r = parse(&format!("LangItem{{id={u}}}"), &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected get (id-field brace sugar), got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), u);
    }

    #[test]
    fn parse_quoted_string_json_style_escapes() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse(r#"LangItem(id="line1\n\nline2\t\"x\"\\")"#, &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert_eq!(
            g.reference.simple_id().map(|s| s.as_str()),
            Some("line1\n\nline2\t\"x\"\\")
        );
    }

    #[test]
    fn parse_quoted_string_unknown_escape_errors_with_heredoc_hint() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let err = parse(r#"LangItem(id="bad\q")"#, &cgs).unwrap_err();
        let msg = err.message();
        assert!(
            matches!(err.kind, ParseErrorKind::UnknownEscape { escape: 'q' }),
            "kind={:?} msg={msg}",
            err.kind
        );
        assert!(
            msg.contains("tagged heredoc") || msg.contains("<<TAG"),
            "expected heredoc hint, got: {msg}"
        );
    }

    #[test]
    fn parse_bare_value_backslash_escapes_delimiters() {
        if !has_petstore() {
            return;
        }
        let cgs = petstore_cgs();
        let r = parse(r#"Pet{name=acme\(test\)}"#, &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String("acme(test)".to_string()));
    }

    /// Unquoted multi-word string in `{…}` predicate (lenient RHS).
    #[test]
    fn parse_langitem_query_unquoted_phrase_owner_typechecks() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangItem{owner=Sprint Sandbox}", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_dotted_call_langitem_create_unquoted_title_typechecks() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangItem.create(title=Sprint Sandbox)", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Create(_)));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// In-memory CGS only — no `apis/` fixture on disk required.
    fn empty_get_parens_fixture_cgs() -> CGS {
        let mut cgs = CGS::new();
        seed_fx_str(&mut cgs);
        cgs.add_resource(ResourceSchema {
            name: "Widget".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![registry_test_util::entity_field_from_values(
                &cgs, "fx_str", "id", true, "",
            )],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "widget_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Widget".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "widget"}]}).into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
})
        .unwrap();
        cgs.validate().unwrap();
        cgs
    }

    #[test]
    fn parse_empty_get_parens_emits_kind_not_expected_value() {
        let cgs = empty_get_parens_fixture_cgs();
        for input in ["Widget()", "Widget(  )"] {
            let err = parse(input, &cgs).unwrap_err();
            assert!(
                matches!(
                    &err.kind,
                    ParseErrorKind::EmptyGetParens { entity } if entity == "Widget"
                ),
                "input {input:?}: expected EmptyGetParens for Widget, got {:?}",
                err.kind
            );
            let step = crate::error_render::render_parse_error(&err, input, &cgs);
            assert!(
                step.correction.contains("query")
                    && !step.correction.contains("Put a non-empty id"),
                "query-only entity must not name a keyed Get: {}",
                step.correction
            );
        }
    }

    #[test]
    fn empty_parens_on_sole_nullary_names_pathless_seat() {
        let dir = std::path::Path::new("../../fixtures/schemas/sole_nullary_get");
        assert!(dir.exists(), "missing fixture {dir:?}");
        let cgs = crate::loader::load_schema_dir(dir).unwrap();
        let err = parse("Profile()", &cgs).unwrap_err();
        assert!(
            matches!(
                &err.kind,
                ParseErrorKind::EmptyGetParens { entity } if entity == "Profile"
            ),
            "got {:?}",
            err.kind
        );
        let step = crate::error_render::render_parse_error(&err, "Profile()", &cgs);
        assert!(
            step.correction.contains("pathless singleton")
                && step.correction.contains("never `Profile()`"),
            "sole-nullary empty parens must name the pathless seat: {}",
            step.correction
        );
    }

    fn str_field(cgs: &CGS, name: &str) -> crate::schema::FieldSchema {
        registry_test_util::entity_field_from_values(cgs, "fx_str", name, true, "")
    }

    /// `Book.library` is an `EntityRef` to compound-key `Library` — exercises nested `Library(...)` in `{…}`.
    fn book_library_entity_ref_fixture_cgs() -> CGS {
        let mut cgs = CGS::new();
        seed_fx_str(&mut cgs);
        cgs.values.insert(
            "fx_ref_library".into(),
            NamedValueSchema {
                domain: Default::default(),
                description: String::new(),
                field_type: FieldType::EntityRef {
                    entry_id: Default::default(),
                    target: "Library".into(),
                },
                value_format: None,
                allowed_values: None,
                array_items: None,
                currency: None,
            },
        );
        cgs.add_resource(ResourceSchema {
            name: "Library".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                str_field(&cgs, "id"),
                str_field(&cgs, "region"),
                str_field(&cgs, "code"),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["region".into(), "code".into()],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: Some("library_get".into()),
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_resource(ResourceSchema {
            name: "Book".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                str_field(&cgs, "id"),
                registry_test_util::entity_field_from_values(
                    &cgs,
                    "fx_ref_library",
                    "library",
                    true,
                    "",
                ),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "book_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Book".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template:
                    serde_json::json!({"method":"GET","path":[{"type":"literal","value":"books"}]})
                        .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "library_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Library".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method":"GET",
                    "path":[
                        {"type":"var","name":"region"},
                        {"type":"literal","value":"/"},
                        {"type":"var","name":"code"}
                    ]
                })
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs.validate().unwrap();
        cgs
    }

    #[test]
    fn parse_strict_compound_constructor_rejects_duplicate_keys() {
        let cgs = book_library_entity_ref_fixture_cgs();
        let err = parse("Book{library=Library(region=r1, region=r2, code=c)}", &cgs).unwrap_err();
        assert!(err.message().contains("duplicate key"), "{}", err.message());
    }

    #[test]
    fn parse_nested_compound_entity_ref_constructor_in_brace_query() {
        let cgs = book_library_entity_ref_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_row_filter_body(
            "Book{library=Library(region=us-west, code=shared-shelf)}",
            &stack,
            map,
            &[],
            &std::collections::BTreeSet::new(),
        )
        .unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "library");
        let wire = value.to_value();
        let Value::Object(m) = wire else {
            panic!("expected normalized object, got {value:?}");
        };
        assert_eq!(m.get("region"), Some(&Value::String("us-west".into())));
        assert_eq!(m.get("code"), Some(&Value::String("shared-shelf".into())));
        // `parse_row_filter_body` returns predicate parser IR; the enclosing
        // pipe owns row-grain type checking.
    }

    /// Nested entity constructor on a scoped query param (`LangTag{item_id=LangItem(id=…)}`).
    #[test]
    fn parse_langtag_query_nested_langitem_constructor() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangTag{item_id=LangItem(id=i1)}", &cgs)
            .expect("nested LangItem constructor should parse");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "item_id");
        let _ = crate::type_checker::type_check_expr(&r.expr, &cgs);
        let _ = value;
    }

    /// Mid-call heredoc with comma-suffixed close on the same line (user repro / PLP-2 staging).
    #[test]
    fn parse_method_call_heredoc_mid_arg_glued_comma_trailing() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let expr = concat!(
            "LangItem.create(title=<<PLASM_INLINE_SAME\n",
            "same-line body\n",
            "PLASM_INLINE_SAME, score=0, owner=\"inline-same-line\")",
        );
        let r = parse(expr, &cgs).expect("same-line heredoc close with trailing args should parse");
        assert!(
            matches!(r.expr, Expr::Create(_) | Expr::Invoke(_) | Expr::Chain(_)),
            "expected create/invoke path, got {:?}",
            r.expr
        );
    }

    /// Glued `TAG)` after heredoc body in a method call must parse (language matrix).
    #[test]
    fn parse_langitem_create_glued_heredoc_close() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let expr = concat!(
            "LangItem.create(",
            "title=<<B\n# Item Comment\n- [ ] Task 1\n- [ ] Task 2\nB, score=0, owner=\"matrix\")",
        );
        let r = parse(expr, &cgs).expect("glued TAG) close should parse");
        let _ = crate::type_checker::type_check_expr(&r.expr, &cgs);
        assert!(
            matches!(r.expr, Expr::Create(_) | Expr::Invoke(_) | Expr::Chain(_)),
            "expected create/invoke path, got {:?}",
            r.expr
        );
    }

    /// Unary `Entity($)` parses inside brace-query RHS (teaching table fill-in, same as scalar `$`).
    #[test]
    fn parse_accepts_unary_entity_ctor_dollar_in_brace_query() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let r = parse("LangTag{item_id=LangItem($)}", &cgs)
            .expect("LangItem($) in filter should parse");
        let Expr::Query(q) = r.expr else {
            panic!("expected Query, got {:?}", r.expr);
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected simple comparison, got {pred:?}");
        };
        assert_eq!(value.to_value(), Value::String("$".into()));
    }

    fn compound_get_fixture_cgs() -> CGS {
        let mut cgs = CGS::new();
        seed_fx_str(&mut cgs);
        let f = |n: &str| registry_test_util::entity_field_from_values(&cgs, "fx_str", n, true, "");
        cgs.add_resource(ResourceSchema {
            name: "Ticket".into(),
            description: String::new(),
            id_field: "n".into(),
            id_format: None,
            id_from: None,
            fields: vec![f("owner"), f("repo"), f("n")],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["owner".into(), "repo".into(), "n".into()],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "ticket_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Ticket".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "var", "name": "owner"},
                        {"type": "var", "name": "repo"},
                        {"type": "var", "name": "n"}
                    ]
                })
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs.validate().unwrap();
        cgs
    }

    #[test]
    fn parse_compound_get_rejects_scalar_form() {
        let cgs = compound_get_fixture_cgs();
        let err = parse("Ticket(1)", &cgs).unwrap_err();
        let msg = err.message();
        assert!(
            msg.contains("compound key") || msg.contains("key=value"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn parse_compound_get_accepts_kv_form() {
        let cgs = compound_get_fixture_cgs();
        let r = parse("Ticket(owner=o,repo=r,n=9)", &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get");
        };
        let EntityKey::Compound(m) = &g.reference.key else {
            panic!("expected compound key");
        };
        assert_eq!(m.get("owner").and_then(|s| s.as_lit_str()), Some("o"));
        assert_eq!(m.get("repo").and_then(|s| s.as_lit_str()), Some("r"));
        assert_eq!(m.get("n").and_then(|s| s.as_lit_str()), Some("9"));
    }

    #[test]
    fn program_parse_compound_get_maps_binding_slot_to_identity_slot() {
        let cgs = compound_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut refs = BTreeSet::new();
        refs.insert("zone".into());
        let r = parse_with_cgs_layers_program(
            "Ticket(owner=zone,repo=\"r\",n=9)",
            &stack,
            sym_map,
            Some(&refs),
            false,
        )
        .expect("compound get with program binding");
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get");
        };
        let EntityKey::Compound(m) = &g.reference.key else {
            panic!("expected compound ref");
        };
        assert!(
            matches!(
                m.get("owner"),
                Some(crate::IdentitySlot::Binding(PlasmInputRef::NodeInput { node, path }))
                    if node == "zone" && path.is_empty()
            ),
            "expected Binding(zone) on owner slot, got {:?}",
            m.get("owner")
        );
        assert_eq!(m.get("repo").and_then(|s| s.as_lit_str()), Some("r"));
        assert_eq!(m.get("n").and_then(|s| s.as_lit_str()), Some("9"));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn get_expr_serde_json_roundtrip_with_compound_identity_slots() {
        let mut parts = BTreeMap::new();
        parts.insert("repo".into(), crate::IdentitySlot::lit("r"));
        parts.insert("n".into(), crate::IdentitySlot::lit("9"));
        parts.insert(
            "owner".into(),
            crate::IdentitySlot::binding(PlasmInputRef::node_output("zone", vec![])),
        );
        let g = GetExpr::from_ref(Ref::compound_slots("Ticket", parts));
        let json = serde_json::to_value(&g).unwrap();
        let back: GetExpr = serde_json::from_value(json).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn parse_compound_get_nested_entity_constructor_stringifies_slot() {
        let mut cgs = compound_get_fixture_cgs();
        cgs.add_resource(ResourceSchema {
            name: "Library".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                str_field(&cgs, "id"),
                str_field(&cgs, "region"),
                str_field(&cgs, "code"),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["region".into(), "code".into()],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: Some("library_get_nested_fixture".into()),
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "library_get_nested_fixture".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Library".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method":"GET",
                    "path":[
                        {"type":"var","name":"region"},
                        {"type":"literal","value":"/"},
                        {"type":"var","name":"code"}
                    ]
                })
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs.validate().unwrap();
        let r = parse(
            "Ticket(owner=acme, repo=Library(region=eu, code=main), n=42)",
            &cgs,
        )
        .unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get");
        };
        let EntityKey::Compound(m) = &g.reference.key else {
            panic!("expected compound key");
        };
        let repo = m.get("repo").expect("repo").as_lit_str().expect("lit repo");
        assert!(repo.contains("eu"), "{repo}");
        assert!(repo.contains("main"), "{repo}");
    }

    #[test]
    fn parse_compound_get_requires_all_keys() {
        let cgs = compound_get_fixture_cgs();
        let err = parse("Ticket(owner=o,repo=r)", &cgs).unwrap_err();
        assert!(err.message().contains("exactly keys"));
    }

    fn many_rel_unmaterialized_cgs() -> CGS {
        let mut cgs = CGS::new();
        seed_fx_str(&mut cgs);
        let id_field = registry_test_util::entity_field_from_values(&cgs, "fx_str", "id", true, "");
        cgs.add_resource(ResourceSchema {
            name: "Parent".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![id_field.clone()],
            relations: vec![RelationSchema {
                name: "items".into(),
                description: String::new(),
                target_resource: "Child".into(),
                cardinality: Cardinality::Many,
                materialize: None,
                discovery: None,
            }],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_resource(ResourceSchema {
            name: "Child".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![id_field],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "parent_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Parent".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({"method":"GET","path":[
                    {"type":"literal","value":"parent"},
                    {"type":"literal","value":"/"},
                    {"type":"var","name":"id"}
                ]})
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "child_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Child".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({"method":"GET","path":[
                    {"type":"literal","value":"children"}
                ]})
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        // Intentionally skip `validate()` — many-relations without `materialize:` are
        // load-rejected but the parser still surfaces `ManyRelationUnmaterialized` for
        // programmatic / legacy CGS built in-memory.
        cgs
    }

    #[test]
    fn parse_many_relation_unmaterialized_errors() {
        let cgs = many_rel_unmaterialized_cgs();
        let err = parse("Parent(p1).items", &cgs).unwrap_err();
        assert!(
            matches!(err.kind, ParseErrorKind::ManyRelationUnmaterialized { .. }),
            "expected ManyRelationUnmaterialized, got {:?}",
            err.kind
        );
    }

    #[test]
    fn parse_many_relation_braces_cannot_replace_receiver_with_query() {
        let cgs = many_rel_unmaterialized_cgs();
        let error = parse("Parent(p1).items{id=a}", &cgs).unwrap_err();
        assert!(error.to_string().contains("does not accept query braces"));
    }

    #[test]
    fn parse_langitem_materialized_many_relation_yields_chain() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        for input in ["LangItem(i1).tags", "LangItem(i1).children"] {
            let r = parse(input, &cgs).unwrap();
            assert!(
                matches!(r.expr, Expr::Chain(_)),
                "{input}: expected Chain, got {:?}",
                r.expr
            );
        }
    }

    /// `Entity:id` must not parse as query-all + ignored tail (would return wrong first row).
    #[test]
    fn parse_rejects_entity_colon_after_name() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let err = parse("Pet:1", &cgs).unwrap_err();
        assert!(
            matches!(
                err.kind,
                ParseErrorKind::ColonAfterEntityName { ref entity } if entity == "Pet"
            ),
            "expected ColonAfterEntityName, got {:?}",
            err.kind
        );
    }

    #[test]
    fn parse_rejects_get_wrapper_keyword() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let err = parse("Get(Pet:1)", &cgs).unwrap_err();
        assert!(
            matches!(err.kind, ParseErrorKind::Other { .. }),
            "expected Other hint for `Get(`, got {:?}",
            err.kind
        );
        assert!(
            err.message().contains("does not use a `Get(`"),
            "msg: {}",
            err.message()
        );
    }

    #[test]
    fn program_parse_maps_known_binding_to_plasm_input_ref_in_predicate() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut refs = std::collections::BTreeSet::new();
        refs.insert("report".into());
        let r = parse_with_cgs_layers_program(
            "LangItem{status=report}",
            &stack,
            sym_map,
            Some(&refs),
            false,
        )
        .expect("parse program predicate");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert!(
            matches!(
                value.to_value(),
                Value::PlasmInputRef(crate::PlasmInputRef::NodeInput { node, path })
                    if node == "report" && path.is_empty()
            ),
            "expected PlasmInputRef(report), got {value:?}"
        );
    }

    #[test]
    fn program_parse_unknown_ident_becomes_phrase_ident_in_predicate() {
        // Bare unquoted words in query `{…}` predicates coerce to string at parse time via
        // [`coerce_value_for_field_type`] (same path as teaching-table query filters).
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut refs = std::collections::BTreeSet::new();
        refs.insert("not_report".into());
        let r = parse_with_cgs_layers_program(
            "LangItem{title=report}",
            &stack,
            sym_map,
            Some(&refs),
            false,
        )
        .expect("parse");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String("report".into()));
    }

    #[test]
    fn langitem_search_method_sugar() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_with_cgs_layers(
            r#"LangItem.search(q="bug", team_key="ENG")"#,
            &stack,
            sym_map,
        )
        .expect("parse");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        assert_eq!(q.capability_name.as_deref(), Some("langitem_search"));
    }

    #[test]
    fn bare_search_tilde_without_text_is_search_text_error() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let err = parse_with_cgs_layers(r#"LangItem~"#, &stack, sym_map).unwrap_err();
        assert!(
            matches!(err.kind, ParseErrorKind::SearchTextMustBeString),
            "expected SearchTextMustBeString, got {err:?}"
        );
    }

    #[test]
    fn language_matrix_search_tilde_and_brace_q_are_search_not_query_all() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(dir).expect("language matrix");
        let tilde = parse(r#"LangItem~"no-such-item""#, &cgs).expect("parse tilde");
        let Expr::Query(tq) = &tilde.expr else {
            panic!("expected Query, got {:?}", tilde.expr);
        };
        assert_eq!(tq.capability_name.as_deref(), Some("langitem_search"));
        let brace = parse(r#"LangItem{q="no-such-item"}"#, &cgs).expect("parse brace q");
        let Expr::Query(bq) = &brace.expr else {
            panic!("expected Query, got {:?}", brace.expr);
        };
        assert!(
            bq.capability_name.is_none(),
            "brace does not stamp Search at parse; resolve must"
        );
        let cap = crate::query_resolve::resolve_query_capability(bq, &cgs).expect("RA-2");
        assert_eq!(cap.name.as_str(), "langitem_search");
    }

    #[test]
    fn search_tilde_text_binds_the_structural_selection_parameter() {
        let dir = std::path::Path::new("../../fixtures/schemas/auth_bearer_search");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse(r#"SecuredNote~"trip""#, &cgs).expect("parse search");
        let Expr::Query(q) = &r.expr else {
            panic!("expected Query, got {:?}", r.expr);
        };
        let pred = q.predicate.as_ref().expect("predicate");
        fn access_token_eq_trip(p: &Predicate) -> bool {
            match p {
                Predicate::Comparison { field, value, .. } if field == "access_token" => {
                    matches!(value.to_value(), Value::String(s) if s == "trip")
                }
                Predicate::And { args } | Predicate::Or { args } => {
                    args.iter().any(access_token_eq_trip)
                }
                Predicate::Not { predicate } => access_token_eq_trip(predicate),
                _ => false,
            }
        }
        assert!(
            !access_token_eq_trip(pred),
            "search text must bind the selection lane, not an authentication input; pred={pred:?}"
        );
        let fields = pred.referenced_fields();
        assert!(
            fields.iter().any(|f| f == "query"),
            "expected query field from tilde text; fields={fields:?}"
        );
    }

    #[test]
    fn create_only_filter_on_get_only_entity_does_not_name_a_search_wire() {
        let dir = std::path::Path::new("../../fixtures/schemas/create_filter_polarity");
        assert!(
            dir.exists(),
            "missing fixture schemas/create_filter_polarity"
        );
        let cgs = load_schema_dir(dir).unwrap();
        let err = parse(r#"Doc{access_token="tok"}"#, &cgs).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no Query or Search filter surface"),
            "expected get-only honesty, got {msg}"
        );
        assert!(
            !msg.contains("use the Search-capability param wire"),
            "must not name a Search wire when none exists: {msg}"
        );
    }

    #[test]
    fn unquoted_string_filter_wire_value_typechecks_at_compile_time() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("LangItem{owner=matrix-dev}", &cgs)
            .expect("parse unquoted string filter wire value");
        crate::type_checker::type_check_expr(&r.expr, &cgs).expect("string wire value typechecks");
    }

    #[test]
    fn unquoted_integer_field_rejects_non_numeric_at_compile_time() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        // RA-8: hard coerce rejects non-numeric tokens at parse (not soft-leave-as-string).
        let err = parse("LangItem{score=notanint}", &cgs).expect_err("non-numeric integer token");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("cannot coerce") && msg.contains("integer"),
            "expected RA-8 integer coerce reject, got {msg}"
        );
    }

    /// Program-mode bare tokens on invoke/create body fields must coerce via the **same**
    /// [`coerce_value_for_field_type_with_policy`] (`InvokeArg`) path as query predicates
    /// (PhraseIdent ≡ stringish). Covers payload-lane Create — not arguments-only.
    #[test]
    fn program_invoke_bare_bool_coerces_like_query_filter() {
        use crate::InvokeInputPayload;
        use std::sync::Arc;

        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);

        // Update (payload body): bare true must become Bool before typecheck.
        let mut r = parse_with_cgs_layers_program(
            r#"LangItem("i1").update(active=true, score=3)"#,
            &stack,
            Arc::clone(&sym_map),
            None,
            false,
        )
        .expect("program invoke parse");
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        let Some(InvokeInputPayload::Raw(Value::Object(map))) = &inv.input else {
            panic!("expected raw object input, got {:?}", inv.input);
        };
        assert_eq!(
            map.get("active"),
            Some(&Value::Bool(true)),
            "invoke coerce must yield Bool(true), got {:?}",
            map.get("active")
        );
        assert_eq!(map.get("score"), Some(&Value::Integer(3)));

        let labels = std::collections::BTreeSet::new();
        crate::lower_program_phrase_idents_in_expr(&mut r.expr, &labels, &cgs)
            .expect("phrase lower");
        crate::type_checker::type_check_expr(&r.expr, &cgs)
            .expect("program invoke with bare bool must typecheck");
    }

    /// Create capabilities author body fields under `inputs.payload`. Parse coerce must walk
    /// that lane (payload ∪ arguments) — Boolean and temporal PhraseIdent alike.
    #[test]
    fn program_create_payload_bare_tokens_coerce_like_invoke_args() {
        use std::sync::Arc;

        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);

        let mut r = parse_with_cgs_layers_program(
            r#"LangItem.create(title="payload-coerce", active=false, recorded_at=now)"#,
            &stack,
            sym_map,
            None,
            false,
        )
        .expect("program create parse");
        let Expr::Create(create) = &r.expr else {
            panic!("expected Create, got {:?}", r.expr);
        };
        let Value::Object(map) = create.input.to_value() else {
            panic!("expected object create input, got {:?}", create.input);
        };
        assert_eq!(
            map.get("active"),
            Some(&Value::Bool(false)),
            "Create payload coerce must yield Bool(false), got {:?}",
            map.get("active")
        );
        let recorded = map
            .get("recorded_at")
            .expect("recorded_at present after coerce");
        assert!(
            !matches!(recorded, Value::PhraseIdent(_)),
            "Create payload temporal must leave PhraseIdent via shared coerce, got {recorded:?}"
        );
        assert!(
            matches!(recorded, Value::String(_)),
            "expected temporal normalize to String, got {recorded:?}"
        );

        let labels = std::collections::BTreeSet::new();
        crate::lower_program_phrase_idents_in_expr(&mut r.expr, &labels, &cgs)
            .expect("phrase lower");
        crate::type_checker::type_check_expr(&r.expr, &cgs)
            .expect("program Create with payload bare bool + temporal must typecheck");
    }

    #[test]
    fn federated_langitem_collision_parse_uses_opaque_entity_catalog() {
        use crate::symbol_tuning::TeachingExposureSession;
        use std::path::Path;
        use std::sync::Arc;

        let dir = Path::new("../../fixtures/schemas/plasm_language_matrix");
        let mut cgs_a = load_schema_dir(dir).expect("plasm_language_matrix");
        cgs_a.bind_registry_entry_id("langmatrix_a");
        let mut cgs_b = load_schema_dir(dir).expect("plasm_language_matrix");
        cgs_b.bind_registry_entry_id("langmatrix_b");
        let layers = [&cgs_a, &cgs_b];
        let stack = cgs_layer_stack(&["langmatrix_a", "langmatrix_b"], &layers);
        let mut exp = TeachingExposureSession::new(&cgs_a, "langmatrix_a", &["LangItem"]);
        exp.expose_entities(
            &layers,
            Arc::new(cgs_b.clone()),
            "langmatrix_b",
            &["LangItem"],
        );
        let map = exp.symbol_map_arc();
        assert_eq!(
            map.entry_id_for_entity_symbol("e1").as_deref(),
            Some("langmatrix_a")
        );
        assert_eq!(
            map.entry_id_for_entity_symbol("e2").as_deref(),
            Some("langmatrix_b")
        );
        let e2 = "e2";
        let team_key =
            map.ident_sym_cap_param_for("langmatrix_b", "LangItem", "langitem_search", "team_key");
        let expr = format!(r#"{e2}~"plasm"{{{team_key}="ENG"}}"#);
        let r =
            parse_with_cgs_layers(&expr, &stack, map).expect("parse langmatrix_b LangItem search");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query, got {:?}", r.expr);
        };
        assert_eq!(q.capability_name.as_deref(), Some("langitem_search"));
        assert_eq!(q.entity, "LangItem");
        assert_eq!(
            q.catalog_entry_id.as_deref(),
            Some("langmatrix_b"),
            "e2 must stamp langmatrix_b catalog ownership"
        );
    }

    /// Dual-catalog LangItem: pathless Action on stamped `e2` must keep `catalog_entry_id`.
    /// Matrix counterpart: `lang_federated_duplicate_entity_pathless_action`.
    #[test]
    fn federated_pathless_action_preserves_opaque_entity_catalog_stamp() {
        use crate::symbol_tuning::TeachingExposureSession;
        use std::path::Path;
        use std::sync::Arc;

        let dir = Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.is_dir() {
            return;
        }
        let mut cgs_a = load_schema_dir(dir).expect("matrix a");
        cgs_a.bind_registry_entry_id("langmatrix_a");
        let mut cgs_b = load_schema_dir(dir).expect("matrix b");
        cgs_b.bind_registry_entry_id("langmatrix_b");
        let layers = [&cgs_a, &cgs_b];
        let stack = cgs_layer_stack(&["langmatrix_a", "langmatrix_b"], &layers);
        let mut exp = TeachingExposureSession::new(&cgs_a, "langmatrix_a", &["LangItem"]);
        exp.expose_entities(
            &layers,
            Arc::new(cgs_b.clone()),
            "langmatrix_b",
            &["LangItem"],
        );
        let map = exp.symbol_map_arc();
        assert_eq!(
            map.entry_id_for_entity_symbol("e2").as_deref(),
            Some("langmatrix_b")
        );
        let m_sym = map.method_sym_for("langmatrix_b", "LangItem", "broadcast");
        let expr = format!(r#"e2.{m_sym}(message="stamp-pathless")"#);
        let r = parse_with_cgs_layers_program(&expr, &stack, map.clone(), None, false)
            .expect("parse stamped pathless Action");
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        assert_eq!(inv.capability.as_str(), "langitem_broadcast");
        assert_eq!(
            inv.catalog_entry_id.as_deref(),
            Some("langmatrix_b"),
            "pathless Action must preserve e2 langmatrix_b stamp"
        );

        let zero = format!("e2.{m_sym}()");
        let r0 = parse_with_cgs_layers_program(&zero, &stack, map, None, false)
            .expect("parse stamped zero-arity pathless Action");
        let Expr::Invoke(inv0) = &r0.expr else {
            panic!("expected Invoke, got {:?}", r0.expr);
        };
        assert_eq!(
            inv0.catalog_entry_id.as_deref(),
            Some("langmatrix_b"),
            "zero-arity pathless Action must preserve e2 langmatrix_b stamp"
        );
    }

    /// Bare wire pathless Action under LangItem homographs must be AmbiguousEntityCatalog,
    /// not UnknownEntity / session-token lies.
    #[test]
    fn federated_pathless_action_bare_wire_is_ambiguous_entity_catalog() {
        use crate::symbol_tuning::TeachingExposureSession;
        use std::path::Path;
        use std::sync::Arc;

        let dir = Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.is_dir() {
            return;
        }
        let mut cgs_github = load_schema_dir(dir).expect("matrix github");
        cgs_github.bind_registry_entry_id("github");
        let mut cgs_linear = load_schema_dir(dir).expect("matrix linear");
        cgs_linear.bind_registry_entry_id("linear");
        let layers = [&cgs_github, &cgs_linear];
        let stack = cgs_layer_stack(&["github", "linear"], &layers);
        let mut exp = TeachingExposureSession::new(&cgs_github, "github", &["LangItem"]);
        exp.expose_entities(
            &layers,
            Arc::new(cgs_linear.clone()),
            "linear",
            &["LangItem"],
        );
        let map = exp.symbol_map_arc();
        let m_sym = map.method_sym_for("linear", "LangItem", "broadcast");
        let expr = format!(r#"LangItem.{m_sym}(message="x")"#);
        let err = parse_with_cgs_layers_program(&expr, &stack, map, None, false)
            .expect_err("bare LangItem pathless Action must not resolve under homograph");
        assert!(
            matches!(
                err.kind,
                ParseErrorKind::AmbiguousEntityCatalog { ref entity } if entity == "LangItem"
            ),
            "expected AmbiguousEntityCatalog(LangItem), got {err:?}"
        );
    }

    /// Session `e2` (Library) compound ctor inside brace predicate on `e1` (Book) — referential transparency.
    #[test]
    fn parse_predicate_compound_entity_ref_via_session_symbol() {
        use std::sync::Arc;

        let cgs = book_library_entity_ref_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let lib_sym = sym_map.entity_sym_for("", "Library");
        let book_sym = sym_map.entity_sym_for("", "Book");
        if lib_sym == "Library" || book_sym == "Book" {
            return;
        }
        let expr = format!("{book_sym}{{library={lib_sym}(region=us-west, code=shared-shelf)}}");
        let r = parse_row_filter_body(
            &expr,
            &stack,
            sym_map,
            &[],
            &std::collections::BTreeSet::new(),
        )
        .expect("symbolic nested ctor");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "library");
        let wire = value.to_value();
        let Value::Object(m) = wire else {
            panic!("expected object, got {value:?}");
        };
        assert_eq!(m.get("region"), Some(&Value::String("us-west".into())));
        assert_eq!(m.get("code"), Some(&Value::String("shared-shelf".into())));
        // The enclosing row-filter pipe, not this parser-shaped Query witness,
        // owns row-grain type checking.
    }

    /// Opaque `e#` LangItem ctor inside LangTag query predicate.
    #[test]
    fn parse_langtag_predicate_session_langitem_ctor() {
        use std::sync::Arc;

        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let mut cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        cgs.bind_registry_entry_id("langmatrix");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let tag_sym = sym_map.entity_sym_for("langmatrix", "LangTag");
        let item_sym = sym_map.entity_sym_for("langmatrix", "LangItem");
        let item_id =
            sym_map.ident_sym_cap_param_for("langmatrix", "LangTag", "langtag_query", "item_id");
        let id_field = sym_map.ident_sym_entity_field_for("langmatrix", "LangItem", "id");
        let expr = format!(r#"{tag_sym}{{{item_id}={item_sym}({id_field}=i1)}}"#);
        let r =
            parse_with_cgs_layers(&expr, &stack, sym_map).expect("langmatrix symbolic predicate");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        assert!(q.predicate.is_some());
        let _ = crate::type_checker::type_check_expr(&r.expr, &cgs);
    }

    /// Nested session entity ctor + binding field ref with symbolic `p#` path on language matrix.
    #[test]
    fn program_parse_binding_field_ref_and_nested_entity_ctor_in_method_args() {
        use std::collections::BTreeSet;
        use std::sync::Arc;

        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        let mut cgs = load_schema_dir(dir).expect("plasm_language_matrix");
        cgs.bind_registry_entry_id("langmatrix");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let item_sym = sym_map.entity_sym_for("langmatrix", "LangItem");
        let method_sym = sym_map.method_sym_for("langmatrix", "LangItem", "langitem_create");
        let title =
            sym_map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "title");
        let owner =
            sym_map.ident_sym_cap_param_for("langmatrix", "LangItem", "langitem_create", "owner");
        let owner_field = sym_map.ident_sym_entity_field_for("langmatrix", "LangItem", "owner");
        let mut refs = BTreeSet::new();
        refs.insert("item".into());
        refs.insert("body".into());
        let expr =
            format!(r#"{item_sym}.{method_sym}({title}=body.content, {owner}=item.{owner_field})"#);
        let r = parse_with_cgs_layers_program(&expr, &stack, sym_map, Some(&refs), false)
            .expect("method args with nested e# ctor + issue.p# + body.content");
        let _ = crate::type_checker::type_check_expr(&r.expr, &cgs);
        fn find_input_refs(v: &Value) -> Vec<PlasmInputRef> {
            match v {
                Value::PlasmInputRef(r) => vec![r.clone()],
                Value::Object(m) => m.values().flat_map(find_input_refs).collect(),
                Value::Array(a) => a.iter().flat_map(find_input_refs).collect(),
                _ => vec![],
            }
        }
        let payload = match &r.expr {
            Expr::Create(c) => c.input.to_value(),
            Expr::Invoke(i) => i.input.as_ref().expect("invoke input").to_value(),
            other => panic!("expected create/invoke, got {other:?}"),
        };
        let refs_found: Vec<_> = find_input_refs(&payload);
        assert!(
            refs_found.iter().any(|r| matches!(
                r,
                PlasmInputRef::NodeInput { node, path }
                    if node == "item" && path == &["owner"]
            )),
            "expected item.owner PlasmInputRef, got {refs_found:?}"
        );
        assert!(
            refs_found.iter().any(|r| matches!(
                r,
                PlasmInputRef::NodeInput { node, path }
                    if node == "body" && path == &["content"]
            )),
            "expected body.content PlasmInputRef, got {refs_found:?}"
        );
    }

    fn simple_name_id_get_fixture_cgs() -> CGS {
        let mut cgs = CGS::new();
        seed_fx_str(&mut cgs);
        let f = |n: &str| registry_test_util::entity_field_from_values(&cgs, "fx_str", n, true, "");
        cgs.add_resource(ResourceSchema {
            name: "Pet".into(),
            description: String::new(),
            id_field: "name".into(),
            id_format: None,
            id_from: None,
            fields: vec![f("name")],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "pet_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Pet".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "var", "name": "name"}]
                })
                .into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        // Pathful Action: identity projects into CML path `name` (Lit|Binding parity).
        cgs.add_capability(CapabilitySchema {
            name: "pet_act".into(),
            description: String::new(),
            kind: CapabilityKind::Action,
            domain: "Pet".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "POST",
                    "path": [
                        {"type": "literal", "value": "pets"},
                        {"type": "var", "name": "name"},
                        {"type": "literal", "value": "act"}
                    ],
                    "body": {
                        "type": "object",
                        "fields": [["message", {"type": "var", "name": "message"}]]
                    }
                })
                .into(),
            }),
            derived: None,
            inputs: crate::schema::CapabilityInputs {
                receiver: Some(crate::CapabilityReceiver::Entity {
                    entity: "Pet".into(),
                }),
                arguments: Some(InputSchema {
                    input_type: InputType::Object {
                        fields: vec![InputFieldSchema {
                            name: "message".into(),
                            wire: InputFieldWire::Registry(
                                crate::schema::ValueDomainKey::new("fx_str").unwrap(),
                            ),
                            required: true,
                            description: None,
                            default: None,
                            wire_json_path: None,
                            wire_array_element_key: None,
                            sink_class: None,
                        }],
                        additional_fields: false,
                    },
                    validation: Default::default(),
                    description: None,
                    examples: vec![],
                }),
                ..Default::default()
            },
            output_schema: Some(OutputSchema {
                output_type: OutputType::SideEffect {
                    description: "acts on the pet".into(),
                },
                decoder: serde_json::json!({}),
                idempotent: false,
                reconcile: None,
            }),
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        // Pathless Action: no path vars — bare `Pet.broadcast(...)` (teaching + binding typecheck).
        cgs.add_capability(CapabilitySchema {
            name: "pet_broadcast".into(),
            description: String::new(),
            kind: CapabilityKind::Action,
            domain: "Pet".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "POST",
                    "path": [
                        {"type": "literal", "value": "pets"},
                        {"type": "literal", "value": "broadcast"}
                    ],
                    "body": {
                        "type": "object",
                        "fields": [["message", {"type": "var", "name": "message"}]]
                    }
                })
                .into(),
            }),
            derived: None,
            inputs: crate::schema::CapabilityInputs {
                arguments: Some(InputSchema {
                    input_type: InputType::Object {
                        fields: vec![InputFieldSchema {
                            name: "message".into(),
                            wire: InputFieldWire::Registry(
                                crate::schema::ValueDomainKey::new("fx_str").unwrap(),
                            ),
                            required: false,
                            description: None,
                            default: None,
                            wire_json_path: None,
                            wire_array_element_key: None,
                            sink_class: None,
                        }],
                        additional_fields: false,
                    },
                    validation: Default::default(),
                    description: None,
                    examples: vec![],
                }),
                ..Default::default()
            },
            output_schema: Some(OutputSchema {
                output_type: OutputType::SideEffect {
                    description: "broadcasts without path identity".into(),
                },
                decoder: serde_json::json!({}),
                idempotent: false,
                reconcile: None,
            }),
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],

            deterministic: None,
        })
        .unwrap();
        cgs.validate().unwrap();
        cgs
    }

    #[test]
    fn parse_id_field_eq_in_get_parens_lowers_to_get() {
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_with_cgs_layers_program(
            r#"Pet(name == "pikachu")"#,
            &stack,
            sym_map,
            None,
            false,
        )
        .expect("parse");
        let Expr::Get(g) = r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), "pikachu");
    }

    #[test]
    fn parse_id_field_shadow_sugar_single_eq_unchanged() {
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_with_cgs_layers_program(r#"Pet(name=pikachu)"#, &stack, sym_map, None, false)
            .expect("parse");
        let Expr::Get(g) = r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), "pikachu");
    }

    #[test]
    fn program_parse_positional_get_accepts_unquoted_phrase_ident() {
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_with_cgs_layers_program("Pet(pikachu)", &stack, sym_map, None, false)
            .expect("parse");
        let Expr::Get(g) = r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), "pikachu");
    }

    #[test]
    fn program_parse_identity_brace_with_binding_matches_paren_get() {
        use std::collections::BTreeSet;
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut labels = BTreeSet::new();
        labels.insert("sess".into());
        let paren = parse_with_cgs_layers_program(
            "Pet(sess.name)",
            &stack,
            Arc::clone(&sym_map),
            Some(&labels),
            false,
        )
        .expect("paren binding get");
        let braced = parse_with_cgs_layers_program(
            "Pet{name=sess.name}",
            &stack,
            sym_map,
            Some(&labels),
            false,
        )
        .expect("brace binding get");
        let (Expr::Get(paren_g), Expr::Get(brace_g)) = (&paren.expr, &braced.expr) else {
            panic!("expected Gets, got {:?} / {:?}", paren.expr, braced.expr);
        };
        assert_eq!(
            paren_g.reference.key, brace_g.reference.key,
            "e#{{id_field=path}} must lower to the same Get identity as e#(path)"
        );
        assert!(
            matches!(
                &brace_g.reference.key,
                EntityKey::Simple(crate::IdentitySlot::Binding(PlasmInputRef::NodeInput { node, path }))
                    if node == "sess" && path.as_slice() == ["name"]
            ),
            "expected Binding(sess.name), got {:?}",
            brace_g.reference.key
        );
    }

    #[test]
    fn program_parse_positional_get_with_binding_uses_node_input() {
        use std::collections::BTreeSet;
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut labels = BTreeSet::new();
        labels.insert("pikachu".into());
        let r =
            parse_with_cgs_layers_program("Pet(pikachu)", &stack, sym_map, Some(&labels), false)
                .expect("binding ref");
        let Expr::Get(g) = r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert!(
            matches!(
                &g.reference.key,
                EntityKey::Simple(crate::IdentitySlot::Binding(PlasmInputRef::NodeInput {
                    node,
                    path
                })) if node == "pikachu" && path.is_empty()
            ),
            "expected Simple(Binding(pikachu)), got {:?}",
            g.reference.key
        );
    }

    /// An entity action accepts literal and bound identity without changing its arguments.
    #[test]
    fn binding_and_literal_method_invoke_ir_equivalent() {
        use std::collections::BTreeSet;
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);

        let lit = parse_with_cgs_layers_program(
            r#"Pet("pikachu").act(message="hi")"#,
            &stack,
            Arc::clone(&sym_map),
            None,
            false,
        )
        .expect("lit invoke");
        let mut labels = BTreeSet::new();
        labels.insert("principal".into());
        let bind = parse_with_cgs_layers_program(
            r#"Pet(principal).act(message="hi")"#,
            &stack,
            sym_map,
            Some(&labels),
            false,
        )
        .expect("binding invoke");

        let (Expr::Invoke(lit_inv), Expr::Invoke(bind_inv)) = (&lit.expr, &bind.expr) else {
            panic!("expected Invokes, got {:?} / {:?}", lit.expr, bind.expr);
        };
        assert_eq!(lit_inv.capability, bind_inv.capability);
        assert_eq!(
            lit_inv.input.as_ref().map(|i| i.to_value()),
            bind_inv.input.as_ref().map(|i| i.to_value()),
            "body args must match — no invented path keys"
        );
        if let Some(Value::Object(m)) = lit_inv.input.as_ref().map(|i| i.to_value()) {
            assert!(
                !m.contains_key("pet_id") && !m.contains_key("name"),
                "identity must not be smuggled into body: {m:?}"
            );
        }
        assert!(matches!(
            &lit_inv.target.key,
            EntityKey::Simple(crate::IdentitySlot::Lit(id)) if id.as_str() == "pikachu"
        ));
        assert!(matches!(
            &bind_inv.target.key,
            EntityKey::Simple(crate::IdentitySlot::Binding(PlasmInputRef::NodeInput { node, path }))
                if node == "principal" && path.is_empty()
        ));
        crate::type_checker::type_check_expr(&lit.expr, &cgs).unwrap();
        crate::type_checker::type_check_expr(&bind.expr, &cgs).unwrap();
    }

    #[test]
    fn receiver_free_action_rejects_an_entity_receiver() {
        use std::collections::BTreeSet;
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_with_cgs_layers_program(
            r#"Pet.broadcast(message="hi")"#,
            &stack,
            Arc::clone(&sym_map),
            None,
            false,
        )
        .expect("bare pathless Action");
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        assert!(
            inv.target.is_pathless_nullary()
                || inv
                    .target
                    .simple_id()
                    .is_some_and(|s| s.as_str().is_empty()),
            "bare pathless Action should not invent identity, got {:?}",
            inv.target
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();

        let mut labels = BTreeSet::new();
        labels.insert("principal".into());
        let bind = parse_with_cgs_layers_program(
            r#"Pet(principal).broadcast(message="hi")"#,
            &stack,
            sym_map,
            Some(&labels),
            false,
        )
        .expect("binding pathless Action");
        let Expr::Invoke(bind_inv) = &bind.expr else {
            panic!("expected Invoke");
        };
        if let Some(Value::Object(m)) = bind_inv.input.as_ref().map(|i| i.to_value()) {
            assert!(
                !m.contains_key("pet_id") && !m.contains_key("name"),
                "pathless+binding must not invent path keys in body: {m:?}"
            );
        }
        let err = crate::type_checker::type_check_expr(&bind.expr, &cgs).unwrap_err();
        assert!(err.to_string().contains("no entity receiver"));
    }

    #[test]
    fn program_parse_hyphenated_identity_phrase_ident() {
        use std::sync::Arc;

        let cgs = simple_name_id_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_with_cgs_layers_program("Pet(name=PLA-1)", &stack, sym_map, None, false)
            .expect("parse");
        let Expr::Get(g) = r.expr else {
            panic!("expected Get, got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), "PLA-1");
    }
}

#[cfg(test)]
mod chained_groups_tests;

#[cfg(test)]
mod value_boundary_tests;
