//! Role-scoped opaque-symbol resolution policy for [`SymbolMap`].
//!
//! All reverse lookup (`p#` → wire) for parse, DAG validation, and compound keys lives here
//! so teaching-table assignment and runtime resolution cannot diverge via ad hoc fallbacks.

mod error;
mod site;
#[cfg(test)]
mod tests;

pub use error::SymbolResolveError;
pub use site::PSymResolution;

use crate::cgs_federation::{lookup_capability_in_layer_stack, CgsLayer};
use crate::schema::resolve_capability_input_param_field;
use crate::CapabilityKind;
use crate::CapabilitySchema;
use crate::EntityDef;
use crate::EntityFieldName;

use super::keys::{
    CapParamKey, CatalogScope, EntityFieldKey, OpaqueESym, OpaqueMSym, OpaquePSym, OpaqueRSym,
};
use super::{EntityBinding, MethodBinding, RelationBinding, SlotBinding, SlotKind, SymbolMap};

impl SymbolMap {
    fn cap_param_keys_for_psym<'b>(
        &'b self,
        psym: OpaquePSym,
    ) -> impl Iterator<Item = &'b CapParamKey> + 'b {
        self.tables
            .cap_param_to_sym
            .iter()
            .filter_map(move |(key, sym)| (*sym == psym).then_some(key))
    }

    fn capability_accepts_query_filter(&self, cgs: &crate::CGS, key: &CapParamKey) -> bool {
        cgs.get_capability(key.capability.as_str())
            .is_some_and(|cap| {
                key.domain.as_str() == cap.domain.as_str()
                    && matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search)
            })
    }

    fn entity_row_field_wires_for_psym(
        &self,
        catalog: CatalogScope<'_>,
        entity: &str,
        ent: &EntityDef,
        psym: OpaquePSym,
    ) -> Vec<String> {
        let mut out = Vec::new();
        if let CatalogScope::Qualified(entry_id) = catalog {
            for field_wire in ent
                .fields
                .keys()
                .map(|k| k.as_str())
                .chain(ent.relations.keys().map(|k| k.as_str()))
            {
                let key = EntityFieldKey::new(entry_id, entity, field_wire);
                if self.tables.entity_field_to_sym.get(&key) == Some(&psym) {
                    out.push(field_wire.to_string());
                }
            }
        } else if let Some(field_wire) = self.lookup_entity_field_by_opaque_psym(entity, psym) {
            if ent.fields.contains_key(field_wire.as_str())
                || ent.relations.contains_key(field_wire.as_str())
            {
                out.push(field_wire);
            }
        }
        out
    }

    fn lookup_entity_field_by_opaque_psym(&self, entity: &str, psym: OpaquePSym) -> Option<String> {
        self.tables
            .entity_field_to_sym
            .iter()
            .find(|(key, sym)| key.entity.as_str() == entity && **sym == psym)
            .map(|(key, _)| key.field.as_str().to_string())
    }

    /// Role-scoped opaque `p#` → wire resolution.
    pub fn resolve_opaque_p(
        &self,
        catalog: CatalogScope<'_>,
        resolution: PSymResolution<'_>,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        let psym = OpaquePSym::parse(t).ok_or_else(|| SymbolResolveError::UnknownSessionPSym {
            token: t.to_string(),
        })?;
        match resolution {
            PSymResolution::EntityRowField { entity, ent } => self
                .entity_row_field_wires_for_psym(catalog, entity, ent, psym)
                .into_iter()
                .next()
                .ok_or(SymbolResolveError::UnknownEntityPSym {
                    catalog_entry_id: catalog.entry_id().unwrap_or("").to_string(),
                    entity: entity.to_string(),
                    token: t.to_string(),
                }),
            PSymResolution::CompoundKey { entity, key_vars } => {
                if let CatalogScope::Qualified(entry_id) = catalog {
                    for kv in key_vars {
                        let key = EntityFieldKey::new(entry_id, entity, kv.as_str());
                        if self.tables.entity_field_to_sym.get(&key) == Some(&psym) {
                            return Ok(kv.to_string());
                        }
                    }
                } else if let Some(field_wire) =
                    self.lookup_entity_field_by_opaque_psym(entity, psym)
                {
                    if key_vars.iter().any(|k| k.as_str() == field_wire.as_str()) {
                        return Ok(field_wire);
                    }
                }
                Err(SymbolResolveError::UnknownCompoundKey {
                    entity: entity.to_string(),
                    token: t.to_string(),
                    expected: key_vars.iter().map(|k| k.as_str().to_string()).collect(),
                })
            }
            PSymResolution::QueryFilter { entity, ent, cgs } => {
                let mut candidates =
                    self.entity_row_field_wires_for_psym(catalog, entity, ent, psym);
                candidates.extend(
                    self.cap_param_keys_for_psym(psym)
                        .filter(|key| catalog.matches_entry(key.entry_id.as_str()))
                        .filter(|key| key.domain.as_str() == entity)
                        .filter(|key| self.capability_accepts_query_filter(cgs, key))
                        .map(|key| key.param.to_string()),
                );
                if candidates.is_empty() {
                    if let Ok(binding) = self.resolve_session_slot(t) {
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
                    }
                    return Err(SymbolResolveError::UnknownQueryFilterPSym {
                        entity: entity.to_string(),
                        token: t.to_string(),
                    });
                }
                candidates.sort();
                candidates.dedup();
                match candidates.as_slice() {
                    [wire] => Ok(wire.clone()),
                    _ => Err(SymbolResolveError::AmbiguousQueryFilterPSym {
                        entity: entity.to_string(),
                        token: t.to_string(),
                        candidates,
                    }),
                }
            }
            PSymResolution::InvokeParam {
                domain,
                capability,
                cap,
            } => {
                let invoke_keys: Vec<_> = self
                    .cap_param_keys_for_psym(psym)
                    .filter(|key| catalog.matches_entry(key.entry_id.as_str()))
                    .filter(|key| {
                        key.domain.as_str() == domain
                            && key.capability.as_str() == capability
                            && Self::cap_declares_param_wire(cap, key.param.as_str())
                    })
                    .collect();
                if invoke_keys.len() == 1 {
                    return Ok(invoke_keys[0].param.to_string());
                }
                if invoke_keys.is_empty() {
                    // Shared scope/param symbols: accept when the invoke capability declares a wire
                    // committed on another occurrence (e.g. repository on issue_create when p# was
                    // first assigned on issue_query).
                    let shared_keys: Vec<_> = self
                        .cap_param_keys_for_psym(psym)
                        .filter(|key| catalog.matches_entry(key.entry_id.as_str()))
                        .filter(|key| key.domain.as_str() == domain)
                        .filter(|key| Self::cap_declares_param_wire(cap, key.param.as_str()))
                        .collect();
                    if !shared_keys.is_empty() {
                        let mut wires: Vec<String> = shared_keys
                            .iter()
                            .map(|key| key.param.to_string())
                            .collect();
                        wires.sort();
                        wires.dedup();
                        if wires.len() == 1 {
                            return Ok(wires[0].clone());
                        }
                    }
                }
                if let Ok(binding) = self.resolve_session_slot(t) {
                    if let SlotKind::CapParam {
                        domain: bound_domain,
                        capability: bound_cap,
                        param_wire,
                        ..
                    } = &binding.kind
                    {
                        if bound_domain.as_str() == domain
                            && bound_cap.as_str() == capability
                            && Self::cap_declares_param_wire(cap, param_wire.as_str())
                        {
                            return Ok(param_wire.to_string());
                        }
                    }
                    if invoke_keys.is_empty() {
                        return Err(SymbolResolveError::UnknownCapParam {
                            catalog_entry_id: binding.entry_id.to_string(),
                            domain: domain.to_string(),
                            capability: capability.to_string(),
                            token: t.to_string(),
                        });
                    }
                } else if invoke_keys.is_empty() {
                    return Err(SymbolResolveError::UnknownCapParam {
                        catalog_entry_id: catalog.entry_id().unwrap_or("").to_string(),
                        domain: domain.to_string(),
                        capability: capability.to_string(),
                        token: t.to_string(),
                    });
                }
                // Homographed union-variant leaves share one `p#` but retain distinct occurrence
                // paths in `cap_param_to_sym`; pick a stable representative for invoke reverse lookup.
                let mut wires: Vec<String> = invoke_keys
                    .iter()
                    .map(|key| key.param.to_string())
                    .collect();
                wires.sort();
                wires.dedup();
                Ok(wires.into_iter().next().expect("invoke_keys non-empty"))
            }
        }
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
            return self.resolve_opaque_p(
                catalog,
                PSymResolution::EntityRowField { entity, ent },
                t,
            );
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
            return self.resolve_opaque_p(
                catalog,
                PSymResolution::CompoundKey { entity, key_vars },
                raw_key,
            );
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
        cgs: &crate::CGS,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        let t = token.trim();
        if t.is_empty() {
            return Ok(String::new());
        }
        if Self::is_opaque_p_sym(t) {
            return self.resolve_opaque_p(
                catalog,
                PSymResolution::QueryFilter { entity, ent, cgs },
                t,
            );
        }
        if ent.fields.contains_key(t) || ent.relations.contains_key(t) {
            return Ok(t.to_string());
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
            return self.resolve_opaque_p(
                catalog,
                PSymResolution::InvokeParam {
                    domain,
                    capability,
                    cap: invoke_cap,
                },
                t,
            );
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
