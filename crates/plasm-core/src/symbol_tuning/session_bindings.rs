//! All forward rows are written during [`TeachingExposureSession`] symbol assignment
//! (`expose_entities`, method waves, [`assign_new_slot_symbols`]) — not recomputed at snapshot time.

use crate::schema::{CapabilitySchema, InputType, ParameterRole, CGS};

use super::{slot_meta_is_relation, IdentMetadata, IdentRole, SymbolMap, TeachingExposureSession};

/// Owning catalog + entity for a session `e#` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityBinding {
    pub entry_id: String,
    pub entity: String,
}

/// Owning catalog + domain + capability wire for a session `m#` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodBinding {
    pub entry_id: String,
    pub domain: String,
    pub capability: String,
}

/// Declared relation hop for a session `r#` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationBinding {
    pub entry_id: String,
    pub source_entity: String,
    pub relation_wire: String,
    pub target_entity: String,
}

/// Semantic role of a session `p#` slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotKind {
    EntityField {
        entity: String,
        field_wire: String,
    },
    CapParam {
        domain: String,
        capability: String,
        param_wire: String,
        param_role: Option<ParameterRole>,
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
    pub entry_id: String,
    pub kind: SlotKind,
}

impl SlotBinding {
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

pub(crate) fn slot_binding_from_meta(
    meta: &IdentMetadata,
    cgs: Option<&CGS>,
) -> Option<SlotBinding> {
    let entry_id = meta.catalog_entry_id().to_string();
    match meta.allocation_ident_role() {
        IdentRole::EntityField => Some(SlotBinding {
            entry_id,
            kind: SlotKind::EntityField {
                entity: meta.entity().as_str().to_string(),
                field_wire: meta.wire_name().to_string(),
            },
        }),
        IdentRole::CapabilityParam { capability } => {
            let param_role = cgs.and_then(|c| {
                c.get_capability(capability.as_str())
                    .and_then(|cap| param_role_for_cap_wire(cap, meta.wire_name()))
            });
            Some(SlotBinding {
                entry_id,
                kind: SlotKind::CapParam {
                    domain: meta.entity().as_str().to_string(),
                    capability: capability.as_str().to_string(),
                    param_wire: meta.wire_name().to_string(),
                    param_role,
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
        entry_id: catalog_entry_id.clone(),
        source_entity: entity.as_str().to_string(),
        relation_wire: wire_name.clone(),
        target_entity: target.as_str().to_string(),
    })
}

impl TeachingExposureSession {
    pub(crate) fn record_entity_binding(&mut self, sym: &str, entry_id: &str, entity: &str) {
        self.sym_to_entity_binding.insert(
            sym.to_string(),
            EntityBinding {
                entry_id: entry_id.to_string(),
                entity: entity.to_string(),
            },
        );
    }

    pub(crate) fn record_method_binding(
        &mut self,
        sym: &str,
        entry_id: String,
        domain: String,
        capability: String,
    ) {
        let binding = MethodBinding {
            entry_id,
            domain,
            capability,
        };
        self.sym_to_method.insert(sym.to_string(), binding);
    }

    pub(crate) fn record_slot_binding_for_fp(&mut self, fp: &str) {
        let Some(meta) = self.fingerprint_meta.get(fp) else {
            return;
        };
        if slot_meta_is_relation(meta) {
            return;
        }
        let Some(sym) = self.slot_fingerprint_to_sym.get(fp) else {
            return;
        };
        let cgs = self
            .catalog_cgs
            .get(meta.catalog_entry_id())
            .map(|a| a.as_ref());
        if let Some(binding) = slot_binding_from_meta(meta, cgs) {
            self.sym_to_slot.insert(sym.clone(), binding);
        }
    }

    pub(crate) fn record_relation_binding_for_fp(&mut self, fp: &str) {
        let Some(meta) = self.fingerprint_meta.get(fp) else {
            return;
        };
        let Some(sym) = self.relation_fingerprint_to_sym.get(fp) else {
            return;
        };
        if let Some(binding) = relation_binding_from_meta(meta) {
            self.sym_to_relation_binding.insert(sym.clone(), binding);
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
        let mut out: Vec<String> = self
            .sym_to_slot
            .iter()
            .filter(|(_, b)| {
                b.entry_id == catalog_entry_id
                    && matches!(
                        &b.kind,
                        SlotKind::CapParam {
                            domain: d,
                            capability: c,
                            ..
                        } if d == domain && c == capability
                    )
            })
            .map(|(sym, _)| sym.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr_parser::parse_with_cgs_layers_program;
    use crate::loader::load_schema_dir;
    use std::path::PathBuf;

    #[test]
    fn sym_to_slot_populated_at_assignment_for_github_label_query_repository() {
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
        let p_repository =
            map.ident_sym_cap_param_for("github", "Label", "label_query", "repository");
        if !SymbolMap::is_opaque_p_sym(p_repository.as_str()) {
            return;
        }
        let binding = map
            .resolve_session_slot(p_repository.as_str())
            .expect("p# binding");
        let (_, _, wire) = binding.cap_param().expect("cap param");
        assert_eq!(wire, "repository");
        assert_eq!(binding.entry_id, "github");
    }

    #[test]
    fn session_opaque_tokens_parse_verbatim_from_forward_tables() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
        let exp =
            TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA", "HomographRowB"]);
        let map = exp.symbol_map_arc();
        let e_a = map
            .resolve_session_entity(map.entity_sym_for("langmatrix", "HomographRowA").as_str())
            .expect("e# for HomographRowA");
        assert_eq!(e_a.entity, "HomographRowA");
        let p_headline = map.ident_sym_entity_field("HomographRowA", "headline");
        let slot = map
            .resolve_session_slot(p_headline.as_str())
            .expect("p# headline");
        assert_eq!(slot.entity_field(), Some(("HomographRowA", "headline")));
        let source = format!(
            "{e_sym}[{p_sym}]",
            e_sym = map.entity_sym_for("langmatrix", "HomographRowA"),
            p_sym = p_headline,
        );
        parse_with_cgs_layers_program(&source, &[&cgs], map, None, false, None)
            .expect("verbatim e#/p# program must parse via forward session tables");
    }

    #[test]
    fn forward_tables_match_render_assembly_tokens() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("matrix");
        let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["HomographRowA"]);
        let map = exp.symbol_map_arc();
        let p_sym = map.ident_sym_entity_field("HomographRowA", "headline");
        if SymbolMap::is_opaque_p_sym(p_sym.as_str()) {
            assert!(
                map.sym_to_slot.contains_key(p_sym.as_str()),
                "render assembly token {p_sym} must exist in sym_to_slot"
            );
        }
    }
}
