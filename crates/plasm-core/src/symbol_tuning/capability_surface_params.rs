//! Canonical capability-parameter symbol resolution for teaching legends, reuse recap, and ranked deltas.

use std::collections::{BTreeSet, HashSet};

use crate::schema::{CapabilitySchema, InputFieldSchema, InputType, ParameterRole};

use super::{
    field_is_filter_like_gloss, ExposureCapabilityKey, ExposureSlotKey, SymbolMap,
    TeachingExposureSession,
};

/// Which capability input params to include when building wire→`p#` pairs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityParamSurfaceFilter {
    /// Optional invoke params for `;;` legends (`optional params: wire=p#`) — schema-wide, no exposure gate.
    OptionalLegend,
    /// Optional params admitted on the exposure surface.
    OptionalOnSurface,
    /// All non-scope params admitted on the exposure surface (reuse / ranked recap).
    AllOnSurface,
}

fn iter_cap_input_fields(cap: &CapabilitySchema) -> Vec<&InputFieldSchema> {
    let Some(is) = &cap.input_schema else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let raw: Vec<&InputFieldSchema> = match &is.input_type {
        InputType::Object { fields, .. } => fields.iter().collect(),
        InputType::Union { variants } => variants.iter().flat_map(|v| v.fields.iter()).collect(),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for f in raw {
        if seen.insert(f.name.as_str()) {
            out.push(f);
        }
    }
    out
}

fn field_matches_filter(f: &InputFieldSchema, filter: CapabilityParamSurfaceFilter) -> bool {
    if matches!(f.role, Some(ParameterRole::Scope)) {
        return false;
    }
    if !field_is_filter_like_gloss(f) {
        return false;
    }
    match filter {
        CapabilityParamSurfaceFilter::OptionalLegend
        | CapabilityParamSurfaceFilter::OptionalOnSurface => !f.required,
        CapabilityParamSurfaceFilter::AllOnSurface => true,
    }
}

/// Wire→`p#` pairs for teaching-table `;;` optional legends (no exposure surface gate).
pub fn capability_optional_legend_param_pairs(
    map: &SymbolMap,
    entry_id: &str,
    domain: &str,
    cap: &CapabilitySchema,
) -> Vec<(String, String)> {
    let cap_name = cap.name.as_str();
    let mut out = Vec::new();
    for f in iter_cap_input_fields(cap) {
        if !field_matches_filter(f, CapabilityParamSurfaceFilter::OptionalLegend) {
            continue;
        }
        let sym = map.ident_sym_cap_param_for(entry_id, domain, cap_name, f.name.as_str());
        out.push((f.name.clone(), sym));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// `(wire_name, opaque p#)` pairs for one capability on the current exposure surface.
pub fn capability_exposure_param_pairs(
    exp: &TeachingExposureSession,
    map: &SymbolMap,
    cap_key: &ExposureCapabilityKey,
    cap: &CapabilitySchema,
    filter: CapabilityParamSurfaceFilter,
) -> Vec<(String, String)> {
    let entry_id = cap_key.entry_id.as_str();
    let domain = cap_key.domain.as_str();
    let cap_name = cap_key.capability.as_str();
    let mut out = Vec::new();
    for f in iter_cap_input_fields(cap) {
        if !field_matches_filter(f, filter) {
            continue;
        }
        if filter != CapabilityParamSurfaceFilter::OptionalLegend {
            let slot = ExposureSlotKey::CapabilityParam {
                capability: cap_key.clone(),
                param: crate::CapabilityParamName::new(f.name.clone()),
            };
            if !exp.surface.slots.contains(&slot) {
                continue;
            }
        }
        let sym = map.ident_sym_cap_param_for(entry_id, domain, cap_name, f.name.as_str());
        out.push((f.name.clone(), sym));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Mutating capabilities on the exposure surface (stable sort).
pub fn exposed_mutator_capability_keys(
    exp: &TeachingExposureSession,
) -> Vec<ExposureCapabilityKey> {
    let mut keys: Vec<ExposureCapabilityKey> = exp
        .surface
        .capabilities
        .iter()
        .filter(|cap_key| {
            let Some(cgs) = exp.catalog_cgs_for_entry(cap_key.entry_id.as_str()) else {
                return false;
            };
            let Some(cap) = cgs.capabilities.get(cap_key.capability.as_str()) else {
                return false;
            };
            !matches!(
                cap.kind,
                crate::CapabilityKind::Query
                    | crate::CapabilityKind::Search
                    | crate::CapabilityKind::Get
            )
        })
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// Registry rows loaded in this session (for ranked wire resolution).
pub fn loaded_catalog_entry_ids(exp: &TeachingExposureSession) -> BTreeSet<String> {
    let mut ids: BTreeSet<String> = exp.entity_catalog_entry_ids.iter().cloned().collect();
    for cap_key in &exp.surface.capabilities {
        ids.insert(cap_key.entry_id.clone());
    }
    ids
}

/// Resolve a ranked wire name to catalog-qualified capability keys defined in loaded catalogs.
pub fn resolve_ranked_wire_candidates(
    exp: &TeachingExposureSession,
    ranked_wire: &str,
) -> Vec<ExposureCapabilityKey> {
    let mut out = Vec::new();
    for entry_id in loaded_catalog_entry_ids(exp) {
        let Some(cgs) = exp.catalog_cgs_for_entry(entry_id.as_str()) else {
            continue;
        };
        if let Some(cap) = cgs.get_capability(ranked_wire) {
            out.push(ExposureCapabilityKey {
                entry_id,
                domain: cap.domain.clone(),
                capability: cap.name.clone(),
            });
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Seeded-session candidates for a ranked wire (domain entity must be in symbol space).
pub fn seeded_ranked_wire_candidates(
    exp: &TeachingExposureSession,
    ranked_wire: &str,
) -> Vec<ExposureCapabilityKey> {
    resolve_ranked_wire_candidates(exp, ranked_wire)
        .into_iter()
        .filter(|k| exp.contains_qualified_entity(k.entry_id.as_str(), k.domain.as_str()))
        .collect()
}
