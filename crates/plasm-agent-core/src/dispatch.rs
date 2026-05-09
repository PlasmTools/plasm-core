use clap::ArgMatches;
use indexmap::IndexMap;
use plasm_compile::{
    parse_capability_template, path_var_names_from_request, template_pagination,
    template_var_names, CapabilityTemplate,
};
use plasm_core::{
    CapabilityKind, ChainExpr, CreateExpr, DeleteExpr, EntityDef, EntityKey, Expr, FieldType,
    GetExpr, InvokeExpr, QueryExpr, QueryPagination, Ref, Value, CGS,
};
use plasm_runtime::{ExecuteOptions, ExecutionMode, ExprExecutor, GraphCache, StreamConsumeOpts};
use tracing::Instrument;

use crate::error::AgentError;
use crate::invoke_args::args_to_input;
use crate::output::{format_result_with_cgs, OutputFormat};
use crate::query_args::args_to_query_predicate;
use crate::subcommand_util::{
    field_subcommand_kebab, normalize_cli_token, path_param_arg_id, pluralize_entity,
};

/// Dispatch a parsed CLI invocation through the execution engine.
pub async fn dispatch<E: ExprExecutor>(
    matches: &ArgMatches,
    cgs: &CGS,
    engine: &E,
    cache: &mut GraphCache,
    mode: ExecutionMode,
    output_format: OutputFormat,
) -> Result<(), AgentError> {
    let (entity_name, entity_matches) = matches
        .subcommand()
        .ok_or_else(|| AgentError::Argument("No entity command specified".into()))?;

    let original_entity_name = cgs
        .entities
        .keys()
        .find(|k| k.to_lowercase() == entity_name)
        .ok_or_else(|| AgentError::EntityNotFound(entity_name.into()))?
        .clone();

    let entity = cgs
        .get_entity(&original_entity_name)
        .ok_or_else(|| AgentError::EntityNotFound(original_entity_name.to_string()))?;

    let (expr, consume) = build_expr(entity_matches, &original_entity_name, entity, cgs)?;

    let cli_span = crate::spans::execute_cli_expression(&original_entity_name);
    cli_span.in_scope(|| {
        tracing::trace!(
            expression = %format!("→ {}", crate::expr_display::expr_display(&expr)),
            "execute expression"
        );
    });
    let result = engine
        .execute(
            &expr,
            cgs,
            cache,
            Some(mode),
            consume,
            ExecuteOptions::default(),
        )
        .instrument(cli_span)
        .await?;
    let (formatted, omitted, _fidelity) = format_result_with_cgs(&result, output_format, Some(cgs));
    println!("{}", formatted);
    if !omitted.is_empty() {
        println!("(omitted from summary: {})", omitted.join(", "));
    }

    Ok(())
}

