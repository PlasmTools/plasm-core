//! Shared MCP reuse prompt copy (dynamic symbol maps + discovery TSV preamble).

use std::collections::BTreeSet;

use crate::symbol_tuning::{
    exposed_mutator_capability_keys, resolve_ranked_wire_candidates, seeded_ranked_wire_candidates,
    ExposureCapabilityKey,
};
use crate::TeachingExposureSession;

use super::capability_delta::render_mutator_recap_lines_for_caps;

/// MCP unified discovery TSV preamble (language flow + decision semantics).
pub const DISCOVER_TSV_LANGUAGE_PREAMBLE: &str = "\
# Plasm is a source language. These rows are NOT a program.\n\
# Next: pass selected api/entity rows to plasm_context.seeds, then write plasm.program using returned e#/m#/r# symbols and catalog wire names.\n\
# Catalogs with Get but no Search teach identity get (e#(id_field=…)) after plasm_context — not e#~\"text\" search syntax.";

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
    render_mutator_recap_lines_for_caps(exp, &exposed_mutator_capability_keys(exp))
}

fn cap_key_on_surface(exp: &TeachingExposureSession, key: &ExposureCapabilityKey) -> bool {
    exp.surface.capabilities.contains(key)
}

fn cap_key_was_on_surface(
    caps_before: &BTreeSet<ExposureCapabilityKey>,
    key: &ExposureCapabilityKey,
) -> bool {
    caps_before.contains(key)
}

