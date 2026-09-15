//! Canonical capability-parameter symbol resolution for teaching legends, reuse recap, and capability deltas.

use std::collections::{BTreeSet, HashSet};

use crate::schema::{CapabilitySchema, InputFieldSchema, InputFieldWire, InputType};
use crate::{CapabilityKind, FieldType, CGS};

use super::{ExposureCapabilityKey, ExposureSlotKey, SymbolMap, TeachingExposureSession};

/// Which capability input params to include when building wire→`p#` pairs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityParamSurfaceFilter {
    /// Optional invoke / Query-Search **selection**, Query optional **scope**, and invocation
    /// slots for Meaning `optional:` — never control-lane wires (RA-1: controls are not braces).
    OptionalLegend,
    /// Optional params admitted on the exposure surface.
    OptionalOnSurface,
    /// All non-scope params admitted on the exposure surface (reuse / capability recap).
    AllOnSurface,
}

fn iter_cap_input_fields(
    cap: &CapabilitySchema,
    filter: CapabilityParamSurfaceFilter,
) -> Vec<&InputFieldSchema> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for f in cap.selection_params() {
        if seen.insert(f.name.as_str()) {
            out.push(f);
        }
    }
    // Query optional scope is brace-owned (RA-1). Search/method scope stays on those seats.
    if matches!(cap.kind, CapabilityKind::Query)
        && matches!(
            filter,
            CapabilityParamSurfaceFilter::OptionalLegend
                | CapabilityParamSurfaceFilter::OptionalOnSurface
        )
    {
        for f in cap.scope_params() {
            if seen.insert(f.name.as_str()) {
                out.push(f);
            }
        }
    }
    // RA-1: `optional:` invites brace/invoke omission — not pagination/hydrate controls.
    if !matches!(filter, CapabilityParamSurfaceFilter::OptionalLegend) {
        for f in cap.control_params() {
            if seen.insert(f.name.as_str()) {
                out.push(f);
            }
        }
    }
    for schema in cap.invocation_input_schemas() {
        match &schema.input_type {
            InputType::Object { fields, .. } => {
                for f in fields {
                    if seen.insert(f.name.as_str()) {
                        out.push(f);
                    }
                }
            }
            InputType::Union { variants } => {
                for variant in variants {
                    for f in &variant.fields {
                        if seen.insert(f.name.as_str()) {
                            out.push(f);
                        }
                    }
                }
            }
            _ => {}
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
    for f in iter_cap_input_fields(cap, CapabilityParamSurfaceFilter::OptionalLegend) {
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
    for f in iter_cap_input_fields(cap, CapabilityParamSurfaceFilter::OptionalLegend) {
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
    for f in iter_cap_input_fields(cap, filter) {
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

#[cfg(test)]
mod tests {
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::{symbol_map_for_prompt, FocusSpec};

    use super::{
        capability_optional_legend_param_pairs, iter_cap_input_fields, CapabilityParamSurfaceFilter,
    };

    fn prompt_matrix_cgs() -> crate::CGS {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        load_schema_dir(&dir).expect("plasm_prompt_matrix")
    }

    #[test]
    fn optional_legend_pairs_omit_query_control_wires() {
        let cgs = prompt_matrix_cgs();
        let map = symbol_map_for_prompt(&cgs, FocusSpec::All, true).expect("symbol map");
        let cap = cgs.get_capability("zone_query").expect("zone_query");
        let pairs = capability_optional_legend_param_pairs(
            map.as_ref(),
            cgs.entry_id.as_deref().unwrap_or(""),
            cap.domain.as_str(),
            cap,
        );
        let wires: Vec<&str> = pairs.iter().map(|(w, _)| w.as_str()).collect();
        assert!(
            wires.contains(&"name") && wires.contains(&"status"),
            "selection optionals remain: {wires:?}"
        );
        assert!(
            !wires
                .iter()
                .any(|w| ["page", "per_page", "sort_by"].contains(w)),
            "RA-1: controls must not join optional: {wires:?}"
        );
    }

    #[test]
    fn optional_legend_iterator_skips_controls_other_filters_keep_them() {
        let cgs = prompt_matrix_cgs();
        let cap = cgs.get_capability("zone_query").expect("zone_query");
        let legend = iter_cap_input_fields(cap, CapabilityParamSurfaceFilter::OptionalLegend);
        let surface = iter_cap_input_fields(cap, CapabilityParamSurfaceFilter::AllOnSurface);
        assert!(legend.iter().all(|f| f.name != "sort_by"));
        assert!(surface.iter().any(|f| f.name == "sort_by"));
    }
}
