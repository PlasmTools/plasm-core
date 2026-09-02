//! Reserved **source-invocation** syntax after an entity head (RA-5).
//!
//! Accepted surface (canonical):
//!
//! ```text
//! Entity(context=<binding>){selection…}
//! Entity(context=<binding>)~search_text
//! Entity(context=<binding>)
//! ```
//!
//! `context=` is **not** a capability parameter and is **not** the keyword `using`.
//! It is reserved source-invocation grammar: an in-scope program binding names the
//! execution context for the following query/search (or bare-entity) selection.
//! Ordinary identity GETs such as `Entity(id)` / `Entity(k=v,…)` remain LL(1)-distinct
//! because this parser only commits when the first parenthesized key is exactly `context`.

use super::{ParseError, ParseErrorKind, Parser};
use crate::rowset::ExecutionContextRef;
use crate::Expr;

impl<'a> Parser<'a> {
    /// Parse the reserved source-invocation form `(context=<binding>)`.
    ///
    /// Returns `None` without consuming input when the parenthesized form is an ordinary entity
    /// identity GET.
    pub(super) fn try_parse_source_context(
        &mut self,
    ) -> Result<Option<ExecutionContextRef>, ParseError> {
        self.skip_ws();
        if self.peek_char() != Some('(') {
            return Ok(None);
        }
        let mark = self.pos;
        self.pos += 1;
        self.skip_ws();
        let Ok((key, _, _)) = self.parse_ident_with_span() else {
            self.pos = mark;
            return Ok(None);
        };
        self.skip_ws();
        if key != "context" || self.peek_char() != Some('=') {
            self.pos = mark;
            return Ok(None);
        }
        self.pos += 1;
        let (binding, binding_start, _) = self.parse_ident_with_span()?;
        if !self
            .program_nodes
            .is_some_and(|nodes| nodes.contains(binding.as_str()))
        {
            return Err(ParseError {
                kind: ParseErrorKind::Other {
                    message: format!(
                        "execution context `{binding}` is not an in-scope program binding"
                    ),
                },
                offset: binding_start,
            });
        }
        self.expect_char(')')?;
        ExecutionContextRef::new(binding)
            .map(Some)
            .map_err(|message| ParseError {
                kind: ParseErrorKind::Other { message },
                offset: binding_start,
            })
    }

    /// Attach a parsed `(context=…)` ref to a query/search source selection.
    ///
    /// Identity GETs and other non-query heads reject `context=` — it is source-invocation
    /// only, not a capability argument.
    pub(super) fn attach_source_context(
        &self,
        expr: Expr,
        context: Option<ExecutionContextRef>,
    ) -> Result<Expr, ParseError> {
        match (expr, context) {
            (Expr::Query(query), Some(context)) => Ok(Expr::Query(query.with_context(context))),
            (expr, None) => Ok(expr),
            (expr, Some(_)) => Err(self.err(ParseErrorKind::Other {
                message: format!(
                    "`context=` applies to a query/search source, not `{}`",
                    expr.primary_entity()
                ),
            })),
        }
    }
}
