//! Opaque-symbol gloss deduplication — shared by teaching-table synthesis (not MCP-specific).
//!
//! Gloss **identity** is strictly typed ([`FieldGlossMeaning`], [`GlossEmitIdentity`]); TSV strings
//! are a projection via [`FieldGlossMeaning::apply_to_teaching_field_gloss`], never dedupe keys.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use crate::identity::EntityName;
use crate::schema::CGS;
use crate::symbol_tuning::{IdentMetadata, SymbolMap};

/// Typed fragment of a field-gloss `Meaning` cell (TSV projection only).
#[derive(Clone, Debug)]
pub(crate) enum FieldGlossMeaningAtom {
    FieldType(String),
    AllowedValues(String),
    Description(String),
}

/// Trimmed agent-facing prose (construction-time only).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GlossDescription(String);

impl GlossDescription {
    pub(crate) fn from_trimmed(desc: &str) -> Self {
        Self(crate::symbol_tuning::gloss_description_truncated(desc))
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Structural value-domain identity — mirrors [`IdentMetadata::value_domain_allocation_fp`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ValueDomainStructuralKey(String);

impl ValueDomainStructuralKey {
    pub(crate) fn from_registry_meta(meta: &IdentMetadata) -> Option<Self> {
        meta.value_domain_allocation_fp()
            .map(ValueDomainStructuralKey)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PointOfUseProse {
    None,
    Distinct(GlossDescription),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FieldGlossMeaning {
    ValueDomain(ValueDomainStructuralKey),
    RegistryBackedSlot {
        value: ValueDomainStructuralKey,
        wire: String,
        point_of_use: PointOfUseProse,
    },
    TypedField {
        type_label: String,
        allowed_values: String,
        description: GlossDescription,
    },
    Relation {
        wire: String,
        description: GlossDescription,
    },
    InlineUnionSummary {
        summary: String,
    },
    OpaquePSlot {
        value_sym: String,
        wire: String,
        point_of_use: PointOfUseProse,
    },
    OpaqueLegend {
        description: String,
    },
}

impl FieldGlossMeaning {
    /// TSV Meaning atoms — sole projection path for field gloss rows.
    pub(crate) fn to_meaning_atoms(&self, display_override: &str) -> Vec<FieldGlossMeaningAtom> {
        match self {
            Self::ValueDomain(_) => {
                if display_override.is_empty() {
                    Vec::new()
                } else {
                    vec![FieldGlossMeaningAtom::Description(
                        display_override.to_string(),
                    )]
                }
            }
            Self::RegistryBackedSlot { point_of_use, .. } => match point_of_use {
                PointOfUseProse::None => Vec::new(),
                PointOfUseProse::Distinct(d) => {
                    vec![FieldGlossMeaningAtom::Description(d.as_str().to_string())]
                }
            },
            Self::TypedField {
                type_label,
                allowed_values,
                description,
            } => {
                let mut atoms = vec![FieldGlossMeaningAtom::FieldType(type_label.clone())];
                if !allowed_values.is_empty() {
                    atoms.push(FieldGlossMeaningAtom::AllowedValues(allowed_values.clone()));
                }
                if !description.is_empty() {
                    if allowed_values.is_empty() {
                        atoms.push(FieldGlossMeaningAtom::Description(format!(
                            "{type_label} · {}",
                            description.as_str()
                        )));
                    }
                } else if allowed_values.is_empty() && !type_label.is_empty() {
                    atoms.push(FieldGlossMeaningAtom::Description(type_label.clone()));
                }
                atoms
            }
            Self::Relation { wire, description } => {
                let mut atoms = vec![FieldGlossMeaningAtom::FieldType(wire.clone())];
                if !description.is_empty() {
                    atoms.push(FieldGlossMeaningAtom::Description(
                        description.as_str().to_string(),
                    ));
                }
                atoms
            }
            Self::InlineUnionSummary { summary } => {
                vec![FieldGlossMeaningAtom::Description(summary.clone())]
            }
            Self::OpaquePSlot {
                value_sym,
                wire,
                point_of_use,
            } => {
                let body = match point_of_use {
                    PointOfUseProse::None => format!("{value_sym} · {wire}"),
                    PointOfUseProse::Distinct(d) => {
                        format!("{value_sym} · {wire} · {}", d.as_str())
                    }
                };
                vec![FieldGlossMeaningAtom::Description(body)]
            }
            Self::OpaqueLegend { description } => {
                if description.is_empty() {
                    Vec::new()
                } else {
                    vec![FieldGlossMeaningAtom::Description(description.clone())]
                }
            }
        }
    }

    pub(crate) fn apply_to_teaching_field_gloss(
        &self,
        g: &mut super::TeachingFieldGloss,
        _map: Option<&SymbolMap>,
        _cgs: Option<&CGS>,
    ) {
        match self {
            Self::ValueDomain(_key) => {
                // Display text supplied by caller (`legend_rhs` / pre-rendered v# gloss).
            }
            Self::RegistryBackedSlot { point_of_use, .. } => match point_of_use {
                PointOfUseProse::None => {
                    g.field_type.clear();
                    g.allowed_values.clear();
                }
                PointOfUseProse::Distinct(desc) => {
                    g.field_type.clear();
                    g.allowed_values.clear();
                    g.description = desc.as_str().to_string();
                }
            },
            Self::TypedField {
                type_label,
                allowed_values,
                description,
            } => {
                g.field_type = type_label.clone();
                g.allowed_values = allowed_values.clone();
                g.description = if description.is_empty() {
                    type_label.clone()
                } else if allowed_values.is_empty() {
                    format!("{type_label} · {}", description.as_str())
                } else {
                    String::new()
                };
            }
            Self::Relation { wire, description } => {
                g.field_type = wire.clone();
                g.allowed_values.clear();
                g.description = description.as_str().to_string();
            }
            Self::InlineUnionSummary { summary } => {
                g.field_type.clear();
                g.allowed_values.clear();
                g.description = summary.clone();
            }
            Self::OpaquePSlot {
                value_sym,
                wire,
                point_of_use,
            } => {
                g.field_type.clear();
                g.allowed_values.clear();
                g.description = match point_of_use {
                    PointOfUseProse::None => format!("{value_sym} · {wire}"),
                    PointOfUseProse::Distinct(d) => {
                        format!("{value_sym} · {wire} · {}", d.as_str())
                    }
                };
            }
            Self::OpaqueLegend { description } => {
                g.field_type.clear();
                g.allowed_values.clear();
                g.description = description.clone();
            }
        }
    }
}

/// Stable gloss dedupe identity — cap params must not alias entity fields with the same compact meaning.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum GlossSlotIdentity {
    CapabilityParam {
        domain: String,
        cap: String,
        param: String,
    },
    EntityField {
        entity: String,
        wire: String,
    },
}

pub(crate) fn gloss_slot_identity_for_p_sym(
    map: &SymbolMap,
    sym: &str,
    meta: &IdentMetadata,
) -> GlossSlotIdentity {
    map.capability_param_quad_for_p_sym(sym)
        .map(|(_, dom, cap, path)| GlossSlotIdentity::CapabilityParam {
            domain: dom.to_string(),
            cap: cap.to_string(),
            param: path,
        })
        .unwrap_or_else(|| GlossSlotIdentity::EntityField {
            entity: meta.entity().to_string(),
            wire: meta.wire_name().to_string(),
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PSlotSemanticKey {
    pub compact_body: String,
    pub catalog_entry_id: String,
}

pub(crate) fn gloss_p_slot_semantic_key(compact: &str, catalog_entry_id: &str) -> PSlotSemanticKey {
    PSlotSemanticKey {
        compact_body: compact.to_string(),
        catalog_entry_id: catalog_entry_id.to_string(),
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GlossTsvDedupe {
    identities: std::collections::HashSet<GlossEmitIdentity>,
    symbols: std::collections::HashSet<String>,
}

impl GlossTsvDedupe {
    pub(crate) fn try_emit_slot(&mut self, identity: &GlossEmitIdentity, symbol: &str) -> bool {
        if !self.identities.insert(identity.clone()) {
            return false;
        }
        self.symbols.insert(symbol.to_string());
        true
    }

    pub(crate) fn try_emit_projection_slot(
        &mut self,
        identity: &GlossEmitIdentity,
        symbol: &str,
    ) -> bool {
        let identity_new = self.identities.insert(identity.clone());
        if !identity_new && self.symbols.contains(symbol) {
            return false;
        }
        self.symbols.insert(symbol.to_string());
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GlossEmitIdentity {
    ValueDomain(ValueDomainStructuralKey),
    RegistryWireSlot {
        value: ValueDomainStructuralKey,
        wire: String,
        point_of_use: PointOfUseProse,
    },
    OpaquePSlot(PSlotSemanticKey),
    Relation {
        catalog_entry_id: String,
        entity: EntityName,
        wire: String,
    },
    TypedWire {
        catalog_entry_id: String,
        entity: EntityName,
        wire: String,
        meaning: FieldGlossMeaning,
    },
}

pub(crate) fn gloss_emit_identity_for_row(g: &super::TeachingFieldGloss) -> GlossEmitIdentity {
    g.emit_identity.clone().unwrap_or_else(|| {
        gloss_emit_identity_from_parts(
            &g.meaning,
            g.symbol.as_str(),
            g.catalog_entry_id.as_str(),
            g.entity.as_str(),
            None,
        )
    })
}

pub(crate) enum WireGlossRole {
    RedundantWithValueDomain,
    EmitRegistrySlot {
        value: ValueDomainStructuralKey,
        wire: String,
        point_of_use: PointOfUseProse,
    },
    EmitTyped(FieldGlossMeaning),
}

pub(crate) fn classify_registry_wire_gloss_role(
    teaching_key: &str,
    meta: &IdentMetadata,
    _cgs: &CGS,
    values_row_description: &str,
) -> WireGlossRole {
    let wire = crate::symbol_tuning::registry_backed_compact_wire_label(meta);
    let Some(value) = ValueDomainStructuralKey::from_registry_meta(meta) else {
        return WireGlossRole::EmitTyped(FieldGlossMeaning::OpaqueLegend {
            description: String::new(),
        });
    };
    let slot_norm = crate::symbol_tuning::trim_description_for_agent_gloss(meta.description());
    let point_of_use = if slot_norm.is_empty() || slot_norm == values_row_description {
        PointOfUseProse::None
    } else {
        PointOfUseProse::Distinct(GlossDescription::from_trimmed(meta.description()))
    };
    if !SymbolMap::is_opaque_p_sym(teaching_key) && teaching_key == wire {
        return match &point_of_use {
            PointOfUseProse::None => WireGlossRole::RedundantWithValueDomain,
            PointOfUseProse::Distinct(_) => WireGlossRole::EmitRegistrySlot {
                value,
                wire,
                point_of_use,
            },
        };
    }
    WireGlossRole::EmitRegistrySlot {
        value,
        wire,
        point_of_use,
    }
}

pub(crate) fn build_opaque_p_slot_meaning(
    vsym: &str,
    wire: &str,
    meta: &IdentMetadata,
    cgs: &CGS,
) -> FieldGlossMeaning {
    let nv_desc = values_row_description_for_meta(meta, cgs);
    let slot_norm = crate::symbol_tuning::trim_description_for_agent_gloss(meta.description());
    let point_of_use = if slot_norm.is_empty() || slot_norm == nv_desc.as_str() {
        PointOfUseProse::None
    } else {
        PointOfUseProse::Distinct(GlossDescription::from_trimmed(meta.description()))
    };
    FieldGlossMeaning::OpaquePSlot {
        value_sym: vsym.to_string(),
        wire: wire.to_string(),
        point_of_use,
    }
}

pub(crate) fn values_row_description_for_meta(meta: &IdentMetadata, cgs: &CGS) -> String {
    match meta {
        IdentMetadata::RegistryBacked {
            value_registry_key, ..
        } => cgs
            .values
            .get(value_registry_key.as_str())
            .map(|nv| {
                crate::symbol_tuning::trim_description_for_agent_gloss(nv.description.as_str())
                    .to_string()
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(crate) fn build_typed_field_meaning_from_render(
    render_gloss: &str,
    meta: &IdentMetadata,
) -> FieldGlossMeaning {
    let (type_label, tail) = render_gloss
        .split_once(" · ")
        .map(|(ty, t)| (ty.trim().to_string(), t.trim().to_string()))
        .unwrap_or_else(|| (render_gloss.trim().to_string(), String::new()));
    let is_enumish = matches!(type_label.as_str(), "select" | "multiselect");
    let allowed_values = if is_enumish {
        tail.clone()
    } else {
        meta.allowed_values()
            .filter(|vals| !vals.is_empty())
            .map(|vals| vals.join(", "))
            .unwrap_or_default()
    };
    let description = if is_enumish && !allowed_values.is_empty() {
        GlossDescription::from_trimmed("")
    } else if !meta.description().trim().is_empty() {
        GlossDescription::from_trimmed(meta.description())
    } else {
        GlossDescription::from_trimmed(&tail)
    };
    FieldGlossMeaning::TypedField {
        type_label,
        allowed_values,
        description,
    }
}

pub(crate) fn build_typed_field_meaning(meta: &IdentMetadata) -> FieldGlossMeaning {
    let render = meta.render_gloss(None);
    build_typed_field_meaning_from_render(render.as_str(), meta)
}

fn typed_wire_emit_entity(symbol: &str, entity: &str) -> EntityName {
    if symbol
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && !SymbolMap::is_opaque_p_sym(symbol)
        && !symbol.starts_with('v')
        && !SymbolMap::is_opaque_r_sym(symbol)
    {
        EntityName::from("")
    } else {
        EntityName::from(entity.to_string())
    }
}

pub(crate) fn gloss_emit_identity_from_parts(
    meaning: &FieldGlossMeaning,
    symbol: &str,
    catalog_entry_id: &str,
    entity: &str,
    slot: Option<&GlossSlotIdentity>,
) -> GlossEmitIdentity {
    match meaning {
        FieldGlossMeaning::ValueDomain(key) => GlossEmitIdentity::ValueDomain(key.clone()),
        FieldGlossMeaning::RegistryBackedSlot {
            value,
            wire,
            point_of_use,
        } => GlossEmitIdentity::RegistryWireSlot {
            value: value.clone(),
            wire: wire.clone(),
            point_of_use: point_of_use.clone(),
        },
        FieldGlossMeaning::OpaquePSlot {
            value_sym,
            wire,
            point_of_use,
        } => {
            let compact = match point_of_use {
                PointOfUseProse::None => format!("{value_sym} · {wire}"),
                PointOfUseProse::Distinct(d) => {
                    format!("{value_sym} · {wire} · {}", d.as_str())
                }
            };
            if let Some(slot) = slot {
                let _ = slot;
            }
            GlossEmitIdentity::OpaquePSlot(gloss_p_slot_semantic_key(&compact, catalog_entry_id))
        }
        FieldGlossMeaning::Relation { wire, .. } => GlossEmitIdentity::Relation {
            catalog_entry_id: catalog_entry_id.to_string(),
            entity: EntityName::from(entity.to_string()),
            wire: wire.clone(),
        },
        other => GlossEmitIdentity::TypedWire {
            catalog_entry_id: catalog_entry_id.to_string(),
            entity: typed_wire_emit_entity(symbol, entity),
            wire: symbol.to_string(),
            meaning: other.clone(),
        },
    }
}

/// Returns [`None`] when `sym` is a synonym for an earlier opaque symbol with the same `meaning`:
/// caller skips emitting a duplicate gloss row. Otherwise returns the canonical symbol for this meaning.
pub(crate) fn meaning_canonical_sym_for_emit(
    meaning: &str,
    sym: &str,
    meaning_to_canonical: &mut HashMap<String, String>,
    sym_alias: &mut HashMap<String, String>,
) -> Option<String> {
    match meaning_to_canonical.entry(meaning.to_string()) {
        Entry::Occupied(e) => {
            let canonical = e.get().clone();
            if canonical == sym {
                Some(canonical)
            } else {
                sym_alias.insert(sym.to_string(), canonical);
                None
            }
        }
        Entry::Vacant(v) => {
            v.insert(sym.to_string());
            Some(sym.to_string())
        }
    }
}

pub(crate) fn meaning_canonical_sym_for_emit_key<K: Eq + std::hash::Hash>(
    key: K,
    sym: &str,
    meaning_to_canonical: &mut HashMap<K, String>,
    sym_alias: &mut HashMap<String, String>,
) -> Option<String> {
    match meaning_to_canonical.entry(key) {
        Entry::Occupied(e) => {
            let canonical = e.get().clone();
            if canonical == sym {
                Some(canonical)
            } else {
                sym_alias.insert(sym.to_string(), canonical);
                None
            }
        }
        Entry::Vacant(v) => {
            v.insert(sym.to_string());
            Some(sym.to_string())
        }
    }
}

pub(crate) fn merge_opaque_alias_maps(
    p: &HashMap<String, String>,
    v: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut rep = p.clone();
    for (k, val) in v {
        if let Some(existing) = rep.get(k) {
            debug_assert_eq!(
                existing, val,
                "opaque alias collision for key {k:?}: p-map vs v-map disagree"
            );
        }
        rep.insert(k.clone(), val.clone());
    }
    rep
}

/// Teaching slot keys already demonstrated on a teaching-row LHS (projection brackets or witness).
pub(crate) fn lhs_demonstrated_syms_for_teaching_expr(
    expr: &str,
    result_gloss: Option<&str>,
    map: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
) -> std::collections::HashSet<String> {
    use crate::symbol_tuning::{field_syms_in_expr, wire_slot_idents_in_teaching_fragment};

    let mut out = std::collections::HashSet::new();
    for sym in field_syms_in_expr(expr) {
        if SymbolMap::is_opaque_p_sym(sym.as_str()) || SymbolMap::is_opaque_r_sym(sym.as_str()) {
            out.insert(sym);
        }
    }
    if let Some(g) = result_gloss {
        for sym in field_syms_in_expr(g) {
            if SymbolMap::is_opaque_p_sym(sym.as_str()) || SymbolMap::is_opaque_r_sym(sym.as_str())
            {
                out.insert(sym);
            }
        }
    }
    for wire in wire_slot_idents_in_teaching_fragment(expr) {
        let key = map
            .map(|m| {
                m.teaching_slot_token_for_entity_row_wire(catalog_entry_id, entity, wire.as_str())
            })
            .unwrap_or(wire);
        out.insert(key);
    }
    out
}
