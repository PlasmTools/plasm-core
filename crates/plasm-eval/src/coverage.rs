//! CGS-derived eval form buckets and coverage reporting (deterministic; no LLM).

use std::collections::{HashMap, HashSet};

use plasm_core::expr::{ChainStep, Expr};
use plasm_core::expr_parser;
use plasm_core::predicate::Predicate;
use plasm_core::schema::CapabilityKind;
use plasm_core::{ParameterRole, Value, CGS};
use serde::Deserialize;

type UnionCaseEntitiesResult = (HashSet<String>, HashMap<String, Vec<String>>);

/// Closed vocabulary for `covers:` in eval YAML (snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalFormId {
    /// List/query with no CGS input object (true unfiltered query — not pagination-only).
    QueryAll,
    /// Predicate filters on a query (`Entity{field=…}`) — scope/filter/search params.
    QueryFiltered,
    /// Full-text / relevance search (`Entity~"…"`) — schema has Search capability.
    SearchText,
    /// Fetch by id (`Entity(id)`).
    Get,
    /// Forward relation / EntityRef navigation (`.field`).
    Chain,
    /// Reverse traversal (`.^Entity`).
    Reverse,
    /// Field projection (`[a,b]`) — required whenever Get or Query exists (expressible surface).
    Projection,
    /// Zero-arity or entity-scoped action (`invoke` IR / `E.method()`).
    Invoke,
    Create,
    Update,
    Delete,
    /// Multi-step plan (multiple expressions) — required when schema has 2+ entities.
    MultiStep,
    /// Opaque pagination continuation (`page(pg#)`).
    PageNext,
}

impl EvalFormId {
    pub fn as_str(self) -> &'static str {
        match self {
            EvalFormId::QueryAll => "query_all",
            EvalFormId::QueryFiltered => "query_filtered",
            EvalFormId::SearchText => "search_text",
            EvalFormId::Get => "get",
            EvalFormId::Chain => "chain",
            EvalFormId::Reverse => "reverse",
            EvalFormId::Projection => "projection",
            EvalFormId::Invoke => "invoke",
            EvalFormId::Create => "create",
            EvalFormId::Update => "update",
            EvalFormId::Delete => "delete",
            EvalFormId::MultiStep => "multi_step",
            EvalFormId::PageNext => "page_next",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "query_all" => Some(Self::QueryAll),
            "query_filtered" => Some(Self::QueryFiltered),
            "search_text" => Some(Self::SearchText),
            "get" => Some(Self::Get),
            "chain" => Some(Self::Chain),
            "reverse" => Some(Self::Reverse),
            "projection" => Some(Self::Projection),
            "invoke" => Some(Self::Invoke),
            "create" => Some(Self::Create),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            "multi_step" => Some(Self::MultiStep),
            "page_next" => Some(Self::PageNext),
            _ => None,
        }
    }
}

/// Optional `apis/<api>/eval/coverage.yaml` — add buckets beyond CGS (`required_extra` only).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CoverageOverride {
    /// Must match directory / case `schema:` field.
    #[serde(default)]
    pub schema: String,
    /// Extra buckets required beyond CGS derivation.
    #[serde(default)]
    pub required_extra: Vec<String>,
}

/// True when domain-authored parameters are absent or an empty object (`parameters:` omitted
/// or `[]`).
fn query_has_empty_object_params(cap: &plasm_core::CapabilitySchema) -> bool {
    cap.object_params().is_none_or(|fields| fields.is_empty())
}

fn mapping_declares_pagination(cap: &plasm_core::CapabilitySchema) -> bool {
    cap.mapping.template.0.get("pagination").is_some()
}

/// Unfiltered list query in CGS terms: no typed domain parameters **and** no cursor/pagination
/// block in the mapping. (Omitted `parameters:` plus `pagination:` in CML is a paginated list,
/// not this eval bucket — e.g. Notion `user_query`.)
fn query_has_truly_unfiltered_list_surface(cap: &plasm_core::CapabilitySchema) -> bool {
    matches!(cap.kind, CapabilityKind::Query)
        && query_has_empty_object_params(cap)
        && !mapping_declares_pagination(cap)
}

