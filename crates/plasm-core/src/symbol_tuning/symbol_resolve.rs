//! Role-scoped opaque-symbol resolution policy for [`SymbolMap`].
//!
//! All reverse lookup (`p#` → wire) for parse, DAG validation, and compound keys lives here
//! so teaching-table assignment and runtime resolution cannot diverge via ad hoc fallbacks.

use crate::cgs_federation::{lookup_capability_in_layer_stack, CgsLayer};
use crate::schema::resolve_capability_input_param_field;
use crate::CapabilityKind;
use crate::CapabilitySchema;
use crate::EntityDef;
use crate::EntityFieldName;

use super::keys::{CatalogScope, EntityFieldKey, OpaqueESym, OpaqueMSym, OpaquePSym, OpaqueRSym};
use super::{EntityBinding, MethodBinding, RelationBinding, SlotBinding, SlotKind, SymbolMap};

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
    UnknownMethodSym {
        token: String,
    },
    UnknownEntitySym {
        token: String,
    },
    UnknownSessionPSym {
        token: String,
    },
    WrongSlotKind {
        token: String,
        expected: &'static str,
        got: String,
    },
    MethodAnchorMismatch {
        token: String,
        bound_domain: String,
        anchor_entity: String,
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
            Self::UnknownMethodSym { .. } | Self::MethodAnchorMismatch { .. } => Some(
                "Use `m#` symbols from the teaching table for this session.",
            ),
            Self::UnknownEntitySym { .. } => Some(
                "Use `e#` symbols from the teaching table for this session.",
            ),
            Self::UnknownSessionPSym { .. } | Self::WrongSlotKind { .. } => Some(
                "Use `p#` symbols from the teaching table for this session.",
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
            Self::UnknownMethodSym { token } => {
                write!(f, "`{token}` is not a method symbol in this session")
            }
            Self::UnknownEntitySym { token } => {
                write!(f, "`{token}` is not an entity symbol in this session")
            }
            Self::UnknownSessionPSym { token } => {
                write!(f, "`{token}` is not a slot symbol in this session")
            }
            Self::WrongSlotKind {
                token,
                expected,
                got,
            } => write!(
                f,
                "`{token}` is `{got}` in this session, expected {expected}"
            ),
            Self::MethodAnchorMismatch {
                token,
                bound_domain,
                anchor_entity,
            } => write!(
                f,
                "`{token}` is bound to `{bound_domain}`, not `{anchor_entity}` in this session"
            ),
        }
    }
}

impl SymbolMap {
    fn lookup_entity_field_by_opaque_psym(&self, entity: &str, psym: OpaquePSym) -> Option<String> {
        self.tables
            .entity_field_to_sym
            .iter()
            .find(|(key, sym)| key.entity.as_str() == entity && **sym == psym)
            .map(|(key, _)| key.field.as_str().to_string())
    }

    /// Resolve opaque `p#` to an entity row field wire when the token is homographed with a cap param.
    fn lookup_entity_field_wire_for_opaque_p(
        &self,
        catalog: CatalogScope<'_>,
        entity: &str,
        ent: &EntityDef,
        token: &str,
    ) -> Option<String> {
        if let Ok(binding) = self.resolve_session_slot(token) {
            if let Some((bound_entity, field_wire)) = binding.entity_field() {
                if bound_entity == entity
                    && (ent.fields.contains_key(field_wire)
                        || ent.relations.contains_key(field_wire))
                {
                    return Some(field_wire.to_string());
                }
            }
        }
        let CatalogScope::Qualified(entry_id) = catalog else {
            if let Some(psym) = OpaquePSym::parse(token) {
                return self.lookup_entity_field_by_opaque_psym(entity, psym);
            }
            return None;
        };
        if let Some(psym) = OpaquePSym::parse(token) {
            for field_wire in ent
                .fields
                .keys()
                .map(|k| k.as_str())
                .chain(ent.relations.keys().map(|k| k.as_str()))
            {
                let key = EntityFieldKey::new(entry_id, entity, field_wire);
                if self.tables.entity_field_to_sym.get(&key) == Some(&psym) {
                    return Some(field_wire.to_string());
                }
            }
        }
        None
    }