fn build_expr(
    entity_matches: &ArgMatches,
    entity_name: &str,
    entity: &EntityDef,
    cgs: &CGS,
) -> Result<(Expr, StreamConsumeOpts), AgentError> {
    // Check for subcommand first
    if let Some((sub_name, sub_matches)) = entity_matches.subcommand() {
        if sub_name == "query" {
            // When an entity has no unscoped primary query capability the CLI still
            // generates a "query" subcommand for the sole named (scoped) capability
            // (e.g. `comment_query` → subcommand "query" after stripping "comment_"
            // prefix). In that case fall through to the named-capability path below.
            if let Some(query_cap) = cgs.primary_query_capability(entity_name) {
                let predicate = args_to_query_predicate(sub_matches, query_cap, cgs);
                let mut query = match predicate {
                    Some(pred) => QueryExpr::filtered(entity_name, pred),
                    None => QueryExpr::all(entity_name),
                };
                query.capability_name = Some(query_cap.name.clone());
                let consume = attach_query_pagination_if_present(&mut query, sub_matches, cgs);
                if cgs
                    .find_capability(entity_name, CapabilityKind::Get)
                    .is_some()
                    && sub_matches.get_flag("query_summary")
                {
                    query.hydrate = Some(false);
                }
                return Ok((Expr::Query(query), consume));
            }
            // No primary query cap — fall through to named-capability lookup.
        }

        if sub_name == "search" {
            if let Some(search_cap) = cgs.primary_search_capability(entity_name) {
                let predicate = args_to_query_predicate(sub_matches, search_cap, cgs);
                let mut query = match predicate {
                    Some(pred) => QueryExpr::filtered(entity_name, pred),
                    None => QueryExpr::all(entity_name),
                };
                query.capability_name = Some(search_cap.name.clone());
                let consume = attach_query_pagination_if_present(&mut query, sub_matches, cgs);
                return Ok((Expr::Query(query), consume));
            }
            // No primary search cap — fall through.
        }

        // Check if this is a collection-level capability (create or scoped query/search)
        if let Ok(cap) = find_capability(sub_name, entity_name, cgs) {
            // Scoped query/search → build QueryExpr with capability_name
            if matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search) {
                let predicate = args_to_query_predicate(sub_matches, cap, cgs);
                let target_entity = &cap.domain;
                let mut query = match predicate {
                    Some(pred) => QueryExpr::filtered(target_entity, pred),
                    None => QueryExpr::all(target_entity),
                };
                query.capability_name = Some(cap.name.clone());
                let consume = attach_query_pagination_if_present(&mut query, sub_matches, cgs);
                return Ok((Expr::Query(query), consume));
            }

            if cap.kind == CapabilityKind::Create {
                let input = args_to_input(sub_matches, cap, cgs).unwrap_or(Value::Null);
                return Ok((
                    Expr::Create(CreateExpr::new(&cap.name, entity_name, input)),
                    StreamConsumeOpts::default(),
                ));
            }
        }

        // Node-level operation — ID must be present on the entity command
        let id = entity_matches.get_one::<String>("id").ok_or_else(|| {
            AgentError::Argument(format!(
                "'{}' requires an ID. Usage: {} <ID> {}",
                sub_name,
                entity_name.to_lowercase(),
                sub_name
            ))
        })?;

        let node_ref = cli_entity_node_ref(entity_name, entity, entity_matches, id.as_str(), cgs)?;

        // Relation (CLI may use kebab-case: team_members → team-members)
        if let Some(rel_key) = resolve_relation_key(entity, sub_name) {
            return build_relation_expr(rel_key, sub_matches, entity_name, entity, &node_ref, cgs);
        }

        // EntityRef field navigation (FK auto-resolve via ChainExpr)
        if let Some(field_key) = resolve_entity_ref_field(entity, sub_name, cgs) {
            let mut get = GetExpr::from_ref(node_ref.clone());
            if let Some(get_cap) = cgs.find_capability(entity_name, CapabilityKind::Get) {
                get.path_vars = path_vars_for_cml(
                    &get_cap.mapping.template,
                    id.as_str(),
                    entity_matches,
                    None,
                )?;
            }
            let chain = ChainExpr::auto_get(Expr::Get(get), field_key);
            return Ok((Expr::Chain(chain), StreamConsumeOpts::default()));
        }

        // Reverse traversal: `pet 10 orders` → query(Order, petId=10)
        if let Some((target_entity_name, param_name)) =
            resolve_reverse_traversal(entity_name, sub_name, cgs)
        {
            cgs.get_entity(&target_entity_name)
                .ok_or_else(|| AgentError::EntityNotFound(target_entity_name.clone()))?;
            let rt_query_cap = cgs.find_capability(&target_entity_name, CapabilityKind::Query);
            let user_pred =
                rt_query_cap.and_then(|cap| args_to_query_predicate(sub_matches, cap, cgs));
            let fk_val = relation_scope_string(entity, &node_ref);
            let fk_pred = plasm_core::Predicate::eq(&param_name, fk_val);
            let combined = match user_pred {
                Some(p) => plasm_core::Predicate::and(vec![fk_pred, p]),
                None => fk_pred,
            };
            let mut query = QueryExpr::filtered(&target_entity_name, combined);
            let consume = attach_query_pagination_if_present(&mut query, sub_matches, cgs);
            if cgs
                .find_capability(&target_entity_name, CapabilityKind::Get)
                .is_some()
                && sub_matches.get_flag("query_summary")
            {
                query.hydrate = Some(false);
            }
            return Ok((Expr::Query(query), consume));
        }

        // Find the matching capability
        let cap = find_capability(sub_name, entity_name, cgs)?;

        match cap.kind {
            CapabilityKind::Create => {
                // Create is collection-level but might appear as a subcommand
                let input = args_to_input(sub_matches, cap, cgs).unwrap_or(Value::Null);
                return Ok((
                    Expr::Create(CreateExpr::new(&cap.name, entity_name, input)),
                    StreamConsumeOpts::default(),
                ));
            }
            CapabilityKind::Delete => {
                let mut del = DeleteExpr::with_target(&cap.name, node_ref.clone());
                del.path_vars = path_vars_for_cml(
                    &cap.mapping.template,
                    id.as_str(),
                    entity_matches,
                    Some(sub_matches),
                )?;
                return Ok((Expr::Delete(del), StreamConsumeOpts::default()));
            }
            _ => {
                // Update, Action, or anything else -> Invoke
                let input = args_to_input(sub_matches, cap, cgs);
                let mut inv = InvokeExpr::with_target(&cap.name, node_ref.clone(), input);
                inv.path_vars = path_vars_for_cml(
                    &cap.mapping.template,
                    id.as_str(),
                    entity_matches,
                    Some(sub_matches),
                )?;
                return Ok((Expr::Invoke(inv), StreamConsumeOpts::default()));
            }
        }
    }

    // No subcommand — if ID is present, it's an implicit get
    let id = entity_matches
        .get_one::<String>("id")
        .ok_or_else(|| AgentError::Argument(format!(
            "Provide an ID or use 'query'. Usage:\n  {} query [--filters]\n  {} <ID> [--key flags when SCHEMA uses compound keys]\n  {} <ID> <relation>",
            entity_name.to_lowercase(),
            entity_name.to_lowercase(),
            entity_name.to_lowercase(),
        )))?;

    let get_cap = cgs
        .find_capability(entity_name, CapabilityKind::Get)
        .ok_or_else(|| AgentError::CapabilityNotFound {
            entity: entity_name.into(),
            kind: "get".into(),
        })?;
    let node_ref = cli_entity_node_ref(entity_name, entity, entity_matches, id.as_str(), cgs)?;
    let mut get = GetExpr::from_ref(node_ref);
    get.path_vars =
        path_vars_for_cml(&get_cap.mapping.template, id.as_str(), entity_matches, None)?;
    Ok((Expr::Get(get), StreamConsumeOpts::default()))
}

