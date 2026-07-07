//! Role-scoped wire-name resolution policy for [`SymbolMap`].
//!
//! Field, filter, and invoke parameters use catalog wire names resolved under the
//! scope anchor (`e#` / `m#` / relation position). Opaque `p#` tokens are not accepted.

mod error;
#[cfg(test)]
mod tests;

pub use error::SymbolResolveError;

use crate::cgs_federation::{lookup_capability_in_layer_stack, CgsLayer};
use crate::schema::resolve_capability_input_param_field;
use crate::CapabilityKind;
use crate::CapabilitySchema;
use crate::EntityDef;
use crate::EntityFieldName;

use super::keys::{CatalogScope, OpaqueESym, OpaqueMSym, OpaqueRSym};
use super::{EntityBinding, MethodBinding, RelationBinding, SymbolMap};

impl SymbolMap {
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

    fn reject_legacy_p_sym(token: &str) -> Result<(), SymbolResolveError> {
        if Self::is_opaque_p_sym(token) {
            return Err(SymbolResolveError::UnknownSessionPSym {
                token: token.to_string(),
            });
        }
        Ok(())
    }

    /// Resolve a row projection / postfix field token for a known binding entity.
    pub fn resolve_entity_field(
        &self,
        _catalog: CatalogScope<'_>,
        entity: &str,
        ent: &EntityDef,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        Self::reject_legacy_p_sym(t)?;
        if ent.fields.contains_key(t) || ent.relations.contains_key(t) {
            return Ok(t.to_string());
        }
        Err(SymbolResolveError::NotARowField {
            entity: entity.to_string(),
            token: t.to_string(),
        })
    }

    /// Map compound constructor keys (`owner`, `repo`, `name`, …) accepting wire names only.
    pub fn resolve_compound_key(
        &self,
        _catalog: CatalogScope<'_>,
        entity: &str,
        key_vars: &[EntityFieldName],
        raw_key: &str,
    ) -> Result<String, SymbolResolveError> {
        Self::reject_legacy_p_sym(raw_key)?;
        if key_vars.iter().any(|k| k.as_str() == raw_key) {
            return Ok(raw_key.to_string());
        }
        Err(SymbolResolveError::UnknownCompoundKey {
            entity: entity.to_string(),
            token: raw_key.to_string(),
            expected: key_vars.iter().map(|k| k.as_str().to_string()).collect(),
        })
    }

    /// Resolve wire name for query/search `{…}` filter LHS — entity row fields or query/search cap inputs.
    pub fn resolve_query_filter_field(
        &self,
        _catalog: CatalogScope<'_>,
        entity: &str,
        ent: &EntityDef,
        cgs: &crate::CGS,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        Self::reject_legacy_p_sym(t)?;
        if ent.fields.contains_key(t) {
            return Ok(t.to_string());
        }
        let entry_id = cgs.entry_id.as_deref().unwrap_or("");
        if self.is_capability_param_wire_on_entity(entry_id, entity, t) {
            return Ok(t.to_string());
        }
        for cap in cgs.capabilities.values() {
            if cap.domain.as_str() != entity {
                continue;
            }
            if !matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search) {
                continue;
            }
            if Self::cap_declares_param_wire(cap, t) {
                return Ok(t.to_string());
            }
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

    /// Resolve invoke parameters for a specific mutator (wire names only).
    pub fn resolve_cap_param(
        &self,
        _catalog: CatalogScope<'_>,
        domain: &str,
        capability: &str,
        token: &str,
        invoke_cap: &CapabilitySchema,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        Self::reject_legacy_p_sym(t)?;
        if Self::cap_declares_param_wire(invoke_cap, t) {
            return Ok(t.to_string());
        }
        Err(SymbolResolveError::UnknownCapParam {
            catalog_entry_id: _catalog.entry_id().unwrap_or("").to_string(),
            domain: domain.to_string(),
            capability: capability.to_string(),
            capability_kind: invoke_cap.kind,
            token: t.to_string(),
        })
    }

    /// Binding field path segment: wire names pass through; legacy `p#` rejected.
    pub fn resolve_binding_field_segment(&self, token: &str) -> String {
        let t = token.trim();
        if t.is_empty() || !Self::is_opaque_p_sym(t) {
            return t.to_string();
        }
        t.to_string()
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
