//! Resolve which **query** capability backs a [`QueryExpr`] when `capability_name` is unset.
//!
//! **Query and Search are structurally distinct.** The parser sets `capability_name` on
//! `Entity~"text"` (Search) at parse time; CLI dispatch stamps it on the `"search"` verb.
//! This module only resolves **Query** capabilities — Search never reaches the fallback path.

use std::collections::HashSet;

use thiserror::Error;

use crate::expr::QueryExpr;
use crate::schema::{capability_is_zero_arity_invoke, CapabilityKind, CapabilitySchema, CGS};

/// Failure to pick exactly one query/search capability for a [`QueryExpr`].
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum QueryCapabilityResolveError {
    #[error("capability '{capability}' not found for entity '{entity}'")]
    CapabilityNotFound { capability: String, entity: String },
    #[error(
        "ambiguous query for entity '{entity}': predicate matches more than one capability ({names})"
    )]
    Ambiguous { entity: String, names: String },
    #[error("no query capability matches for entity '{entity}': {message}")]
    NoMatchingCapability { entity: String, message: String },
    #[error("rowset normalize for entity '{entity}': {message}")]
    RowsetNormalize { entity: String, message: String },
}

/// Required parent-scope parameter names for `cap`, in stable order.
pub fn required_scope_param_names(cap: &CapabilitySchema) -> Vec<String> {
    let mut names: Vec<String> = cap
        .scope_params()
        .iter()
        .filter(|f| f.required)
        .map(|f| f.name.to_string())
        .collect();
    names.sort();
    names
}

fn scoped_query_caps<'a>(cgs: &'a CGS, entity: &str) -> Vec<&'a CapabilitySchema> {
    let mut v: Vec<_> = cgs
        .find_capabilities(entity, CapabilityKind::Query)
        .into_iter()
        .filter(|c| c.has_required_scope_param())
        .collect();
    v.sort_by_key(|c| c.name.as_str());
    v
}

/// True when `pred_fields` names every required scope parameter of at least one scoped Query capability.
/// Used so we do not pick [`CGS::primary_query_capability`] when the predicate already selects a scoped query
/// (e.g. `User{issueIdOrKey=…}` → `issue_watcher_query` vs primary `user_myself`).
fn predicate_selects_scoped_query(cgs: &CGS, entity: &str, pred_fields: &HashSet<String>) -> bool {
    if pred_fields.is_empty() {
        return false;
    }
    for cap in scoped_query_caps(cgs, entity) {
        let req = required_scope_param_names(cap);
        if req.is_empty() {
            continue;
        }
        if req.iter().all(|s| pred_fields.contains(s)) {
            return true;
        }
    }
    false
}

/// Required scope **and** required filter-like parameter names for a query capability (stable,
/// deduplicated). Used so scoped queries like `Message{channel=…, ts=…}` match `channel_replies`
/// (needs both) while `Message{channel=…}` matches only `channel_history`.
fn required_predicate_field_names_for_scoped_match(cap: &CapabilitySchema) -> Vec<String> {
    let mut names = required_scope_param_names(cap);
    names.extend(required_filter_like_param_names(cap));
    names.sort();
    names.dedup();
    names
}

/// Required non-scope, filter-like parameter names for a query capability (stable order).
/// When an entity has only [`CapabilityKind::Search`] (e.g. Linear `Issue`), brace filters
/// `Issue{team_key=ENG}` must resolve to `issue_search`, not fail with "no query capability".
fn try_resolve_search_for_filter_query<'a>(
    query: &'a QueryExpr,
    cgs: &'a CGS,
) -> Option<&'a CapabilitySchema> {
    if query.capability_name.is_some() {
        return None;
    }
    let pred_fields: HashSet<String> = query
        .predicate
        .as_ref()
        .map(|p| p.referenced_fields().into_iter().collect())
        .unwrap_or_default();
    if pred_fields.is_empty() {
        return None;
    }
    let mut search_caps: Vec<_> = cgs
        .find_capabilities(&query.entity, CapabilityKind::Search)
        .into_iter()
        .collect();
    if search_caps.is_empty() {
        return None;
    }
    search_caps.sort_by_key(|c| c.name.as_str());
    let mut matching: Vec<&CapabilitySchema> = Vec::new();
    for cap in &search_caps {
        let names: HashSet<String> = cap
            .query_surface_fields()
            .map(|f| f.name.to_string())
            .collect();
        if pred_fields.iter().all(|f| names.contains(f)) {
            matching.push(*cap);
        }
    }
    match matching.len() {
        0 if search_caps.len() == 1 => Some(search_caps[0]),
        1 => Some(matching[0]),
        _ => None,
    }
}

