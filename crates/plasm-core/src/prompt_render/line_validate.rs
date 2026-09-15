//! Teaching-row validation: Expr-stratum [`parse_session_line`] **or** program-stratum
//! query bind (`ident = <query>`) and relation fanout (`ident => _.r#` via [`parse_expr_node`]).

use std::collections::HashMap;
use std::sync::Arc;

use crate::expr_parser::{
    parse_expr_node, split_assignment_for_binding, validate_program_label, Applicator, RowExpr,
};
use crate::relation_nav::relation_nav_admissible;
use crate::schema::{EntityDef, RelationSchema};
use crate::symbol_tuning::{
    strip_prompt_expression_annotations, SymbolMap, SymbolSession, TeachingExposureSession,
};
use crate::CGS;

pub(crate) type DomainLineValidCacheKey = u64;

/// Validated teaching-line IR: Expr hop **or** admitted program-stratum seats.
#[derive(Clone, Debug)]
pub(crate) enum ValidatedTeachingLine {
    Expr {
        parsed: Arc<crate::expr_parser::ParsedExpr>,
        wire: String,
    },
    /// `ident = <list query>` — bind echoing the taught query expression.
    QueryBind { wire: String },
    /// `ident => _.r#` / `ident => _.wire` — [`Applicator::Relation`] only.
    RelationFanout { wire: String },
}

impl ValidatedTeachingLine {
    pub(crate) fn wire(&self) -> &str {
        match self {
            Self::Expr { wire, .. } | Self::QueryBind { wire } | Self::RelationFanout { wire } => {
                wire.as_str()
            }
        }
    }
}

