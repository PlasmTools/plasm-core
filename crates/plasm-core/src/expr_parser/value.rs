//! Value and literal parsing for path expressions.
//!
//! # Strict vs lenient RHS
//!
//! - [`Parser::parse_value`]: strict — Get `Entity(id)`, search `~`, inner id of `Team(42)`.
//!   When the bare token names a **CGS entity**, `Entity(...)` is parsed as an entity-reference
//!   constructor: compound `key_vars` use `k=v,…` (strict key set) with recursive values; otherwise
//!   the legacy single-argument unwrap applies. Non-entity tokens keep the unwrap behavior.
//!   String literals: normal quoted `"` / `'` (with `\\` escapes), plus **structured heredocs** `<<TAG` … `TAG` (tagged,
//!   bash-inspired) for multiline or quote-heavy payloads without escape rules inside the block. The opener must be
//!   `<<` immediately followed by a tag (`[A-Za-z_][A-Za-z0-9_]*`) and a newline — not `<<` + newline alone.
//!   For CGS slots with non-`short` [`crate::schema::StringSemantics`], DOMAIN prompts require a heredoc for those cases;
//!   `string_semantics: short` scalars use normal quotes.
//! - [`Parser::parse_predicate_value_rhs`] / [`Parser::parse_dotted_call_arg_value_rhs`]: `Entity{…}` and
//!   dotted-call `method(k=v,…)` allow unquoted phrases (spaces) until top-level `,` or `}` / `)`.
//!   RHS may also be an **array literal** `[v1, v2]` (comma-separated; same strict [`Parser::parse_value`]
//!   tokens per element). Unary `Entity($)` is allowed here (DOMAIN fill-in, same as scalar `$`).
//!
//! **Binding-substitution matrix (program `node_input`):**
//! - Predicate `{…}`, dotted-call `(k=v)`, compound ctor `Entity(k=v,…)`, and simple `Entity(id)` **when
//!   [`Parser::program_nodes`] is set** use the lenient RHS paths above (or [`Parser::parse_compound_key_value_rhs`]),
//!   so bare labels can become [`crate::PlasmInputRef::node_output`].
//! - Positions that always use strict [`Parser::parse_value`] **without** `program_nodes` coerce bare tokens to
//!   [`Value::String`] (no `node_input`).
//! - DAG plan template uses (`collect_expr_for_template_uses` in `plasm-agent-core`) record `node_input` from query
//!   predicates, create payloads, invoke input/`path_vars`, delete `path_vars`, chain bodies, and **get** `path_vars`;
//!   they intentionally skip [`crate::PlasmInputRef::RowBinding`] (`for_each` row holes).
//!   Trailing `[`…`]` projection syntax applies only **after** the expression, not inside `{…}`.
//!
//! # Lenient parsing (references)
//!
//! - Scannerless / contextual lexing; PEG prioritized choice — Ford, *Parsing Expression Grammars*
//!   (2002), <https://dl.acm.org/doi/10.1145/512950.512955>
//! - Predicated LL / lookahead — Parr & Fisher, *LL(\*)* (OOPSLA 2011)
//! - Corrections: [`crate::error_render`], [`crate::expr_correction`]

use super::{ParseError, ParseErrorKind, Parser, Value};
use crate::PlasmInputRef;

/// How a structured heredoc closing line was recognized (tagged `TAG` line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeredocCloseLineKind {
    /// Trimmed line is exactly the close sentinel; consume trailing newline after the line.
    LineOnly,
    /// Sentinel plus optional ASCII whitespace and a single `)` / `,` / `}`; leave that suffix for the outer parser.
    GluedSuffix,
}

