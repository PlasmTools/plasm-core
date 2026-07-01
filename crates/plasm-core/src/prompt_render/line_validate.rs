//! Teaching-row validation: one opaque [`parse_session_line`] → normalize → typecheck → wire surface.

use std::collections::HashMap;
use std::sync::Arc;

use crate::symbol_tuning::{
    strip_prompt_expression_annotations, SymbolMap, SymbolSession, TeachingExposureSession,
};
use crate::CGS;

pub(crate) type DomainLineValidCacheKey = u64;

#[derive(Clone)]
pub(crate) enum DomainLineValidEntry {
    Invalid,
    Valid {
        parsed: Arc<crate::expr_parser::ParsedExpr>,
        wire: String,
    },
}

#[inline]
fn domain_line_cache_key(
    cache_seed: u64,
    stripped_expr: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> DomainLineValidCacheKey {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    cache_seed.hash(&mut h);
    stripped_expr.hash(&mut h);
    map_arc.is_some().hash(&mut h);
    if let Some(arc) = map_arc {
        let rows = arc.exposed_entity_symbol_rows();
        rows.len().hash(&mut h);
        for row in rows.iter().take(8) {
            row.entry_id.hash(&mut h);
            row.entity.hash(&mut h);
            row.symbol.hash(&mut h);
        }
    }
    h.finish()
}

#[inline]
pub(crate) fn prompt_line_valid_cache_seed_cgs(cgs: &CGS) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    cgs.catalog_cgs_hash_hex().hash(&mut h);
    h.finish()
}

#[inline]
pub(crate) fn prompt_line_valid_cache_seed_exposure(exposure: &TeachingExposureSession) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    for (entity, entry_id) in exposure
        .entities
        .iter()
        .zip(exposure.entity_catalog_entry_ids.iter())
    {
        entity.hash(&mut h);
        entry_id.hash(&mut h);
    }
    h.finish()
}

/// Parse with session map when present, normalize, type-check; render wire from parsed IR.
fn validate_teaching_line_uncached(
    cgs: &CGS,
    stripped: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> Option<(crate::expr_parser::ParsedExpr, String)> {
    let mut parsed = if let Some(arc) = map_arc {
        let cloned = Arc::clone(arc);
        let sym: Arc<dyn SymbolSession> = cloned;
        crate::expr_parser::parse_session_line(stripped, cgs, Some(sym)).ok()?
    } else {
        crate::expr_parser::parse(stripped, cgs).ok()?
    };
    if crate::normalize_expr_query_capabilities(&mut parsed.expr, cgs).is_err() {
        return None;
    }
    if crate::type_check_expr(&parsed.expr, cgs).is_err() {
        return None;
    }
    let wire = if map_arc.is_some() {
        crate::expr_surface_render::render_expr_surface(&parsed.expr, cgs)
    } else {
        stripped.to_string()
    };
    Some((parsed, wire))
}

/// Wire-only ingress (no session [`SymbolMap`]); for tests and canonical wire lines.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn validate_teaching_line_wire(
    cgs: &CGS,
    wire: &str,
) -> Option<crate::expr_parser::ParsedExpr> {
    validate_teaching_line_uncached(cgs, wire, None).map(|(p, _)| p)
}

/// Memoized validation for one teaching row — **one parse** per cache miss.
pub(crate) fn domain_line_validate_cached(
    cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    cache_seed: u64,
    cgs: &CGS,
    expr: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> Option<(crate::expr_parser::ParsedExpr, String)> {
    let stripped = strip_prompt_expression_annotations(expr);
    let key = domain_line_cache_key(cache_seed, &stripped, map_arc);
    if let Some(entry) = cache.get(&key) {
        return match entry {
            DomainLineValidEntry::Invalid => None,
            DomainLineValidEntry::Valid { parsed, wire } => {
                Some((parsed.as_ref().clone(), wire.clone()))
            }
        };
    }
    let entry = match validate_teaching_line_uncached(cgs, &stripped, map_arc) {
        Some((parsed, wire)) => DomainLineValidEntry::Valid {
            parsed: Arc::new(parsed),
            wire,
        },
        None => DomainLineValidEntry::Invalid,
    };
    let out = match &entry {
        DomainLineValidEntry::Valid { parsed, wire } => {
            Some((parsed.as_ref().clone(), wire.clone()))
        }
        DomainLineValidEntry::Invalid => None,
    };
    // Only memoize successes — a failed receiver probe for one suffix must not poison later witnesses.
    if matches!(&entry, DomainLineValidEntry::Valid { .. }) {
        cache.insert(key, entry);
    }
    out
}

#[inline]
pub(crate) fn domain_line_work_valid_cached(
    cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    cache_seed: u64,
    cgs: &CGS,
    expr: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> bool {
    domain_line_validate_cached(cache, cache_seed, cgs, expr, map_arc).is_some()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::loader::load_schema_dir_unvalidated;
    use crate::symbol_tuning::{teaching_exposure_session_from_focus, FocusSpec};

    #[test]
    fn proof_document_edit_v2_dotted_call_line_validates() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !p.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir_unvalidated(&p).expect("proof");
        cgs.entry_id = Some("proof".to_string());
        let missing = crate::cgs_expression_validate::uncovered_capabilities(&cgs);
        assert!(
            !missing
                .iter()
                .any(|(c, d)| c == "document_edit_v2" && d == "Document"),
            "document_edit_v2 should be covered; missing={missing:?}"
        );
    }

    #[test]
    fn invalid_teaching_line_probe_is_not_memoized() {
        let p = std::path::Path::new("../../fixtures/schemas/plasm_prompt_matrix");
        if !p.is_dir() {
            return;
        }
        let cgs = load_schema_dir_unvalidated(p).expect("plasm_prompt_matrix");
        let exposure = teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let map = exposure.symbol_map_arc();
        let seed = prompt_line_valid_cache_seed_cgs(&cgs);
        let mut cache = HashMap::new();
        let bogus = "e1(p1).m99()";
        assert!(domain_line_validate_cached(&mut cache, seed, &cgs, bogus, Some(&map)).is_none());
        assert!(
            cache.is_empty(),
            "invalid probes must not poison shared session cache"
        );
    }
}