fn build_relation_expr(
    relation_name: &str,
    sub_matches: &ArgMatches,
    entity_name: &str,
    entity: &EntityDef,
    source_ref: &Ref,
    cgs: &CGS,
) -> Result<(Expr, StreamConsumeOpts), AgentError> {
    let relation_schema = cgs
        .get_entity(entity_name)
        .and_then(|e| e.relations.get(relation_name))
        .ok_or_else(|| AgentError::Argument(format!("Relation '{}' not found", relation_name)))?;
    let target = &relation_schema.target_resource;

    // Scoped relations: inject scope from the source entity; CLI does not expose those args.
    use plasm_core::RelationMaterialization;
    let mat = relation_schema
        .materialize
        .as_ref()
        .unwrap_or(&RelationMaterialization::Unavailable);
    match mat {
        RelationMaterialization::QueryScoped { capability, param } => {
            let scope_val = relation_scope_string(entity, source_ref);
            let scope_pred = plasm_core::Predicate::eq(param.as_str(), scope_val);
            let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
                AgentError::Argument(format!(
                    "Unknown materialize capability '{}' (relation '{}')",
                    capability, relation_name
                ))
            })?;
            let extra_pred = args_to_query_predicate(sub_matches, cap, cgs);
            let combined = match extra_pred {
                Some(p) => plasm_core::Predicate::and(vec![scope_pred, p]),
                None => scope_pred,
            };
            let mut query = QueryExpr::filtered(target.clone(), combined);
            query.capability_name = Some(cap.name.clone());
            let consume = attach_query_pagination_if_present(&mut query, sub_matches, cgs);
            if cgs
                .find_capability(target.as_str(), CapabilityKind::Get)
                .is_some()
                && sub_matches.get_flag("query_summary")
            {
                query.hydrate = Some(false);
            }
            return Ok((Expr::Query(query), consume));
        }
        RelationMaterialization::QueryScopedBindings {
            capability,
            bindings,
        } => {
            let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
                AgentError::Argument(format!(
                    "Unknown materialize capability '{}' (relation '{}')",
                    capability, relation_name
                ))
            })?;
            let preds: Vec<plasm_core::Predicate> = bindings
                .iter()
                .map(|(cap_param, parent_field)| {
                    let v = relation_binding_field_value(entity, source_ref, parent_field);
                    plasm_core::Predicate::eq(cap_param.as_str(), plasm_core::Value::String(v))
                })
                .collect();
            let scope_pred = if preds.len() == 1 {
                preds.into_iter().next().expect("bindings non-empty")
            } else {
                plasm_core::Predicate::and(preds)
            };
            let extra_pred = args_to_query_predicate(sub_matches, cap, cgs);
            let combined = match extra_pred {
                Some(p) => plasm_core::Predicate::and(vec![scope_pred, p]),
                None => scope_pred,
            };
            let mut query = QueryExpr::filtered(target.clone(), combined);
            query.capability_name = Some(cap.name.clone());
            let consume = attach_query_pagination_if_present(&mut query, sub_matches, cgs);
            if cgs
                .find_capability(target.as_str(), CapabilityKind::Get)
                .is_some()
                && sub_matches.get_flag("query_summary")
            {
                query.hydrate = Some(false);
            }
            return Ok((Expr::Query(query), consume));
        }
        _ => {}
    }

    let target_query_cap = cgs.find_capability(target.as_str(), CapabilityKind::Query);
    let predicate = target_query_cap.and_then(|cap| args_to_query_predicate(sub_matches, cap, cgs));
    let mut query = match predicate {
        Some(pred) => QueryExpr::filtered(target.clone(), pred),
        None => QueryExpr::all(target.clone()),
    };
    if let Some(cap) = cgs.find_capability(target.as_str(), CapabilityKind::Query) {
        query.capability_name = Some(cap.name.clone());
    }
    let consume = attach_query_pagination_if_present(&mut query, sub_matches, cgs);
    if cgs
        .find_capability(target.as_str(), CapabilityKind::Get)
        .is_some()
        && sub_matches.get_flag("query_summary")
    {
        query.hydrate = Some(false);
    }

    Ok((Expr::Query(query), consume))
}

