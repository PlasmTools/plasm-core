//! Role-scoped opaque-symbol resolution policy for [`SymbolMap`].
//!
//! All reverse lookup (`p#` → wire) for parse, DAG validation, and compound keys lives here
//! so teaching-table assignment and runtime resolution cannot diverge via ad hoc fallbacks.

use std::collections::BTreeSet;

use crate::CapabilityKind;
use crate::EntityDef;
use crate::EntityFieldName;

use super::SymbolMap;

/// Typed failure when an opaque teaching symbol cannot be resolved in context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolResolveError {
    UnknownEntityPSym {
        catalog_entry_id: String,
        entity: String,
        token: String,
    },
    NotARowField {
        entity: String,
        token: String,
    },
    UnknownQueryFilterPSym {
        entity: String,
        token: String,
    },
    AmbiguousQueryFilterPSym {
        entity: String,
        token: String,
        candidates: Vec<String>,
    },
    UnknownCapParam {
        catalog_entry_id: String,
        domain: String,
        capability: String,
        token: String,
    },
    UnknownCompoundKey {
        entity: String,
        token: String,
        expected: Vec<String>,
    },
}

impl SymbolResolveError {
    /// Optional agent-facing hint appended after the primary error line.
    pub fn agent_program_hint(&self) -> Option<&'static str> {
        match self {
            Self::UnknownEntityPSym { .. } | Self::NotARowField { .. } => Some(
                "Use `p#` symbols from the teaching `rows:` column for this binding.",
            ),
            Self::UnknownQueryFilterPSym { .. } | Self::AmbiguousQueryFilterPSym { .. } => Some(
                "Use `p#` from the teaching `rows:` column or the query/search input signature for this entity.",
            ),
            Self::UnknownCapParam { .. } => Some(
                "Use `p#` symbols from the teaching table input signature for this capability.",
            ),
            Self::UnknownCompoundKey { .. } => Some(
                "Supply every compound identity key using wire names or the taught `p#` symbols.",
            ),
        }
    }

    /// Primary error line plus optional `help:` suffix for agent program surfaces.
    pub fn to_agent_program_error(&self) -> String {
        match self.agent_program_hint() {
            Some(hint) => format!("{self}\nhelp: {hint}"),
            None => self.to_string(),
        }
    }
}

impl std::fmt::Display for SymbolResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEntityPSym {
                entity,
                token,
                ..
            } => write!(
                f,
                "`{token}` is not a row symbol for `{entity}` in this session"
            ),
            Self::NotARowField { entity, token } => write!(
                f,
                "`{token}` is not a row field on `{entity}` for this binding"
            ),
            Self::UnknownQueryFilterPSym { entity, token } => write!(
                f,
                "`{token}` is not a query filter symbol for `{entity}` in this session (not a row field or query/search scope param)"
            ),
            Self::AmbiguousQueryFilterPSym {
                entity,
                token,
                candidates,
            } => write!(
                f,
                "`{token}` is ambiguous for `{entity}` query filters — matches params {candidates:?}"
            ),
            Self::UnknownCapParam {
                domain,
                capability,
                token,
                ..
            } => write!(
                f,
                "`{token}` is not an input parameter on `{domain}.{capability}` in this session"
            ),
            Self::UnknownCompoundKey {
                entity,
                token,
                expected,
            } => write!(
                f,
                "compound constructor key `{token}` is not valid for `{entity}` — expected one of {expected:?}"
            ),
        }
    }
}

impl SymbolMap {
    /// Resolve a row projection / postfix field token for a known binding entity.
    pub fn resolve_entity_field(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        ent: &EntityDef,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        if Self::is_opaque_p_sym(t) {
            let wire = self
                .resolve_wire_for_p_sym_entity(catalog_entry_id, entity, t)
                .filter(|wire| {
                    ent.fields.contains_key(wire.as_str())
                        || ent.relations.contains_key(wire.as_str())
                });
            return wire.ok_or(SymbolResolveError::UnknownEntityPSym {
                catalog_entry_id: catalog_entry_id.to_string(),
                entity: entity.to_string(),
                token: t.to_string(),
            });
        }
        if ent.fields.contains_key(t) || ent.relations.contains_key(t) {
            return Ok(t.to_string());
        }
        Err(SymbolResolveError::NotARowField {
            entity: entity.to_string(),
            token: t.to_string(),
        })
    }

    /// Map compound constructor keys (`owner`, `repo`, `name`, …) accepting wire names and taught `p#`.
    pub fn resolve_compound_key(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        key_vars: &[EntityFieldName],
        raw_key: &str,
    ) -> Result<String, SymbolResolveError> {
        if key_vars.iter().any(|k| k.as_str() == raw_key) {
            return Ok(raw_key.to_string());
        }
        if Self::is_opaque_p_sym(raw_key) {
            if let Some(wire) =
                self.resolve_wire_for_p_sym_entity(catalog_entry_id, entity, raw_key)
            {
                if key_vars.iter().any(|k| k.as_str() == wire.as_str()) {
                    return Ok(wire);
                }
            }
        }
        for kv in key_vars {
            if self.ident_sym_entity_field_for(catalog_entry_id, entity, kv.as_str()) == raw_key {
                return Ok(kv.to_string());
            }
        }
        Err(SymbolResolveError::UnknownCompoundKey {
            entity: entity.to_string(),
            token: raw_key.to_string(),
            expected: key_vars.iter().map(|k| k.as_str().to_string()).collect(),
        })
    }

