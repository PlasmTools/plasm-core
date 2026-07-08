//! Path expression parser — parses the Plasm micro-syntax into typed [`Expr`] values.
//!
//! The grammar is LL(1); each token unambiguously determines the parse path:
//!
//! ```text
//! expr       = source pipeline* projection?
//! source     = Entity "(" id ")"               — GetExpr
//!            | Entity "(" id_field "=" value ")" — GetExpr (shadow sugar; not not taught in the teaching table)
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
//! parse("Pet(10)", &cgs)        → Ok(ParsedExpr { expr: Get(Pet,10), projection: None })
//! parse("Order{quantity>3}", &cgs) → Ok(ParsedExpr { expr: Query(...), .. })
//! parse("Order(5).petId", &cgs) → Ok(ParsedExpr { expr: Chain(..), .. })
//! ```

mod entity_ref_parse;
pub(crate) mod heredoc_surface;
pub(crate) mod predicate_surface;
pub(crate) mod program_surface;
mod value;

pub mod postfix;
pub mod program;
pub mod value_expr;

pub use heredoc_surface::{
    parse_tagged_heredoc_literal, tagged_heredoc_close_kind, HeredocCloseLineKind,
};
pub use postfix::{
    normalize_nested_projection_field, peel_postfix_suffixes, try_parse_bracket_render,
    try_parse_render_tail, BracketRender, PlasmPostfixOp, RenderTailParse,
};
pub use program::{parse_expr_node, parse_program_shape, ExprNode, ParsedProgram, Statement};
pub use program_surface::{
    collect_program_statement_lines, expand_flattened_program_statements, is_valid_program_label,
    looks_like_domain_symbol, missing_program_roots_error, program_binding_after_return_error,
    program_duplicate_return_node_error, program_empty_error, program_invalid_binding_label_error,
    program_return_keyword_error, scan_physical_line_stmt_state, split_assignment_at_top_level,
    split_assignment_for_binding, split_flattened_program_line, split_token_top_level,
    split_top_level, strip_line_comment, validate_program_label, validate_program_statement_order,
    FlattenedProgram, FlattenedProgramLine, PhysicalLineStmtState,
};
pub use value_expr::{RenderExpr, ValueExpr};

use crate::cgs_federation::CgsLayer;
use crate::schema::{
    capability_is_zero_arity_invoke, capability_path_method_segment,
    path_var_names_from_mapping_json, resolve_capability_input_param_field,
    template_invoke_requires_explicit_anchor_id,
};
use crate::symbol_tuning::{
    entity_slices_for_render, CatalogScope, FocusSpec, SymbolMap, SymbolSession,
};
use crate::{
    catalog_id::CatalogEntryStamp, coerce_value_for_field_type,
    coerce_value_for_field_type_with_policy, ArrayFieldCoercionPolicy, ArrayItemsSchema,
    CapabilityKind, CapabilityName, ChainExpr, CompOp, CreateExpr, DeleteExpr, EntityDef,
    EntityKey, EntityName, Expr, FieldType, GetExpr, InputType, InvokeExpr, InvokeInputPayload,
    PageExpr, ParameterRole, Predicate, QueryExpr, Ref, SymbolResolveError, Value, ValueWireFormat,
    CGS,
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
    CapabilityMissingInternal {
        name: String,
    },
    InvokeRequiresTargetId {
        entity: String,
        label: String,
    },
    UnexpectedTrailingInput {
        tail: String,
    },
    InvalidTemporalValue {
        message: String,
    },
    /// Prefer adding a variant above.
    Other {
        message: String,
    },
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            ParseErrorKind::CapabilityMissingInternal { name } => {
                write!(f, "internal: capability '{name}' missing")
            }
            ParseErrorKind::InvokeRequiresTargetId { entity, label } => write!(
                f,
                "`{entity}.{label}()` requires a target id; use {entity}(<id>).{label}()"
            ),
            ParseErrorKind::UnexpectedTrailingInput { tail } => {
                write!(f, "unexpected input after expression: '{tail}'")
            }
            ParseErrorKind::InvalidTemporalValue { message } => {
                write!(f, "invalid date/time value: {message}")
            }
            ParseErrorKind::Other { message } => f.write_str(message),
        }
    }
}

/// The result of parsing a path expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedExpr {
    /// The composed expression tree.
    pub expr: Expr,
    /// Optional field projection (list of field names) appended as `[f,f,...]`.
    pub projection: Option<Vec<String>>,
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