fn required_filter_like_param_names(cap: &CapabilitySchema) -> Vec<String> {
    let mut names: Vec<String> = cap
        .selection_params()
        .iter()
        .filter(|f| f.required)
        .map(|f| f.name.to_string())
        .collect();
    names.sort();
    names
}

/// Exactly one pathless zero-arity Get on `entity`, and every Get on that entity is such a Get;
/// no Query/Search. Shared by teaching and bare-`e#` normalize.
pub fn sole_nullary_singleton_get<'a>(cgs: &'a CGS, entity: &str) -> Option<&'a CapabilitySchema> {
    if !cgs
        .find_capabilities(entity, CapabilityKind::Query)
        .is_empty()
    {
        return None;
    }
    if !cgs
        .find_capabilities(entity, CapabilityKind::Search)
        .is_empty()
    {
        return None;
    }
    let get_caps: Vec<_> = cgs.find_capabilities(entity, CapabilityKind::Get);
    if get_caps.is_empty() {
        return None;
    }
    let mut singleton: Vec<_> = get_caps
        .iter()
        .copied()
        .filter(|c| {
            !c.domain_exemplar_requires_entity_anchor()
                && capability_is_zero_arity_invoke(c)
                && !c.get_requires_identity_anchor(cgs)
        })
        .collect();
    if singleton.len() != get_caps.len() || singleton.len() != 1 {
        return None;
    }
    singleton.sort_by_key(|c| c.name.as_str());
    Some(singleton[0])
}

/// When a bare entity head (`Entity` / `e#`, no predicate) qualifies, return that sole Get.
pub fn sole_nullary_singleton_get_for_bare_query<'a>(
    query: &QueryExpr,
    cgs: &'a CGS,
) -> Option<&'a CapabilitySchema> {
    if query.capability_name.is_some() || query.predicate.is_some() {
        return None;
    }
    sole_nullary_singleton_get(cgs, query.entity.as_str())
}

/// Rewrite a bare query to a pathless nullary [`Expr::Get`] (empty identity, stamped capability).
pub fn rewrite_bare_query_to_sole_get(
    query: &QueryExpr,
    get_cap: &CapabilitySchema,
) -> crate::Expr {
    let mut g = crate::expr::GetExpr::pathless_nullary(query.entity.clone());
    g.capability_name = Some(get_cap.name.clone());
    g.catalog_entry_id = query.catalog_entry_id.clone();
    crate::Expr::Get(g)
}

fn normalize_query_arm(
    expr: &mut crate::Expr,
    cgs: &CGS,
) -> Result<(), QueryCapabilityResolveError> {
    let crate::Expr::Query(q) = expr else {
        return Ok(());
    };
    if q.capability_name.is_none() {
        match resolve_query_capability(q, cgs) {
            Ok(cap) => {
                q.capability_name = Some(cap.name.clone());
            }
            Err(err) => {
                let Some(get_cap) = sole_nullary_singleton_get_for_bare_query(q, cgs) else {
                    return Err(err);
                };
                *expr = rewrite_bare_query_to_sole_get(q, get_cap);
                return Ok(());
            }
        }
    }

    // Capability stamped — enforce the ResolvedRowset seam (lane checks) before compile.
    let crate::Expr::Query(q) = expr else {
        return Ok(());
    };
    let entry_id = q
        .catalog_entry_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(cgs.entry_id.as_deref())
        .unwrap_or("");
    crate::rowset::normalize_query_expr_to_rowset(q, cgs, entry_id).map_err(|message| {
        QueryCapabilityResolveError::RowsetNormalize {
            entity: q.entity.to_string(),
            message,
        }
    })?;
    Ok(())
}

