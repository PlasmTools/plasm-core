//! Shared MCP reuse prompt copy (dynamic symbol maps + mutator surface recap).

use crate::symbol_tuning::exposed_mutator_capability_keys;
use crate::TeachingExposureSession;

use super::capability_delta::render_mutator_recap_lines_for_caps;

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