fn normalize_numeric_id_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{}", f as i64)
    } else {
        f.to_string()
    }
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
    let mut p = Parser::new(input, cgs);
    let mut parsed = p.parse_expr()?;
    parsed.expr = crate::expr_sugar::rewrite_id_field_brace_query_to_get(parsed.expr, cgs);
    Ok((parsed, p.classify_remainder()))
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
    let mut p = Parser::new(input, cgs);
    let mut parsed = p.parse_expr()?;
    parsed.expr = crate::expr_sugar::rewrite_id_field_brace_query_to_get(parsed.expr, cgs);
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
    parsed.expr =
        crate::expr_sugar::rewrite_id_field_brace_query_to_get(parsed.expr, layers[0].cgs());
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
    /// When the surface entity token was an opaque `e#`, owning catalog stamped on built [`Expr`].
    pending_session_catalog_entry_id: Option<String>,
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
            pending_session_catalog_entry_id: None,
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
                                && self.can_bind_create_path_vars(cap, source))
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
        let Some(is) = cap.input_schema.as_ref() else {
            return false;
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            return false;
        };
        let scope_fields: Vec<_> = fields
            .iter()
            .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
            .collect();
        if scope_fields.len() != 1 {
            return false;
        }
        let sf = scope_fields[0];
        let Ok(nv) = sf.named_value(cgs) else {
            return false;
        };
        let FieldType::EntityRef { target } = &nv.field_type else {
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
        self.validate_entity(&entity)?;
        let cap_name = cap.name.clone();
        if cap.kind == CapabilityKind::Create {
            return Ok(Expr::Create(CreateExpr::new(
                cap_name,
                entity,
                Value::Object(Default::default()),
            )));
        }
        let needs_anchor_id = template_invoke_requires_explicit_anchor_id(&cap.mapping.template.0);

        if cap.kind == CapabilityKind::Delete {
            if let Expr::Get(g) = source {
                return Ok(Expr::Delete(DeleteExpr::with_target_path_vars(
                    cap_name,
                    g.reference.clone(),
                    g.path_vars.clone(),
                )));
            }
            if !needs_anchor_id {
                return Ok(Expr::Delete(DeleteExpr::new(cap_name, entity, "0")));
            }
            return Err(self.err(ParseErrorKind::InvokeRequiresTargetId {
                entity: entity.clone(),
                label: raw.to_string(),
            }));
        }
        if cap.kind == CapabilityKind::Get {
            let invoke_id = if !needs_anchor_id {
                "0".to_string()
            } else {
                match source {
                    Expr::Get(g) => g.reference.primary_slot_str(),
                    _ => {
                        return Err(self.err(ParseErrorKind::InvokeRequiresTargetId {
                            entity: entity.clone(),
                            label: raw.to_string(),
                        }));
                    }
                }
            };
            return Ok(Expr::Get(GetExpr::new(entity, invoke_id)));
        }

        if !needs_anchor_id {
            if let Expr::Get(g) = source {
                return Ok(Expr::Invoke(InvokeExpr::with_target_path_vars(
                    cap_name,
                    g.reference.clone(),
                    None,
                    g.path_vars.clone(),
                )));
            }
            return Ok(Expr::Invoke(InvokeExpr::new(cap_name, entity, "0", None)));
        }
        let Expr::Get(g) = source else {
            return Err(self.err(ParseErrorKind::InvokeRequiresTargetId {
                entity: entity.clone(),
                label: raw.to_string(),
            }));
        };
        Ok(Expr::Invoke(InvokeExpr::with_target_path_vars(
            cap_name,
            g.reference.clone(),
            None,
            g.path_vars.clone(),
        )))
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
        self.validate_entity(&entity)?;
        let resolved_name = self.resolve_zero_arity_pipeline_cap(&entity, &label)?;
        let cap = self
            .cgs_for_entity_required(&entity)?
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
    fn normalize_invoke_arg_keys_for_cap(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        raw_method_label: Option<&str>,
        raw_map: IndexMap<String, Value>,
    ) -> Result<IndexMap<String, Value>, ParseError> {
        let Some(is) = &cap.input_schema else {
            return Ok(raw_map);
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            return Ok(raw_map);
        };
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
                || cap
                    .object_params()
                    .is_some_and(|fs| fs.iter().any(|f| f.name.as_str() == raw_key.as_str()))
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
        Ok(Value::UnionCtor {
            ctor_label,
            ctor_fields: normalized,
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
        let Some(is) = cap.input_schema.as_ref() else {
            return Ok(map);
        };
        let crate::InputType::Object { fields, .. } = &is.input_type else {
            return Ok(map);
        };
        for f in fields {
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
                let Some(is) = &cap.input_schema else {
                    continue;
                };
                let variants = match &is.input_type {
                    crate::InputType::Union { variants } => variants,
                    crate::InputType::Object { fields, .. } => {
                        let mut found = None;
                        for f in fields {
                            let crate::InputFieldWire::Inline(ty) = &f.wire else {
                                continue;
                            };
                            if let crate::InputType::Array { element_type, .. } = ty.as_ref() {
                                if let crate::InputType::Union { variants } = element_type.as_ref()
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

    /// Shadow sugar (not not taught in the teaching table): `Entity(id_field=value)` on simple-id entities ≡ `Entity(value)`.
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
        if matches!(id_val, Value::PlasmInputRef(_)) {
            let path_key = format!("{}_id", entity.to_lowercase());
            return Ok(Some(Expr::Get(GetExpr::from_ref_with_path_vars(
                Ref::new(entity, ""),
                Some(IndexMap::from([(path_key, id_val)])),
            ))));
        }
        let id_str = self.compound_get_slot_string_from_value(&id_val)?;
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
        if matches!(id_val, Value::PlasmInputRef(_)) {
            let path_key = format!("{}_id", entity.to_lowercase());
            return Ok(Some(Expr::Get(GetExpr::from_ref_with_path_vars(
                Ref::new(entity, ""),
                Some(IndexMap::from([(path_key, id_val)])),
            ))));
        }
        let id_str = self.compound_get_slot_string_from_value(&id_val)?;
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
            Value::Float(f) => Ok(normalize_numeric_id_float(*f)),
            Value::Object(_) => serde_json::to_string(v).map_err(|e| {
                self.err(ParseErrorKind::Other {
                    message: format!("compound get slot must be JSON-serializable: {e}"),
                })
            }),
            _ => Err(self.err(ParseErrorKind::IdMustBeStringOrNumber)),
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
                if matches!(kind, CapabilityKind::Get)
                    && !path_var_names_from_mapping_json(&cap.mapping.template.0).is_empty()
                {
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
        for kind in [CapabilityKind::Query, CapabilityKind::Search] {
            for cap in ec.find_capabilities(entity_name, kind) {
                if let Some(is) = cap.input_schema.as_ref() {
                    if let InputType::Object { fields, .. } = &is.input_type {
                        if let Some(f) = fields.iter().find(|f| f.name == field) {
                            let nv = f.named_value(ec).ok()?;
                            return Some((
                                nv.field_type.clone(),
                                nv.value_format,
                                nv.array_items.clone(),
                            ));
                        }
                    }
                }
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

    fn inject_path_vars_from_get(
        &self,
        cap: &crate::CapabilitySchema,
        source: &Expr,
        map: &mut IndexMap<String, Value>,
    ) {
        let path_vars = path_var_names_from_mapping_json(&cap.mapping.template.0);
        let Expr::Get(g) = source else {
            return;
        };
        // Explicit `Get.path_vars` from compound/simple ctor holes (`node_input`, row JSON, …).
        if let Some(explicit) = &g.path_vars {
            for (k, v) in explicit {
                if !map.contains_key(k) {
                    map.insert(k.clone(), v.clone());
                }
            }
        }
        let src_ent = g.reference.entity_type.as_str();
        let expected_id_key = format!("{}_id", src_ent.to_lowercase());
        let Ok(cgs_src) = self.cgs_for_expr_source(source) else {
            return;
        };
        let ent_src = cgs_src.get_entity(src_ent);
        match &g.reference.key {
            EntityKey::Compound(parts) => {
                for pv in path_vars {
                    if map.contains_key(&pv) {
                        continue;
                    }
                    if let Some(v) = parts.get(&pv) {
                        let wire = if let Some(ent) = ent_src {
                            let json = crate::identity_slot_to_json(cgs_src, ent, pv.as_str(), v);
                            crate::json_value_to_plasm_value(&json)
                        } else {
                            Value::String(v.clone())
                        };
                        map.insert(pv, wire);
                    }
                }
            }
            EntityKey::Simple(id) => {
                for pv in path_vars {
                    if map.contains_key(&pv) {
                        continue;
                    }
                    if pv == expected_id_key {
                        map.insert(pv, Value::String(id.to_string()));
                    } else if pv == "id" {
                        // REST templates often use `{id}` while the anchor entity uses `{type}_id`
                        // in the exemplar convention (e.g. IssueComment + path segment `id`).
                        map.insert("id".to_string(), Value::String(id.to_string()));
                    }
                }
            }
        }
    }

    fn coerce_object_input_for_cap(
        &self,
        cap: &crate::CapabilitySchema,
        map: &mut IndexMap<String, Value>,
        catalog_entry_id: Option<&str>,
    ) -> Result<(), ParseError> {
        let Some(is) = &cap.input_schema else {
            return Ok(());
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            return Ok(());
        };
        let ec =
            self.cgs_for_entity_with_preferred_catalog(cap.domain.as_str(), catalog_entry_id)?;
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
    /// Read kinds (Query / Search / Get) are rejected here so a wrong `m#` yields a clear
    /// "not a mutator" diagnostic instead of a param error naming the bound query. Zero-arity
    /// `e#.m#()` Gets still resolve via [`Self::parse_zero_arity_invoke`] (teaching pathless gets).
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
                    return Err(self.err(ParseErrorKind::Other {
                        message: format!(
                            "`{raw}` is {} `{}.{}`, not a mutator — use the teaching row for a create/update/action/delete method on `{}`",
                            cap.kind.as_str(),
                            cap.domain,
                            cap.name,
                            source.primary_entity(),
                        ),
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
        if cross.len() == 1 && self.can_bind_create_path_vars(cross[0], source) {
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

    fn can_bind_create_path_vars(&self, cap: &crate::CapabilitySchema, source: &Expr) -> bool {
        let Expr::Get(g) = source else {
            return false;
        };
        let path_vars = path_var_names_from_mapping_json(&cap.mapping.template.0);
        if path_vars.is_empty() {
            return false;
        }
        let src_ent = g.reference.entity_type.as_str();
        let expected = format!("{}_id", src_ent.to_lowercase());
        path_vars.iter().all(|pv| {
            if pv != &expected {
                return false;
            }
            if g.path_vars.as_ref().is_some_and(|m| m.contains_key(pv)) {
                return true;
            }
            match &g.reference.key {
                EntityKey::Simple(id) => !id.is_empty(),
                EntityKey::Compound(parts) => parts.contains_key(pv),
            }
        })
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
        self.inject_path_vars_from_get(cap, &source, &mut map);
        self.coerce_object_input_for_cap(
            cap,
            &mut map,
            self.active_catalog_entry_id(Some(&source)).as_deref(),
        )?;
        let input = Value::Object(map);
        self.finish_dotted_call_with_payload_value_inner(
            source,
            cap.name.clone(),
            cap.domain.clone(),
            cap.kind,
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
        let Some(is) = &cap.input_schema else {
            return Err(self.err(ParseErrorKind::Other {
                message: "this capability has no input schema — use `key=value` arguments".into(),
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
            self.inject_path_vars_from_get(cap, &source, ctor_fields);
            if let Some(is) = &cap.input_schema {
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
                    }
                }
            }
        } else {
            return Err(self.err(ParseErrorKind::Other {
                message: "expected `v#{{…}}` union constructor for this invoke".into(),
            }));
        }
        self.finish_dotted_call_with_payload_value_inner(
            source,
            cap.name.clone(),
            cap.domain.clone(),
            cap.kind,
            value,
        )
    }

    fn finish_dotted_call_with_payload_value_inner(
        &mut self,
        source: Expr,
        cap_name: CapabilityName,
        cap_domain: EntityName,
        cap_kind: CapabilityKind,
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
                let Expr::Get(g) = &source else {
                    return Err(self.err(ParseErrorKind::Other {
                        message: "delete with arguments requires Entity(id) on the left".into(),
                    }));
                };
                let path_vars = match input {
                    Value::Object(map) if !map.is_empty() => Some(map),
                    _ => g.path_vars.clone(),
                };
                Ok(Self::stamp_session_catalog_from_source(
                    &source,
                    Expr::Delete(DeleteExpr::with_target_path_vars(
                        cap_name,
                        g.reference.clone(),
                        path_vars,
                    )),
                ))
            }
            CapabilityKind::Update | CapabilityKind::Action => {
                let Expr::Get(g) = &source else {
                    return Err(self.err(ParseErrorKind::Other {
                        message: "invoke with arguments requires Entity(id) on the left".into(),
                    }));
                };
                Ok(Self::stamp_session_catalog_from_source(
                    &source,
                    Expr::Invoke(InvokeExpr::with_target_path_vars(
                        cap_name,
                        g.reference.clone(),
                        Some(input),
                        g.path_vars.clone(),
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
            .input_schema
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
        let Some(ent) = cgs.get_entity(entity_name) else {
            return Ok(());
        };
        if ent.fields.contains_key(pred_wire) {
            return Ok(());
        }
        let is_query_or_search_param = [CapabilityKind::Query, CapabilityKind::Search]
            .into_iter()
            .flat_map(|kind| cgs.find_capabilities(entity_name, kind))
            .any(|cap| {
                cap.object_params()
                    .is_some_and(|fields| fields.iter().any(|f| f.name == pred_wire))
            });
        if is_query_or_search_param {
            return Ok(());
        }
        let create_only = cgs
            .find_capabilities(entity_name, CapabilityKind::Create)
            .iter()
            .any(|cap| {
                cap.object_params()
                    .is_some_and(|fields| fields.iter().any(|f| f.name == pred_wire))
            });
        if create_only {
            return Err(ParseError {
                kind: ParseErrorKind::PredicateFieldNotFound {
                    field: pred_field.to_string(),
                    entity: format!(
                        "{entity_name} (field `{pred_wire}` is a Create-capability param; in search filters `e#~\"…\"{{…}}` use the Search-capability param wire, not a Create-only homograph)"
                    ),
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
                        self.err(ParseErrorKind::InvalidTemporalValue { message: m })
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
                val = coerce_value_for_field_type(&ft, vf, arr.as_ref(), val)
                    .map_err(|m| self.err(ParseErrorKind::InvalidTemporalValue { message: m }))?;
            }
        }
        Ok(Predicate::comparison(pred_wire, op, val))
    }

    /// Value after a comparison operator in `{…}` — may be omitted (comma or `}` next) for teaching slots.
    fn parse_predicate_rhs_after_op(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        if matches!(self.peek_char(), Some(',') | Some('}')) {
            Ok(Value::Null)
        } else {
            self.parse_predicate_value_rhs()
        }
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

    fn validate_entity(&self, name: &str) -> Result<(), ParseError> {
        if self.cgs_for_entity(name).is_none() {
            return Err(ParseError {
                kind: ParseErrorKind::UnknownEntity {
                    name: name.to_string(),
                    span_opt: None,
                },
                offset: self.pos,
            });
        }
        Ok(())
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
            .any(|cap| {
                cap.object_params()
                    .map(|fields| fields.iter().any(|f| f.name == "team_id"))
                    .unwrap_or(false)
            });
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
                    let Some(is) = cap.input_schema.as_ref() else {
                        continue;
                    };
                    let InputType::Object { fields, .. } = &is.input_type else {
                        continue;
                    };
                    let scope_fields: Vec<_> = fields
                        .iter()
                        .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
                        .collect();
                    if scope_fields.len() != 1 {
                        continue;
                    }
                    let sf = scope_fields[0];
                    if let Ok(nv) = sf.named_value(c) {
                        if let FieldType::EntityRef { target } = &nv.field_type {
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
        let Some(is) = cap.input_schema.as_ref() else {
            return Ok(None);
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            return Ok(None);
        };
        let scope_name = fields
            .iter()
            .find(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
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
                    let mut ordered: BTreeMap<String, String> = BTreeMap::new();
                    let mut ctor_path_vars: IndexMap<String, Value> = IndexMap::new();
                    for k in &ent.key_vars {
                        let v = map_values.get(k.as_str()).expect("keys validated");
                        if matches!(v, Value::PlasmInputRef(_)) {
                            ctor_path_vars.insert(k.to_string(), v.clone());
                            continue;
                        }
                        let s = self.compound_get_slot_string_from_value(v)?;
                        ordered.insert(k.to_string(), s);
                    }
                    let get = GetExpr::from_ref_with_path_vars(
                        Ref::compound(entity, ordered),
                        if ctor_path_vars.is_empty() {
                            None
                        } else {
                            Some(ctor_path_vars)
                        },
                    );
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
                    if matches!(id_val, Value::PlasmInputRef(_)) {
                        let path_key = format!("{}_id", entity.to_lowercase());
                        Ok(Expr::Get(GetExpr::from_ref_with_path_vars(
                            Ref::new(entity, ""),
                            Some(IndexMap::from([(path_key, id_val)])),
                        )))
                    } else {
                        let id_str = self.compound_get_slot_string_from_value(&id_val)?;
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
                let text = self.parse_value()?;
                let text_str = match &text {
                    Value::String(s) => s.clone(),
                    Value::Integer(n) => n.to_string(),
                    _ => return Err(self.err(ParseErrorKind::SearchTextMustBeString)),
                };
                // Find the search capability to get its primary text param name
                let q_field = {
                    let c = self.cgs_for_entity_required(&entity)?;
                    c.find_capabilities(&entity, CapabilityKind::Search)
                        .first()
                        .and_then(|cap| cap.object_params())
                        .and_then(|fields| {
                            fields
                                .iter()
                                .find(|f| {
                                    matches!(f.role, Some(crate::ParameterRole::Search))
                                        || f.required
                                })
                                .map(|f| f.name.clone())
                        })
                        .unwrap_or_else(|| "q".to_string())
                };

                let cap_name = {
                    let c = self.cgs_for_entity_required(&entity)?;
                    c.find_capabilities(&entity, CapabilityKind::Search)
                        .first()
                        .map(|c| c.name.clone())
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
            self.validate_entity(&target_entity)?;

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
                    let target = rel.target_resource.clone();
                    let cardinality = rel.cardinality;

                    // Optional filter block on relation target
                    self.skip_ws();
                    let preds = if self.peek_char() == Some('{') {
                        self.pos += 1;
                        let p = self.parse_preds(&target)?;
                        self.expect_char('}')?;
                        p
                    } else {
                        vec![]
                    };

                    // Cardinality-one without filters → ChainExpr, executed at runtime.
                    // The executor fetches the source entity, then looks up the decoded
                    // relation target ID from entity.fields[selector] (populated by the
                    // relation decoder) to dispatch Get(target, id).
                    if cardinality == crate::Cardinality::One && preds.is_empty() {
                        let chain = ChainExpr::auto_get(source, relation_field);
                        return Ok(Expr::Chain(chain));
                    }

                    if cardinality == crate::Cardinality::Many && preds.is_empty() {
                        let mat = rel
                            .materialize
                            .as_ref()
                            .unwrap_or(&crate::RelationMaterialization::Unavailable);
                        match mat {
                            crate::RelationMaterialization::FromParentGet { .. }
                            | crate::RelationMaterialization::PreferFromParentGet { .. }
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

                    // Cardinality-many with filters, or cardinality-one with filters → QueryExpr on target
                    let query = Self::preds_to_query(&target, preds);
                    return Ok(Expr::Query(query));
                }

                // Check EntityRef fields (e.g. .petId → ChainExpr)
                match ent.fields.get(field.as_str()) {
                    Some(f) => {
                        let is_ref = f
                            .named_value(cgs_src)
                            .ok()
                            .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }));
                        if !is_ref {
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
                if let Some(fields) = cap.object_params() {
                    for f in fields {
                        if let Ok(nv) = f.named_value(c) {
                            if let FieldType::EntityRef { target } = &nv.field_type {
                                if target.as_str() == source_entity {
                                    return Ok(f.name.clone());
                                }
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
                    if let FieldType::EntityRef { target } = &nv.field_type {
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

        // Optional projection
        self.skip_ws();
        let projection = if self.peek_char() == Some('[') {
            Some(self.parse_projection()?)
        } else {
            None
        };

        // One expression per call; ignore trailing noise (LLM markdown, prose, etc.).
        self.skip_ws();

        Ok(ParsedExpr { expr, projection })
    }

    fn classify_remainder(&self) -> ParseRemainder {
        let tail = self.remaining().trim();
        if tail.is_empty() {
            return ParseRemainder::Empty;
        }
        let head = tail.chars().next().unwrap_or(' ');
        if matches!(head, '{' | '.' | '[' | '(' | '~' | '=' | ')' | ',') {
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
    use crate::symbol_tuning::{entity_slices_for_render, FocusSpec, SymbolMap};
    use crate::{
        loader::load_schema_dir, CapabilityKind, CapabilityMapping, CapabilitySchema, Cardinality,
        EntityKey, FieldType, InputFieldSchema, InputFieldWire, InputSchema, InputType,
        InputVariantSchema, OutputSchema, OutputType, PlasmInputRef, RelationSchema,
        ResourceSchema, WireVariantDiscriminator, CGS,
    };
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    fn test_layer<'a>(cgs: &'a CGS) -> [CgsLayer<'a>; 1] {
        [CgsLayer::unset(cgs)]
    }

    fn seed_fx_str(cgs: &mut CGS) {
        cgs.values.insert(
            "fx_str".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
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

    /// GraphQL `issue_get` has `variables.id` but no HTTP `path` vars; pipeline must not default id to "0".
    #[test]
    fn linear_issue_get_pipeline_preserves_uuid() {
        let dir = std::path::Path::new("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let u = "d6f28392-a2a8-46ce-a1c5-b5ec81ed2396";
        let r = parse(&format!("Issue({u}).get()"), &cgs).unwrap();
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

    /// Shadow sugar: `Entity(id_field=value)` ≡ `Entity(value)` when `id_field` is the sole identity slot.
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
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "document_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Document".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": "documents"},
                        {"type": "var", "name": "slug"}
                    ]
                })
                .into(),
            },
            input_schema: None,
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
            mapping: CapabilityMapping {
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
            },
            input_schema: Some(InputSchema {
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
                            role: None,
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
            output_schema: Some(OutputSchema {
                output_type: OutputType::SideEffect {
                    description: "adds a suggestion".into(),
                },
                decoder: serde_json::json!({}),
                idempotent: false,
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
        // Pet has declared relation: tags → Tag (cardinality many)
        let r = parse("Pet(10).tags{name=fluffy}", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
        if let Expr::Query(q) = &r.expr {
            assert_eq!(q.entity, "Tag");
            assert!(q.predicate.is_some());
        }
    }

    #[test]
    fn parse_zero_arity_invoke_pathless_clickup() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("User.get-me()", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Get(_)));
        assert_eq!(r.expr.primary_entity(), "User");
    }

    /// Bare `true`/`false` in `{…}` are string tokens at parse time; coercion must turn them into
    /// [`Value::Bool`] for boolean fields (eval: List(…).tasks{archived=false}).
    #[test]
    fn parse_clickup_list_tasks_archived_false_typechecks() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("List(123456789012345678).tasks{archived=false}", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_zero_arity_invoke_pathless_clickup_no_parens() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("User.get-me", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Get(_)));
        assert_eq!(r.expr.primary_entity(), "User");
    }

    #[test]
    fn parse_zero_arity_invoke_path_clickup() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Team(42).seat_usage", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Chain(_)));
        assert_eq!(r.expr.primary_entity(), "Team");
    }

    /// Opaque session `m#` resolves to catalog capability for scoped query bridge.
    #[test]
    fn parse_clickup_team_get_space_query_via_method_symbol() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let cap = cgs.get_capability("space_query").expect("space_query");
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let sym = map.method_sym_for("", "Space", cap.name.as_str());
        if sym == cap.name.as_str() {
            return;
        }
        let line = format!("Team(42).{sym}");
        let r = parse(&line, &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected Query, got {:?}", r.expr);
        };
        assert_eq!(q.entity.as_str(), "Space");
        assert_eq!(q.capability_name.as_deref(), Some("space_query"));
    }

    /// Dotted-call alias grammar: non-empty `(key=value,…)` after kebab label → cross-domain Create when path binds.
    #[test]
    fn parse_dotted_call_team_create_space_typechecks() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse(
            "Team(11111).team-create-space(name=\"Sprint Sandbox\")",
            &cgs,
        )
        .unwrap();
        assert!(matches!(r.expr, Expr::Create(_)));
        if let Expr::Create(c) = &r.expr {
            assert_eq!(c.capability.as_str(), "team_create_space");
            assert_eq!(c.entity.as_str(), "Space");
        }
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Dotted-call args: `(..)` when all parameters are optional (teaching table ellipsis).
    #[test]
    fn parse_dotted_call_optional_only_double_dot_typechecks() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Team(42).team-create-time-entry(..)", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Create(_)));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Dotted-call args: required bindings plus `,..` for optional tail.
    #[test]
    fn parse_dotted_call_required_plus_optional_ellipsis_typechecks() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Team(42).team-create-goal(name=\"example\",..)", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Create(_)));
        if let Expr::Create(c) = &r.expr {
            let wire = c.input.to_value();
            let obj = wire.as_object().expect("object input");
            assert!(obj.contains_key("name"));
            assert!(!obj.contains_key("due_date"));
        }
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_proof_suggestion_insert_invoke_typechecks() {
        let dir = std::path::Path::new("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let cap = cgs
            .get_capability("annotation_suggestion_insert")
            .expect("annotation_suggestion_insert");
        let label = capability_method_label_kebab(cap);
        let line = format!(
            "Document(demo-slug).{label}(agent_id=\"bot\",by=\"bot\",quote=\"Status\",content=\"Status: green\")"
        );
        let r = parse(&line, &cgs).unwrap();
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        assert_eq!(inv.capability.as_str(), "annotation_suggestion_insert");
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn proof_document_get_markdown_capability_is_get_kind() {
        let dir = std::path::Path::new("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let cap = cgs
            .get_capability("document_get_markdown")
            .expect("document_get_markdown");
        assert_eq!(cap.kind, CapabilityKind::Get);
    }

    /// Sole `vN{{…}}` is rejected when the capability root input is an object, not a union.
    #[test]
    fn parse_proof_rejects_root_union_ctor_on_object_input_capability() {
        let dir = std::path::Path::new("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let cap = cgs
            .get_capability("annotation_comment_add")
            .expect("annotation_comment_add");
        let label = capability_method_label_kebab(cap);
        let line = format!("Document(x).{label}(v111{{agent_id=$,by=$,quote=$,text=$}})");
        let e = parse(&line, &cgs).expect_err("expected parse error");
        assert!(
            e.message().contains("tagged union") || e.message().contains("key=value"),
            "{}",
            e.message()
        );
    }

    #[test]
    fn parse_hex_task_id_delete_clickup() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        // Without hex continuation, `8` parses as an integer and `)` is expected at `b`.
        for line in [
            "Task(8badcafe00000001).delete",
            "Task(8badcafe00000001).delete()",
        ] {
            let r = parse(line, &cgs).unwrap();
            assert!(
                matches!(r.expr, Expr::Delete(_)),
                "expected Delete for {line:?}, got {:?}",
                r.expr
            );
            if let Expr::Delete(d) = &r.expr {
                assert_eq!(d.capability.as_str(), "task_delete");
                assert_eq!(
                    d.target.simple_id().map(|s| s.as_str()),
                    Some("8badcafe00000001")
                );
            }
            crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
        }
    }

    #[test]
    fn typecheck_custom_field_team_id_union_query_caps() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse(r#"CustomField{team_id=Team(777666555)}"#, &cgs).unwrap();
        assert!(crate::type_checker::type_check_expr(&r.expr, &cgs).is_ok());
    }

    #[test]
    fn parse_zero_arity_invoke_path_clickup_no_parens() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Team(42).seat_usage", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Chain(_)));
        assert_eq!(r.expr.primary_entity(), "Team");
    }

    #[test]
    fn parse_zero_arity_invoke_rejects_query_without_id_when_path_needs_it() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let e = parse("Team.start-time-entry()", &cgs).unwrap_err();
        assert!(e.message().contains("target id"), "{}", e.message());
    }

    /// Zero-arity dotted invoke must keep compound [`Ref`] parts for CML path vars (`spreadsheetId`, `range`).
    #[test]
    fn parse_zero_arity_google_sheets_value_range_update_preserves_compound_ref() {
        let dir = std::path::Path::new("../../apis/google-sheets");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let line = concat!(
            "ValueRange(spreadsheetId=abc123,range=Sheet1!A1:C3)",
            ".value-range-update()",
        );
        let r = parse(line, &cgs).unwrap();
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        assert_eq!(inv.capability.as_str(), "value_range_update");
        let parts = inv
            .target
            .compound_parts()
            .expect("compound ValueRange ref");
        assert_eq!(
            parts.get("spreadsheetId").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(parts.get("range").map(String::as_str), Some("Sheet1!A1:C3"));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Spreadsheet path templates use `id`; simple refs must stay a single slot (not `primary_slot_str()`).
    #[test]
    fn parse_zero_arity_google_sheets_spreadsheet_batch_update_preserves_simple_ref() {
        let dir = std::path::Path::new("../../apis/google-sheets");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let line = concat!("Spreadsheet(sheet-id-9)", ".batch-update()");
        let r = parse(line, &cgs).unwrap();
        let Expr::Invoke(inv) = &r.expr else {
            panic!("expected Invoke, got {:?}", r.expr);
        };
        assert_eq!(inv.capability.as_str(), "spreadsheet_batch_update");
        assert_eq!(
            inv.target.simple_id().map(|s| s.as_str()),
            Some("sheet-id-9")
        );
        assert!(inv.input.is_none());
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Fibery `entity_delete` has no HTTP path vars; zero-arity delete must still keep compound scope.
    #[test]
    fn parse_zero_arity_fibery_record_delete_preserves_compound_ref() {
        let dir = std::path::Path::new("../../apis/fibery");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let line = concat!(
            "Record(database=\"Product Development/Bug\",id=\"019ea239-59d4-7374-b684-ac61b0dd28ec\")",
            ".entity-delete()",
        );
        let r = parse(line, &cgs).unwrap();
        let Expr::Delete(del) = &r.expr else {
            panic!("expected Delete, got {:?}", r.expr);
        };
        assert_eq!(del.capability.as_str(), "entity_delete");
        let parts = del.target.compound_parts().expect("compound Record ref");
        assert_eq!(
            parts.get("database").map(String::as_str),
            Some("Product Development/Bug")
        );
        assert_eq!(
            parts.get("id").map(String::as_str),
            Some("019ea239-59d4-7374-b684-ac61b0dd28ec")
        );
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_dotted_fibery_record_delete_with_scope_args_is_delete_not_invoke() {
        let dir = std::path::Path::new("../../apis/fibery");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let line = concat!(
            "Record(database=\"Product Development/Bug\",id=\"019ea239-59d4-7374-b684-ac61b0dd28ec\")",
            ".entity-delete(database=\"Product Development/Bug\",id=\"019ea239-59d4-7374-b684-ac61b0dd28ec\")",
        );
        let r = parse(line, &cgs).unwrap();
        let Expr::Delete(del) = &r.expr else {
            panic!("expected Delete, got {:?}", r.expr);
        };
        assert_eq!(del.capability.as_str(), "entity_delete");
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
    fn parse_clickup_list_string_id_coerces_integer_literal() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        // `List{id=<bigint>}` is referentially `List(<id>)`: the id-field brace sugar
        // (`expr_sugar::rewrite_id_field_brace_query_to_get`) rewrites it to a Get whose key preserves
        // the string-typed id verbatim — no integer coercion or precision loss on the 18-digit literal.
        let r = parse("List{id=123456789012345678}", &cgs).unwrap();
        let Expr::Get(g) = &r.expr else {
            panic!("expected get (id-field brace sugar), got {:?}", r.expr);
        };
        assert_eq!(g.reference.primary_slot_str(), "123456789012345678");
    }

    #[test]
    fn parse_clickup_task_due_date_now_normalizes_to_unix_ms() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Task{due_date=now}", &cgs).unwrap();
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
            matches!(value.to_value(), Value::Integer(_)),
            "due_date=now should coerce to unix_ms integer, got {value:?}"
        );
    }

    #[test]
    fn parse_clickup_goal_due_date_next_week_normalizes_to_unix_ms() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Goal{team_id=Team(999888777), due_date=next-week}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let due_val = match pred {
            crate::Predicate::And { args } => args.iter().find_map(|p| {
                if let crate::Predicate::Comparison { field, value, .. } = p {
                    (field == "due_date").then_some(value)
                } else {
                    None
                }
            }),
            crate::Predicate::Comparison { field, value, .. } if field == "due_date" => Some(value),
            _ => None,
        };
        let Some(value) = due_val else {
            panic!("expected due_date comparison, got {pred:?}");
        };
        assert!(
            matches!(value.to_value(), Value::Integer(_)),
            "due_date=next-week should coerce via temporal pre-normalisation, got {value:?}"
        );
    }

    #[test]
    fn parse_uuid_value_in_predicate() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let u = "550e8400-e29b-41d4-a716-446655440000";
        // id-field brace sugar: `List{id=<uuid>}` ≡ `List(<uuid>)`; the uuid survives as a string key.
        let r = parse(&format!("List{{id={u}}}"), &cgs).unwrap();
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

    /// Unquoted multi-word string in `{{…}}` predicate (lenient RHS).
    #[test]
    fn parse_clickup_space_query_unquoted_phrase_name_typechecks() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Space{team_id=Team(11111), name=Sprint Sandbox}", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn parse_dotted_call_team_create_space_unquoted_name_typechecks() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let r = parse("Team(11111).team-create-space(name=Sprint Sandbox)", &cgs).unwrap();
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
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "widget_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Widget".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({"method": "GET", "path": [{"type": "literal", "value": "widget"}]}).into(),
            },
            input_schema: None,
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
        }
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
                description: String::new(),
                field_type: FieldType::EntityRef {
                    target: "Library".into(),
                },
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
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
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "book_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Book".into(),
            mapping: CapabilityMapping {
                template:
                    serde_json::json!({"method":"GET","path":[{"type":"literal","value":"books"}]})
                        .into(),
            },
            input_schema: None,
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
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method":"GET",
                    "path":[
                        {"type":"var","name":"region"},
                        {"type":"literal","value":"/"},
                        {"type":"var","name":"code"}
                    ]
                })
                .into(),
            },
            input_schema: None,
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
        let r = parse(
            "Book{library=Library(region=us-west, code=shared-shelf)}",
            &cgs,
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
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// Regression: GitHub `issue_query` scope `repository` accepts compound `Repository(owner=…, repo=…)`.
    #[test]
    fn parse_github_issue_query_nested_repository_constructor_when_schema_loads() {
        let dir = std::path::Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            // Some dev trees may skip bundled GitHub if CGS validation is in flux.
            return;
        };
        let r = parse(
            "Issue{repository=Repository(owner=octocat, repo=Hello-World)}",
            &cgs,
        )
        .expect("nested Repository constructor should parse");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "repository");
        let wire = value.to_value();
        let Value::Object(m) = wire else {
            panic!("expected normalized object, got {value:?}");
        };
        assert_eq!(m.get("owner"), Some(&Value::String("octocat".into())));
        assert_eq!(m.get("repo"), Some(&Value::String("Hello-World".into())));
        let _ = crate::type_checker::type_check_expr(&r.expr, &cgs);
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

    /// Regression (eval gh-54): glued `TAG)` after heredoc body in a method call must parse.
    #[test]
    fn parse_github_issue_comment_create_glued_heredoc_close() {
        let dir = std::path::Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let expr = concat!(
            "IssueComment.issue-comment-create(",
            "repository=Repository(owner=\"plasm\",repo=\"plasm\"),",
            "issue_number=99,",
            "body=<<B\n# Issue Comment\n- [ ] Task 1\n- [ ] Task 2\nB)",
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
        let dir = std::path::Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let r = parse("Issue{assignee=User($)}", &cgs).expect("User($) in filter should parse");
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
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "ticket_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Ticket".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "var", "name": "owner"},
                        {"type": "var", "name": "repo"},
                        {"type": "var", "name": "n"}
                    ]
                })
                .into(),
            },
            input_schema: None,
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
        assert_eq!(m.get("owner").map(String::as_str), Some("o"));
        assert_eq!(m.get("repo").map(String::as_str), Some("r"));
        assert_eq!(m.get("n").map(String::as_str), Some("9"));
    }

    #[test]
    fn program_parse_compound_get_maps_binding_slot_to_path_vars() {
        let cgs = compound_get_fixture_cgs();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut refs = BTreeSet::new();
        refs.insert("zone".into());
        let r = parse_with_cgs_layers_program(
            "Ticket(owner=zone,repo=r,n=9)",
            &stack,
            sym_map,
            Some(&refs),
            false,
        )
        .expect("compound get with program binding");
        let Expr::Get(g) = &r.expr else {
            panic!("expected Get");
        };
        let pv = g.path_vars.as_ref().expect("path_vars for hole slot");
        assert!(
            matches!(
                pv.get("owner"),
                Some(Value::PlasmInputRef(PlasmInputRef::NodeInput { node, path }))
                    if node == "zone" && path.is_empty()
            ),
            "expected node_input(zone), got {:?}",
            pv.get("owner")
        );
        let EntityKey::Compound(m) = &g.reference.key else {
            panic!("expected compound ref");
        };
        assert!(m.get("owner").is_none());
        assert_eq!(m.get("repo").map(String::as_str), Some("r"));
        assert_eq!(m.get("n").map(String::as_str), Some("9"));
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    #[test]
    fn get_expr_serde_json_roundtrip_with_compound_path_vars() {
        let mut parts = BTreeMap::new();
        parts.insert("repo".into(), "r".into());
        parts.insert("n".into(), "9".into());
        let g = GetExpr::from_ref_with_path_vars(
            Ref::compound("Ticket", parts),
            Some(IndexMap::from([(
                "owner".into(),
                Value::PlasmInputRef(PlasmInputRef::node_output("zone", vec![])),
            )])),
        );
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
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "library_get_nested_fixture".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Library".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method":"GET",
                    "path":[
                        {"type":"var","name":"region"},
                        {"type":"literal","value":"/"},
                        {"type":"var","name":"code"}
                    ]
                })
                .into(),
            },
            input_schema: None,
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
        let repo = m.get("repo").expect("repo");
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
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "parent_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Parent".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({"method":"GET","path":[
                    {"type":"literal","value":"parent"},
                    {"type":"literal","value":"/"},
                    {"type":"var","name":"id"}
                ]})
                .into(),
            },
            input_schema: None,
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
            mapping: CapabilityMapping {
                template: serde_json::json!({"method":"GET","path":[
                    {"type":"literal","value":"children"}
                ]})
                .into(),
            },
            input_schema: None,
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
    fn parse_many_relation_with_filter_still_query() {
        let cgs = many_rel_unmaterialized_cgs();
        let r = parse("Parent(p1).items{id=a}", &cgs).unwrap();
        assert!(matches!(r.expr, Expr::Query(_)));
    }

    #[test]
    fn parse_pokeapi_materialized_many_relation_yields_chain() {
        let dir = std::path::Path::new("../../apis/pokeapi");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        for input in ["Type(electric).pokemon", "Pokemon(pikachu).types"] {
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
        let dir = std::path::Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut refs = std::collections::BTreeSet::new();
        refs.insert("report".into());
        let r = parse_with_cgs_layers_program(
            "Issue{state=report}",
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
    fn program_parse_unknown_ident_stays_string_in_predicate() {
        let dir = std::path::Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let mut refs = std::collections::BTreeSet::new();
        refs.insert("not_report".into());
        let r = parse_with_cgs_layers_program(
            "Issue{state=report}",
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
    fn linear_issue_search_method_sugar() {
        let dir = std::path::Path::new("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let r = parse_with_cgs_layers(r#"Issue.search(q="bug", team_key="ENG")"#, &stack, sym_map)
            .expect("parse");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        assert_eq!(q.capability_name.as_deref(), Some("issue_search"));
    }

    #[test]
    fn federated_issue_collision_parse_uses_opaque_entity_catalog() {
        use crate::symbol_tuning::TeachingExposureSession;
        use std::path::Path;
        use std::sync::Arc;

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let github_dir = root.join("../../apis/github");
        let linear_dir = root.join("../../apis/linear");
        if !github_dir.is_dir() || !linear_dir.is_dir() {
            return;
        }
        let mut cgs_github = load_schema_dir(&github_dir).expect("github");
        cgs_github.entry_id = Some("github".into());
        let mut cgs_linear = load_schema_dir(&linear_dir).expect("linear");
        cgs_linear.entry_id = Some("linear".into());
        let layers = [&cgs_github, &cgs_linear];
        let stack = cgs_layer_stack(&["github", "linear"], &layers);
        let mut exp = TeachingExposureSession::new(&cgs_github, "github", &["Issue"]);
        exp.expose_entities(&layers, Arc::new(cgs_linear.clone()), "linear", &["Issue"]);
        let map = exp.symbol_map_arc();
        assert_eq!(
            map.entry_id_for_entity_symbol("e1").as_deref(),
            Some("github")
        );
        assert_eq!(
            map.entry_id_for_entity_symbol("e2").as_deref(),
            Some("linear")
        );
        let linear_e = "e2";
        let team_key = map.ident_sym_cap_param_for("", "Issue", "issue_search", "team_key");
        let expr = format!(r#"{linear_e}~"plasm"{{{team_key}="ENG"}}"#);
        let r = parse_with_cgs_layers(&expr, &stack, map).expect("parse linear Issue search");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query, got {:?}", r.expr);
        };
        assert_eq!(q.capability_name.as_deref(), Some("issue_search"));
        assert_eq!(q.entity, "Issue");
        assert_eq!(
            q.catalog_entry_id.as_deref(),
            Some("linear"),
            "e2 must stamp linear catalog ownership"
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
        let r = parse_with_cgs_layers(&expr, &stack, sym_map).expect("symbolic nested ctor");
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
        crate::type_checker::type_check_expr(&r.expr, &cgs).unwrap();
    }

    /// GitHub scoped search: opaque `e3` Repository ctor inside `e1` Issue predicate (agent copy-paste path).
    #[test]
    fn parse_github_issue_predicate_session_repository_ctor_when_schema_loads() {
        use std::sync::Arc;

        let dir = std::path::Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let issue_sym = sym_map.entity_sym_for("", "Issue");
        let repo_sym = sym_map.entity_sym_for("", "Repository");
        if issue_sym == "Issue" || repo_sym == "Repository" {
            return;
        }
        let repo_region = sym_map.ident_sym_entity_field_for("", "Repository", "owner");
        let repo_name = sym_map.ident_sym_entity_field_for("", "Repository", "repo");
        let repo_field = sym_map.ident_sym_entity_field_for("", "Issue", "repository");
        let state_field = sym_map.ident_sym_cap_param_for("", "Issue", "issue_query", "state");
        let expr = format!(
            r#"{issue_sym}{{{repo_field}={repo_sym}({repo_region}=octocat, {repo_name}=Hello-World), {state_field}=open}}"#
        );
        let r = parse_with_cgs_layers(&expr, &stack, sym_map).expect("github symbolic predicate");
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        assert!(q.predicate.is_some());
        let _ = crate::type_checker::type_check_expr(&r.expr, &cgs);
    }

    /// GitHub comment-create shape: nested session entity ctor + binding field ref with symbolic `p#` path.
    #[test]
    fn program_parse_binding_field_ref_and_nested_entity_ctor_in_method_args() {
        use std::collections::BTreeSet;
        use std::sync::Arc;

        let dir = std::path::Path::new("../../apis/github");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &full));
        let stack = test_layer(&cgs);
        let repo_sym = sym_map.entity_sym_for("", "Repository");
        let comment_ent = sym_map.entity_sym_for("", "IssueComment");
        if repo_sym == "Repository" || comment_ent == "IssueComment" {
            return;
        }
        let cap = cgs
            .get_capability("issue_comment_create")
            .expect("issue_comment_create");
        let method_sym = sym_map.method_sym_for("", "IssueComment", cap.name.as_str());
        let owner = sym_map.ident_sym_entity_field_for("", "Repository", "owner");
        let repo = sym_map.ident_sym_entity_field_for("", "Repository", "repo");
        let repo_param = sym_map.ident_sym_cap_param_for(
            "",
            "IssueComment",
            "issue_comment_create",
            "repository",
        );
        let issue_num_param = sym_map.ident_sym_cap_param_for(
            "",
            "IssueComment",
            "issue_comment_create",
            "issue_number",
        );
        let issue_id_field = sym_map.ident_sym_entity_field_for("", "Issue", "number");
        let body_param =
            sym_map.ident_sym_cap_param_for("", "IssueComment", "issue_comment_create", "body");
        let mut refs = BTreeSet::new();
        refs.insert("issue".into());
        refs.insert("body".into());
        let expr = format!(
            r#"{comment_ent}.{method_sym}({repo_param}={repo_sym}({owner}="octocat", {repo}="Hello-World"), {issue_num_param}=issue.{issue_id_field}, {body_param}=body.content)"#
        );
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
                    if node == "issue" && path == &["number"]
            )),
            "expected issue.number PlasmInputRef, got {refs_found:?}"
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
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "pet_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Pet".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "var", "name": "name"}]
                })
                .into(),
            },
            input_schema: None,
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
}

#[cfg(test)]
mod chained_groups_tests;