/// True when query inputs include at least one predicate/scope/search field (not only
/// pagination or sort plumbing).
fn query_has_predicate_narrowing_params(cap: &plasm_core::CapabilitySchema) -> bool {
    cap.object_params().is_some_and(|fields| {
        fields.iter().any(|f| {
            !matches!(
                f.role,
                Some(ParameterRole::ResponseControl)
                    | Some(ParameterRole::Sort)
                    | Some(ParameterRole::SortDirection)
            )
        })
    })
}

/// Derive which eval form buckets this CGS can meaningfully exercise.
pub fn required_form_buckets(cgs: &CGS) -> HashMap<EvalFormId, String> {
    let mut out: HashMap<EvalFormId, String> = HashMap::new();

    let mut has_query = false;
    let mut has_query_unfiltered = false;
    let mut has_query_filter = false;
    let mut has_search = false;
    let mut has_get = false;
    let mut has_create = false;
    let mut has_update = false;
    let mut has_delete = false;
    let mut has_action = false;

    for cap in cgs.capabilities.values() {
        match cap.kind {
            CapabilityKind::Query => {
                has_query = true;
                if query_has_truly_unfiltered_list_surface(cap) {
                    has_query_unfiltered = true;
                }
                if query_has_predicate_narrowing_params(cap) {
                    has_query_filter = true;
                }
            }
            CapabilityKind::Search => has_search = true,
            CapabilityKind::Get => has_get = true,
            CapabilityKind::Create => has_create = true,
            CapabilityKind::Update => has_update = true,
            CapabilityKind::Delete => has_delete = true,
            CapabilityKind::Action => has_action = true,
        }
    }

    if has_query_unfiltered {
        out.insert(
            EvalFormId::QueryAll,
            "schema declares at least one Query with no domain parameters and no mapping pagination (unfiltered list)"
                .into(),
        );
    }
    if has_query_filter {
        out.insert(
            EvalFormId::QueryFiltered,
            "at least one Query capability exposes scope/filter/search parameters".into(),
        );
    }
    if has_search {
        out.insert(
            EvalFormId::SearchText,
            "schema declares a Search capability".into(),
        );
    }
    if has_get {
        out.insert(
            EvalFormId::Get,
            "schema declares at least one Get capability".into(),
        );
        out.insert(
            EvalFormId::Projection,
            "Get/Query surface allows `[field,…]` projection in path expressions".into(),
        );
    } else if has_query {
        out.insert(
            EvalFormId::Projection,
            "Query surface allows `[field,…]` projection in path expressions".into(),
        );
    }

    let mut has_paginated_query = false;
    for cap in cgs.capabilities.values() {
        if matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search)
            && mapping_declares_pagination(cap)
        {
            has_paginated_query = true;
            break;
        }
    }
    if has_paginated_query {
        out.insert(
            EvalFormId::PageNext,
            "schema declares pagination on at least one list capability — cover `page(pg#)` when results truncate"
                .into(),
        );
    }

    let mut has_entity_ref = false;
    for ent in cgs.entities.values() {
        for f in ent.fields.values() {
            if cgs
                .named_value_for_slot(f)
                .ok()
                .is_some_and(|nv| matches!(nv.field_type, plasm_core::FieldType::EntityRef { .. }))
            {
                has_entity_ref = true;
                break;
            }
        }
        if !ent.relations.is_empty() {
            has_entity_ref = true;
        }
        if has_entity_ref {
            break;
        }
    }
    if has_entity_ref {
        out.insert(
            EvalFormId::Chain,
            "at least one entity has EntityRef fields or relations for forward navigation".into(),
        );
    }

    let mut has_reverse = false;
    for ent_name in cgs.entities.keys() {
        if !cgs
            .find_reverse_traversal_caps(ent_name.as_str())
            .is_empty()
        {
            has_reverse = true;
            break;
        }
    }
    if has_reverse {
        out.insert(
            EvalFormId::Reverse,
            "at least one reverse query capability scopes by EntityRef to another entity".into(),
        );
    }

    if has_create {
        out.insert(
            EvalFormId::Create,
            "schema declares Create capability".into(),
        );
    }
    if has_update {
        out.insert(
            EvalFormId::Update,
            "schema declares Update capability".into(),
        );
    }
    if has_delete {
        out.insert(
            EvalFormId::Delete,
            "schema declares Delete capability".into(),
        );
    }
    if has_action {
        out.insert(
            EvalFormId::Invoke,
            "schema declares Action capability (invoke-style)".into(),
        );
    }

    if cgs.entities.len() >= 2 {
        out.insert(
            EvalFormId::MultiStep,
            "multiple entity types — exercise at least one multi-step NL goal".into(),
        );
    }

    out
}