/// Tagged heredoc close: trim matches `TAG` alone, or `TAG` + optional ASCII ws + one of `)` `,` `}`.
fn tagged_heredoc_close_kind(line_slice: &str, tag: &str) -> Option<(HeredocCloseLineKind, usize)> {
    let leading_ws = line_slice.len() - line_slice.trim_start().len();
    let t = line_slice.trim();
    if t == tag {
        return Some((HeredocCloseLineKind::LineOnly, leading_ws));
    }
    if !t.starts_with(tag) {
        return None;
    }
    let after = &t[tag.len()..];
    let after = after.trim_start();
    if after.len() == 1 {
        let b = after.as_bytes()[0];
        if matches!(b, b')' | b',' | b'}') {
            return Some((HeredocCloseLineKind::GluedSuffix, leading_ws));
        }
    }
    None
}

#[derive(Clone, Copy, Debug)]
enum PhraseClose {
    Predicate,
    DottedCallParen,
    /// Comma-separated values inside `[` … `]` (array literal element).
    ArrayElement,
}

impl<'a> Parser<'a> {
    /// True when `<<` begins a **tagged** structured heredoc (`<<TAG` …). Not `<<<`. The byte
    /// immediately after `<<` must start the tag (`[A-Za-z_]`) so `<<` + newline / spaces / digits
    /// do not count — those must be rejected (see [`Self::malformed_double_angle_heredoc_prefix`])
    /// before phrase-style RHS parsing would otherwise swallow them as a bare phrase.
    pub(super) fn structured_heredoc_starts_here(&self) -> bool {
        let b = self.input.as_bytes();
        if self.pos + 2 > b.len() {
            return false;
        }
        if &b[self.pos..self.pos + 2] != b"<<" {
            return false;
        }
        if self.pos + 3 <= b.len() && b[self.pos + 2] == b'<' {
            // `<<<` — not a tagged `<<TAG` heredoc opener.
            return false;
        }
        let after = self.pos + 2;
        if after >= b.len() {
            return false;
        }
        let b0 = b[after];
        b0.is_ascii_alphabetic() || b0 == b'_'
    }

    /// `<<` present as if heredoc were intended, but not a valid `<<TAG` opener (`<<<` excluded).
    pub(super) fn malformed_double_angle_heredoc_prefix(&self) -> bool {
        let b = self.input.as_bytes();
        if self.pos + 2 > b.len() {
            return false;
        }
        if &b[self.pos..self.pos + 2] != b"<<" {
            return false;
        }
        if self.pos + 3 <= b.len() && b[self.pos + 2] == b'<' {
            return false;
        }
        !self.structured_heredoc_starts_here()
    }