/// Resolve the **query** capability that executes `query`.
///
/// Search capabilities are **never** selected here — the parser stamps `capability_name`
/// on `Entity~"text"` at parse time, and CLI dispatch stamps it on the `"search"` verb.
/// Both hit the early `capability_name` return and skip this fallback entirely.
pub fn resolve_query_capability<'a>(
    query: &'a QueryExpr,
    cgs: &'a CGS,
) -> Result<&'a CapabilitySchema, QueryCapabilityResolveError> {
    // Explicit capability (set by parser for ~search, CLI dispatch, or prior normalization).
    if let Some(name) = query.capability_name.as_deref() {
        return cgs.get_capability(name).ok_or_else(|| {
            QueryCapabilityResolveError::CapabilityNotFound {
                capability: name.to_string(),
                entity: query.entity.to_string(),
            }
        });
    }

    let pred_fields: HashSet<String> = query
        .predicate
        .as_ref()
        .map(|p| p.referenced_fields().into_iter().collect())
        .unwrap_or_default();

    // Unscoped query with a predicate: prefer the capability whose **required filter**
    // parameters are all named in the predicate (e.g. Pet `tags` → `pet_findByTags` vs
    // `status` → `pet_findByStatus`). Must run **before** [`CGS::primary_query_capability`],
    // which otherwise always picks a single "primary" among several unscoped query caps.
    if !pred_fields.is_empty() {
        let mut matches: Vec<&CapabilitySchema> = Vec::new();
        for cap in cgs.find_capabilities(&query.entity, CapabilityKind::Query) {
            if cap.has_required_scope_param() {
                continue;
            }
            let req = required_filter_like_param_names(cap);
            if req.is_empty() {
                continue;
            }
            if req.iter().all(|n| pred_fields.contains(n)) {
                matches.push(cap);
            }
        }
        if !matches.is_empty() {
            let max_req = matches
                .iter()
                .map(|c| required_filter_like_param_names(c).len())
                .max()
                .unwrap_or(0);
            let mut best: Vec<&CapabilitySchema> = matches
                .iter()
                .copied()
                .filter(|c| required_filter_like_param_names(c).len() == max_req)
                .collect();
            best.sort_by_key(|c| c.name.as_str());
            if best.len() == 1 {
                return Ok(best[0]);
            }
            if let Some(primary) = cgs.primary_query_capability(&query.entity) {
                if let Some(cap) = best.iter().copied().find(|c| c.name == primary.name) {
                    return Ok(cap);
                }
            }
            if best.len() > 1 {
                let mut names: Vec<String> = best.iter().map(|c| c.name.to_string()).collect();
                names.sort();
                return Err(QueryCapabilityResolveError::Ambiguous {
                    entity: query.entity.to_string(),
                    names: names.join(", "),
                });
            }
        }
    }

    // Unscoped primary query — only when the predicate does not already select a scoped query
    // (required scope fields present in the predicate).
    if !predicate_selects_scoped_query(cgs, &query.entity, &pred_fields) {
        if let Some(cap) = cgs.primary_query_capability(&query.entity) {
            return Ok(cap);
        }
    }

    // Scoped query matching: find the scoped Query cap whose required scope param names
    // are all present in the predicate.

    let scoped = scoped_query_caps(cgs, &query.entity);
    let mut candidates: Vec<&CapabilitySchema> = Vec::new();
    for cap in &scoped {
        let req = required_scope_param_names(cap);
        if req.is_empty() {
            continue;
        }
        let req_all = required_predicate_field_names_for_scoped_match(cap);
        if req_all.iter().all(|s| pred_fields.contains(s)) {
            candidates.push(*cap);
        }
    }

    match candidates.len() {
        0 => {
            let all_query = cgs.find_capabilities(&query.entity, CapabilityKind::Query);
            if !all_query.is_empty() {
                let names: Vec<_> = all_query.iter().map(|c| c.name.as_str()).collect();
                return Err(QueryCapabilityResolveError::NoMatchingCapability {
                    entity: query.entity.to_string(),
                    message: format!(
                        "every query capability for this entity requires scope parameters in the predicate; include every required scope field so one query row can match (partial scope is not enough). Available: {}",
                        names.join(", ")
                    ),
                });
            }
            if let Some(cap) = try_resolve_search_for_filter_query(query, cgs) {
                return Ok(cap);
            }
            Err(QueryCapabilityResolveError::NoMatchingCapability {
                entity: query.entity.to_string(),
                message: "no query capability for this entity".to_string(),
            })
        }
        1 => Ok(candidates[0]),
        _ => {
            // Prefer the most specific match: the candidate that requires the largest predicate
            // field set (scope + required filters). This disambiguates e.g. Slack
            // `channel_replies` (channel + ts) vs `channel_history` (channel only) when both could
            // apply, and e.g. issue_comment_query (owner+repo+issue_number) vs repo_comment_query
            // (owner+repo) when scope-count alone matched.
            let req_sizes: Vec<usize> = candidates
                .iter()
                .map(|c| required_predicate_field_names_for_scoped_match(c).len())
                .collect();
            let max_req = *req_sizes.iter().max().unwrap_or(&0);
            let most_specific: Vec<&CapabilitySchema> = candidates
                .iter()
                .zip(req_sizes.iter())
                .filter(|(_, &cnt)| cnt == max_req)
                .map(|(cap, _)| *cap)
                .collect();
            if most_specific.len() == 1 {
                return Ok(most_specific[0]);
            }
            // Second tie-break: largest required *scope* set (original superset behaviour).
            let scope_counts: Vec<usize> = most_specific
                .iter()
                .map(|c| required_scope_param_names(c).len())
                .collect();
            let max_scope = *scope_counts.iter().max().unwrap_or(&0);
            let by_scope: Vec<&CapabilitySchema> = most_specific
                .iter()
                .zip(scope_counts.iter())
                .filter(|(_, &cnt)| cnt == max_scope)
                .map(|(cap, _)| *cap)
                .collect();
            if by_scope.len() == 1 {
                return Ok(by_scope[0]);
            }
            let mut names: Vec<String> = by_scope.iter().map(|c| c.name.to_string()).collect();
            names.sort();
            Err(QueryCapabilityResolveError::Ambiguous {
                entity: query.entity.to_string(),
                names: names.join(", "),
            })
        }
    }
}