/// Populate [`QueryExpr::pagination`] from built-in CLI flags when the query mapping declares CML pagination.
/// Returns consumer-side limits for [`ExecutionEngine::execute`]; position-only fields go on [`QueryExpr::pagination`].
fn attach_query_pagination_if_present(
    query: &mut QueryExpr,
    sub_matches: &ArgMatches,
    cgs: &CGS,
) -> StreamConsumeOpts {
    let cap_opt = query
        .capability_name
        .as_deref()
        .and_then(|name| cgs.get_capability(name))
        .or_else(|| cgs.find_capability(&query.entity, CapabilityKind::Query))
        .or_else(|| cgs.find_capability(&query.entity, CapabilityKind::Search));
    let Some(cap) = cap_opt else {
        return StreamConsumeOpts::default();
    };
    let Ok(template) = parse_capability_template(&cap.mapping.template) else {
        return StreamConsumeOpts::default();
    };
    let Some(pconf) = template_pagination(&template) else {
        return StreamConsumeOpts::default();
    };

    let is_block_range = pconf.location == plasm_compile::PaginationLocation::BlockRange;
    let has_from_response = pconf
        .params
        .values()
        .any(|p| matches!(p, plasm_compile::PaginationParam::FromResponse { .. }));
    let has_offset_counter = pconf.params.iter().any(|(n, p)| {
        matches!(p, plasm_compile::PaginationParam::Counter { .. })
            && n.to_lowercase().contains("offset")
    });

    let offset = if has_offset_counter {
        sub_matches.get_one::<i64>("pagination_offset").copied()
    } else {
        None
    };
    let page = if !has_offset_counter && !has_from_response && !is_block_range {
        sub_matches.get_one::<i64>("pagination_page").copied()
    } else {
        None
    };
    let cursor = if has_from_response {
        sub_matches
            .get_one::<String>("pagination_cursor")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let from_block = if is_block_range {
        sub_matches.get_one::<u64>("pagination_from_block").copied()
    } else {
        None
    };
    let to_block = if is_block_range {
        sub_matches.get_one::<u64>("pagination_to_block").copied()
    } else {
        None
    };
    let fetch_all =
        sub_matches.get_flag("pagination_all") || (is_block_range && to_block.is_some());
    let max_items = sub_matches.get_one::<usize>("pagination_limit").copied();

    let consume = StreamConsumeOpts {
        fetch_all,
        max_items,
        one_page: false,
    };

    if !fetch_all
        && max_items.is_none()
        && offset.is_none()
        && page.is_none()
        && cursor.is_none()
        && from_block.is_none()
        && to_block.is_none()
    {
        return StreamConsumeOpts::default();
    }

    query.pagination = Some(QueryPagination {
        offset,
        page,
        cursor,
        from_block,
        to_block,
    });
    consume
}

/// Match a CLI subcommand (e.g. "orders") to a reverse-traversal via EntityRef back-index.
/// Returns `(target_entity_name, param_name)` if found.
fn resolve_reverse_traversal(
    source_entity: &str,
    user_sub: &str,
    cgs: &CGS,
) -> Option<(String, String)> {
    let u = normalize_cli_token(user_sub);
    let reverse_caps = cgs.find_reverse_traversal_caps(source_entity);
    for (cap, param_name) in &reverse_caps {
        let plural = pluralize_entity(cap.domain.as_str());
        let sub_kebab = normalize_cli_token(&plural.replace('_', "-"));
        if sub_kebab == u {
            return Some((cap.domain.to_string(), param_name.to_string()));
        }
    }
    None
}

/// Match a CLI subcommand token to an EntityRef field on the entity.
/// Handles both camelCase (`petId` → `pet-id`) and snake_case (`pet_id` → `pet-id`).
fn resolve_entity_ref_field<'a>(
    entity: &'a EntityDef,
    user_sub: &str,
    cgs: &plasm_core::CGS,
) -> Option<&'a str> {
    let user_kebab = normalize_cli_token(user_sub);
    entity
        .fields
        .keys()
        .find(|k| {
            let field_kebab = normalize_cli_token(&field_subcommand_kebab(k));
            if field_kebab != user_kebab {
                return false;
            }
            entity
                .fields
                .get(*k)
                .and_then(|f| cgs.named_value_for_slot(f).ok())
                .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }))
        })
        .map(|s| s.as_str())
}