fn format_qualified_cap(key: &ExposureCapabilityKey) -> String {
    format!("{}:{}.{}", key.entry_id, key.domain, key.capability)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j] + 1).min(cur[j] + 1).min(prev[j + 1] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Mutator wire names on seeded entities (full catalog, not only teaching surface).
fn mutator_wires_on_seeded_entities(exp: &TeachingExposureSession) -> Vec<String> {
    use crate::schema::CapabilityKind;
    let mut wires = Vec::new();
    for (entity, entry_id) in exp.entities.iter().zip(exp.entity_catalog_entry_ids.iter()) {
        let Some(cgs) = exp.catalog_cgs_for_entry(entry_id.as_str()) else {
            continue;
        };
        for cap in cgs.capabilities.values() {
            if cap.domain.as_str() != entity.as_str() {
                continue;
            }
            if matches!(
                cap.kind,
                CapabilityKind::Create
                    | CapabilityKind::Update
                    | CapabilityKind::Delete
                    | CapabilityKind::Action
            ) {
                wires.push(cap.name.as_str().to_string());
            }
        }
    }
    wires.sort();
    wires.dedup();
    wires
}

fn suggest_nearest_capability_wire(exp: &TeachingExposureSession, unknown: &str) -> Option<String> {
    let unk = unknown.trim().to_ascii_lowercase();
    if unk.is_empty() {
        return None;
    }
    let mut candidates = mutator_wires_on_seeded_entities(exp);
    for key in &exp.surface.capabilities {
        candidates.push(key.capability.to_string());
    }
    candidates.sort();
    candidates.dedup();

    let mut best: Option<(usize, String)> = None;
    for wire in candidates {
        let w = wire.to_ascii_lowercase();
        if w == unk {
            continue;
        }
        let score = if w.contains(&unk) || unk.contains(&w) {
            1
        } else {
            levenshtein(&unk, &w)
        };
        if score <= 6 && best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
            best = Some((score, wire));
        }
    }
    best.map(|(_, w)| w)
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
    let mut ambiguous = Vec::new();

    for name in ranked_names {
        let candidates = resolve_ranked_wire_candidates(exp, name.as_str());
        if candidates.is_empty() {
            unknown.push(name.as_str());
            continue;
        }
        let seeded = seeded_ranked_wire_candidates(exp, name.as_str());
        if seeded.is_empty() {
            let domains: Vec<String> = candidates
                .iter()
                .map(|k| format!("{}:{}", k.entry_id, k.domain))
                .collect();
            non_seeded.push(format!("{name} (domains: {})", domains.join(", ")));
            continue;
        }
        if seeded.len() > 1 {
            ambiguous.push(format!(
                "{name} ({})",
                seeded
                    .iter()
                    .map(format_qualified_cap)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        }
        let key = &seeded[0];
        let on_surface = cap_key_on_surface(exp, key);
        let was_on_surface = cap_key_was_on_surface(caps_before, key);
        if was_on_surface && on_surface {
            already_exposed.push(format_qualified_cap(key));
        } else if !was_on_surface && on_surface {
            newly_added.push(format_qualified_cap(key));
        } else {
            rejected.push(format_qualified_cap(key));
        }
    }

    let mut parts = Vec::new();
    if !newly_added.is_empty() {
        parts.push(format!("ranked added: {}", newly_added.join(", ")));
    }
    if !already_exposed.is_empty() {
        parts.push(format!("already exposed: {}", already_exposed.join(", ")));
    }
    if !unknown.is_empty() {
        let available = mutator_wires_on_seeded_entities(exp);
        let available_note = if available.is_empty() {
            String::new()
        } else {
            format!("; mutators on seeded entities: {}", available.join(", "))
        };
        let parts_with_hints: Vec<String> = unknown
            .iter()
            .map(|name| {
                suggest_nearest_capability_wire(exp, name)
                    .map(|hint| format!("{name} (did you mean {hint}?)"))
                    .unwrap_or_else(|| (*name).to_string())
            })
            .collect();
        parts.push(format!(
            "unsupported in this catalog: {} — no capability with this wire name exists in loaded catalogs; do not invent names; use only wires from the teaching table{available_note}",
            parts_with_hints.join(", ")
        ));
    }
    if !non_seeded.is_empty() {
        parts.push(format!(
            "not on seeded entities (add to seeds): {}",
            non_seeded.join(", ")
        ));
    }
    if !ambiguous.is_empty() {
        parts.push(format!("ambiguous ranked wire: {}", ambiguous.join("; ")));
    }
    if !rejected.is_empty() {
        parts.push(format!("rejected by intent gate: {}", rejected.join(", ")));
    }
    if parts.is_empty() {
        "ranked replay: no surface change".to_string()
    } else {
        format!("Ranked replay: {}.", parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{derive_intent_exposure_surface_batch, ExposureSurfaceOptions};
    use crate::loader::load_schema_dir;
    use crate::ExposureEntityKey;
    use std::path::PathBuf;

    #[test]
    fn diagnostics_use_qualified_capability_keys() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema_dir(&root.join("../../fixtures/schemas/plasm_language_matrix"))
            .expect("matrix");
        let entities = ["LangItem"];
        let endpoints = entities
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "matrix".into(),
                entity: crate::EntityName::from(*e),
            })
            .collect::<Vec<_>>();
        let intent = "create new langitem title";
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            intent,
            &endpoints,
            &entities
                .iter()
                .map(|e| (*e).to_string())
                .collect::<Vec<_>>(),
            Some(&["langitem_create".to_string()]),
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let exp = TeachingExposureSession::new_with_intent_delta(&cgs, "matrix", &entities, delta);
        let caps_before = exp.surface.capabilities.clone();
        let diag =
            format_ranked_replay_diagnostics(&exp, &["langitem_create".to_string()], &caps_before);
        assert!(
            diag.contains("matrix:LangItem.langitem_create"),
            "expected qualified key in diagnostic: {diag}"
        );
    }

    #[test]
    fn diagnostics_mark_invented_wires_unsupported_in_catalog() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema_dir(&root.join("../../fixtures/schemas/plasm_language_matrix"))
            .expect("matrix");
        let entities = ["LangItem"];
        let endpoints = entities
            .iter()
            .map(|e| ExposureEntityKey {
                entry_id: "matrix".into(),
                entity: crate::EntityName::from(*e),
            })
            .collect::<Vec<_>>();
        let delta = derive_intent_exposure_surface_batch(
            &cgs,
            "matrix",
            "create langitem",
            &endpoints,
            &entities
                .iter()
                .map(|e| (*e).to_string())
                .collect::<Vec<_>>(),
            None,
            ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        let exp = TeachingExposureSession::new_with_intent_delta(&cgs, "matrix", &entities, delta);
        let caps_before = exp.surface.capabilities.clone();
        let diag =
            format_ranked_replay_diagnostics(&exp, &["branch_delete".to_string()], &caps_before);
        assert!(
            diag.contains("unsupported in this catalog"),
            "expected not-in-catalog framing: {diag}"
        );
        assert!(
            !diag.contains("corrected ranked_capabilities"),
            "must not tell agents to invent better names: {diag}"
        );
        assert!(
            diag.contains("langitem_create") || diag.contains("mutators on seeded"),
            "should list real mutators on seeded entities: {diag}"
        );
    }
}
