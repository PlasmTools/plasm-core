//! Shared MCP reuse prompt copy (dynamic symbol maps + discovery TSV preamble).

use crate::TeachingExposureSession;

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
