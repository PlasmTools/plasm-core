//! All forward rows are written during [`TeachingExposureSession`] symbol assignment
//! (`expose_entities`, method waves, [`assign_new_slot_symbols`]) — not recomputed at snapshot time.

use crate::identity::{
    CapabilityName, CapabilityParamName, EntityFieldName, EntityName, RegistryEntryId,
    RelationName,
};
use crate::schema::{
    resolve_capability_input_param_field, CapabilitySchema, InputType, ParameterRole, CGS,
};
use crate::FieldType;
use crate::CapabilityKind;

use super::keys::{OpaqueESym, OpaqueMSym};
use super::{slot_meta_is_relation, IdentMetadata, IdentRole, SymbolMap, TeachingExposureSession};

/// Owning catalog + entity for a session `e#` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityBinding {
    pub entry_id: RegistryEntryId,
    pub entity: EntityName,
}

impl EntityBinding {
    pub fn entry_id_str(&self) -> &str {
        self.entry_id.as_str()
    }

    pub fn entity_str(&self) -> &str {
        self.entity.as_str()
    }
}

/// Owning catalog + domain + capability wire for a session `m#` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodBinding {
    pub entry_id: RegistryEntryId,
    pub domain: EntityName,
    pub capability: CapabilityName,
    pub kind: CapabilityKind,
}

impl MethodBinding {
    pub fn entry_id_str(&self) -> &str {
        self.entry_id.as_str()
    }

    pub fn domain_str(&self) -> &str {
        self.domain.as_str()
    }

    pub fn capability_str(&self) -> &str {
        self.capability.as_str()
    }
}

/// Declared relation hop for a session `r#` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationBinding {
    pub entry_id: RegistryEntryId,
    pub source_entity: EntityName,
    pub relation_wire: RelationName,
    pub target_entity: EntityName,
}

impl RelationBinding {
    pub fn relation_wire_str(&self) -> &str {
        self.relation_wire.as_str()
    }
}

/// Semantic role of a session `p#` slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotKind {
    EntityField {
        entity: EntityName,
        field_wire: EntityFieldName,
    },
    CapParam {
        domain: EntityName,
        capability: CapabilityName,
        param_wire: CapabilityParamName,
        param_role: Option<ParameterRole>,
        capability_kind: CapabilityKind,
        scope_target_entity: Option<EntityName>,
    },
}

impl SlotKind {
    pub fn agent_label(&self) -> &'static str {
        match self {
            SlotKind::EntityField { .. } => "entity field",
            SlotKind::CapParam { .. } => "capability parameter",
        }
    }
}

/// Fully qualified binding for one opaque `p#` token in this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotBinding {
    pub entry_id: RegistryEntryId,
    pub kind: SlotKind,
}

impl SlotBinding {
    pub fn entry_id_str(&self) -> &str {
        self.entry_id.as_str()
    }

    pub fn entity_field(&self) -> Option<(&str, &str)> {
        match &self.kind {
            SlotKind::EntityField { entity, field_wire } => {
                Some((entity.as_str(), field_wire.as_str()))
            }
            _ => None,
        }
    }

    pub fn cap_param(&self) -> Option<(&str, &str, &str)> {
        match &self.kind {
            SlotKind::CapParam {
                domain,
                capability,
                param_wire,
                ..
            } => Some((domain.as_str(), capability.as_str(), param_wire.as_str())),
            _ => None,
        }
    }

    pub fn param_role(&self) -> Option<ParameterRole> {
        match &self.kind {
            SlotKind::CapParam { param_role, .. } => *param_role,
            _ => None,
        }
    }

    pub fn agent_description(&self) -> String {
        match &self.kind {
            SlotKind::EntityField { entity, field_wire } => {
                format!("entity field `{entity}.{field_wire}`")
            }
            SlotKind::CapParam {
                domain,
                capability,
                param_wire,
                ..
            } => format!("capability parameter `{domain}.{capability}.{param_wire}`"),
        }
    }
}

fn param_role_for_cap_wire(cap: &CapabilitySchema, param_wire: &str) -> Option<ParameterRole> {
    let is = cap.input_schema.as_ref()?;
    let fields = match &is.input_type {
        InputType::Object { fields, .. } => fields,
        _ => return None,
    };
    fields
        .iter()
        .find(|f| f.name.as_str() == param_wire)
        .and_then(|f| f.role)
}

fn scope_target_entity_for_cap_param(
    cap: &CapabilitySchema,
    param_wire: &str,
    cgs: &CGS,
) -> Option<EntityName> {
    let field = resolve_capability_input_param_field(cap, param_wire)?;
    if !matches!(field.role, Some(ParameterRole::Scope)) {
        return None;
    }
    let nv = field.named_value(cgs).ok()?;
    match &nv.field_type {
        FieldType::EntityRef { target } => Some(EntityName::from(target.as_str())),
        _ => None,
    }
}