fn resolve_relation_key<'a>(entity: &'a EntityDef, user_sub: &str) -> Option<&'a str> {
    let u = normalize_cli_token(user_sub);
    entity
        .relations
        .keys()
        .find(|k| normalize_cli_token(k) == u)
        .map(|s| s.as_str())
}

/// Template vars from CML may be wired as GET-style `--flag` args (`path_param__{var}`) or as
/// invoke/query flags named `{var}` (see [`crate::invoke_args::build_invoke_args`]). Only one id exists
/// per command level — use [`ArgMatches::try_get_one`] so missing ids never panic.
fn optional_template_var_flag(matches: &ArgMatches, var_name: &str) -> Option<String> {
    let path_style = path_param_arg_id(var_name);
    if let Ok(Some(s)) = matches.try_get_one::<String>(path_style) {
        return Some(s.clone());
    }
    match matches.try_get_one::<String>(var_name) {
        Ok(Some(s)) => Some(s.clone()),
        Ok(None) | Err(_) => None,
    }
}

/// Positional `id` binds the **last** CML path var; earlier vars use `--{kebab(name)}`.
/// Path segments are read from `entity_matches`; optional `cap_matches` supplies
/// invoke/delete-only template flags (parent still holds shared path flags).
fn collect_template_string_bindings(
    template: &serde_json::Value,
    positional_id: &str,
    path_matches: &ArgMatches,
    extra_matches: Option<&ArgMatches>,
) -> Result<IndexMap<String, String>, AgentError> {
    let template = parse_capability_template(template)
        .map_err(|_| AgentError::Argument("Invalid capability template".into()))?;
    let mut out = IndexMap::new();

    let http_path_vars = match &template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            path_var_names_from_request(cml)
        }
        CapabilityTemplate::View(_)
        | CapabilityTemplate::EvmCall(_)
        | CapabilityTemplate::EvmLogs(_) => Vec::new(),
    };

    if !http_path_vars.is_empty() {
        if http_path_vars.len() > 1 {
            for var_name in http_path_vars.iter().take(http_path_vars.len() - 1) {
                let arg_id = path_param_arg_id(var_name);
                let s = path_matches.get_one::<String>(arg_id).ok_or_else(|| {
                    AgentError::Argument(format!(
                        "Missing required path flag --{} (CML path variable `{}`)",
                        var_name.replace('_', "-"),
                        var_name
                    ))
                })?;
                out.insert(var_name.clone(), s.clone());
            }
        }

        let last = http_path_vars.last().expect("non-empty");
        out.insert(last.clone(), positional_id.to_string());
    }

    for var_name in template_var_names(&template) {
        if var_name == "id" || http_path_vars.contains(&var_name) {
            continue;
        }
        let s = extra_matches
            .and_then(|m| optional_template_var_flag(m, &var_name))
            .or_else(|| optional_template_var_flag(path_matches, &var_name));
        if let Some(s) = s {
            out.insert(var_name.clone(), s.clone());
        }
    }

    Ok(out)
}

