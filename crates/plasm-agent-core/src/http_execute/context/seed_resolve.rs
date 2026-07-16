//! Resolve capability seeds against the live catalog registry (entry aliases + entity case-fold).

use std::sync::Arc;

use plasm_core::discovery::InMemoryCgsRegistry;
use plasm_core::schema::CGS;

use crate::http_execute::CapabilitySeed;

use super::seeds::normalize_capability_seeds;

/// Resolve a seed entity string to the canonical CGS entity key (ASCII case-insensitive).
///
/// Exact match wins; otherwise a unique case-insensitive match is accepted. Zero or multiple
/// candidates return an error with candidates listed (and nearest-name hints on miss).
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
        [] => {
            let hints = nearest_entity_names(cgs, raw, 5);
            if hints.is_empty() {
                Err(format!(
                    "unknown entity `{raw}` in this schema (entity keys are catalog PascalCase; check teaching TSV / browse preview)"
                ))
            } else {
                Err(format!(
                    "unknown entity `{raw}` in this schema — nearest: {}; use an exact catalog key from teaching TSV / browse preview",
                    hints.join(", ")
                ))
            }
        }
        many => Err(format!(
            "ambiguous entity `{raw}` — matches {}; use the exact catalog key",
            many.join(", ")
        )),
    }
}

/// Compact nearest-name suggestions for invalid seed / expand entity diagnostics.
pub fn nearest_entity_names(cgs: &CGS, raw: &str, limit: usize) -> Vec<String> {
    let needle = raw.trim().to_ascii_lowercase();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(u32, &str)> = cgs
        .entities
        .keys()
        .map(|k| {
            let cand = k.as_str();
            let lower = cand.to_ascii_lowercase();
            let score = entity_name_distance(&needle, &lower);
            (score, cand)
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, name)| name.to_string())
        .collect()
}

fn entity_name_distance(a: &str, b: &str) -> u32 {
    if a == b {
        return 0;
    }
    if b.contains(a) || a.contains(b) {
        return 1;
    }
    // Bounded Levenshtein (early exit when > 6).
    let (a_chars, b_chars): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (n, m) = (a_chars.len(), b_chars.len());
    if n.abs_diff(m) > 6 {
        return 7;
    }
    let mut prev: Vec<u32> = (0..=m as u32).collect();
    let mut cur = vec![0u32; m + 1];
    for i in 1..=n {
        cur[0] = i as u32;
        let mut row_min = cur[0];
        for j in 1..=m {
            let cost = u32::from(a_chars[i - 1] != b_chars[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(cur[j]);
        }
        if row_min > 6 {
            return 7;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
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
    use super::super::ranked_replay_fixtures::load_github_cgs;
    use super::*;

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
        assert!(err.contains("nearest:"), "{err}");
    }

    #[test]
    fn nearest_entity_names_suggests_close_catalog_keys() {
        let cgs = load_github_cgs();
        let hints = nearest_entity_names(&cgs, "CommitFil", 3);
        assert!(
            hints.iter().any(|h| h == "CommitFile"),
            "expected CommitFile in {hints:?}"
        );
    }
}