    fn lookup_compound_key_wire_for_opaque_p(
        &self,
        catalog: CatalogScope<'_>,
        entity: &str,
        key_vars: &[EntityFieldName],
        token: &str,
    ) -> Option<String> {
        if let Ok(binding) = self.resolve_session_slot(token) {
            if let Some((bound_entity, field_wire)) = binding.entity_field() {
                if bound_entity == entity && key_vars.iter().any(|k| k.as_str() == field_wire) {
                    return Some(field_wire.to_string());
                }
            }
        }
        let CatalogScope::Qualified(entry_id) = catalog else {
            if let Some(psym) = OpaquePSym::parse(token) {
                if let Some(field_wire) = self.lookup_entity_field_by_opaque_psym(entity, psym) {
                    if key_vars.iter().any(|k| k.as_str() == field_wire.as_str()) {
                        return Some(field_wire);
                    }
                }
            }
            return None;
        };
        if let Some(psym) = OpaquePSym::parse(token) {
            for kv in key_vars {
                let key = EntityFieldKey::new(entry_id, entity, kv.as_str());
                if self.tables.entity_field_to_sym.get(&key) == Some(&psym) {
                    return Some(kv.to_string());
                }
            }
        }
        None
    }

    /// Opaque session `e#` → catalog-qualified entity binding.
    pub fn resolve_session_entity(&self, token: &str) -> Result<EntityBinding, SymbolResolveError> {
        let t = token.trim();
        if !Self::is_opaque_e_sym(t) {
            return Err(SymbolResolveError::UnknownEntitySym {
                token: t.to_string(),
            });
        }
        self.tables
            .sym_to_entity_binding
            .get(
                &OpaqueESym::parse(t).ok_or(SymbolResolveError::UnknownEntitySym {
                    token: t.to_string(),
                })?,
            )
            .cloned()
            .ok_or(SymbolResolveError::UnknownEntitySym {
                token: t.to_string(),
            })
    }

    /// Opaque session `p#` → fully qualified slot binding.
    pub fn resolve_session_slot(&self, token: &str) -> Result<SlotBinding, SymbolResolveError> {
        let t = token.trim();
        if !Self::is_opaque_p_sym(t) {
            return Err(SymbolResolveError::UnknownSessionPSym {
                token: t.to_string(),
            });
        }
        self.tables
            .sym_to_slot
            .get(&OpaquePSym::parse(t).expect("p#"))
            .cloned()
            .ok_or(SymbolResolveError::UnknownSessionPSym {
                token: t.to_string(),
            })
    }

    /// Opaque session `r#` → declared relation binding.
    pub fn resolve_session_relation(
        &self,
        token: &str,
    ) -> Result<RelationBinding, SymbolResolveError> {
        let t = token.trim();
        if !Self::is_opaque_r_sym(t) {
            return Err(SymbolResolveError::UnknownSessionPSym {
                token: t.to_string(),
            });
        }
        self.tables
            .sym_to_relation_binding
            .get(&OpaqueRSym::parse(t).expect("r#"))
            .cloned()
            .ok_or(SymbolResolveError::UnknownSessionPSym {
                token: t.to_string(),
            })
    }

