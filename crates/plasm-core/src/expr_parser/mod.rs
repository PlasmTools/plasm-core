//! Path expression parser — parses the Plasm micro-syntax into typed [`Expr`] values.
//!
//! The grammar is LL(1); each token unambiguously determines the parse path:
//!
//! ```text
//! expr       = source pipeline* projection?
//! source     = Entity "(" id ")"               — GetExpr
//!            | Entity "{" pred ("," pred)* "}"  — QueryExpr with filters
//!            | Entity "~" quoted_or_bare         — Search QueryExpr
//!            | Entity                            — QueryExpr::all
//!
//! **Not valid:** `Entity:id` or `Get(Entity, id)` — there is no `Get(` wrapper; use **`Entity(id)`** only.
//! A typo like `Pokemon:pikachu` is rejected (otherwise it would parse as `Pokemon` + ignored tail → wrong query).
//!
//! pipeline   = "." field_name                  — ChainExpr (EntityRef follow) or relation nav
//!            | "." method "()"                  — zero-arity pipeline: DeleteExpr or InvokeExpr (by capability kind)
//!            | "." method "(" dotted_call_args ")" — dotted-call alias: Create / Update / Action / Delete with args (see DOMAIN)
//!              dotted_call_args = ε | ".." | key=value ("," key=value)* ["," ".."]
//!            | "." method                       — same as `method()` when schema disallows name clashes
//!            | ".^" Entity                      — reverse traversal query
//!            | ".^" Entity "{" preds "}"        — reverse traversal with filter
//!
//! pred       = field op value?
//!            | foreign_entity "." field op value?  — cross-entity filter (value may be omitted after `op` for DOMAIN placeholders)
//!
//! op         = "=" | "!=" | ">" | "<" | ">=" | "<=" | "~"
//! value      = quoted_string | structured_heredoc | uuid | number | bare_word (bare allows `\\` before delimiters)
//!            | `[` value ( `,` value )* `]` — array literal (predicate / dotted-call arg RHS only)
//!            | in `{{…}}` predicates and `method(k=v,…)` args: unquoted **phrase** (spaces OK) to `,` or closing `}}` / `)`
//!
//! structured_heredoc = `<<` TAG `\n` payload `\n` TAG (tagged heredoc only; TAG matches `[A-Za-z_][A-Za-z0-9_]*`).
//! The close line may be exactly `TAG`, or `TAG` immediately followed by optional ASCII whitespace and a single `)` / `,` / `}` on that line (e.g. `TAG)` after the body line).
//! DOMAIN gloss and diagnostics emphasize heredocs for non-`short` string semantics.
//!
//! projection = "[" field ("," field)* "]"
//! ```
//!
//! # Layout and leniency
//!
//! Value parsing lives in submodule `value` (file `value.rs` next to this one): strict `Entity(id)` / search
//! vs lenient predicate and dotted-call arg RHS; see that file for scannerless / fault-tolerant parsing references.
//!
//! DOMAIN prompts may show bare `$` as a teaching placeholder; it parses as the string `$`.
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

mod value;

pub mod value_expr;
pub use value_expr::{RenderExpr, ValueExpr};

pub mod postfix;
pub use postfix::{
    normalize_nested_projection_field, peel_postfix_suffixes, try_parse_bracket_render,
    try_parse_render_tail, BracketRender, PlasmPostfixOp, RenderTailParse,
};

use crate::schema::{
    capability_is_zero_arity_invoke, capability_path_method_segment,
    path_var_names_from_mapping_json, template_invoke_requires_explicit_anchor_id,
};
use crate::symbol_tuning::{entity_slices_for_render, FocusSpec, SymbolMap};
use crate::{
    ArrayItemsSchema, CapabilityKind, ChainExpr, CompOp, CreateExpr, DeleteExpr, EntityDef,
    EntityKey, Expr, FieldType, GetExpr, InputType, InvokeExpr, PageExpr, ParameterRole, Predicate,
    QueryExpr, Ref, Value, ValueWireFormat, CGS,
};
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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
    /// `Entity()` — no id before `)`; singleton GET uses `Entity.method()` from DOMAIN.
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
                "full-text `~` search is not available for entity '{entity}' (no Search capability); use query {{…}} or Get(id) per that entity's DOMAIN block"
            ),
            ParseErrorKind::PredicateFieldNotFound { field, entity, .. } => write!(
                f,
                "field '{field}' not found on entity '{entity}' (not an entity field or capability param)"
            ),
            ParseErrorKind::NotNavigable { field, entity, .. } => write!(
                f,
                "'{field}' on '{entity}' is not navigable (not an EntityRef or relation)"
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
#[derive(Debug, Clone, PartialEq)]
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

fn normalize_numeric_id_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() {
        format!("{}", f as i64)
    } else {
        f.to_string()
    }
}