/// Apply optional `coverage.yaml` `required_extra` overrides.
pub fn apply_coverage_override(
    mut base: HashMap<EvalFormId, String>,
    o: &CoverageOverride,
) -> anyhow::Result<HashMap<EvalFormId, String>> {
    for ex in &o.required_extra {
        let Some(id) = EvalFormId::parse(ex) else {
            anyhow::bail!("unknown required_extra form {:?}", ex);
        };
        base.entry(id)
            .or_insert_with(|| "from eval/coverage override".into());
    }
    Ok(base)
}

/// Entity names that must appear in eval coverage: the union of [`CapabilitySchema::domain`]
/// across all capabilities (each distinct resource the schema can operate on).
pub fn required_domain_entities(cgs: &CGS) -> HashSet<String> {
    cgs.capabilities
        .values()
        .map(|c| c.domain.to_string())
        .collect()
}

/// `expect.entities_any` entries must be valid CGS entity names.
pub fn validate_case_entities_against_schema(
    cases: &[crate::EvalCase],
    schema_key: &str,
    cgs: &CGS,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    for c in cases {
        if c.schema != schema_key {
            continue;
        }
        for e in &c.expect.entities_any {
            if !cgs.entities.contains_key(e.as_str()) {
                errors.push(format!(
                    "case {}: unknown entity {:?} in expect.entities_any (not in CGS)",
                    c.id, e
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        errors.sort();
        anyhow::bail!("invalid expect.entities_any:\n{}", errors.join("\n"))
    }
}

/// Union of domain entities covered by eval cases: `expect.entities_any`, optionally merged with
/// [`crate::entities_from_reference_expr`] per [`CoversSource`] (same rules as `covers`).
pub fn union_case_entities(
    cases: &[crate::EvalCase],
    schema_key: &str,
    cgs: &CGS,
    source: CoversSource,
) -> anyhow::Result<UnionCaseEntitiesResult> {
    let mut union = HashSet::new();
    let mut by_case: HashMap<String, Vec<String>> = HashMap::new();

    for c in cases {
        if c.schema != schema_key {
            continue;
        }
        let row_set: HashSet<String> = match source {
            CoversSource::Yaml => c.expect.entities_any.iter().cloned().collect(),
            CoversSource::Reference | CoversSource::Merge => {
                let from_yaml: HashSet<String> = c.expect.entities_any.iter().cloned().collect();
                if let Some(ref re) = c.reference_expr {
                    let re = re.trim();
                    if !re.is_empty() {
                        let derived = crate::entities_from_reference_expr(re, cgs)
                            .map_err(|e| anyhow::anyhow!("case {} reference_expr: {}", c.id, e))?;
                        if matches!(source, CoversSource::Reference) {
                            derived
                        } else {
                            from_yaml.union(&derived).cloned().collect()
                        }
                    } else {
                        from_yaml
                    }
                } else {
                    from_yaml
                }
            }
        };
        for e in &row_set {
            union.insert(e.clone());
        }
        let mut row: Vec<String> = row_set.into_iter().collect();
        row.sort();
        by_case.insert(c.id.clone(), row);
    }

    Ok((union, by_case))
}

/// How `plasm-eval coverage` builds per-case `covers` when combining YAML with
/// [`EvalCase::reference_expr`](crate::EvalCase::reference_expr).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CoversSource {
    /// Use only `covers:` from YAML (default).
    #[default]
    Yaml,
    /// When `reference_expr` is set, replace `covers` with the derived set; otherwise keep YAML.
    Reference,
    /// Union YAML `covers` with the derived set from `reference_expr` when present.
    Merge,
}

/// Derive [`EvalFormId`] buckets from a static parse of `reference_expr` against `cgs`.
///
/// Walks the [`Expr`] IR plus optional trailing `[a,b]` projection from [`expr_parser::parse`].
pub fn derive_eval_form_ids_from_reference(
    reference_expr: &str,
    cgs: &CGS,
) -> Result<HashSet<EvalFormId>, expr_parser::ParseError> {
    let pe = expr_parser::parse(reference_expr, cgs)?;
    let mut out = HashSet::new();
    collect_expr_forms(&pe.expr, cgs, &mut out);
    if let Some(proj) = &pe.projection {
        if !proj.is_empty() {
            out.insert(EvalFormId::Projection);
        }
    }
    Ok(out)
}

fn collect_expr_forms(expr: &Expr, cgs: &CGS, out: &mut HashSet<EvalFormId>) {
    match expr {
        Expr::Query(q) => {
            if query_expr_is_search(q, cgs) {
                out.insert(EvalFormId::SearchText);
            } else {
                if predicate_mentions_source_placeholder(&q.predicate) {
                    out.insert(EvalFormId::Reverse);
                }
                match &q.predicate {
                    None | Some(Predicate::True) => {
                        out.insert(EvalFormId::QueryAll);
                    }
                    Some(_) => {
                        out.insert(EvalFormId::QueryFiltered);
                    }
                }
            }
            if q.projection.as_ref().is_some_and(|p| !p.is_empty()) {
                out.insert(EvalFormId::Projection);
            }
        }
        Expr::Get(_) => {
            out.insert(EvalFormId::Get);
        }
        Expr::Create(_) => {
            out.insert(EvalFormId::Create);
        }
        Expr::Delete(_) => {
            out.insert(EvalFormId::Delete);
        }
        Expr::Invoke(inv) => {
            if let Some(cap) = cgs.get_capability(&inv.capability) {
                if matches!(cap.kind, CapabilityKind::Update) {
                    out.insert(EvalFormId::Update);
                } else {
                    out.insert(EvalFormId::Invoke);
                }
            } else {
                out.insert(EvalFormId::Invoke);
            }
        }
        Expr::Chain(c) => {
            out.insert(EvalFormId::Chain);
            if cgs.entities.len() >= 2 {
                out.insert(EvalFormId::MultiStep);
            }
            collect_expr_forms(&c.source, cgs, out);
            if let ChainStep::Explicit { expr: inner } = &c.step {
                collect_expr_forms(inner, cgs, out);
            }
        }
        Expr::Page(_) => {
            out.insert(EvalFormId::PageNext);
        }
        Expr::Wait(_) | Expr::Cancel(_) => {}
        Expr::TeachingValue { .. } => {}
    }
}

fn query_expr_is_search(q: &plasm_core::expr::QueryExpr, cgs: &CGS) -> bool {
    q.capability_name
        .as_ref()
        .and_then(|n| cgs.get_capability(n))
        .is_some_and(|c| c.kind == CapabilityKind::Search)
}

fn predicate_mentions_source_placeholder(pred: &Option<Predicate>) -> bool {
    pred.as_ref()
        .is_some_and(predicate_contains_source_placeholder)
}

fn predicate_contains_source_placeholder(p: &Predicate) -> bool {
    match p {
        Predicate::Comparison { value, .. } => {
            value.to_value() == Value::String("__source_id__".into())
        }
        Predicate::And { args } | Predicate::Or { args } => {
            args.iter().any(predicate_contains_source_placeholder)
        }
        Predicate::Not { predicate } => predicate_contains_source_placeholder(predicate),
        Predicate::ExistsRelation { predicate, .. } => predicate
            .as_ref()
            .is_some_and(|b| predicate_contains_source_placeholder(b)),
        Predicate::True | Predicate::False => false,
    }
}

/// Build an effective case list for coverage: optionally merge or replace `covers` from
/// [`derive_eval_form_ids_from_reference`].
pub fn cases_with_effective_covers(
    cases: &[crate::EvalCase],
    cgs: &CGS,
    source: CoversSource,
) -> anyhow::Result<Vec<crate::EvalCase>> {
    match source {
        CoversSource::Yaml => Ok(cases.to_vec()),
        CoversSource::Reference | CoversSource::Merge => {
            let mut out = Vec::with_capacity(cases.len());
            for c in cases {
                let mut row = c.clone();
                if let Some(ref re) = c.reference_expr {
                    let re = re.trim();
                    if !re.is_empty() {
                        let derived = derive_eval_form_ids_from_reference(re, cgs)
                            .map_err(|e| anyhow::anyhow!("case {} reference_expr: {}", c.id, e))?;
                        if matches!(source, CoversSource::Reference) {
                            row.covers = sorted_form_id_strings(&derived);
                        } else {
                            let mut merged: HashSet<EvalFormId> = c
                                .covers
                                .iter()
                                .filter_map(|s| EvalFormId::parse(s))
                                .collect();
                            merged.extend(derived);
                            row.covers = sorted_form_id_strings(&merged);
                        }
                    }
                }
                out.push(row);
            }
            Ok(out)
        }
    }
}

fn sorted_form_id_strings(ids: &HashSet<EvalFormId>) -> Vec<String> {
    let mut v: Vec<EvalFormId> = ids.iter().copied().collect();
    v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    v.into_iter().map(|id| id.as_str().to_string()).collect()
}

/// Compare YAML `covers` to [`derive_eval_form_ids_from_reference`] for every case that has
/// `reference_expr`. Fails if any mismatch is found (see `allow_extra_claims`).
pub fn compare_case_covers_to_derived(
    cases: &[crate::EvalCase],
    schema_key: &str,
    cgs: &CGS,
    allow_extra_claims: bool,
) -> anyhow::Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for c in cases {
        if c.schema != schema_key {
            continue;
        }
        let Some(ref re) = c.reference_expr else {
            continue;
        };
        let re = re.trim();
        if re.is_empty() {
            continue;
        }
        let derived = derive_eval_form_ids_from_reference(re, cgs)
            .map_err(|e| anyhow::anyhow!("case {} reference_expr: {}", c.id, e))?;
        let claimed: HashSet<EvalFormId> = c
            .covers
            .iter()
            .filter_map(|s| EvalFormId::parse(s))
            .collect();
        let ok = if allow_extra_claims {
            derived.is_subset(&claimed)
        } else {
            derived == claimed
        };
        if !ok {
            failures.push(format!(
                "case {}: derived [{}] vs claimed [{}] (subset_ok={})",
                c.id,
                format_form_set(&derived),
                format_form_set(&claimed),
                allow_extra_claims
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        failures.sort();
        anyhow::bail!(
            "reference_expr vs covers mismatch:\n{}",
            failures.join("\n")
        )
    }
}

fn format_form_set(s: &HashSet<EvalFormId>) -> String {
    let mut v: Vec<_> = s.iter().map(|x| x.as_str()).collect();
    v.sort();
    v.join(", ")
}

/// Every `covers` token must be a valid [`EvalFormId`] and must appear in `allowed`
/// (the CGS-derived required set, after optional `coverage.yaml` `required_extra` only).
///
/// Call this after `required` is final; pass `allowed` = `required.keys().copied().collect()`.
pub fn validate_case_covers_against_allowed(
    cases: &[crate::EvalCase],
    schema_key: &str,
    allowed: &HashSet<EvalFormId>,
) -> anyhow::Result<()> {
    let mut errors: Vec<String> = Vec::new();
    for c in cases {
        if c.schema != schema_key {
            continue;
        }
        for s in &c.covers {
            match EvalFormId::parse(s) {
                None => errors.push(format!(
                    "case {}: unknown covers token {:?}",
                    c.id, s
                )),
                Some(id) if !allowed.contains(&id) => errors.push(format!(
                    "case {}: covers token {:?} is not in the CGS-derived allowed set for this schema",
                    c.id, s
                )),
                Some(_) => {}
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        errors.sort();
        anyhow::bail!("invalid covers:\n{}", errors.join("\n"))
    }
}

/// Union of `covers` over cases for this schema, plus optional reference_expr classification.
pub fn union_case_covers(
    cases: &[crate::EvalCase],
    schema_key: &str,
) -> (HashSet<EvalFormId>, HashMap<String, Vec<EvalFormId>>) {
    let mut union = HashSet::new();
    let mut by_case: HashMap<String, Vec<EvalFormId>> = HashMap::new();

    for c in cases {
        if c.schema != schema_key {
            continue;
        }
        let mut row = Vec::new();
        for s in &c.covers {
            if let Some(id) = EvalFormId::parse(s) {
                union.insert(id);
                row.push(id);
            }
        }
        by_case.insert(c.id.clone(), row);
    }

    (union, by_case)
}

/// Full coverage check result for reporting.
#[derive(Debug, serde::Serialize)]
pub struct CoverageReport {
    pub schema: String,
    pub required: Vec<RequiredFormRow>,
    pub satisfied: Vec<String>,
    pub missing: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub orphan_covers: Vec<String>,
    pub by_case: Vec<CaseCoverRow>,
    /// Capability domain entities that must appear in at least one case.
    pub required_entities: Vec<String>,
    /// Required entities that appear in the union of case coverage.
    pub entities_satisfied: Vec<String>,
    /// Required entities with no eval case listing them (nor derived from `reference_expr`).
    pub entities_missing: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub orphan_entities: Vec<String>,
    pub by_case_entities: Vec<CaseEntityRow>,
    pub ok: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct RequiredFormRow {
    pub id: String,
    pub rationale: String,
}

#[derive(Debug, serde::Serialize)]
pub struct CaseCoverRow {
    pub id: String,
    pub covers: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CaseEntityRow {
    pub id: String,
    pub entities: Vec<String>,
}

pub fn build_coverage_report(
    schema_key: &str,
    required: &HashMap<EvalFormId, String>,
    union: &HashSet<EvalFormId>,
    by_case: &HashMap<String, Vec<EvalFormId>>,
    required_entities: &HashSet<String>,
    entity_union: &HashSet<String>,
    by_case_entities: &HashMap<String, Vec<String>>,
) -> CoverageReport {
    let mut req_rows: Vec<RequiredFormRow> = required
        .iter()
        .map(|(k, v)| RequiredFormRow {
            id: k.as_str().to_string(),
            rationale: v.clone(),
        })
        .collect();
    req_rows.sort_by(|a, b| a.id.cmp(&b.id));

    let mut satisfied: Vec<String> = union.iter().map(|f| f.as_str().to_string()).collect();
    satisfied.sort();

    let mut missing: Vec<String> = required
        .keys()
        .filter(|k| !union.contains(k))
        .map(|k| k.as_str().to_string())
        .collect();
    missing.sort();
    let ok = missing.is_empty();

    let req_set: HashSet<EvalFormId> = required.keys().copied().collect();
    let mut orphan_covers: Vec<String> = union
        .difference(&req_set)
        .map(|f| f.as_str().to_string())
        .collect();
    orphan_covers.sort();

    let mut case_rows: Vec<CaseCoverRow> = by_case
        .iter()
        .map(|(id, covers)| CaseCoverRow {
            id: id.clone(),
            covers: covers.iter().map(|f| f.as_str().to_string()).collect(),
        })
        .collect();
    case_rows.sort_by(|a, b| a.id.cmp(&b.id));

    let mut req_ent: Vec<String> = required_entities.iter().cloned().collect();
    req_ent.sort();

    let mut entities_satisfied: Vec<String> = required_entities
        .intersection(entity_union)
        .cloned()
        .collect();
    entities_satisfied.sort();

    let mut entities_missing: Vec<String> = required_entities
        .difference(entity_union)
        .cloned()
        .collect();
    entities_missing.sort();

    let mut orphan_entities: Vec<String> = entity_union
        .difference(required_entities)
        .cloned()
        .collect();
    orphan_entities.sort();

    let mut entity_rows: Vec<CaseEntityRow> = by_case_entities
        .iter()
        .map(|(id, entities)| CaseEntityRow {
            id: id.clone(),
            entities: entities.clone(),
        })
        .collect();
    entity_rows.sort_by(|a, b| a.id.cmp(&b.id));

    let entities_ok = entities_missing.is_empty();
    let ok = ok && entities_ok;

    CoverageReport {
        schema: schema_key.to_string(),
        required: req_rows,
        satisfied,
        missing,
        orphan_covers,
        by_case: case_rows,
        required_entities: req_ent,
        entities_satisfied,
        entities_missing,
        orphan_entities,
        by_case_entities: entity_rows,
        ok,
    }
}

pub fn print_coverage_text(report: &CoverageReport) {
    println!("## Eval coverage: {}", report.schema);
    println!();
    println!("### Required (from CGS + optional override)");
    for r in &report.required {
        println!("- **{}** — {}", r.id, r.rationale);
    }
    println!();
    println!("### Satisfied by eval cases (union of `covers`)");
    if report.satisfied.is_empty() {
        println!("(none)");
    } else {
        for s in &report.satisfied {
            println!("- {}", s);
        }
    }
    println!();
    println!("### Missing");
    if report.missing.is_empty() {
        println!("(none — full coverage)");
    } else {
        for m in &report.missing {
            println!(
                "- **{}** — add `covers` on a case or adjust eval/coverage",
                m
            );
        }
    }
    if !report.orphan_covers.is_empty() {
        println!();
        println!("### Orphan covers (not in required set)");
        for o in &report.orphan_covers {
            println!("- {}", o);
        }
    }
    println!();
    println!("### By case (`covers`)");
    for c in &report.by_case {
        let cov = if c.covers.is_empty() {
            "(no covers)".to_string()
        } else {
            c.covers.join(", ")
        };
        println!("- **{}**: {}", c.id, cov);
    }
    println!();
    println!("### Required entities (capability domains — union of `CapabilitySchema.domain`)");
    if report.required_entities.is_empty() {
        println!("(none)");
    } else {
        for e in &report.required_entities {
            println!("- **{}**", e);
        }
    }
    println!();
    println!(
        "### Satisfied entity coverage (union of case `expect.entities_any` / `reference_expr`)"
    );
    if report.entities_satisfied.is_empty() {
        println!("(none)");
    } else {
        for e in &report.entities_satisfied {
            println!("- {}", e);
        }
    }
    println!();
    println!("### Missing entities");
    if report.entities_missing.is_empty() {
        println!("(none — every required entity appears in at least one case).");
    } else {
        for m in &report.entities_missing {
            println!(
                "- **{}** — add `expect.entities_any` (or `reference_expr` with `--covers-source merge`)",
                m
            );
        }
    }
    if !report.orphan_entities.is_empty() {
        println!();
        println!("### Orphan entities (in eval cases but not required)");
        for o in &report.orphan_entities {
            println!("- {}", o);
        }
    }
    println!();
    println!("### By case (`entities`)");
    for c in &report.by_case_entities {
        let ent = if c.entities.is_empty() {
            "(no entities)".to_string()
        } else {
            c.entities.join(", ")
        };
        println!("- **{}**: {}", c.id, ent);
    }
    println!();
    if report.ok {
        println!(
            "Status: **OK** (all required forms and entities have at least one covering case)."
        );
    } else {
        let mut parts = Vec::new();
        if !report.missing.is_empty() {
            parts.push(format!("{} missing form bucket(s)", report.missing.len()));
        }
        if !report.entities_missing.is_empty() {
            parts.push(format!(
                "{} missing entit(y/ies)",
                report.entities_missing.len()
            ));
        }
        println!("Status: **INCOMPLETE** — {}.", parts.join("; "));
    }
}

/// Starter `cases.yaml` body: bucket comments from [`required_form_buckets`] plus one example case.
pub fn scaffold_cases_yaml(cgs: &CGS, schema_key: &str) -> String {
    let required = required_form_buckets(cgs);
    let mut bucket_list: Vec<_> = required.keys().copied().collect();
    bucket_list.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    let mut out = String::new();
    out.push_str(&format!(
        "# Eval cases scaffold for `{schema_key}` — edit goals and expect: blocks.\n"
    ));
    out.push_str("# CGS-derived buckets (see `plasm-eval coverage`):\n");
    for id in bucket_list.iter() {
        if let Some(r) = required.get(id) {
            out.push_str(&format!("# - {} — {}\n", id.as_str(), r));
        }
    }
    let mut ent_list: Vec<_> = required_domain_entities(cgs).into_iter().collect();
    ent_list.sort();
    out.push_str("# Capability domain entities — each should appear in `expect.entities_any` on at least one case:\n");
    for e in ent_list {
        out.push_str(&format!("# - {}\n", e));
    }
    out.push('\n');
    let cover_yaml = if bucket_list.is_empty() {
        "[]".to_string()
    } else {
        let take = bucket_list.len().min(2);
        let parts: Vec<&str> = bucket_list[..take].iter().map(|e| e.as_str()).collect();
        format!("[{}]", parts.join(", "))
    };
    out.push_str(&format!(
        r#"- id: {schema_key}-cover-example
  schema: {schema_key}
  goal: "TODO: one NL goal per bucket you need to cover"
  covers: {cover_yaml}
  tags: [scaffold]
  expect:
    entities_any: []
"#
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;
    use std::path::Path;

    #[test]
    fn petstore_derives_query_filtered_and_get() {
        let dir = Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let m = required_form_buckets(&cgs);
        assert!(!m.contains_key(&EvalFormId::QueryAll));
        assert!(m.contains_key(&EvalFormId::QueryFiltered));
        assert!(m.contains_key(&EvalFormId::Get));
    }

    #[test]
    fn derive_reference_expr_petstore_get_query_and_projection() {
        let dir = Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let g = derive_eval_form_ids_from_reference("Pet(3)", &cgs).unwrap();
        assert!(g.contains(&EvalFormId::Get));
        let q = derive_eval_form_ids_from_reference("Pet", &cgs).unwrap();
        assert!(q.contains(&EvalFormId::QueryAll));
        let qf = derive_eval_form_ids_from_reference("Pet{status=available}", &cgs).unwrap();
        assert!(qf.contains(&EvalFormId::QueryFiltered));
        let gp = derive_eval_form_ids_from_reference("Pet(1)[name,status]", &cgs).unwrap();
        assert!(gp.contains(&EvalFormId::Get));
        assert!(gp.contains(&EvalFormId::Projection));
    }

    #[test]
    fn notion_derives_query_filtered_without_query_all() {
        let dir = Path::new("../../apis/notion");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let m = required_form_buckets(&cgs);
        assert!(!m.contains_key(&EvalFormId::QueryAll));
        assert!(m.contains_key(&EvalFormId::QueryFiltered));
    }

    #[test]
    fn scaffold_cases_yaml_includes_schema_and_example() {
        let dir = Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let s = scaffold_cases_yaml(&cgs, "petstore");
        assert!(s.contains("schema: petstore"));
        assert!(s.contains("petstore-cover-example"));
        assert!(s.contains("# CGS-derived buckets"));
        assert!(s.contains("# Capability domain entities"));
    }

    #[test]
    fn validate_covers_unknown_token_fails() {
        let cases = vec![crate::EvalCase {
            id: "bad".into(),
            schema: "x".into(),
            goal: "g".into(),
            tags: vec![],
            covers: vec!["not_a_real_bucket".into()],
            reference_expr: None,
            expect: crate::ExpectBlock::default(),
        }];
        let allowed = HashSet::from([EvalFormId::Get]);
        let e = validate_case_covers_against_allowed(&cases, "x", &allowed).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("unknown covers token"));
    }

    #[test]
    fn validate_covers_out_of_range_fails() {
        let cases = vec![crate::EvalCase {
            id: "bad".into(),
            schema: "x".into(),
            goal: "g".into(),
            tags: vec![],
            covers: vec!["get".into()],
            reference_expr: None,
            expect: crate::ExpectBlock::default(),
        }];
        let allowed = HashSet::from([EvalFormId::QueryAll]);
        let e = validate_case_covers_against_allowed(&cases, "x", &allowed).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("not in the CGS-derived allowed set"));
    }

    #[test]
    fn validate_covers_accepts_subset_of_allowed() {
        let cases = vec![crate::EvalCase {
            id: "ok".into(),
            schema: "x".into(),
            goal: "g".into(),
            tags: vec![],
            covers: vec!["get".into(), "query_all".into()],
            reference_expr: None,
            expect: crate::ExpectBlock::default(),
        }];
        let allowed = HashSet::from([EvalFormId::Get, EvalFormId::QueryAll]);
        validate_case_covers_against_allowed(&cases, "x", &allowed).unwrap();
    }
}