    /// Structured heredoc: `<<TAG\n` … `\nTAG` (tagged only). No escapes inside the body.
    /// Close line: trimmed `TAG`, or `TAG` plus optional ASCII whitespace and a single `)` / `,` / `}` on the same line.
    pub(super) fn parse_structured_heredoc(&mut self) -> Result<Value, ParseError> {
        let bytes = self.input.as_bytes();
        debug_assert!(self.structured_heredoc_starts_here());
        self.pos += 2; // <<
        if self.pos >= bytes.len() {
            return Err(self.err(ParseErrorKind::UnterminatedString));
        }
        // Untagged `<<` + newline was removed — require `<<TAG` + newline.
        if bytes[self.pos] == b'\n' {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        let tag_start = self.pos;
        let b0 = bytes[self.pos];
        if !(b0.is_ascii_alphabetic() || b0 == b'_') {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        self.pos += 1;
        while self.pos < bytes.len()
            && (bytes[self.pos].is_ascii_alphanumeric() || bytes[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let tag = self.input[tag_start..self.pos].to_string();
        if self.pos >= bytes.len() || bytes[self.pos] != b'\n' {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        self.pos += 1;
        let body_start = self.pos;
        loop {
            let line_start = self.pos;
            while self.pos < bytes.len() && bytes[self.pos] != b'\n' && bytes[self.pos] != b'\r' {
                self.pos += 1;
            }
            let line_slice = &self.input[line_start..self.pos];
            if let Some((kind, leading_ws)) = tagged_heredoc_close_kind(line_slice, tag.as_str()) {
                let content = self.input[body_start..line_start].to_string();
                match kind {
                    HeredocCloseLineKind::LineOnly => {
                        if self.pos < bytes.len() && bytes[self.pos] == b'\r' {
                            self.pos += 1;
                        }
                        if self.pos < bytes.len() && bytes[self.pos] == b'\n' {
                            self.pos += 1;
                        }
                        return Ok(Value::String(content));
                    }
                    HeredocCloseLineKind::GluedSuffix => {
                        self.pos = line_start + leading_ws + tag.len();
                        return Ok(Value::String(content));
                    }
                }
            }
            if self.pos >= bytes.len() {
                return Err(self.err(ParseErrorKind::UnterminatedString));
            }
            if bytes[self.pos] == b'\r' {
                self.pos += 1;
            }
            if self.pos < bytes.len() && bytes[self.pos] == b'\n' {
                self.pos += 1;
            } else {
                return Err(self.err(ParseErrorKind::UnterminatedString));
            }
        }
    }

    /// Parse a value: quoted string, UUID, number, or bare word (with optional `\` escapes).
    pub(super) fn parse_value(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        if self.malformed_double_angle_heredoc_prefix() {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        if self.structured_heredoc_starts_here() {
            return self.parse_structured_heredoc();
        }
        match self.peek_char() {
            Some('"') | Some('\'') => {
                let quote = self.consume_char().unwrap();
                let mut s = String::new();
                loop {
                    match self.consume_char() {
                        None => return Err(self.err(ParseErrorKind::UnterminatedString)),
                        Some(c) if c == quote => break,
                        Some('\\') => match self.consume_char() {
                            Some(c) => s.push(c),
                            None => return Err(self.err(ParseErrorKind::UnterminatedEscape)),
                        },
                        Some(c) => s.push(c),
                    }
                }
                Ok(Value::String(s))
            }
            Some(c) if c.is_ascii_digit() => {
                if let Some(u) = self.try_consume_standard_uuid() {
                    return Ok(Value::String(u));
                }
                let start = self.pos;
                while self.pos < self.input.len()
                    && self.input.as_bytes()[self.pos].is_ascii_digit()
                {
                    self.pos += 1;
                }
                // Hex id continuation: `8badcafe…` is not an integer token; extend to full hex run.
                if self.pos < self.input.len() {
                    let b = self.input.as_bytes()[self.pos];
                    if matches!(b, b'a'..=b'f' | b'A'..=b'F') {
                        self.pos = start;
                        while self.pos < self.input.len()
                            && self.input.as_bytes()[self.pos].is_ascii_hexdigit()
                        {
                            self.pos += 1;
                        }
                        return Ok(Value::String(self.input[start..self.pos].to_string()));
                    }
                }
                let is_float =
                    self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'.';
                if is_float {
                    self.pos += 1;
                    while self.pos < self.input.len()
                        && self.input.as_bytes()[self.pos].is_ascii_digit()
                    {
                        self.pos += 1;
                    }
                    let s = &self.input[start..self.pos];
                    s.parse::<f64>()
                        .map(Value::Float)
                        .map_err(|_| self.err(ParseErrorKind::InvalidFloat { raw: s.to_string() }))
                } else {
                    let s = &self.input[start..self.pos];
                    s.parse::<i64>().map(Value::Integer).map_err(|_| {
                        self.err(ParseErrorKind::InvalidInteger { raw: s.to_string() })
                    })
                }
            }
            Some('-') => {
                let start = self.pos;
                self.pos += 1;
                if self.pos >= self.input.len() || !self.input.as_bytes()[self.pos].is_ascii_digit()
                {
                    return Err(self.err(ParseErrorKind::ExpectedValue));
                }
                while self.pos < self.input.len()
                    && self.input.as_bytes()[self.pos].is_ascii_digit()
                {
                    self.pos += 1;
                }
                let is_float =
                    self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b'.';
                if is_float {
                    self.pos += 1;
                    while self.pos < self.input.len()
                        && self.input.as_bytes()[self.pos].is_ascii_digit()
                    {
                        self.pos += 1;
                    }
                    let s = &self.input[start..self.pos];
                    s.parse::<f64>()
                        .map(Value::Float)
                        .map_err(|_| self.err(ParseErrorKind::InvalidFloat { raw: s.to_string() }))
                } else {
                    let s = &self.input[start..self.pos];
                    s.parse::<i64>().map(Value::Integer).map_err(|_| {
                        self.err(ParseErrorKind::InvalidInteger { raw: s.to_string() })
                    })
                }
            }
            _ => {
                let token = self.parse_bare_value_token()?;
                self.skip_ws();
                // `Foo(bar)` unwraps to a single inner value (entity ref id, etc.). It is not a
                // generic function call — no commas; use `field=now` or quoted text for dates.
                if self.peek_char() == Some('(') {
                    self.pos += 1;
                    let canon = self.canonical_entity_name_in_layers(&token);
                    if self.cgs_for_entity(&canon).is_some() {
                        return self.parse_entity_constructor_value_after_open_paren(&canon);
                    }
                    let id_val = self.parse_value()?;
                    self.expect_char(')')?;
                    return Ok(id_val);
                }
                Ok(Value::String(token))
            }
        }
    }

    /// Bare token until an unescaped delimiter: whitespace `,}[]()`.
    /// Backslash includes the next character literally (e.g. `\(` inside a slug).
    pub(super) fn parse_bare_value_token(&mut self) -> Result<String, ParseError> {
        let bytes = self.input.as_bytes();
        let start = self.pos;
        let mut buf: Option<String> = None;
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b == b'\\' {
                if buf.is_none() {
                    buf = Some(self.input[start..self.pos].to_string());
                }
                self.pos += 1;
                if self.pos >= self.input.len() {
                    return Err(self.err(ParseErrorKind::UnterminatedEscape));
                }
                let ch = self.input[self.pos..].chars().next().unwrap();
                self.pos += ch.len_utf8();
                buf.as_mut().unwrap().push(ch);
                continue;
            }
            if b.is_ascii_whitespace()
                || b == b','
                || b == b'}'
                || b == b'['
                || b == b']'
                || b == b')'
                || b == b'('
            {
                break;
            }
            if let Some(ref mut s) = buf {
                if b < 128 {
                    s.push(b as char);
                    self.pos += 1;
                } else {
                    let ch = self.input[self.pos..].chars().next().unwrap();
                    s.push(ch);
                    self.pos += ch.len_utf8();
                }
            } else {
                self.pos += 1;
            }
        }
        if self.pos == start && buf.is_none() {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        Ok(match buf {
            None => self.input[start..self.pos].to_string(),
            Some(s) => s,
        })
    }

    #[inline]
    pub(super) fn parse_predicate_value_rhs(&mut self) -> Result<Value, ParseError> {
        self.parse_predicate_or_dotted_call_arg_value(PhraseClose::Predicate)
    }

    #[inline]
    pub(super) fn parse_dotted_call_arg_value_rhs(&mut self) -> Result<Value, ParseError> {
        self.parse_predicate_or_dotted_call_arg_value(PhraseClose::DottedCallParen)
    }

    /// RHS of `key=value` inside compound `Entity(k=v,…)` (GET ctor and nested entity constructors).
    ///
    /// With [`Self::program_nodes`], uses the same phrase / `node_input` rules as dotted-call args so
    /// `zone_id=zone` binds a prior program label instead of the literal string `"zone"`.
    pub(super) fn parse_compound_key_value_rhs(&mut self) -> Result<Value, ParseError> {
        if self.program_nodes.is_some() {
            self.parse_predicate_or_dotted_call_arg_value_inner(PhraseClose::DottedCallParen)
        } else {
            self.parse_value()
        }
    }

    fn parse_predicate_or_dotted_call_arg_value(
        &mut self,
        close: PhraseClose,
    ) -> Result<Value, ParseError> {
        self.parse_predicate_or_dotted_call_arg_value_inner(close)
    }

    fn parse_predicate_or_dotted_call_arg_value_inner(
        &mut self,
        close: PhraseClose,
    ) -> Result<Value, ParseError> {
        self.skip_ws();
        if self.peek_char() == Some('[') {
            return self.parse_array_literal();
        }
        if self.malformed_double_angle_heredoc_prefix() {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        if self.structured_heredoc_starts_here() {
            return self.parse_structured_heredoc();
        }
        match self.peek_char() {
            Some('"') | Some('\'') => return self.parse_value(),
            Some(c) if c.is_ascii_digit() => return self.parse_value(),
            Some('-') => {
                let bytes = self.input.as_bytes();
                if self.pos + 1 < bytes.len() && bytes[self.pos + 1].is_ascii_digit() {
                    return self.parse_value();
                }
            }
            _ => {}
        }
        if self.ident_starts_here() {
            let id_start = self.pos;
            self.consume_raw_ident();
            let name = self.input[id_start..self.pos].to_string();
            self.skip_ws();
            if self.peek_char() == Some('(') {
                self.pos = id_start;
                return self.parse_value();
            }
            // `for_each` row holes: `_.field` …
            if name == "_" && self.peek_char() == Some('.') && self.for_each_row_context {
                self.pos += 1;
                self.skip_ws();
                let mut path = Vec::new();
                while self.ident_starts_here() {
                    let p0 = self.pos;
                    self.consume_raw_ident();
                    path.push(self.input[p0..self.pos].to_string());
                    self.skip_ws();
                    if self.peek_char() == Some('.') {
                        self.pos += 1;
                        self.skip_ws();
                    } else {
                        break;
                    }
                }
                if !path.is_empty() && self.at_rhs_close_delimiter(close) {
                    return Ok(Value::PlasmInputRef(PlasmInputRef::row_binding("_", path)));
                }
            } else if let Some(refs) = self.program_nodes {
                if refs.contains(name.as_str()) {
                    if self.peek_char() == Some('.') {
                        self.pos += 1;
                        self.skip_ws();
                        let mut path = Vec::new();
                        while self.ident_starts_here() {
                            let p0 = self.pos;
                            self.consume_raw_ident();
                            path.push(self.input[p0..self.pos].to_string());
                            self.skip_ws();
                            if self.peek_char() == Some('.') {
                                self.pos += 1;
                                self.skip_ws();
                            } else {
                                break;
                            }
                        }
                        if self.at_rhs_close_delimiter(close) {
                            return Ok(Value::PlasmInputRef(PlasmInputRef::node_output(
                                name, path,
                            )));
                        }
                    } else if self.at_rhs_close_delimiter(close) {
                        return Ok(Value::PlasmInputRef(PlasmInputRef::node_output(
                            name,
                            Vec::new(),
                        )));
                    }
                }
            }
            self.pos = id_start;
        }
        self.parse_phrase_value(close)
    }

    /// After consuming a program input ref, true if the next non-whitespace char ends this RHS.
    fn at_rhs_close_delimiter(&self, close: PhraseClose) -> bool {
        let bytes = self.input.as_bytes();
        let mut p = self.pos;
        while p < bytes.len() && bytes[p].is_ascii_whitespace() {
            p += 1;
        }
        match bytes.get(p).copied() {
            Some(b',') => true,
            Some(b')') if matches!(close, PhraseClose::DottedCallParen) => true,
            Some(b'}') if matches!(close, PhraseClose::Predicate) => true,
            Some(b']') if matches!(close, PhraseClose::ArrayElement) => true,
            None => true,
            _ => false,
        }
    }

    /// Array literal for predicate / dotted-call arg RHS: `[v1, v2]` (distinct from trailing `[proj]`).
    pub(super) fn parse_array_literal(&mut self) -> Result<Value, ParseError> {
        self.expect_char('[')?;
        let mut elements = Vec::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some(']') {
                self.pos += 1;
                break;
            }
            elements.push(self.parse_array_element()?);
            self.skip_ws();
            match self.peek_char() {
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                Some(',') => {
                    self.pos += 1;
                }
                _ => {
                    return Err(self.err(ParseErrorKind::ExpectedChar {
                        expected: ',',
                        got: self.peek_char(),
                    }));
                }
            }
        }
        Ok(Value::Array(elements))
    }

    fn parse_array_element(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        if self.peek_char() == Some('[') {
            self.parse_array_literal()
        } else if self.program_nodes.is_some() || self.for_each_row_context {
            self.parse_predicate_or_dotted_call_arg_value(PhraseClose::ArrayElement)
        } else {
            self.parse_value()
        }
    }

    fn ident_starts_here(&self) -> bool {
        let b = self.input.as_bytes();
        self.pos < b.len() && (b[self.pos].is_ascii_alphabetic() || b[self.pos] == b'_')
    }

    fn consume_raw_ident(&mut self) {
        let b = self.input.as_bytes();
        if self.pos >= b.len() {
            return;
        }
        self.pos += 1;
        while self.pos < b.len() && (b[self.pos].is_ascii_alphanumeric() || b[self.pos] == b'_') {
            self.pos += 1;
        }
    }

    fn parse_phrase_value(&mut self, close: PhraseClose) -> Result<Value, ParseError> {
        let mut out = String::new();
        let bytes = self.input.as_bytes();
        let mut paren: i32 = 0;
        let mut bracket: i32 = 0;
        let mut brace: i32 = 0;
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b == b'\\' {
                self.pos += 1;
                if self.pos >= bytes.len() {
                    return Err(self.err(ParseErrorKind::UnterminatedEscape));
                }
                let ch = self.input[self.pos..].chars().next().unwrap();
                self.pos += ch.len_utf8();
                out.push(ch);
                continue;
            }
            if paren == 0 && bracket == 0 && brace == 0 {
                if b == b',' {
                    break;
                }
                match close {
                    PhraseClose::Predicate if b == b'}' => break,
                    PhraseClose::DottedCallParen if b == b')' => break,
                    PhraseClose::ArrayElement if b == b']' => break,
                    _ => {}
                }
            }
            match b {
                b'(' => {
                    paren += 1;
                    self.pos += 1;
                    out.push('(');
                }
                b')' => {
                    paren -= 1;
                    if paren < 0 {
                        return Err(self.err(ParseErrorKind::ExpectedValue));
                    }
                    self.pos += 1;
                    out.push(')');
                }
                b'[' => {
                    bracket += 1;
                    self.pos += 1;
                    out.push('[');
                }
                b']' => {
                    bracket -= 1;
                    if bracket < 0 {
                        return Err(self.err(ParseErrorKind::ExpectedValue));
                    }
                    self.pos += 1;
                    out.push(']');
                }
                b'{' => {
                    brace += 1;
                    self.pos += 1;
                    out.push('{');
                }
                b'}' => {
                    brace -= 1;
                    if brace < 0 {
                        break;
                    }
                    self.pos += 1;
                    out.push('}');
                }
                _ => {
                    let ch = self.input[self.pos..].chars().next().unwrap();
                    self.pos += ch.len_utf8();
                    out.push(ch);
                }
            }
        }
        let t = out.trim();
        if t.is_empty() {
            return Err(self.err(ParseErrorKind::ExpectedValue));
        }
        Ok(Value::String(t.to_string()))
    }
}