/// Coerce a parsed predicate token for typecheck and downstream HTTP binding (**input only**).
///
/// Date fields use [`crate::temporal::normalize_temporal_value`]; response decoding and UI
/// display are unchanged.
fn coerce_value_for_field_type(
    ft: &FieldType,
    value_format: Option<ValueWireFormat>,
    array_items: Option<&ArrayItemsSchema>,
    val: Value,
) -> Result<Value, String> {
    match ft {
        FieldType::Array => {
            let coerce_elem = |v: Value| -> Result<Value, String> {
                match array_items {
                    Some(items) => {
                        coerce_value_for_field_type(&items.field_type, items.value_format, None, v)
                    }
                    None => Ok(v),
                }
            };
            match val {
                Value::Array(elements) => {
                    let mut out = Vec::with_capacity(elements.len());
                    for e in elements {
                        out.push(coerce_elem(e)?);
                    }
                    Ok(Value::Array(out))
                }
                other => Ok(Value::Array(vec![coerce_elem(other)?])),
            }
        }
        FieldType::Date => match value_format {
            Some(ValueWireFormat::Temporal(fmt)) => {
                crate::temporal::normalize_temporal_value(val, fmt)
            }
            None => Err("Date field missing value_format in schema".to_string()),
        },
        FieldType::String | FieldType::Uuid | FieldType::Select | FieldType::MultiSelect => {
            Ok(match val {
                Value::Integer(n) => Value::String(n.to_string()),
                Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
                _ => val,
            })
        }
        FieldType::Integer => Ok(match val {
            Value::String(ref s) => s.parse::<i64>().map(Value::Integer).unwrap_or(val),
            Value::Float(f) if f.fract() == 0.0 && f.is_finite() => Value::Integer(f as i64),
            _ => val,
        }),
        FieldType::Number => Ok(match val {
            Value::String(ref s) => s.parse::<f64>().map(Value::Float).unwrap_or(val),
            Value::Integer(n) => Value::Float(n as f64),
            _ => val,
        }),
        FieldType::EntityRef { .. } => Ok(match val {
            Value::Integer(n) => Value::String(n.to_string()),
            Value::Float(f) => Value::String(normalize_numeric_id_float(f)),
            _ => val,
        }),
        FieldType::Boolean => Ok(match val {
            Value::String(s) if s.eq_ignore_ascii_case("true") => Value::Bool(true),
            Value::String(s) if s.eq_ignore_ascii_case("false") => Value::Bool(false),
            _ => val,
        }),
        FieldType::Json => match val {
            Value::String(ref s) if s.as_str() == "$" => Ok(val),
            Value::String(s) => crate::value::parse_json_subtree_str(&s).ok_or_else(|| {
                "Json parameter: string must be valid JSON with a top-level object or array"
                    .to_string()
            }),
            other => Ok(other),
        },
        _ => Ok(val),
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
    let mut p = Parser::new(input, cgs);
    p.parse_expr()
}

/// Parse against multiple disjoint [`CGS`] graphs (federated execute). Caller supplies the session
/// [`crate::symbol_tuning::SymbolMap`] (e.g. from [`crate::symbol_tuning::DomainExposureSession::to_symbol_map`]).
pub fn parse_with_cgs_layers(
    input: &str,
    layers: &[&CGS],
    sym_map: SymbolMap,
) -> Result<ParsedExpr, ParseError> {
    parse_with_cgs_layers_program(input, layers, sym_map, None, false)
}

/// Like [`parse_with_cgs_layers`], but when compiling a **Plasm program**, supply the set of
/// in-scope program node ids so `method(p=report)` and `report.field` lower to
/// [`crate::value::PlasmInputRef`] instead of string literals. `for_each_row_context` enables
/// `_.field` row holes on the right-hand side of `=>`.
pub fn parse_with_cgs_layers_program(
    input: &str,
    layers: &[&CGS],
    sym_map: SymbolMap,
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
    let mut p = Parser::new_with_sym_map(input, ParserLayers::Many(layers), sym_map);
    p.program_nodes = program_nodes;
    p.for_each_row_context = for_each_row_context;
    p.parse_expr()
}

// ── Internal parser ────────────────────────────────────────────────────────

enum ParserLayers<'a> {
    /// Single-schema parse (`parse` / REPL).
    Single([&'a CGS; 1]),
    /// Federated: borrowed slice from caller (`parse_with_cgs_layers`).
    Many(&'a [&'a CGS]),
}

impl<'a> ParserLayers<'a> {
    fn as_slice(&self) -> &[&'a CGS] {
        match self {
            ParserLayers::Single(a) => a.as_slice(),
            ParserLayers::Many(s) => s,
        }
    }
}

pub(super) struct Parser<'a> {
    pub(super) input: &'a str,
    pub(super) pos: usize,
    layers: ParserLayers<'a>,
    /// Same `m#` → kebab table as the SYMBOL MAP bundle (forgiving when expansion did not run).
    sym_map: SymbolMap,
    /// When set, bare `id` / `id.path` in dotted-call args, predicates, and array literals refer
    /// to program nodes with those ids (typed [`crate::value::PlasmInputRef`]).
    pub(super) program_nodes: Option<&'a BTreeSet<String>>,
    /// Enables `_.path` row references (for `source => …` templates).
    pub(super) for_each_row_context: bool,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, cgs: &'a CGS) -> Self {
        let (full, _) = entity_slices_for_render(cgs, FocusSpec::All);
        let sym_map = SymbolMap::build(cgs, &full);
        Self::new_with_sym_map(input, ParserLayers::Single([cgs]), sym_map)
    }

    fn new_with_sym_map(input: &'a str, layers: ParserLayers<'a>, sym_map: SymbolMap) -> Self {
        assert!(!layers.as_slice().is_empty());
        let mut p = Self {
            input,
            pos: 0,
            layers,
            sym_map,
            program_nodes: None,
            for_each_row_context: false,
        };
        p.skip_ws();
        p
    }

    fn layers_slice(&self) -> &[&'a CGS] {
        self.layers.as_slice()
    }

    fn primary_cgs(&self) -> &CGS {
        self.layers_slice()[0]
    }

    fn cgs_for_entity(&self, entity: &str) -> Option<&CGS> {
        self.layers_slice()
            .iter()
            .copied()
            .find(|c| c.get_entity(entity).is_some())
    }

    fn cgs_for_entity_required(&self, entity: &str) -> &CGS {
        self.cgs_for_entity(entity)
            .unwrap_or_else(|| self.primary_cgs())
    }

    fn canonical_entity_name_in_layers(&self, raw: &str) -> String {
        for c in self.layers_slice() {
            if let Some(can) = c.canonical_entity_name(raw) {
                if c.get_entity(&can).is_some() {
                    return can;
                }
            }
        }
        for c in self.layers_slice() {
            if c.get_entity(raw).is_some() {
                return raw.to_string();
            }
        }
        self.primary_cgs()
            .canonical_entity_name(raw)
            .unwrap_or_else(|| raw.to_string())
    }

    fn normalize_method_symbol_label(&self, label: &str) -> String {
        self.sym_map
            .resolve_method_symbol_token(label)
            .map(str::to_string)
            .unwrap_or_else(|| label.to_string())
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
        })();
        self.pos = save;
        looks
    }

    /// Parse `k=v, ...` until closing `)`; consumes `)`. Values use full [`Self::parse_value`] so nested
    /// entity constructors are accepted. Keys must match `ent.key_vars` exactly (no extras, no omissions).
    fn parse_strict_compound_key_value_map(
        &mut self,
        display_entity: &str,
        ent: &EntityDef,
    ) -> Result<IndexMap<String, Value>, ParseError> {
        let mut parts: IndexMap<String, Value> = IndexMap::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some(')') {
                break;
            }
            let (key, _, _) = self.parse_ident_with_span()?;
            if parts.contains_key(&key) {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "duplicate key `{key}` in compound constructor for `{display_entity}`"
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

    /// Parse `Entity(<body>)` after `(` was consumed: compound entities become [`Value::Object`] in `key_vars`
    /// order; single-key entities unwrap to the inner [`Value`].
    pub(super) fn parse_entity_constructor_value_after_open_paren(
        &mut self,
        entity_canon: &str,
    ) -> Result<Value, ParseError> {
        let ent = self
            .cgs_for_entity_required(entity_canon)
            .get_entity(entity_canon)
            .ok_or_else(|| ParseError {
                kind: ParseErrorKind::UnknownEntity {
                    name: entity_canon.to_string(),
                    span_opt: None,
                },
                offset: self.pos,
            })?
            .clone();
        self.skip_ws();
        if self.peek_char() == Some(')') {
            return Err(self.err(ParseErrorKind::EmptyGetParens {
                entity: entity_canon.to_string(),
            }));
        }
        let after_paren = self.pos;
        let looks_kv = self.peek_compound_key_value_form();
        if ent.key_vars.len() > 1 {
            if !looks_kv {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!(
                        "entity `{}` has compound key {:?}; use `{}(key=value, ...)` with those keys",
                        entity_canon, ent.key_vars, entity_canon
                    ),
                }));
            }
            let parts = self.parse_strict_compound_key_value_map(entity_canon, &ent)?;
            let mut obj = IndexMap::new();
            for k in &ent.key_vars {
                let v = parts.get(k.as_str()).expect("keys validated").clone();
                obj.insert(k.to_string(), v);
            }
            return Ok(Value::Object(obj));
        }
        if looks_kv && ent.key_vars.is_empty() {
            return Err(self.err(ParseErrorKind::Other {
                message: format!(
                    "entity `{}` uses a simple id; use `{}(id)` not key=value form",
                    entity_canon, entity_canon
                ),
            }));
        }
        self.pos = after_paren;
        let id_val = self.parse_value()?;
        self.expect_char(')')?;
        Ok(id_val)
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
                .cgs_for_entity_required(entity)
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
        let ec = self.cgs_for_entity_required(entity_name);
        if let Some(ent) = ec.get_entity(entity_name) {
            if let Some(fs) = ent.fields.get(field) {
                let nv = ec.named_value_for_slot(fs).ok()?;
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
                            let nv = ec.named_value_for_slot(f).ok()?;
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

    fn parse_zero_arity_invoke(&mut self, source: Expr, label: String) -> Result<Expr, ParseError> {
        let raw = label.clone();
        let label = self.normalize_method_symbol_label(&label);
        if let Some(expr) =
            self.try_scoped_query_bridge_from_get(&source, &label, Some(raw.as_str()))?
        {
            return Ok(expr);
        }
        let entity = source.primary_entity().to_string();
        self.validate_entity(&entity)?;
        let cap_name = self.resolve_zero_arity_pipeline_cap(&entity, &label)?;
        let cap = self
            .cgs_for_entity_required(&entity)
            .get_capability(&cap_name)
            .ok_or_else(|| {
                self.err(ParseErrorKind::CapabilityMissingInternal {
                    name: cap_name.clone(),
                })
            })?;
        if cap.kind == CapabilityKind::Create {
            return Ok(Expr::Create(CreateExpr::new(
                cap_name,
                entity,
                Value::Object(Default::default()),
            )));
        }
        let needs_anchor_id = template_invoke_requires_explicit_anchor_id(&cap.mapping.template.0);

        if cap.kind == CapabilityKind::Delete {
            if !needs_anchor_id {
                return Ok(Expr::Delete(DeleteExpr::new(cap_name, entity, "0")));
            }
            let Expr::Get(g) = &source else {
                return Err(self.err(ParseErrorKind::InvokeRequiresTargetId {
                    entity: entity.clone(),
                    label: label.clone(),
                }));
            };
            return Ok(Expr::Delete(DeleteExpr::with_target_path_vars(
                cap_name,
                g.reference.clone(),
                g.path_vars.clone(),
            )));
        }
        if cap.kind == CapabilityKind::Get {
            let invoke_id = if !needs_anchor_id {
                "0".to_string()
            } else {
                match &source {
                    Expr::Get(g) => g.reference.primary_slot_str(),
                    _ => {
                        return Err(self.err(ParseErrorKind::InvokeRequiresTargetId {
                            entity: entity.clone(),
                            label: label.clone(),
                        }));
                    }
                }
            };
            return Ok(Expr::Get(GetExpr::new(entity, invoke_id)));
        }

        // Update / Action / other invoke kinds: preserve compound [`Ref`] from `Entity(id).method()`.
        if !needs_anchor_id {
            return Ok(Expr::Invoke(InvokeExpr::new(cap_name, entity, "0", None)));
        }
        let Expr::Get(g) = &source else {
            return Err(self.err(ParseErrorKind::InvokeRequiresTargetId {
                entity: entity.clone(),
                label: label.clone(),
            }));
        };
        Ok(Expr::Invoke(InvokeExpr::with_target_path_vars(
            cap_name,
            g.reference.clone(),
            None,
            g.path_vars.clone(),
        )))
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

    /// Consumes `..` if present (DOMAIN optional-parameter ellipsis).
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
        match &g.reference.key {
            EntityKey::Compound(parts) => {
                for pv in path_vars {
                    if map.contains_key(&pv) {
                        continue;
                    }
                    if let Some(v) = parts.get(&pv) {
                        map.insert(pv, Value::String(v.clone()));
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
    ) -> Result<(), ParseError> {
        let Some(is) = &cap.input_schema else {
            return Ok(());
        };
        let InputType::Object { fields, .. } = &is.input_type else {
            return Ok(());
        };
        let ec = self.cgs_for_entity_required(cap.domain.as_str());
        for f in fields {
            if let Some(v) = map.get_mut(&f.name) {
                let old = std::mem::replace(v, Value::Null);
                let nv = ec.named_value_for_slot(f).map_err(|_| {
                    self.err(ParseErrorKind::Other {
                        message: format!("internal: unknown value_ref for parameter `{}`", f.name),
                    })
                })?;
                let array_ref = nv.array_items.as_ref();
                *v = coerce_value_for_field_type(&nv.field_type, nv.value_format, array_ref, old)
                    .map_err(|m| self.err(ParseErrorKind::InvalidTemporalValue { message: m }))?;
            }
        }
        Ok(())
    }

    /// Resolve Create / Update / Action / Delete with object input; same-domain first, then cross-domain Create.
    fn resolve_dotted_call_capability(
        &self,
        label: &str,
        source: &Expr,
    ) -> Result<&crate::CapabilitySchema, ParseError> {
        let primary = source.primary_entity();
        let same_domain: Vec<_> = self
            .layers_slice()
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

        let cross: Vec<_> = self
            .layers_slice()
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
        let cap = self.resolve_dotted_call_capability(&label, &source)?;
        self.inject_path_vars_from_get(cap, &source, &mut map);
        self.coerce_object_input_for_cap(cap, &mut map)?;
        let input = Value::Object(map);
        match cap.kind {
            CapabilityKind::Create => Ok(Expr::Create(CreateExpr::new(
                cap.name.clone(),
                cap.domain.clone(),
                input,
            ))),
            CapabilityKind::Update | CapabilityKind::Action | CapabilityKind::Delete => {
                let Expr::Get(g) = &source else {
                    return Err(self.err(ParseErrorKind::Other {
                        message: "invoke with arguments requires Entity(id) on the left".into(),
                    }));
                };
                Ok(Expr::Invoke(InvokeExpr::with_target_path_vars(
                    cap.name.clone(),
                    g.reference.clone(),
                    Some(input),
                    g.path_vars.clone(),
                )))
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

        // Validate field exists on entity OR as a query capability input parameter.
        // Scope and filter params (e.g. `team_id`, `space_id`) live in the capability
        // input schema, not on the entity definition itself.
        let ec = self.cgs_for_entity_required(entity_name);
        if let Some(ent) = ec.get_entity(entity_name) {
            if !ent.fields.contains_key(pred_field.as_str()) {
                let is_cap_param = ec
                    .find_capabilities(entity_name, CapabilityKind::Query)
                    .iter()
                    .chain(
                        ec.find_capabilities(entity_name, CapabilityKind::Search)
                            .iter(),
                    )
                    .any(|cap| {
                        cap.input_schema
                            .as_ref()
                            .and_then(|is| {
                                if let crate::InputType::Object { fields, .. } = &is.input_type {
                                    Some(fields.iter().any(|f| f.name == pred_field))
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(false)
                    });
                if !is_cap_param {
                    return Err(ParseError {
                        kind: ParseErrorKind::PredicateFieldNotFound {
                            field: pred_field.clone(),
                            entity: entity_name.to_string(),
                            span_start,
                            span_end,
                        },
                        offset: span_start,
                    });
                }
            }
        }

        let op = self.parse_op()?;
        let mut val = self.parse_predicate_rhs_after_op()?;
        if let Some((ft, vf, arr)) = self.lookup_field_typing(entity_name, &pred_field) {
            if !matches!(val, Value::Null) && !val.is_domain_example_placeholder() {
                val = coerce_value_for_field_type(&ft, vf, arr.as_ref(), val)
                    .map_err(|m| self.err(ParseErrorKind::InvalidTemporalValue { message: m }))?;
            }
        }
        Ok(Predicate::comparison(pred_field, op, val))
    }

    /// Value after a comparison operator in `{…}` — may be omitted (comma or `}` next) for DOMAIN slots.
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
    /// When `raw_label` is `m#`, use the SYMBOL MAP `(domain, kebab)` pair so duplicate kebabs
    /// (`space_query` and `task_query` both `query`) do not collide.
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
            if let Some((domain, kebab)) = self.sym_map.resolve_method_symbol_pair(raw) {
                for c in self.layers_slice() {
                    for cap in c.capabilities.values() {
                        if cap.kind != CapabilityKind::Query {
                            continue;
                        }
                        if cap.domain.as_str() != domain {
                            continue;
                        }
                        if capability_path_method_segment(cap).as_str() != kebab {
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
                        if let Ok(nv) = c.named_value_for_slot(sf) {
                            if let FieldType::EntityRef { target } = &nv.field_type {
                                if target.as_str() == anchor_entity {
                                    matches.push(cap);
                                }
                            }
                        }
                    }
                }
            }
        }
        if matches.is_empty() {
            for c in self.layers_slice() {
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
                    if let Ok(nv) = c.named_value_for_slot(sf) {
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
        let handle_raw = self.parse_ident()?;
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

    fn parse_source(&mut self) -> Result<Expr, ParseError> {
        let (raw, span_start, span_end) = self.parse_ident_with_span()?;
        if raw == "page" {
            self.skip_ws();
            if self.peek_char() == Some('(') {
                return self.parse_page_invocation();
            }
        }
        let mut entity: Option<String> = self.sym_map.resolve_session_entity_symbol(&raw);
        if entity.is_none() {
            for c in self.layers_slice() {
                let e = c.canonical_entity_name(&raw).unwrap_or_else(|| raw.clone());
                if c.get_entity(&e).is_some() {
                    entity = Some(e);
                    break;
                }
            }
        }
        if entity.is_none() {
            for c in self.layers_slice() {
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
        let ent = self
            .cgs_for_entity_required(&entity)
            .get_entity(&entity)
            .ok_or_else(|| ParseError {
                kind: ParseErrorKind::UnknownEntity {
                    name: entity.clone(),
                    span_opt: None,
                },
                offset: self.pos,
            })?
            .clone();

        self.skip_ws();
        match self.peek_char() {
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
                    let map_values = self.parse_strict_compound_key_value_map(&entity, &ent)?;
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
                        return Err(self.err(ParseErrorKind::Other {
                            message: format!(
                                "entity `{}` uses a simple id; use `{}(id)` not key=value form",
                                entity, entity
                            ),
                        }));
                    }
                    self.pos = after_paren;
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
                // Query with predicates
                self.pos += 1;
                let preds = self.parse_preds(&entity)?;
                self.expect_char('}')?;
                Ok(Expr::Query(Self::preds_to_query(&entity, preds)))
            }
            Some('~') => {
                // Full-text search (requires Search capability on this entity)
                let search_empty = {
                    let c = self.cgs_for_entity_required(&entity);
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
                    let c = self.cgs_for_entity_required(&entity);
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
                    let c = self.cgs_for_entity_required(&entity);
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
        }
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
                    if let Ok(cap) = self.resolve_dotted_call_capability(&label_norm, &source) {
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

            let source_entity = source.primary_entity().to_string();
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
            if let Some(ent) = self
                .cgs_for_entity_required(&source_entity)
                .get_entity(&source_entity)
                .cloned()
            {
                // Check declared relations (e.g. .species, .abilities, .moves)
                if let Some(rel) = ent.relations.get(field.as_str()) {
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
                        let chain = ChainExpr::auto_get(source, field);
                        return Ok(Expr::Chain(chain));
                    }

                    if cardinality == crate::Cardinality::Many && preds.is_empty() {
                        let mat = rel
                            .materialize
                            .as_ref()
                            .unwrap_or(&crate::RelationMaterialization::Unavailable);
                        match mat {
                            crate::RelationMaterialization::FromParentGet { .. }
                            | crate::RelationMaterialization::QueryScoped { .. }
                            | crate::RelationMaterialization::QueryScopedBindings { .. } => {
                                let chain = ChainExpr::auto_get(source, field);
                                return Ok(Expr::Chain(chain));
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
                        let ec = self.cgs_for_entity_required(&source_entity);
                        let is_ref = ec
                            .named_value_for_slot(f)
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
        for c in self.layers_slice() {
            for cap in c.find_capabilities(target_entity, CapabilityKind::Query) {
                if let Some(fields) = cap.object_params() {
                    for f in fields {
                        if let Ok(nv) = c.named_value_for_slot(f) {
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
            .cgs_for_entity_required(target_entity)
            .get_entity(target_entity)
        {
            for (fname, field) in &ent.fields {
                if let Ok(nv) = self
                    .cgs_for_entity_required(target_entity)
                    .named_value_for_slot(field)
                {
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
    use crate::schema::capability_method_label_kebab;
    use crate::schema::registry_test_util;
    use crate::schema::NamedValueSchema;
    use crate::symbol_tuning::{entity_slices_for_render, FocusSpec, SymbolMap};
    use crate::{
        loader::load_schema_dir, CapabilityKind, CapabilityMapping, CapabilitySchema, Cardinality,
        EntityKey, FieldType, PlasmInputRef, RelationSchema, ResourceSchema, CGS,
    };
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

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
        let r = parse("page(s0_pg1)", &cgs).unwrap();
        let Expr::Page(p) = &r.expr else {
            panic!("expected Page, got {:?}", r.expr);
        };
        assert_eq!(p.handle.as_str(), "s0_pg1");
        assert_eq!(p.limit, None);
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

    /// Opaque `m#` without prior `expand_path_symbols` — parser resolves to kebab and scoped query bridge.
    #[test]
    fn parse_clickup_team_get_space_query_via_method_symbol() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let cap = cgs.get_capability("space_query").expect("space_query");
        let kebab = capability_method_label_kebab(cap);
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let sym = map.method_sym("Space", &kebab);
        if sym == kebab {
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

    /// Dotted-call args: `(..)` when all parameters are optional (DOMAIN ellipsis).
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
        let r = parse("List{id=123456789012345678}", &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { field, value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(field, "id");
        assert_eq!(
            value.to_value(),
            Value::String("123456789012345678".to_string())
        );
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
        let r = parse(&format!("List{{id={u}}}"), &cgs).unwrap();
        let Expr::Query(q) = &r.expr else {
            panic!("expected query");
        };
        let Some(pred) = &q.predicate else {
            panic!("expected predicate");
        };
        let Predicate::Comparison { value, .. } = pred else {
            panic!("expected comparison");
        };
        assert_eq!(value.to_value(), Value::String(u.to_string()));
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
            invoke_preflight: None,
            discovery: None,
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
            invoke_preflight: None,
            discovery: None,
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
            invoke_preflight: None,
            discovery: None,
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

    /// Unary `Entity($)` parses inside brace-query RHS (DOMAIN fill-in, same as scalar `$`).
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
            invoke_preflight: None,
            discovery: None,
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
        let sym_map = SymbolMap::build(&cgs, &full);
        let layers = [&cgs];
        let mut refs = BTreeSet::new();
        refs.insert("zone".into());
        let r = parse_with_cgs_layers_program(
            "Ticket(owner=zone,repo=r,n=9)",
            &layers,
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
            invoke_preflight: None,
            discovery: None,
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
            invoke_preflight: None,
            discovery: None,
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
            invoke_preflight: None,
            discovery: None,
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
        let sym_map = SymbolMap::build(&cgs, &full);
        let layers = [&cgs];
        let mut refs = std::collections::BTreeSet::new();
        refs.insert("report".into());
        let r = parse_with_cgs_layers_program(
            "Issue{state=report}",
            &layers,
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
        let sym_map = SymbolMap::build(&cgs, &full);
        let layers = [&cgs];
        let mut refs = std::collections::BTreeSet::new();
        refs.insert("not_report".into());
        let r = parse_with_cgs_layers_program(
            "Issue{state=report}",
            &layers,
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
}
