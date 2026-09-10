//! Canonical capability-parameter symbol resolution for teaching legends, reuse recap, and capability deltas.

use std::collections::{BTreeSet, HashSet};

use crate::schema::{CapabilitySchema, InputFieldSchema, InputFieldWire, InputType};
use crate::{FieldType, CGS};

use super::{ExposureCapabilityKey, ExposureSlotKey, SymbolMap, TeachingExposureSession};

/// Which capability input params to include when building wire→`p#` pairs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityParamSurfaceFilter {
    /// Optional invoke params for Meaning `optional` legend — includes opaque `p#` and teaching wire tokens when both apply.
    OptionalLegend,
    /// Optional params admitted on the exposure surface.
    OptionalOnSurface,
    /// All non-scope params admitted on the exposure surface (reuse / capability recap).
    AllOnSurface,
}

fn iter_cap_input_fields(cap: &CapabilitySchema) -> Vec<&InputFieldSchema> {
    let mut seen = HashSet::new();
    let raw = cap
        .selection_params()
        .iter()
        .chain(cap.control_params())
        .chain(
            cap.invocation_input_schemas()
                .flat_map(|schema| match &schema.input_type {
                    InputType::Object { fields, .. } => {
                        Box::new(fields.iter()) as Box<dyn Iterator<Item = &InputFieldSchema>>
                    }
                    InputType::Union { variants } => {
                        Box::new(variants.iter().flat_map(|variant| variant.fields.iter()))
                    }
                    _ => Box::new(std::iter::empty()),
                }),
        );
    let mut out = Vec::new();
    for f in raw {
        if seen.insert(f.name.as_str()) {
            out.push(f);
        }
    }
    out
}

fn field_matches_filter(f: &InputFieldSchema, filter: CapabilityParamSurfaceFilter) -> bool {
    match filter {
        CapabilityParamSurfaceFilter::OptionalLegend
        | CapabilityParamSurfaceFilter::OptionalOnSurface => !f.required,
        CapabilityParamSurfaceFilter::AllOnSurface => true,
    }
}

pub fn optional_legend_param_syms(
    map: &SymbolMap,
    entry_id: &str,
    domain: &str,
    cap: &CapabilitySchema,
) -> Vec<String> {
    let cap_name = cap.name.as_str();
    let mut syms = Vec::new();
    for f in iter_cap_input_fields(cap) {
        if !field_matches_filter(f, CapabilityParamSurfaceFilter::OptionalLegend) {
            continue;
        }
        let sym = map.teaching_slot_token_cap_param(entry_id, domain, cap_name, f.name.as_str());
        if !sym.is_empty() && !syms.iter().any(|s| s == &sym) {
            syms.push(sym);
        }
    }
    syms.sort();
    syms
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
        let sym = map.teaching_slot_token_cap_param(entry_id, domain, cap_name, f.name.as_str());
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
    let Some(cgs) = exp.catalog_cgs_for_entry(cap_key.entry_id.as_str()) else {
        return Vec::new();
    };
    capability_exposure_param_triples(exp, map, cap_key, cap, filter, cgs)
        .into_iter()
        .map(|(wire, sym, _)| (wire, sym))
        .collect()
}

/// Whether a capability input field is typed as an array (registry or inline schema).
pub fn input_field_is_array(f: &InputFieldSchema, cgs: &CGS) -> bool {
    match &f.wire {
        InputFieldWire::Inline(ty) => matches!(ty.as_ref(), InputType::Array { .. }),
        InputFieldWire::Registry(_) => f
            .named_value(cgs)
            .map(|nv| matches!(nv.field_type, FieldType::Array))
            .unwrap_or(false),
    }
}

/// Compact type/role suffix for mutator recap rows (`[]` array, `!` required).
pub fn compact_mutator_param_marker(f: &InputFieldSchema, cgs: &CGS) -> String {
    let mut marker = String::new();
    if input_field_is_array(f, cgs) {
        marker.push_str("[]");
    }
    if f.required {
        marker.push('!');
    }
    marker
}

/// `(wire_name, opaque p#, type/role marker)` for mutator recap / capability selection.
pub fn capability_exposure_param_triples(
    exp: &TeachingExposureSession,
    map: &SymbolMap,
    cap_key: &ExposureCapabilityKey,
    cap: &CapabilitySchema,
    filter: CapabilityParamSurfaceFilter,
    cgs: &CGS,
) -> Vec<(String, String, String)> {
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
        let sym = map.teaching_slot_token_cap_param(entry_id, domain, cap_name, f.name.as_str());
        let marker = compact_mutator_param_marker(f, cgs);
        out.push((f.name.clone(), sym, marker));
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

/// Registry rows loaded in this session.
pub fn loaded_catalog_entry_ids(exp: &TeachingExposureSession) -> BTreeSet<String> {
    let mut ids: BTreeSet<String> = exp.entity_catalog_entry_ids.iter().cloned().collect();
    for cap_key in &exp.surface.capabilities {
        ids.insert(cap_key.entry_id.clone());
    }
    ids
}
