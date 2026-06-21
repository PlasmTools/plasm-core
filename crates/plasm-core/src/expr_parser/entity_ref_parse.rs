//! Session `e#` and wire entity constructor head resolution — referential transparency seam.
//!
//! Top-level gets resolve opaque `e3` via [`SymbolMap::resolve_session_entity_symbol`];
//! predicate / dotted-call / strict value paths must use the same head resolver so
//! `e3(p5=…, p13=…)` denotes the same compound identity in every value position.
//! Program binding field paths (`issue.p27`, `body.content`) lower to [`PlasmInputRef`] with
//! opaque `p#` segments normalized to wire names via the session map.

use super::value::PhraseClose;
use super::Parser;
use super::{ParseError, ParseErrorKind};
use crate::schema::EntityDef;
use crate::value::{PlasmInputRef, Value};

/// Surface token before `(` — opaque `e#`, wire entity name, or legacy alias.
pub(super) struct EntityCtorHead {
    pub surface: String,
    pub canonical: String,
    pub from_session_sym: bool,
}

/// How to parse scalar slots inside an entity constructor body.
#[allow(dead_code)]
pub(super) enum EntityRefRhsMode {
    Strict,
    Lenient(PhraseClose),
}

impl<'a> Parser<'a> {
    /// Resolve `surface` → canonical CGS entity when it is a session `e#` or wire entity name.
    pub(super) fn resolve_entity_ctor_head(&self, surface: &str) -> Option<EntityCtorHead> {
        if let Some(canonical) = self.sym_map.resolve_session_entity_symbol(surface) {
            if self.cgs_for_entity(&canonical).is_some()
                || self
                    .layers_slice()
                    .iter()
                    .any(|c| c.get_entity(&canonical).is_some())
            {
                return Some(EntityCtorHead {
                    surface: surface.to_string(),
                    canonical,
                    from_session_sym: true,
                });
            }
        }
        let canon = self.canonical_entity_name_in_layers(surface);
        if self.cgs_for_entity(&canon).is_some()
            || self
                .layers_slice()
                .iter()
                .any(|c| c.get_entity(&canon).is_some())
        {
            return Some(EntityCtorHead {
                surface: surface.to_string(),
                canonical: canon,
                from_session_sym: false,
            });
        }
        None
    }

    /// After consuming an ident, if the next token is `(` and the ident resolves to a CGS entity,
    /// parse a compound or unary entity-ref value. Returns `Ok(None)` when the head is not an entity.
    pub(super) fn try_parse_entity_ref_value(
        &mut self,
        surface: &str,
        mode: EntityRefRhsMode,
    ) -> Result<Option<Value>, ParseError> {
        self.skip_ws();
        if self.peek_char() != Some('(') {
            return Ok(None);
        }
        let Some(head) = self.resolve_entity_ctor_head(surface) else {
            return Ok(None);
        };
        self.pos += 1;
        let prev_entry = self.active_entity_entry_id.clone();
        if head.from_session_sym {
            self.active_entity_entry_id = self.sym_map.entry_id_for_entity_symbol(&head.surface);
        }
        let result = self.parse_entity_ref_value_after_open_paren(&head.canonical, mode);
        self.active_entity_entry_id = prev_entry;
        result.map(Some)
    }

    /// Parse `Entity(<body>)` after `(` was consumed (head already resolved).
    pub(super) fn parse_entity_ref_value_after_open_paren(
        &mut self,
        entity_canon: &str,
        _mode: EntityRefRhsMode,
    ) -> Result<Value, ParseError> {
        let ent = self
            .cgs_for_entity_required(entity_canon)?
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
            let mut obj = indexmap::IndexMap::new();
            for k in &ent.key_vars {
                let wire = k.as_str();
                let v = parts.get(wire).expect("keys validated").clone();
                obj.insert(wire.to_string(), v);
            }
            return Ok(Value::Object(obj));
        }
        if looks_kv && ent.key_vars.is_empty() {
            if let Some(id_val) =
                self.try_parse_simple_id_field_constructor_sugar(entity_canon, &ent)?
            {
                return Ok(id_val);
            }
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

    /// Map compound-constructor key token to wire `key_vars` name (accepts `p#` without pre-expand).
    pub(super) fn normalize_compound_ctor_key(
        &self,
        entity_canon: &str,
        ent: &EntityDef,
        raw_key: &str,
    ) -> String {
        if ent.key_vars.iter().any(|k| k.as_str() == raw_key) {
            return raw_key.to_string();
        }
        if let Some(wire) = self.sym_map.resolve_ident(raw_key) {
            if ent.key_vars.iter().any(|k| k.as_str() == wire) {
                return wire.to_string();
            }
        }
        for kv in &ent.key_vars {
            if self
                .sym_map
                .ident_sym_entity_field(entity_canon, kv.as_str())
                == raw_key
            {
                return kv.to_string();
            }
        }
        raw_key.to_string()
    }

    /// Normalize binding field-path segments: opaque `p#` → wire when known in session map.
    pub(super) fn normalize_binding_field_path(&self, segments: &[String]) -> Vec<String> {
        segments
            .iter()
            .map(|s| {
                self.sym_map
                    .resolve_ident(s.as_str())
                    .map(|w| w.to_string())
                    .unwrap_or_else(|| s.clone())
            })
            .collect()
    }

    /// Parse `label`, `label.field`, or `label.p#.…` as [`PlasmInputRef`] when `label` is an in-scope
    /// program binding. Also handles `_.field` in `for_each` row context.
    pub(super) fn try_parse_plasm_binding_field_ref(
        &mut self,
        close: PhraseClose,
    ) -> Result<Option<Value>, ParseError> {
        if !self.ident_starts_here() {
            return Ok(None);
        }
        let id_start = self.pos;
        self.consume_raw_ident();
        let name = self.input[id_start..self.pos].to_string();
        self.skip_ws();

        if name == "_" && self.peek_char() == Some('.') && self.for_each_row_context {
            self.pos += 1;
            self.skip_ws();
            let path = self.collect_dot_field_path_segments()?;
            if !path.is_empty() && self.at_rhs_close_delimiter(close) {
                let path = self.normalize_binding_field_path(&path);
                return Ok(Some(Value::PlasmInputRef(PlasmInputRef::row_binding(
                    "_", path,
                ))));
            }
            self.pos = id_start;
            return Ok(None);
        }

        let Some(refs) = self.program_nodes else {
            self.pos = id_start;
            return Ok(None);
        };
        if !refs.contains(name.as_str()) {
            self.pos = id_start;
            return Ok(None);
        }

        if self.peek_char() == Some('.') {
            self.pos += 1;
            self.skip_ws();
            let path = self.collect_dot_field_path_segments()?;
            if !path.is_empty() && self.at_rhs_close_delimiter(close) {
                let path = self.normalize_binding_field_path(&path);
                return Ok(Some(Value::PlasmInputRef(PlasmInputRef::node_output(
                    name, path,
                ))));
            }
        } else if self.at_rhs_close_delimiter(close) {
            return Ok(Some(Value::PlasmInputRef(PlasmInputRef::node_output(
                name,
                Vec::new(),
            ))));
        }

        self.pos = id_start;
        Ok(None)
    }

    /// After `.`, consume one or more field path segments (wire names or opaque `p#`).
    fn collect_dot_field_path_segments(&mut self) -> Result<Vec<String>, ParseError> {
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
        Ok(path)
    }
}