fn path_vars_for_cml(
    template: &serde_json::Value,
    positional_id: &str,
    entity_matches: &ArgMatches,
    cap_matches: Option<&ArgMatches>,
) -> Result<Option<IndexMap<String, Value>>, AgentError> {
    let strings =
        collect_template_string_bindings(template, positional_id, entity_matches, cap_matches)?;
    if strings.is_empty() {
        Ok(None)
    } else {
        Ok(Some(
            strings
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect(),
        ))
    }
}

/// Build [`Ref`] for a CLI node: simple id, or compound map from GET path + `key_vars`.
fn cli_entity_node_ref(
    entity_name: &str,
    entity: &EntityDef,
    entity_matches: &ArgMatches,
    positional_id: &str,
    cgs: &CGS,
) -> Result<Ref, AgentError> {
    if entity.key_vars.len() <= 1 {
        return Ok(Ref::new(entity_name, positional_id));
    }
    let get_cap = cgs.find_capability(entity_name, CapabilityKind::Get).ok_or_else(|| {
        AgentError::Argument(format!(
            "Entity `{entity_name}` uses compound key {:?}; a GET capability is required to resolve CLI path variables.",
            entity.key_vars
        ))
    })?;
    let mut bindings = collect_template_string_bindings(
        &get_cap.mapping.template,
        positional_id,
        entity_matches,
        None,
    )?;

    for kv in &entity.key_vars {
        if !bindings.contains_key(kv.as_str()) {
            if let Some(s) = entity_matches.get_one::<String>(path_param_arg_id(kv)) {
                bindings.insert(kv.as_str().to_string(), s.clone());
            }
        }
    }

    let mut parts = std::collections::BTreeMap::new();
    for kv in &entity.key_vars {
        let Some(v) = bindings.get(kv.as_str()) else {
            return Err(AgentError::Argument(format!(
                "Missing compound key part `{kv}` for entity `{entity_name}` \
                 (use --{} or the positional id for the last URL segment, per SCHEMA `key_vars`)",
                kv.replace('_', "-")
            )));
        };
        parts.insert(kv.as_str().to_string(), v.clone());
    }
    Ok(Ref::compound(entity_name, parts))
}

fn relation_scope_string(entity: &EntityDef, source_ref: &Ref) -> String {
    match &source_ref.key {
        EntityKey::Compound(parts) => parts
            .get(entity.id_field.as_str())
            .cloned()
            .unwrap_or_else(|| source_ref.primary_slot_str()),
        EntityKey::Simple(_) => source_ref
            .simple_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| source_ref.primary_slot_str()),
    }
}

fn relation_binding_field_value(
    entity: &EntityDef,
    source_ref: &Ref,
    parent_field: &plasm_core::EntityFieldName,
) -> String {
    let pf = parent_field.as_str();
    match &source_ref.key {
        EntityKey::Compound(parts) => parts.get(pf).cloned().unwrap_or_else(|| {
            if pf == entity.id_field.as_str() {
                relation_scope_string(entity, source_ref)
            } else {
                String::new()
            }
        }),
        EntityKey::Simple(_) => {
            if pf == entity.id_field.as_str() {
                relation_scope_string(entity, source_ref)
            } else {
                String::new()
            }
        }
    }
}

