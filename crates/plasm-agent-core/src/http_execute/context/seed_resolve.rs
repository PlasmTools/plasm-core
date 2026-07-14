//! Resolve capability seeds against the live catalog registry (entry aliases + entity case-fold).

use std::sync::Arc;

use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::schema::CGS;

use crate::http_execute::CapabilitySeed;

use super::seeds::normalize_capability_seeds;

/// Resolve a seed entity string to the canonical CGS entity key (ASCII case-insensitive).
///
/// Exact match wins; otherwise a unique case-insensitive match is accepted. Zero or multiple
/// candidates return an error with candidates listed.
pub fn resolve_entity_name_case_insensitive(cgs: &CGS, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty entity name in seeds".into());
    }
    if cgs.get_entity(raw).is_some() {
        return Ok(raw.to_string());
    }
    let mut matches: Vec<&str> = cgs
        .entities
        .keys()
        .filter(|k| k.eq_ignore_ascii_case(raw))
        .map(|k| k.as_str())
        .collect();
    matches.sort_unstable();
    matches.dedup();
    match matches.as_slice() {
        [one] => Ok((*one).to_string()),
        [] => Err(format!(
            "unknown entity `{raw}` in this schema (entity keys are catalog PascalCase; check teaching TSV / browse preview)"
        )),
        many => Err(format!(
            "ambiguous entity `{raw}` — matches {}; use the exact catalog key",
            many.join(", ")
        )),
    }
}

/// Trim/dedupe seeds, resolve each `entry_id` against the live registry (aliases, label, tags),
/// then case-fold each `entity` to the canonical CGS key for that catalog.
pub fn resolve_capability_seeds(
    seeds: Vec<CapabilitySeed>,
    registry: &InMemoryCgsRegistry,
    allowed_entry_ids: Option<&[String]>,
) -> Result<Vec<CapabilitySeed>, String> {
    let mut out = normalize_capability_seeds(seeds);
    for s in &mut out {
        s.entry_id = registry
            .resolve_entry_id(s.entry_id.as_str(), allowed_entry_ids)
            .map_err(|e| {
                if e.to_string().starts_with("unknown catalog entry:") {
                    e.to_string()
                } else {
                    format!("unknown catalog entry: {e}")
                }
            })?;
        let cgs: Arc<CGS> = registry.cgs_arc(s.entry_id.as_str()).ok_or_else(|| {
            format!(
                "unknown catalog entry: `{}` has no loaded CGS after resolve",
                s.entry_id
            )
        })?;
        s.entity = resolve_entity_name_case_insensitive(cgs.as_ref(), s.entity.as_str())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ranked_replay_fixtures::load_github_cgs;

    #[test]
    fn resolve_entity_name_case_insensitive_folds_unique_match() {
        let cgs = load_github_cgs();
        assert_eq!(
            resolve_entity_name_case_insensitive(&cgs, "repository").unwrap(),
            "Repository"
        );
        assert_eq!(
            resolve_entity_name_case_insensitive(&cgs, "Repository").unwrap(),
            "Repository"
        );
        let err = resolve_entity_name_case_insensitive(&cgs, "nope_entity").unwrap_err();
        assert!(err.contains("unknown entity"), "{err}");
    }
}