    /// Resolve a row projection / postfix field token for a known binding entity.
    pub fn resolve_entity_field(
        &self,
        catalog: CatalogScope<'_>,
        entity: &str,
        ent: &EntityDef,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        if Self::is_opaque_p_sym(t) {
            if let Some(field_wire) =
                self.lookup_entity_field_wire_for_opaque_p(catalog, entity, ent, t)
            {
                return Ok(field_wire);
            }
            return Err(SymbolResolveError::UnknownEntityPSym {
                catalog_entry_id: catalog.entry_id().unwrap_or("").to_string(),
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
        catalog: CatalogScope<'_>,
        entity: &str,
        key_vars: &[EntityFieldName],
        raw_key: &str,
    ) -> Result<String, SymbolResolveError> {
        if key_vars.iter().any(|k| k.as_str() == raw_key) {
            return Ok(raw_key.to_string());
        }
        if Self::is_opaque_p_sym(raw_key) {
            if let Some(field_wire) =
                self.lookup_compound_key_wire_for_opaque_p(catalog, entity, key_vars, raw_key)
            {
                return Ok(field_wire);
            }
            return Err(SymbolResolveError::UnknownCompoundKey {
                entity: entity.to_string(),
                token: raw_key.to_string(),
                expected: key_vars.iter().map(|k| k.as_str().to_string()).collect(),
            });
        }
        if let CatalogScope::Qualified(entry_id) = catalog {
            for kv in key_vars {
                if self.ident_sym_entity_field_for(entry_id, entity, kv.as_str()) == raw_key {
                    return Ok(kv.to_string());
                }
            }
        } else {
            for kv in key_vars {
                if self.ident_sym_entity_field_for("", entity, kv.as_str()) == raw_key {
                    return Ok(kv.to_string());
                }
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
        catalog: CatalogScope<'_>,
        entity: &str,
        ent: &EntityDef,
        _cgs: &crate::CGS,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        if !Self::is_opaque_p_sym(t) {
            return Ok(t.to_string());
        }
        if let Some(p_sym) = OpaquePSym::parse(t) {
            for (key, sym) in &self.tables.cap_param_to_sym {
                if *sym != p_sym {
                    continue;
                }
                if !catalog.matches_entry(key.entry_id.as_str()) {
                    continue;
                }
                if key.domain.as_str() == entity {
                    return Ok(key.param.to_string());
                }
            }
            if let Some(key) = self.tables.sym_to_cap_param_key.get(&p_sym) {
                if key.domain.as_str() == entity {
                    return Ok(key.param.to_string());
                }
            }
        }
        let binding = self.resolve_session_slot(t)?;
        match &binding.kind {
            SlotKind::EntityField {
                entity: bound_entity,
                field_wire,
            } if bound_entity.as_str() == entity
                && ent.fields.contains_key(field_wire.as_str()) =>
            {
                return Ok(field_wire.to_string());
            }
            SlotKind::CapParam {
                domain,
                capability_kind,
                param_wire,
                ..
            } if domain.as_str() == entity
                && matches!(
                    capability_kind,
                    CapabilityKind::Query | CapabilityKind::Search
                ) =>
            {
                return Ok(param_wire.to_string());
            }
            _ => {}
        }
        Err(SymbolResolveError::UnknownQueryFilterPSym {
            entity: entity.to_string(),
            token: t.to_string(),
        })
    }

    /// Opaque session `m#` → catalog-qualified method binding.
    pub fn resolve_session_method(&self, token: &str) -> Result<MethodBinding, SymbolResolveError> {
        let t = token.trim();
        if !Self::is_opaque_m_sym(t) {
            return Err(SymbolResolveError::UnknownMethodSym {
                token: t.to_string(),
            });
        }
        self.tables
            .sym_to_method
            .get(&OpaqueMSym::parse(t).expect("m#"))
            .cloned()
            .ok_or(SymbolResolveError::UnknownMethodSym {
                token: t.to_string(),
            })
    }

    /// Session `m#` lookup with invoke-anchor validation against the bound domain entity.
    pub fn resolve_session_method_for_invoke(
        &self,
        token: &str,
        anchor_entity: &str,
    ) -> Result<MethodBinding, SymbolResolveError> {
        let binding = self.resolve_session_method(token)?;
        if binding.domain.as_str() != anchor_entity {
            return Err(SymbolResolveError::MethodAnchorMismatch {
                token: token.trim().to_string(),
                bound_domain: binding.domain.to_string(),
                anchor_entity: anchor_entity.to_string(),
            });
        }
        Ok(binding)
    }

    /// Resolve opaque `p#` invoke parameters for a specific mutator.
    pub fn resolve_cap_param(
        &self,
        catalog: CatalogScope<'_>,
        domain: &str,
        capability: &str,
        token: &str,
        invoke_cap: &CapabilitySchema,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        if Self::is_opaque_p_sym(t) {
            let psym = OpaquePSym::parse(t).expect("p#");
            if let Some(key) = self.tables.sym_to_cap_param_key.get(&psym) {
                if catalog.matches_entry(key.entry_id.as_str())
                    && key.domain.as_str() == domain
                    && key.capability.as_str() == capability
                    && (Self::cap_declares_param_wire(invoke_cap, key.param.as_str())
                        || invoke_cap.name.as_str() == capability)
                {
                    return Ok(key.param.to_string());
                }
            }
            for (key, sym) in &self.tables.cap_param_to_sym {
                if *sym != psym {
                    continue;
                }
                if !catalog.matches_entry(key.entry_id.as_str()) {
                    continue;
                }
                if key.domain.as_str() == domain
                    && key.capability.as_str() == capability
                    && (Self::cap_declares_param_wire(invoke_cap, key.param.as_str())
                        || invoke_cap.name.as_str() == capability)
                {
                    return Ok(key.param.to_string());
                }
            }
            let binding = self.resolve_session_slot(t)?;
            if let SlotKind::CapParam {
                domain: bound_domain,
                capability: bound_cap,
                param_wire,
                ..
            } = &binding.kind
            {
                if bound_domain.as_str() == domain
                    && bound_cap.as_str() == capability
                    && Self::cap_declares_param_wire(invoke_cap, param_wire.as_str())
                {
                    return Ok(param_wire.to_string());
                }
            }
            return Err(SymbolResolveError::UnknownCapParam {
                catalog_entry_id: binding.entry_id.to_string(),
                domain: domain.to_string(),
                capability: capability.to_string(),
                token: t.to_string(),
            });
        }
        Ok(t.to_string())
    }

    /// Best-effort segment resolution for binding field paths when row entity is unknown.
    /// Opaque `p#` tokens resolve to wire when the session slot is an entity field.
    pub fn resolve_binding_field_segment(&self, token: &str) -> String {
        let t = token.trim();
        if t.is_empty() || !Self::is_opaque_p_sym(t) {
            return t.to_string();
        }
        self.resolve_session_slot(t)
            .ok()
            .and_then(|b| b.entity_field().map(|(_, w)| w.to_string()))
            .unwrap_or_else(|| t.to_string())
    }

    fn cap_declares_param_wire(cap: &CapabilitySchema, param_wire: &str) -> bool {
        resolve_capability_input_param_field(cap, param_wire).is_some()
            || cap
                .object_params()
                .is_some_and(|fields| fields.iter().any(|f| f.name.as_str() == param_wire))
    }

    /// Lookup a session `m#` token against federated CGS layers with invoke-anchor validation.
    pub fn resolve_opaque_session_method_capability<'a>(
        &self,
        layers: &[CgsLayer<'a>],
        token: &str,
        anchor_entity: &str,
    ) -> Result<&'a CapabilitySchema, SymbolResolveError> {
        let binding = self.resolve_session_method_for_invoke(token, anchor_entity)?;
        lookup_capability_in_layer_stack(
            layers,
            binding.entry_id.as_str(),
            binding.domain.as_str(),
            binding.capability.as_str(),
        )
        .ok_or_else(|| SymbolResolveError::UnknownMethodSym {
            token: format!(
                "{} (cap `{}` on `{}` not in loaded catalogs)",
                token.trim(),
                binding.capability,
                binding.entry_id
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load_schema;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::TeachingExposureSession;
    use std::path::PathBuf;

    #[test]
    fn lookup_linear_issue_create_in_federated_layers() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let pokeapi_dir = root.join("../../apis/pokeapi");
        let linear_dir = root.join("../../apis/linear");
        if !pokeapi_dir.is_dir() || !linear_dir.is_dir() {
            return;
        }
        let cgs_pokeapi = load_schema(&pokeapi_dir).expect("pokeapi");
        let cgs_linear = load_schema(&linear_dir).expect("linear");
        let layers = [
            CgsLayer::new("pokeapi", &cgs_pokeapi),
            CgsLayer::new("linear", &cgs_linear),
        ];
        let cap = lookup_capability_in_layer_stack(&layers, "linear", "Issue", "issue_create")
            .expect("linear issue_create");
        assert_eq!(cap.name.as_str(), "issue_create");
        assert_eq!(cap.domain.as_str(), "Issue");
    }

    #[test]
    fn resolve_entity_field_unknown_opaque_p_sym() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/capability_with_input");
        let cgs = load_schema_dir(&dir).expect("capability_with_input");
        let exp = TeachingExposureSession::new(&cgs, "", &["Account"]);
        let map = exp.symbol_map_arc();
        let ent = cgs.get_entity("Account").expect("Account");
        let err = map
            .resolve_entity_field(CatalogScope::SessionReverse, "Account", ent, "p999")
            .expect_err("unknown p#");
        assert!(matches!(
            err,
            SymbolResolveError::UnknownEntityPSym { .. }
                | SymbolResolveError::UnknownSessionPSym { .. }
        ));
    }

    #[test]
    fn resolve_cap_param_accepts_session_reverse_opaque_p_on_unset_fixture() {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        if !dir.is_dir() {
            return;
        }
        let Ok(cgs) = load_schema_dir(&dir) else {
            return;
        };
        let exp = TeachingExposureSession::new(&cgs, "", &["PromptRun"]);
        let map = exp.symbol_map_arc();
        let cap = cgs.capabilities.get("prompt_run_create").expect("cap");
        let slug = map.ident_sym_cap_param_for("", "PromptRun", "prompt_run_create", "slug");
        let wire = map
            .resolve_cap_param(
                CatalogScope::SessionReverse,
                "PromptRun",
                "prompt_run_create",
                slug.as_str(),
                cap,
            )
            .expect("slug p# on unset single-graph fixture");
        assert_eq!(wire, "slug");
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
        let row_a = map.ident_sym_entity_field_for("", "HomographRowA", "headline");
        let ent_b = cgs.get_entity("HomographRowB").expect("HomographRowB");
        let err = map
            .resolve_entity_field(
                CatalogScope::qualified("langmatrix"),
                "HomographRowB",
                ent_b,
                row_a.as_str(),
            )
            .expect_err("HomographRowA p# must not resolve on HomographRowB");
        assert!(matches!(
            err,
            SymbolResolveError::UnknownEntityPSym { .. } | SymbolResolveError::NotARowField { .. }
        ));
    }

    #[test]
    fn resolve_query_filter_field_accepts_cap_scope_param_p_sym() {
        std::env::set_var("PLASM_CGS_FAST_LOAD", "1");
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        if !dir.is_dir() {
            return;
        }
        let Ok(cgs) = load_schema_dir(&dir) else {
            return;
        };
        let exp = TeachingExposureSession::new(&cgs, "github", &["Repository", "Issue", "Label"]);
        let map = exp.symbol_map_arc();
        let ent = cgs.get_entity("Label").expect("Label");
        let p_repository =
            map.ident_sym_cap_param_for("github", "Label", "label_query", "repository");
        if !SymbolMap::is_opaque_p_sym(p_repository.as_str()) {
            return;
        }
        let wire = map
            .resolve_query_filter_field(
                CatalogScope::qualified("github"),
                "Label",
                ent,
                &cgs,
                p_repository.as_str(),
            )
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

    /// Regression: deleted qualified reverse-map fields must not reappear on opaque resolution paths.
    #[test]
    fn opaque_resolution_source_has_no_qualified_reverse_maps() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let paths = [
            manifest.join("src/symbol_tuning/mod.rs"),
            manifest.join("src/expr_parser/mod.rs"),
            manifest.join("src/relation_segment.rs"),
        ];
        let forbidden = [
            "entity_p_sym_to_wire",
            "cap_p_sym_to_param",
            "entity_p_sym_globally_unique",
            "rebuild_qualified_p_sym_indexes",
            "resolve_wire_for_p_sym",
        ];
        for path in paths {
            let text = std::fs::read_to_string(&path).expect("read source");
            for needle in forbidden {
                assert!(
                    !text.contains(needle),
                    "{} must not reference deleted reverse-map `{}`",
                    path.display(),
                    needle
                );
            }
        }
    }
}
