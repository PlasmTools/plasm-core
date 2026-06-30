//! Shared MCP reuse prompt copy (dynamic symbol maps + discovery TSV preamble).

use std::collections::{BTreeSet, HashSet};

use crate::schema::capability_method_label_kebab;
use crate::symbol_tuning::{ExposureCapabilityKey, SymbolMap};
use crate::{CapabilityKind, TeachingExposureSession};

/// MCP unified discovery TSV preamble (language flow + decision semantics).
pub const DISCOVER_TSV_LANGUAGE_PREAMBLE: &str = "\
# Plasm is a source language. These rows are NOT a program.\n\
# Next: pass selected api/entity rows to plasm_context.seeds, then write plasm.program using returned e#/m#/p#/r# symbols.\n\
# Catalogs with Get but no Search teach identity get (e#(p#=…)) after plasm_context — not e#~\"text\" search syntax.";

/// Discovery decision values embedded as `# decision: …` TSV comment lines.
pub const DISCOVER_DECISION_MATCH: &str = "match";
pub const DISCOVER_DECISION_CLARIFY: &str = "clarify";
pub const DISCOVER_DECISION_NO_MATCH: &str = "no_match";

/// Compact `e#=Entity` map for reuse responses (federated rows prefix `entry_id:` only when entity names collide).
pub fn render_compact_exposure_symbol_map(exp: &TeachingExposureSession) -> String {
    let mut name_counts = std::collections::HashMap::<&str, usize>::new();
    for entity in &exp.entities {
        *name_counts.entry(entity.as_str()).or_insert(0) += 1;
    }
    let needs_catalog_prefix = name_counts.values().any(|&c| c > 1);

    exp.entities
        .iter()
        .zip(exp.entity_catalog_entry_ids.iter())
        .enumerate()
        .map(|(i, (entity, entry_id))| {
            let sym = exp
                .qualified_entity_symbol(entry_id, entity)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("e{}", i + 1));
            let label = if needs_catalog_prefix {
                format!("{entry_id}:{entity}")
            } else {
                entity.clone()
            };
            format!("{sym}={label}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Compact active mutator / param recap for duplicate `plasm_context` reuse (TSV body, no fence).
pub fn render_active_mutator_surface_recap(exp: &TeachingExposureSession) -> String {
    let map = exp.symbol_map_arc();
    let mut cap_keys: Vec<&ExposureCapabilityKey> = exp.surface.capabilities.iter().collect();
    cap_keys.sort_by(|a, b| {
        (
            a.entry_id.as_str(),
            a.domain.as_str(),
            a.capability.as_str(),
        )
            .cmp(&(
                b.entry_id.as_str(),
                b.domain.as_str(),
                b.capability.as_str(),
            ))
    });
    let mut lines: Vec<String> = Vec::new();
    for cap_key in cap_keys {
        let Some(cgs) = exp.catalog_cgs_for_entry(cap_key.entry_id.as_str()) else {
            continue;
        };
        let Some(cap) = cgs.capabilities.get(cap_key.capability.as_str()) else {
            continue;
        };
        if matches!(
            cap.kind,
            CapabilityKind::Query | CapabilityKind::Search | CapabilityKind::Get
        ) {
            continue;
        }
        let entity = cap_key.domain.as_str();
        let Some(e_sym) = exp.qualified_entity_symbol(cap_key.entry_id.as_str(), entity) else {
            continue;
        };
        let kebab = capability_method_label_kebab(cap);
        let m_sym = map.method_sym_for(cap_key.entry_id.as_str(), entity, &kebab);
        let mut param_pairs: Vec<String> = Vec::new();
        collect_cap_param_pairs(exp, &map, cap_key, cap, &mut param_pairs);
        param_pairs.sort();
        let cap_wire = cap_key.capability.as_str();
        if param_pairs.is_empty() {
            lines.push(format!("{e_sym}.{m_sym}\t{cap_wire}"));
        } else {
            lines.push(format!(
                "{e_sym}.{m_sym}\t{cap_wire} · {}",
                param_pairs.join(", ")
            ));
        }
    }
    lines.join("\n")
}

fn collect_cap_param_pairs(
    exp: &TeachingExposureSession,
    map: &SymbolMap,
    cap_key: &ExposureCapabilityKey,
    cap: &crate::CapabilitySchema,
    out: &mut Vec<String>,
) {
    let Some(is) = &cap.input_schema else {
        return;
    };
    let entry_id = cap_key.entry_id.as_str();
    let domain = cap_key.domain.as_str();
    let cap_name = cap_key.capability.as_str();
    let mut seen = HashSet::new();
    let fields = match &is.input_type {
        crate::InputType::Object { fields, .. } => fields.iter().collect::<Vec<_>>(),
        crate::InputType::Union { variants } => variants
            .iter()
            .flat_map(|v| v.fields.iter())
            .collect(),
        _ => return,
    };
    for f in fields {
        if matches!(f.role, Some(crate::ParameterRole::Scope)) {
            continue;
        }
        if !seen.insert(f.name.clone()) {
            continue;
        }
        let slot = crate::symbol_tuning::ExposureSlotKey::CapabilityParam {
            capability: cap_key.clone(),
            param: crate::CapabilityParamName::new(f.name.clone()),
        };
        if !exp.surface.slots.contains(&slot) {
            continue;
        }
        let sym = map.ident_sym_cap_param_for(entry_id, domain, cap_name, f.name.as_str());
        out.push(format!("{}={sym}", f.name.as_str()));
    }
}

/// Agent-facing diagnostic when ranked replay did not expand the teaching surface.
pub fn format_ranked_replay_diagnostics(
    exp: &TeachingExposureSession,
    ranked_names: &[String],
    caps_before: &BTreeSet<ExposureCapabilityKey>,
) -> String {
    let mut already_exposed = Vec::new();
    let mut newly_added = Vec::new();
    let mut unknown = Vec::new();
    let mut non_seeded = Vec::new();
    let mut rejected = Vec::new();

    for name in ranked_names {
        let on_surface = exp
            .surface
            .capabilities
            .iter()
            .any(|k| k.capability.as_str() == name.as_str());
        let was_on_surface = caps_before
            .iter()
            .any(|k| k.capability.as_str() == name.as_str());
        if was_on_surface && on_surface {
            already_exposed.push(name.as_str());
            continue;
        }
        if !was_on_surface && on_surface {
            newly_added.push(name.as_str());
            continue;
        }
        let mut domains: Vec<(String, String)> = Vec::new();
        let mut entry_ids: BTreeSet<String> = exp.entity_catalog_entry_ids.iter().cloned().collect();
        for cap_key in &exp.surface.capabilities {
            entry_ids.insert(cap_key.entry_id.clone());
        }
        for entry_id in entry_ids {
            let Some(cgs) = exp.catalog_cgs_for_entry(entry_id.as_str()) else {
                continue;
            };
            if let Some(cap) = cgs.capabilities.get(name.as_str()) {
                domains.push((entry_id, cap.domain.to_string()));
            }
        }
        if domains.is_empty() {
            unknown.push(name.as_str());
            continue;
        }
        let seeded = domains.iter().any(|(eid, entity)| {
            exp.contains_qualified_entity(eid.as_str(), entity.as_str())
        });
        if !seeded {
            non_seeded.push(name.as_str());
        } else {
            rejected.push(name.as_str());
        }
    }

    let mut parts = Vec::new();
    if !newly_added.is_empty() {
        parts.push(format!(
            "ranked added: {}",
            newly_added.join(", ")
        ));
    }
    if !already_exposed.is_empty() {
        parts.push(format!(
            "already exposed: {}",
            already_exposed.join(", ")
        ));
    }
    if !unknown.is_empty() {
        parts.push(format!("unknown capability: {}", unknown.join(", ")));
    }
    if !non_seeded.is_empty() {
        parts.push(format!(
            "not on seeded entities (add to seeds): {}",
            non_seeded.join(", ")
        ));
    }
    if !rejected.is_empty() {
        parts.push(format!(
            "rejected by intent gate: {}",
            rejected.join(", ")
        ));
    }
    if parts.is_empty() {
        "ranked replay: no surface change".to_string()
    } else {
        format!("Ranked replay: {}.", parts.join("; "))
    }
}
