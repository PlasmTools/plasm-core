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
//!   For CGS slots with non-`short` [`crate::schema::StringSemantics`], teaching prompts require a heredoc for those cases;
//!   `string_semantics: short` scalars use normal quotes.
//! - [`Parser::parse_predicate_value_rhs`] / [`Parser::parse_dotted_call_arg_value_rhs`]: `Entity{…}` and
//!   dotted-call `method(k=v,…)` allow unquoted phrases (spaces) until top-level `,` or `}` / `)`.
//!   RHS may also be an **array literal** `[v1, v2]` (comma-separated; same strict [`Parser::parse_value`]
//!   tokens per element). Unary `Entity($)` is allowed here (teaching table fill-in, same as scalar `$`).
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

use super::heredoc_surface::{tagged_heredoc_close_kind, HeredocCloseLineKind};
use super::{ParseError, ParseErrorKind, Parser, Value};
use indexmap::IndexMap;

fn is_v_numeric_union_ctor_label(name: &str) -> bool {
    let b = name.as_bytes();
    if b.first() != Some(&b'v') {
        return false;
    }
    let tail = &name[1..];
    !tail.is_empty() && tail.bytes().all(|x| x.is_ascii_digit())
}

/// After `(`, true when the argument list begins with a root `v#{{…}}` union constructor.
pub(super) fn peek_starts_v_numeric_union_ctor_arg(input: &str, pos: usize) -> bool {
    let bytes = input.as_bytes();
    let mut i = pos;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b'v') {
        return false;
    }
    i += 1;
    let digit_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digit_start {
        return false;
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    bytes.get(i) == Some(&b'{')
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PhraseClose {
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
    /// Close line: trimmed `TAG`, or `TAG` plus optional ASCII whitespace and parser-owned
    /// delimiter tail such as `)`, `}`, `,`, or `})` on the same line.
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
                    use super::entity_ref_parse::EntityRefRhsMode;
                    if let Some(v) =
                        self.try_parse_entity_ref_value(&token, EntityRefRhsMode::Strict)?
                    {
                        return Ok(v);
                    }
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

    pub(super) fn parse_predicate_or_dotted_call_arg_value_inner(
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
            if self.peek_char() == Some('{') && is_v_numeric_union_ctor_label(&name) {
                self.pos += 1;
                let obj = self.parse_brace_kv_object_map()?;
                self.expect_char('}')?;
                return Ok(Value::UnionCtor {
                    ctor_label: name,
                    ctor_fields: obj,
                });
            }
            if self.peek_char() == Some('(') {
                use super::entity_ref_parse::EntityRefRhsMode;
                if let Some(v) =
                    self.try_parse_entity_ref_value(&name, EntityRefRhsMode::Lenient(close))?
                {
                    return Ok(v);
                }
                self.pos = id_start;
                return self.parse_value();
            }
            self.pos = id_start;
            if let Some(v) = self.try_parse_plasm_binding_field_ref(close)? {
                return Ok(v);
            }
            self.pos = id_start;
        }
        self.parse_phrase_value(close)
    }

    /// After consuming a program input ref, true if the next non-whitespace char ends this RHS.
    pub(super) fn at_rhs_close_delimiter(&self, close: PhraseClose) -> bool {
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

    /// Array inside a union-constructor body (`blocks=[{markdown=$}]`): elements may be inline `{…}` maps.
    fn parse_union_ctor_array_literal(&mut self) -> Result<Value, ParseError> {
        self.expect_char('[')?;
        let mut elements = Vec::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some(']') {
                self.pos += 1;
                break;
            }
            elements.push(self.parse_union_ctor_array_element()?);
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

    fn parse_union_ctor_array_element(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        match self.peek_char() {
            Some('[') => self.parse_union_ctor_array_literal(),
            Some('{') => {
                self.pos += 1;
                let m = self.parse_brace_kv_object_map()?;
                self.expect_char('}')?;
                Ok(Value::Object(m))
            }
            _ => self.parse_predicate_or_dotted_call_arg_value(PhraseClose::ArrayElement),
        }
    }

    /// RHS for `k=v` inside `v101{k=v,…}` — nested `[…]` / `{…}` without relying on `program_nodes`.
    fn parse_union_ctor_field_rhs(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        match self.peek_char() {
            Some('[') => self.parse_union_ctor_array_literal(),
            Some('{') => {
                self.pos += 1;
                let m = self.parse_brace_kv_object_map()?;
                self.expect_char('}')?;
                Ok(Value::Object(m))
            }
            _ => self.parse_compound_key_value_rhs(),
        }
    }

    /// Comma-separated `key=value` map inside `{` … `}` for union constructor surface `v21{k=v,…}`.
    pub(super) fn parse_brace_kv_object_map(
        &mut self,
    ) -> Result<IndexMap<String, Value>, ParseError> {
        let mut m = IndexMap::new();
        loop {
            self.skip_ws();
            if self.peek_char() == Some('}') {
                break;
            }
            if !self.ident_starts_here() {
                return Err(self.err(ParseErrorKind::ExpectedValue));
            }
            let key_start = self.pos;
            self.consume_raw_ident();
            let key = self.input[key_start..self.pos].to_string();
            self.skip_ws();
            if self.peek_char() != Some('=') {
                return Err(self.err(ParseErrorKind::ExpectedChar {
                    expected: '=',
                    got: self.peek_char(),
                }));
            }
            self.pos += 1;
            let val = self.parse_union_ctor_field_rhs()?;
            if m.insert(key.clone(), val).is_some() {
                return Err(self.err(ParseErrorKind::Other {
                    message: format!("duplicate key `{key}` in constructor object"),
                }));
            }
            self.skip_ws();
            match self.peek_char() {
                Some('}') => break,
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
        Ok(m)
    }

    fn parse_array_element(&mut self) -> Result<Value, ParseError> {
        self.skip_ws();
        if self.peek_char() == Some('[') {
            return self.parse_array_literal();
        }
        // Invoke / predicate array literals need phrase-style RHS so union constructors (`v101{…}`)
        // and other dotted-call tokens parse without `program_nodes` (teaching table teaching lines).
        self.parse_predicate_or_dotted_call_arg_value(PhraseClose::ArrayElement)
    }

    pub(super) fn ident_starts_here(&self) -> bool {
        let b = self.input.as_bytes();
        self.pos < b.len() && (b[self.pos].is_ascii_alphabetic() || b[self.pos] == b'_')
    }

    pub(super) fn consume_raw_ident(&mut self) {
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