/// When inference succeeds and `capability_name` was unset, set it so intent lines and `expr_display` show `cap=…`.
///
/// Bare entity heads with no Query/Search but a sole nullary singleton Get are rewritten to
/// [`Expr::Get`] via [`rewrite_bare_query_to_sole_get`] so `label = eN` is executable.
pub fn normalize_expr_query_capabilities(
    expr: &mut crate::Expr,
    cgs: &CGS,
) -> Result<(), QueryCapabilityResolveError> {
    match expr {
        crate::Expr::Query(_) => normalize_query_arm(expr, cgs),
        crate::Expr::Chain(c) => {
            normalize_expr_query_capabilities(&mut c.source, cgs)?;
            if let crate::ChainStep::Explicit { expr: inner } = &mut c.step {
                normalize_expr_query_capabilities(inner.as_mut(), cgs)?;
            }
            Ok(())
        }
        crate::Expr::Get(_)
        | crate::Expr::Create(_)
        | crate::Expr::Delete(_)
        | crate::Expr::Invoke(_)
        | crate::Expr::Page(_)
        | crate::Expr::Wait(_)
        | crate::Expr::Cancel(_)
        | crate::Expr::TeachingValue { .. } => Ok(()),
    }
}

/// Like [`normalize_expr_query_capabilities`], but resolves the owning [`CGS`] per query domain.
pub fn normalize_expr_query_capabilities_federated(
    expr: &mut crate::Expr,
    fed: &crate::cgs_federation::FederationDispatch,
    fallback: &CGS,
) -> Result<(), QueryCapabilityResolveError> {
    let cgs_for = |entity: &str| {
        fed.resolve_entity(
            entity,
            crate::row_composition::ResolutionHint::default(),
            fallback,
        )
        .unwrap_or(fallback)
    };
    match expr {
        crate::Expr::Query(q) => {
            let cgs = if let Some(eid) = q.catalog_entry_id.as_deref() {
                fed.cgs_for_catalog_entry_id(eid, q.entity.as_str())
                    .ok_or_else(|| QueryCapabilityResolveError::NoMatchingCapability {
                        entity: q.entity.to_string(),
                        message: format!(
                            "catalog `{eid}` is not loaded or does not define `{}`",
                            q.entity
                        ),
                    })?
            } else {
                cgs_for(q.entity.as_str())
            };
            normalize_query_arm(expr, cgs)
        }
        crate::Expr::Chain(c) => {
            normalize_expr_query_capabilities_federated(&mut c.source, fed, fallback)?;
            if let crate::ChainStep::Explicit { expr: inner } = &mut c.step {
                normalize_expr_query_capabilities_federated(inner.as_mut(), fed, fallback)?;
            }
            Ok(())
        }
        crate::Expr::Get(_)
        | crate::Expr::Create(_)
        | crate::Expr::Delete(_)
        | crate::Expr::Invoke(_)
        | crate::Expr::Page(_)
        | crate::Expr::Wait(_)
        | crate::Expr::Cancel(_)
        | crate::Expr::TeachingValue { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::Predicate;

    #[test]
    fn clickup_task_team_id_resolves_task_query() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered("Task", Predicate::eq("team_id", "1"));
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "task_query");
    }

    #[test]
    fn clickup_task_list_id_resolves_list_task_query() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered("Task", Predicate::eq("list_id", "1"));
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "list_task_query");
    }

    #[test]
    fn clickup_task_both_scope_fields_ambiguous() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered(
            "Task",
            Predicate::and(vec![
                Predicate::eq("team_id", "1"),
                Predicate::eq("list_id", "2"),
            ]),
        );
        assert!(matches!(
            resolve_query_capability(&q, &cgs),
            Err(QueryCapabilityResolveError::Ambiguous { .. })
        ));
    }

    #[test]
    fn venmo_payment_request_access_token_resolves_primary_query() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld/venmo");
        if !dir.is_dir() {
            return;
        }
        let mut cgs = load_schema_dir(&dir).unwrap();
        cgs.bind_registry_entry_id("venmo");
        let q = QueryExpr::filtered("PaymentRequest", Predicate::eq("access_token", "tok"));
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "payment_request_query");
    }

    #[test]
    fn normalize_sets_capability_name() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let mut expr =
            crate::Expr::Query(QueryExpr::filtered("Task", Predicate::eq("list_id", "1")));
        normalize_expr_query_capabilities(&mut expr, &cgs).unwrap();
        match &expr {
            crate::Expr::Query(q) => {
                assert_eq!(q.capability_name.as_deref(), Some("list_task_query"));
            }
            _ => panic!("expected query"),
        }
    }

    #[test]
    fn slack_message_channel_only_resolves_channel_history() {
        let dir = std::path::Path::new("../../apis/slack");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered("Message", Predicate::eq("channel", "C1"));
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "channel_history");
    }

    #[test]
    fn slack_message_channel_and_ts_resolves_channel_replies() {
        let dir = std::path::Path::new("../../apis/slack");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered(
            "Message",
            Predicate::and(vec![
                Predicate::eq("channel", "C1"),
                Predicate::eq("ts", "1512085950.000216"),
            ]),
        );
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "channel_replies");
    }

    #[test]
    fn petstore_tags_predicate_resolves_pet_find_by_tags() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered(
            "Pet",
            Predicate::eq(
                "tags",
                crate::Value::Array(vec![
                    crate::Value::String("puppy".into()),
                    crate::Value::String("friendly".into()),
                ]),
            ),
        );
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "pet_findByTags");
    }

    #[test]
    fn jira_user_unscoped_resolves_user_myself() {
        let dir = std::path::Path::new("../../apis/jira");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::all("User");
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "user_myself");
    }

    #[test]
    fn jira_user_issue_key_resolves_issue_watcher_query() {
        let dir = std::path::Path::new("../../apis/jira");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered(
            "User",
            Predicate::eq("issueIdOrKey", crate::Value::String("PROJ-1".into())),
        );
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "issue_watcher_query");
    }

    #[test]
    fn pokeapi_pokemon_encounter_unscoped_is_not_a_global_list() {
        let dir = std::path::Path::new("../../apis/pokeapi");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::all("PokemonEncounter");
        let err = resolve_query_capability(&q, &cgs).unwrap_err();
        assert!(matches!(
            err,
            QueryCapabilityResolveError::NoMatchingCapability { .. }
        ));
    }

    #[test]
    fn linear_issue_brace_filters_resolve_issue_search() {
        let dir = std::path::Path::new("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered(
            "Issue",
            Predicate::and(vec![
                Predicate::eq("team_key", "ENG"),
                Predicate::eq("state_name", "Todo"),
            ]),
        );
        let cap = resolve_query_capability(&q, &cgs).unwrap();
        assert_eq!(cap.name.as_str(), "issue_search");
    }

    #[test]
    fn federated_query_catalog_entry_id_pins_capability_resolution() {
        use crate::cgs_context::CgsContext;
        use crate::symbol_tuning::TeachingExposureSession;
        use indexmap::IndexMap;
        use std::sync::Arc;

        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix");
        if !dir.exists() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(dir).unwrap());
        let mut cgs_github = (*cgs).clone();
        cgs_github.bind_registry_entry_id("github");
        let mut cgs_linear = (*cgs).clone();
        cgs_linear.bind_registry_entry_id("linear");
        let mut by_entry = IndexMap::new();
        by_entry.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", Arc::new(cgs_github.clone()))),
        );
        by_entry.insert(
            "linear".into(),
            Arc::new(CgsContext::entry("linear", Arc::new(cgs_linear.clone()))),
        );
        let mut exp = TeachingExposureSession::new(&cgs_github, "github", &["LangItem"]);
        exp.expose_entities(
            &[&cgs_github, &cgs_linear],
            Arc::new(cgs_linear.clone()),
            "linear",
            &["LangItem"],
        );
        let fed =
            crate::cgs_federation::FederationDispatch::from_contexts_and_exposure(by_entry, &exp);
        let mut expr = crate::Expr::Query(QueryExpr::filtered(
            "LangItem",
            Predicate::eq("owner", "alice"),
        ));
        if let crate::Expr::Query(q) = &mut expr {
            q.catalog_entry_id =
                crate::CatalogEntryStamp::some(crate::RegistryEntryId::from("github"));
        }
        normalize_expr_query_capabilities_federated(&mut expr, &fed, &cgs_github).unwrap();
        match &expr {
            crate::Expr::Query(q) => {
                assert_eq!(q.capability_name.as_deref(), Some("langitem_query"));
                assert_eq!(q.catalog_entry_id.as_deref(), Some("github"));
            }
            _ => panic!("expected query"),
        }
    }

    #[test]
    fn bare_profile_desugars_to_sole_nullary_get() {
        let dir = std::path::Path::new("../../fixtures/schemas/sole_nullary_get");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::all("Profile");
        assert!(
            sole_nullary_singleton_get_for_bare_query(&q, &cgs)
                .is_some_and(|c| c.name.as_str() == "profile_get"),
            "sole nullary get detection"
        );
        let mut expr = crate::Expr::Query(QueryExpr::all("Profile"));
        normalize_expr_query_capabilities(&mut expr, &cgs).unwrap();
        match &expr {
            crate::Expr::Get(g) => {
                assert_eq!(g.reference.entity_type.as_str(), "Profile");
                assert!(
                    g.reference.primary_slot_str().is_empty(),
                    "pathless nullary must not invent identity"
                );
                assert_eq!(g.capability_name.as_deref(), Some("profile_get"));
            }
            other => panic!("expected Get desugar, got {other:?}"),
        }
    }

    /// Inline CGS: resolve stamps capability then `normalize_query_arm` must build a
    /// [`crate::ResolvedRowset`] (RA seam) — no live `apis/` catalog required.
    #[test]
    fn normalize_expr_invokes_resolved_rowset_seam() {
        use crate::schema::{
            registry_test_util, BackendSelectionSchema, CapabilityInputs, CapabilityKind,
            CapabilityMapping, CapabilitySchema, NamedValueSchema, ParentScopeSchema,
            ResourceSchema,
        };
        use crate::FieldType;

        let mut cgs = CGS::new();
        cgs.values.insert(
            "fx_str".into(),
            NamedValueSchema {
                domain: Default::default(),
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                array_items: None,
                currency: None,
            },
        );
        cgs.bind_registry_entry_id("app");
        cgs.add_resource(ResourceSchema {
            name: "Task".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                registry_test_util::entity_field_from_values(&cgs, "fx_str", "id", true, ""),
                registry_test_util::entity_field_from_values(&cgs, "fx_str", "title", false, ""),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        })
        .unwrap();

        cgs.add_capability(CapabilitySchema {
            name: "task_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Task".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": "tasks"},
                        {"type": "var", "name": "id"}
                    ]
                })
                .into(),
            }),
            derived: None,
            inputs: CapabilityInputs::default(),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],
            deterministic: None,
        })
        .unwrap();

        cgs.add_capability(CapabilitySchema {
            name: "list_task_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Task".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "literal", "value": "tasks"}]
                })
                .into(),
            }),
            derived: None,
            inputs: CapabilityInputs {
                scope: ParentScopeSchema(vec![registry_test_util::object_input_field_from_values(
                    &cgs, "fx_str", "list_id", true,
                )]),
                selection: BackendSelectionSchema(vec![
                    registry_test_util::object_input_field_from_values(
                        &cgs, "fx_str", "status", false,
                    ),
                ]),
                ..CapabilityInputs::default()
            },
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            sanitizes: vec![],
            deterministic: None,
        })
        .unwrap();
        cgs.validate().expect("fixture");

        let mut expr = crate::Expr::Query(QueryExpr::filtered(
            "Task",
            Predicate::and(vec![
                Predicate::eq("list_id", "1"),
                Predicate::eq("status", "open"),
            ]),
        ));
        normalize_expr_query_capabilities(&mut expr, &cgs).unwrap();
        let crate::Expr::Query(q) = &expr else {
            panic!("expected Query");
        };
        assert_eq!(q.capability_name.as_deref(), Some("list_task_query"));

        let rowset = crate::normalize_query_expr_to_rowset(q, &cgs, "app").unwrap();
        let slots: Vec<&str> = rowset.selection.0.iter().map(|b| b.slot.as_str()).collect();
        assert_eq!(slots, vec!["list_id", "status"]);
        match &rowset.source {
            crate::RowSource::External {
                capability,
                parent_scope,
                ..
            } => {
                assert_eq!(capability.as_str(), "list_task_query");
                assert_eq!(*parent_scope, crate::ParentScope::Root);
            }
            other => panic!("expected External, got {other:?}"),
        }

        // Entity-row field in braces must fail the seam (RA-2).
        let mut bad = crate::Expr::Query(QueryExpr::filtered("Task", Predicate::eq("title", "x")));
        if let crate::Expr::Query(q) = &mut bad {
            q.capability_name = Some("list_task_query".into());
        }
        let err = normalize_expr_query_capabilities(&mut bad, &cgs).unwrap_err();
        assert!(
            matches!(err, QueryCapabilityResolveError::RowsetNormalize { .. }),
            "expected RowsetNormalize, got {err:?}"
        );
    }
}