#[derive(Clone)]
pub(crate) enum DomainLineValidEntry {
    Invalid,
    Valid(ValidatedTeachingLine),
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
fn validate_teaching_expr_line(
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

/// Program-stratum admit: `ident = <list Query>` — the bind half of the fanout pair.
///
/// RHS must type-check as [`crate::Expr::Query`] (the taught list expression, not Get-id).
fn validate_query_bind_program_line(
    cgs: &CGS,
    stripped: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> Option<ValidatedTeachingLine> {
    let (label, rhs) = split_assignment_for_binding(stripped)?;
    validate_program_label(label).ok()?;
    let (parsed, wire) = validate_teaching_expr_line(cgs, rhs, map_arc)?;
    if !matches!(parsed.expr, crate::Expr::Query(_)) {
        return None;
    }
    Some(ValidatedTeachingLine::QueryBind {
        wire: format!("{label} = {wire}"),
    })
}

/// Program-stratum admit: **only** `ident => _.r#` / `ident => _.wire` (`Applicator::Relation`).
///
/// Same surface parser as execution (`parse_expr_node`). The ident is a program binding label
/// (Γ is supplied by a prior bind at run time). The `r#` / wire must resolve to an admissible
/// CGS relation — not a string allowlist.
fn validate_relation_fanout_program_line(
    cgs: &CGS,
    stripped: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> Option<ValidatedTeachingLine> {
    let node = parse_expr_node(stripped).ok()?;
    let Applicator::Relation { wire: rel_tok } = node.apply.as_ref()? else {
        return None;
    };
    let RowExpr::Primary { head, collect_meta } = &node.row else {
        return None;
    };
    if !collect_meta.is_empty() {
        return None;
    }
    validate_program_label(head.trim()).ok()?;
    let (_ent, rel) = resolve_fanout_relation(cgs, map_arc, rel_tok)?;
    if !relation_nav_admissible(rel, cgs) {
        return None;
    }
    Some(ValidatedTeachingLine::RelationFanout {
        wire: format!("{} => _.{}", head.trim(), rel.name.as_str()),
    })
}

fn resolve_fanout_relation<'a>(
    cgs: &'a CGS,
    map_arc: Option<&Arc<SymbolMap>>,
    sym_or_wire: &str,
) -> Option<(&'a EntityDef, &'a RelationSchema)> {
    if let Some(map) = map_arc {
        let binding = map.relation_binding_for_sym(sym_or_wire)?;
        let ent = cgs.get_entity(binding.source_entity.as_str())?;
        let rel = ent.relations.get(binding.relation_wire.as_str())?;
        return Some((ent, rel));
    }
    let mut found = None;
    for ent in cgs.entities.values() {
        if let Some(rel) = ent.relations.get(sym_or_wire) {
            if found.is_some() {
                return None;
            }
            found = Some((ent, rel));
        }
    }
    found
}

fn validate_teaching_line_uncached(
    cgs: &CGS,
    stripped: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> Option<ValidatedTeachingLine> {
    if let Some((parsed, wire)) = validate_teaching_expr_line(cgs, stripped, map_arc) {
        return Some(ValidatedTeachingLine::Expr {
            parsed: Arc::new(parsed),
            wire,
        });
    }
    if let Some(bind) = validate_query_bind_program_line(cgs, stripped, map_arc) {
        return Some(bind);
    }
    validate_relation_fanout_program_line(cgs, stripped, map_arc)
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
    validate_teaching_expr_line(cgs, wire, None).map(|(p, _)| p)
}

/// Memoized validation for one teaching row — **one parse** per cache miss.
pub(crate) fn domain_line_validate_cached(
    cache: &mut HashMap<DomainLineValidCacheKey, DomainLineValidEntry>,
    cache_seed: u64,
    cgs: &CGS,
    expr: &str,
    map_arc: Option<&Arc<SymbolMap>>,
) -> Option<ValidatedTeachingLine> {
    let stripped = strip_prompt_expression_annotations(expr);
    // Angle-bracket teaching holes (`<id>` / `<wire>` / `"<query>"` / `"<member>"`) are
    // templates — validate against `$` / `"q"` stand-ins so emit stays non-literal
    // while still typechecking.
    let stripped = super::teaching_util::teaching_expr_for_validation(&stripped);
    let key = domain_line_cache_key(cache_seed, &stripped, map_arc);
    if let Some(entry) = cache.get(&key) {
        return match entry {
            DomainLineValidEntry::Invalid => None,
            DomainLineValidEntry::Valid(line) => Some(line.clone()),
        };
    }
    let entry = match validate_teaching_line_uncached(cgs, &stripped, map_arc) {
        Some(line) => DomainLineValidEntry::Valid(line),
        None => DomainLineValidEntry::Invalid,
    };
    let out = match &entry {
        DomainLineValidEntry::Valid(line) => Some(line.clone()),
        DomainLineValidEntry::Invalid => None,
    };
    // Only memoize successes — a failed receiver probe for one suffix must not poison later witnesses.
    if matches!(&entry, DomainLineValidEntry::Valid(_)) {
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
    fn langitem_ping_dotted_call_line_validates() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let mut cgs = load_schema_dir_unvalidated(&p).expect("plasm_language_matrix");
        cgs.bind_registry_entry_id("langmatrix");
        let missing = crate::cgs_expression_validate::uncovered_capabilities(&cgs);
        assert!(
            !missing
                .iter()
                .any(|(c, d)| c == "langitem_ping" && d == "LangItem"),
            "langitem_ping should be covered; missing={missing:?}"
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

    #[test]
    fn program_stratum_relation_fanout_ident_line_validates() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let cgs = load_schema_dir_unvalidated(&path).expect("plasm_prompt_matrix");
        let exposure = teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let map = exposure.symbol_map_arc();
        let r = map.ident_sym_relation_for("", "Zone", "rulesets");
        assert!(
            r.starts_with('r'),
            "expected opaque r# for Zone.rulesets, got {r}"
        );
        let expr = format!("rows => _.{}", r);
        let seed = prompt_line_valid_cache_seed_cgs(&cgs);
        let mut cache = HashMap::new();
        let got = domain_line_validate_cached(&mut cache, seed, &cgs, &expr, Some(&map));
        assert!(
            matches!(got, Some(ValidatedTeachingLine::RelationFanout { .. })),
            "fanout teaching line must admit via parse_expr_node: {expr} → {got:?}"
        );
        let alt = format!("items => _.{}", r);
        assert!(
            domain_line_validate_cached(&mut cache, seed, &cgs, &alt, Some(&map)).is_some(),
            "any valid program label must admit beside rows: {alt}"
        );
        let es = super::super::symbol_tokens::ent_sym(Some(map.as_ref()), "", "Zone");
        let cap = cgs
            .find_capabilities("Zone", crate::CapabilityKind::Query)
            .into_iter()
            .next()
            .expect("zone_query");
        let query_head = super::super::query_teaching::query_expr_maximal(
            cap,
            &es,
            &cgs,
            Some(map.as_ref()),
            "",
        )
        .expect("Zone list query head");
        let bind = format!("rows = {query_head}");
        let got_bind = domain_line_validate_cached(&mut cache, seed, &cgs, &bind, Some(&map));
        assert!(
            matches!(got_bind, Some(ValidatedTeachingLine::QueryBind { .. })),
            "query bind must admit as program-stratum QueryBind: {bind} → {got_bind:?}"
        );
        let get_bind = format!("rows = {es}(<id>)");
        assert!(
            domain_line_validate_cached(&mut cache, seed, &cgs, &get_bind, Some(&map)).is_none(),
            "Get-id must not admit as query bind: {get_bind}"
        );
    }

    #[test]
    fn program_stratum_admits_only_ident_relation_apply() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let cgs = load_schema_dir_unvalidated(&path).expect("plasm_prompt_matrix");
        let exposure = teaching_exposure_session_from_focus(&cgs, FocusSpec::All);
        let map = exposure.symbol_map_arc();
        let r = map.ident_sym_relation_for("", "Zone", "rulesets");
        let seed = prompt_line_valid_cache_seed_cgs(&cgs);
        let mut cache = HashMap::new();
        for expr in [
            format!("e1{{}} => _.{}", r),
            format!("rows => {{ t: _.id }}"),
            "rows => <<TAG\n{{ _.id }}\nTAG".to_string(),
        ] {
            assert!(
                domain_line_validate_cached(&mut cache, seed, &cgs, &expr, Some(&map)).is_none(),
                "must not admit arbitrary program as teaching line: {expr}"
            );
        }
    }
}
