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
        arc.line_valid_cache_symbol_fingerprint(&mut h);
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
    // Identity braces must lower to Get — never teach a Query that is sole `id_field=`.
    if identity_brace_survived_as_query(&parsed.expr, cgs) {
        return None;
    }
    let wire = if map_arc.is_some() {
        crate::expr_surface_render::render_expr_surface(&parsed.expr, cgs)
    } else {
        stripped.to_string()
    };
    Some((parsed, wire))
}

fn identity_brace_survived_as_query(expr: &crate::Expr, cgs: &CGS) -> bool {
    let crate::Expr::Query(q) = expr else {
        return false;
    };
    if q.capability_name.is_some() {
        return false;
    }
    let Some(ent) = cgs.get_entity(q.entity.as_str()) else {
        return false;
    };
    let Some(pred) = q.predicate.as_ref() else {
        return false;
    };
    crate::expr_sugar::predicate_is_sole_field_eq(pred, ent.id_field.as_str())
        && !cgs
            .find_capabilities(&q.entity, crate::CapabilityKind::Get)
            .is_empty()
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
) -> Option<(Arc<crate::expr_parser::ParsedExpr>, String)> {
    let stripped = strip_prompt_expression_annotations(expr);
    // Angle-bracket teaching holes (`<id>` / `<wire>` / `"<query>"`) are templates — validate
    // against `$` / `"q"` stand-ins so emit stays non-literal while still typechecking.
    let stripped = super::teaching_util::teaching_expr_for_validation(&stripped);
    let key = domain_line_cache_key(cache_seed, &stripped, map_arc);
    if let Some(entry) = cache.get(&key) {
        return match entry {
            DomainLineValidEntry::Invalid => None,
            DomainLineValidEntry::Valid { parsed, wire } => {
                Some((Arc::clone(parsed), wire.clone()))
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
        DomainLineValidEntry::Valid { parsed, wire } => Some((Arc::clone(parsed), wire.clone())),
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
    fn required_entity_reference_scope_has_executable_teaching() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/scoped_query_matrix");
        let cgs = load_schema_dir_unvalidated(&path).unwrap();
        let exposure = teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let map = exposure.symbol_map_arc();
        for entity in ["Child", "ChildWithParent"] {
            let expression = format!("{entity}{{parent_id=Parent(\"parent-one\")}}");
            let mut parsed = crate::expr_parser::parse(&expression, &cgs).unwrap();
            crate::normalize_expr_query_capabilities(&mut parsed.expr, &cgs).unwrap();
            crate::type_check_expr(&parsed.expr, &cgs).unwrap();
            assert!(super::super::domain_example_line_count(&cgs, entity, Some(&map)) > 0);
        }
        crate::loader::load_schema_dir(&path).unwrap();
    }

    #[test]
    fn proof_document_edit_v2_dotted_call_line_validates() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !p.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir_unvalidated(&p).expect("proof");
        cgs.bind_registry_entry_id("proof");
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