fn find_capability<'a>(
    sub_name: &str,
    entity_name: &str,
    cgs: &'a CGS,
) -> Result<&'a plasm_core::CapabilitySchema, AgentError> {
    let user = normalize_cli_token(sub_name);
    for cap in cgs.capabilities.values() {
        if cap.domain.as_str() != entity_name {
            continue;
        }
        let prefix = format!("{}_", entity_name.to_lowercase());
        let stripped = if cap.name.to_lowercase().starts_with(&prefix) {
            cap.name.as_str()[prefix.len()..].to_string()
        } else {
            cap.name.to_string()
        };
        if normalize_cli_token(&stripped) == user
            || normalize_cli_token(&cap.name) == user
            || cap.name.eq_ignore_ascii_case(sub_name)
        {
            return Ok(cap);
        }
    }
    Err(AgentError::CapabilityNotFound {
        entity: entity_name.into(),
        kind: sub_name.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_builder::build_app;
    use plasm_core::{
        CapabilityKind, CapabilityMapping, CapabilitySchema, EntityKey, Expr, FieldSchema,
        FieldType, FieldValueKind, NamedValueSchema, ResourceSchema, StringSemantics,
        ValueDomainKey,
    };

    fn evm_get_cgs() -> CGS {
        let mut cgs = CGS::new();
        for (k, sem) in [
            ("dp_evm_account", Some(StringSemantics::Short)),
            ("dp_evm_balance", Some(StringSemantics::Short)),
        ] {
            cgs.values.insert(
                k.into(),
                NamedValueSchema {
                    description: String::new(),
                    field_type: FieldType::String,
                    value_format: None,
                    allowed_values: None,
                    string_semantics: sem,
                    array_items: None,
                },
            );
        }
        cgs.add_resource(ResourceSchema {
            name: "Balance".into(),
            description: String::new(),
            id_field: "account".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "account".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("dp_evm_account").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "balance".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("dp_evm_balance").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "balance_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Balance".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "transport": "evm_call",
                    "chain": 1,
                    "contract": { "type": "const", "value": "0x0000000000000000000000000000000000000001" },
                    "function": "function balanceOf(address owner) view returns (uint256)",
                    "args": [{ "type": "var", "name": "id" }],
                    "block": { "type": "var", "name": "block" }
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        })
        .unwrap();
        cgs
    }

    #[test]
    fn path_vars_for_cml_captures_evm_template_flags() {
        let cgs = evm_get_cgs();
        let app = build_app(&cgs, crate::cli_builder::AgentCliSurface::CgsClient);
        let matches = app
            .try_get_matches_from(["plasm-agent", "balance", "0xabc", "--block", "latest"])
            .unwrap();
        let (_, entity_matches) = matches.subcommand().unwrap();
        let cap = cgs.find_capability("Balance", CapabilityKind::Get).unwrap();

        let vars = path_vars_for_cml(&cap.mapping.template, "0xabc", entity_matches, None)
            .unwrap()
            .unwrap();

        assert_eq!(
            vars.get("block"),
            Some(&Value::String("latest".to_string()))
        );
        assert!(
            !vars.contains_key("id"),
            "the positional id stays implicit in the env; only extra template vars are captured here"
        );
    }

    #[test]
    fn block_range_to_block_implies_multi_page_query() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "dp_transfer_event_id".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        cgs.add_resource(ResourceSchema {
            name: "Transfer".into(),
            description: String::new(),
            id_field: "event_id".into(),
            id_format: None,
            id_from: None,
            fields: vec![FieldSchema {
                name: "event_id".into(),
                kind: FieldValueKind::Registry(
                    ValueDomainKey::new("dp_transfer_event_id").expect("key"),
                ),
                description: String::new(),
                required: true,
                agent_presentation: None,
                mime_type_hint: None,
                attachment_media: None,
                wire_path: None,
                derive: None,
            }],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "transfer_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Transfer".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "transport": "evm_logs",
                    "chain": 1,
                    "contract": { "type": "const", "value": "0x0000000000000000000000000000000000000001" },
                    "event": "event Transfer(address indexed from, address indexed to, uint256 value)",
                    "pagination": {
                        "location": "block_range",
                        "params": {
                            "range_size": {"fixed": 100}
                        }
                    }
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        })
        .unwrap();

        let app = build_app(&cgs, crate::cli_builder::AgentCliSurface::CgsClient);
        let matches = app
            .try_get_matches_from([
                "plasm-agent",
                "transfer",
                "query",
                "--from-block",
                "0",
                "--to-block",
                "5000",
            ])
            .unwrap();

        let (_, entity_matches) = matches.subcommand().unwrap();
        let (_, sub_matches) = entity_matches.subcommand().unwrap();
        let mut query = QueryExpr::all("Transfer");
        query.capability_name = Some("transfer_query".into());
        let consume = attach_query_pagination_if_present(&mut query, sub_matches, &cgs);

        let pagination = query.pagination.expect("pagination");
        assert!(consume.fetch_all);
        assert_eq!(pagination.from_block, Some(0));
        assert_eq!(pagination.to_block, Some(5000));
    }

    fn compound_issue_cli_cgs() -> CGS {
        let mut cgs = CGS::new();
        for k in ["dp_issue_owner", "dp_issue_repo"] {
            cgs.values.insert(
                k.into(),
                NamedValueSchema {
                    description: String::new(),
                    field_type: FieldType::String,
                    value_format: None,
                    allowed_values: None,
                    string_semantics: Some(StringSemantics::Short),
                    array_items: None,
                },
            );
        }
        cgs.values.insert(
            "dp_issue_number".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Integer,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
        let mk = |n: &str, vk: &str| FieldSchema {
            name: n.into(),
            kind: FieldValueKind::Registry(ValueDomainKey::new(vk).expect("key")),
            description: String::new(),
            required: true,
            agent_presentation: None,
            mime_type_hint: None,
            attachment_media: None,
            wire_path: None,
            derive: None,
        };
        let mk_int = |n: &str, vk: &str| FieldSchema {
            name: n.into(),
            kind: FieldValueKind::Registry(ValueDomainKey::new(vk).expect("key")),
            description: String::new(),
            required: true,
            agent_presentation: None,
            mime_type_hint: None,
            attachment_media: None,
            wire_path: None,
            derive: None,
        };
        cgs.add_resource(ResourceSchema {
            name: "Issue".into(),
            description: String::new(),
            id_field: "number".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                mk("owner", "dp_issue_owner"),
                mk("repo", "dp_issue_repo"),
                mk_int("number", "dp_issue_number"),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["owner".into(), "repo".into(), "number".into()],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();
        cgs.add_capability(CapabilitySchema {
            name: "issue_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Issue".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [
                        {"type": "literal", "value": "repos"},
                        {"type": "var", "name": "owner"},
                        {"type": "var", "name": "repo"},
                        {"type": "literal", "value": "issues"},
                        {"type": "var", "name": "number"}
                    ]
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        })
        .unwrap();
        cgs.validate().unwrap();
        cgs
    }

    #[test]
    fn compound_key_implicit_get_matches_parser_shape() {
        let cgs = compound_issue_cli_cgs();
        let app = build_app(&cgs, crate::cli_builder::AgentCliSurface::CgsClient);
        let m = app
            .try_get_matches_from(["plasm-agent", "issue", "--owner", "o", "--repo", "r", "42"])
            .unwrap();
        let (_, em) = m.subcommand().unwrap();
        let entity = cgs.get_entity("Issue").unwrap();
        let (expr, _) = build_expr(em, "Issue", entity, &cgs).unwrap();
        let Expr::Get(g) = expr else {
            panic!("expected Get, got {expr:?}");
        };
        let EntityKey::Compound(parts) = &g.reference.key else {
            panic!("expected compound ref");
        };
        assert_eq!(parts.get("owner").map(String::as_str), Some("o"));
        assert_eq!(parts.get("repo").map(String::as_str), Some("r"));
        assert_eq!(parts.get("number").map(String::as_str), Some("42"));
    }
}