pub(crate) fn slot_binding_from_meta(
    meta: &IdentMetadata,
    cgs: Option<&CGS>,
) -> Option<SlotBinding> {
    let entry_id = RegistryEntryId::from(meta.catalog_entry_id());
    match meta.allocation_ident_role() {
        IdentRole::EntityField => Some(SlotBinding {
            entry_id,
            kind: SlotKind::EntityField {
                entity: meta.entity().clone(),
                field_wire: EntityFieldName::from(meta.wire_name()),
            },
        }),
        IdentRole::CapabilityParam { capability } => {
            let (param_role, capability_kind, scope_target_entity) = match cgs {
                Some(cgs) => cgs
                    .get_capability(capability.as_str())
                    .map(|cap| {
                        (
                            param_role_for_cap_wire(cap, meta.wire_name()),
                            cap.kind,
                            scope_target_entity_for_cap_param(cap, meta.wire_name(), cgs),
                        )
                    })
                    .unwrap_or((None, CapabilityKind::Action, None)),
                None => (None, CapabilityKind::Action, None),
            };
            Some(SlotBinding {
                entry_id,
                kind: SlotKind::CapParam {
                    domain: meta.entity().clone(),
                    capability: capability.clone(),
                    param_wire: CapabilityParamName::from(meta.wire_name()),
                    param_role,
                    capability_kind,
                    scope_target_entity,
                },
            })
        }
        IdentRole::Relation { .. } => None,
    }
}

pub(crate) fn relation_binding_from_meta(meta: &IdentMetadata) -> Option<RelationBinding> {
    let IdentMetadata::Relation {
        catalog_entry_id,
        entity,
        wire_name,
        target,
        ..
    } = meta
    else {
        return None;
    };
    Some(RelationBinding {
        entry_id: RegistryEntryId::from(catalog_entry_id.as_str()),
        source_entity: entity.clone(),
        relation_wire: RelationName::from(wire_name.as_str()),
        target_entity: target.clone(),
    })
}

impl TeachingExposureSession {
    pub(crate) fn record_entity_binding(
        &mut self,
        sym: OpaqueESym,
        entry_id: &str,
        entity: &str,
    ) {
        self.tables.sym_to_entity_binding.insert(
            sym,
            EntityBinding {
                entry_id: RegistryEntryId::from(entry_id),
                entity: EntityName::from(entity),
            },
        );
    }

    pub(crate) fn record_method_binding(
        &mut self,
        sym: OpaqueMSym,
        entry_id: RegistryEntryId,
        domain: EntityName,
        capability: CapabilityName,
        kind: CapabilityKind,
    ) {
        let binding = MethodBinding {
            entry_id: entry_id.clone(),
            domain: domain.clone(),
            capability: capability.clone(),
            kind,
        };
        self.tables.sym_to_method.insert(sym, binding);
    }

    /// Set `sym_to_slot` once from representative fingerprint metadata (EntityField at allocation).
    pub(crate) fn commit_slot_binding_for_fp(&mut self, fp: &str) {
        let Some(meta) = self.ledger.fingerprint_meta.get(fp) else {
            return;
        };
        if slot_meta_is_relation(meta) {
            return;
        }
        let Some(sym) = self.ledger.slot_fingerprint_to_sym.get(fp).copied() else {
            return;
        };
        let cgs = self
            .catalog_cgs
            .get(meta.catalog_entry_id())
            .map(|a| a.as_ref());
        if let Some(binding) = slot_binding_from_meta(meta, cgs) {
            self.tables.sym_to_slot.insert(sym, binding);
        }
    }

    pub(crate) fn record_relation_binding_for_fp(&mut self, fp: &str) {
        let Some(meta) = self.ledger.fingerprint_meta.get(fp) else {
            return;
        };
        let Some(sym) = self.ledger.relation_fingerprint_to_sym.get(fp).copied() else {
            return;
        };
        if let Some(binding) = relation_binding_from_meta(meta) {
            self.tables.sym_to_relation_binding.insert(sym, binding);
        }
    }
}

impl SymbolMap {
    pub(crate) fn cap_param_syms_from_forward_table(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
    ) -> Vec<String> {
        let entry = RegistryEntryId::from(catalog_entry_id);
        let domain = EntityName::from(domain);
        let capability = CapabilityName::from(capability);
        let mut out: Vec<String> = self
            .tables
            .cap_param_to_sym
            .iter()
            .filter(|(key, _)| {
                key.entry_id == entry
                    && key.domain == domain
                    && key.capability == capability
            })
            .map(|(_, sym)| sym.as_wire())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::keys::OpaquePSym;
    use std::path::PathBuf;

    #[test]
    fn sym_to_slot_populated_at_assignment_for_github_label_query_repository() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA"]);
        let map = exp.symbol_map_arc();
        let repo_p = map.ident_sym_entity_field_for("langmatrix", "HomographRowA", "headline");
        assert!(repo_p.starts_with('p'), "repository p#");
        let binding = map
            .tables
            .sym_to_slot
            .get(&OpaquePSym::parse(&repo_p).expect("p#"))
            .expect("sym_to_slot");
        assert!(matches!(binding.kind, SlotKind::EntityField { .. }));
    }
}
