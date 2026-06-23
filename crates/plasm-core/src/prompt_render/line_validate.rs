//! Teaching-row validation: one opaque parse → normalize → typecheck → wire surface (no double-parse).

use std::collections::HashMap;
use std::sync::Arc;

use crate::symbol_tuning::{
    strip_prompt_expression_annotations, SymbolMap, TeachingExposureSession,
};
use crate::CGS;

pub(crate) type DomainLineValidCacheKey = u64;

#[derive(Clone)]
pub(crate) enum DomainLineValidEntry {
    Invalid,
    Valid {
        parsed: Box<crate::expr_parser::ParsedExpr>,
        wire: String,
    },
}

#[inline]
fn domain_line_cache_key(cache_seed: u64, stripped_expr: &str) -> DomainLineValidCacheKey {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    cache_seed.hash(&mut h);
    stripped_expr.hash(&mut h);
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

/// Parse (opaque or wire), normalize, type-check; render wire when session symbols are active.
fn validate_teaching_line_uncached(
    cgs: &CGS,
    stripped: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> Option<(crate::expr_parser::ParsedExpr, String)> {
    let mut parsed = if let Some(arc) = map_arc {
        let layers = [cgs];
        match crate::expr_parser::parse_with_cgs_layers(stripped, &layers, Arc::clone(arc)) {
            Ok(r) => r,
            Err(_) => return None,
        }
    } else {
        match crate::expr_parser::parse(stripped, cgs) {
            Ok(r) => r,
            Err(_) => return None,
        }
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
    let key = domain_line_cache_key(cache_seed, &stripped);
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
            parsed: Box::new(parsed),
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
    cache.insert(key, entry);
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