    /// Resolve opaque `p#` or wire name for query/search `{…}` filter LHS — entity row fields
    /// or query/search capability input params (e.g. `label_query` scope `repository`).
    pub fn resolve_query_filter_field(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        ent: &EntityDef,
        cgs: &crate::CGS,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        if !Self::is_opaque_p_sym(t) {
            return Ok(t.to_string());
        }
        if let Ok(wire) = self.resolve_entity_field(catalog_entry_id, entity, ent, t) {
            if ent.fields.contains_key(wire.as_str()) {
                return Ok(wire);
            }
        }
        let param_wires =
            self.query_search_param_wires_for_opaque_p_sym(catalog_entry_id, entity, cgs, t);
        match param_wires.len() {
            0 => Err(SymbolResolveError::UnknownQueryFilterPSym {
                entity: entity.to_string(),
                token: t.to_string(),
            }),
            1 => Ok(param_wires.into_iter().next().expect("one param wire")),
            _ => Err(SymbolResolveError::AmbiguousQueryFilterPSym {
                entity: entity.to_string(),
                token: t.to_string(),
                candidates: param_wires,
            }),
        }
    }

    /// Resolve opaque `p#` invoke parameters for a specific mutator.
    pub fn resolve_cap_param(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        if Self::is_opaque_p_sym(t) {
            return self
                .cap_p_sym_to_param
                .get(&(
                    catalog_entry_id.to_string(),
                    domain.to_string(),
                    capability.to_string(),
                    t.to_string(),
                ))
                .cloned()
                .ok_or(SymbolResolveError::UnknownCapParam {
                    catalog_entry_id: catalog_entry_id.to_string(),
                    domain: domain.to_string(),
                    capability: capability.to_string(),
                    token: t.to_string(),
                });
        }
        Ok(t.to_string())
    }

    /// Best-effort segment resolution for binding field paths when row entity is unknown.
    /// Opaque `p#` tokens pass through unless they map to exactly one wire in `sym_to_ident`.
    pub fn resolve_binding_field_segment(&self, token: &str) -> String {
        let t = token.trim();
        if t.is_empty() || !Self::is_opaque_p_sym(t) {
            return t.to_string();
        }
        self.sym_to_ident
            .get(t)
            .map(|wire| {
                let count = self
                    .entity_p_sym_to_wire
                    .values()
                    .filter(|w| w.as_str() == wire)
                    .count();
                if count == 1 {
                    wire.clone()
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_else(|| t.to_string())
    }

    /// Distinct query/search capability param wires resolved from one taught `p#`.
    fn query_search_param_wires_for_opaque_p_sym(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        cgs: &crate::CGS,
        token: &str,
    ) -> Vec<String> {
        let mut param_wires = BTreeSet::new();
        for kind in [CapabilityKind::Query, CapabilityKind::Search] {
            for cap in cgs.find_capabilities(entity, kind) {
                if let Ok(param) =
                    self.resolve_cap_param(catalog_entry_id, entity, cap.name.as_str(), token)
                {
                    param_wires.insert(param);
                }
            }
        }
        param_wires.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::TeachingExposureSession;
    use std::path::PathBuf;

    #[test]
    fn resolve_entity_field_unknown_opaque_p_sym() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/capability_with_input");
        let cgs = load_schema_dir(&dir).expect("capability_with_input");
        let exp = TeachingExposureSession::new(&cgs, "default", &["Account"]);
        let map = exp.symbol_map_arc();
        let ent = cgs.get_entity("Account").expect("Account");
        let err = map
            .resolve_entity_field("", "Account", ent, "p999")
            .expect_err("unknown p#");
        assert!(matches!(err, SymbolResolveError::UnknownEntityPSym { .. }));
    }

    #[test]
    fn resolve_entity_field_rejects_cross_entity_homograph() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let Ok(cgs) = load_schema_dir(&dir) else {
            return;
        };
        let exp =
            TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA", "HomographRowB"]);
        let map = exp.symbol_map_arc();
        let row_a = map.ident_sym_entity_field("HomographRowA", "headline");
        let ent_b = cgs.get_entity("HomographRowB").expect("HomographRowB");
        let err = map
            .resolve_entity_field("langmatrix", "HomographRowB", ent_b, row_a.as_str())
            .expect_err("HomographRowA p# must not resolve on HomographRowB");
        assert!(matches!(
            err,
            SymbolResolveError::UnknownEntityPSym { .. } | SymbolResolveError::NotARowField { .. }
        ));
    }

    #[test]
    fn resolve_query_filter_field_accepts_cap_scope_param_p_sym() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        if !dir.is_dir() {
            return;
        }
        let cgs = load_schema_dir(&dir).expect("github");
        let exp = TeachingExposureSession::new(&cgs, "github", &["Repository", "Issue", "Label"]);
        let map = exp.symbol_map_arc();
        let ent = cgs.get_entity("Label").expect("Label");
        let p_repository =
            map.ident_sym_cap_param_for("github", "Label", "label_query", "repository");
        if !SymbolMap::is_opaque_p_sym(p_repository.as_str()) {
            return;
        }
        let wire = map
            .resolve_query_filter_field("github", "Label", ent, &cgs, p_repository.as_str())
            .expect("label_query repository scope param");
        assert_eq!(wire, "repository");
    }

    #[test]
    fn agent_program_error_includes_query_filter_hint() {
        let err = SymbolResolveError::UnknownQueryFilterPSym {
            entity: "Label".into(),
            token: "p99".into(),
        };
        let msg = err.to_agent_program_error();
        assert!(msg.contains("query filter symbol"));
        assert!(msg.contains("help:"));
        assert!(msg.contains("query/search input signature"));
    }
}
