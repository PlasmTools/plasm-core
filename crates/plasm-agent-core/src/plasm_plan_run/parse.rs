//! Surface parse and typecheck.

use super::*;
use plasm_core::error_render::{render_parse_error_with_feedback, FeedbackStyle};

use plasm_core::cgs_federation::{cgs_layer_stack_from_contexts, CgsLayer};
use plasm_core::symbol_tuning::CatalogScope;

pub fn session_cgs_layer_stack(session: &ExecuteSession) -> Vec<CgsLayer<'_>> {
    if session.contexts_by_entry.is_empty() {
        vec![CgsLayer::new(
            session.entry_id.as_str(),
            session.cgs.as_ref(),
        )]
    } else {
        cgs_layer_stack_from_contexts(&session.contexts_by_entry)
    }
}

/// Legacy slice of inner [`CGS`] graphs — prefer [`session_cgs_layer_stack`].
pub fn session_cgs_layers(session: &ExecuteSession) -> Vec<&CGS> {
    session_cgs_layer_stack(session)
        .iter()
        .map(CgsLayer::cgs)
        .collect()
}

fn agent_program_error(head: impl AsRef<str>, help: Option<impl AsRef<str>>) -> String {
    if let Some(h) = help {
        format!("{}\nhelp: {}", head.as_ref(), h.as_ref())
    } else {
        head.as_ref().to_string()
    }
}

/// Resolve a teaching `p#` token (or pass through a wire name) for a known row entity.
pub fn resolve_wire_field_token(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    token: &str,
) -> Result<String, String> {
    let t = token.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    if let Some(qe) = qe {
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            session,
            qe.entry_id.as_str(),
            qe.entity.as_str(),
        )
        .map_err(|e| agent_program_error(e, None::<&str>))?;
        let ent = cgs.get_entity(qe.entity.as_str()).ok_or_else(|| {
            agent_program_error(
                format!(
                    "entity `{}` is not defined in catalog `{}`",
                    qe.entity, qe.entry_id
                ),
                None::<&str>,
            )
        })?;
        return map
            .resolve_entity_field(
                CatalogScope::qualified(qe.entry_id.as_str()),
                qe.entity.as_str(),
                ent,
                t,
            )
            .map_err(|e| e.to_agent_program_error());
    }
    if plasm_core::symbol_tuning::SymbolMap::is_opaque_p_sym(t) {
        return Err(agent_program_error(
            format!("`{t}` requires a row binding context for field resolution"),
            Some("Use wire field names from the teaching TSV on a bound row or postfix chain with a known receiver."),
        ));
    }
    Ok(t.to_string())
}

/// Resolve optional projection / postfix field list tokens to wire names.
pub fn resolve_wire_field_list(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    fields: &[String],
) -> Result<Vec<String>, String> {
    fields
        .iter()
        .map(|f| resolve_wire_field_token(session, symbol_map_cross_cache, qe, f))
        .collect()
}

/// Symbol map for in-grammar opaque symbol resolution on the parse / program ingress path.
pub fn symbol_map_for_plasm_surface_parse(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
) -> Arc<dyn SymbolSession> {
    crate::symbol_map_resolve::resolve_session_symbol_map(
        &crate::symbol_map_resolve::SessionSymbolMapContext {
            session,
            cross_cache: symbol_map_cross_cache,
        },
    )
}

/// Row-shaped JSON for plan evaluation (`for_each` templates, derive scopes, …).
///
/// [`CachedEntity::payload_to_json`] serializes decoded fields only. Some transports omit the primary
/// key on list-shaped summaries even when [`Ref`] carries identity — merge so `_.id` (and compound
/// slots) resolve consistently.
pub fn parse_plasm_surface_line(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    pipeline: &PromptPipelineConfig,
    line: &str,
) -> Result<ParsedExpr, ParseError> {
    parse_plasm_surface_line_program(session, symbol_map_cross_cache, pipeline, line, None, false)
}

/// Stamp inferred `capability_name` on queries (e.g. Linear `Issue{team_key=…}` → `issue_search`)
/// so plan inference and dry-run match live execution.
pub(crate) fn normalize_query_capabilities_for_session(
    session: &ExecuteSession,
    expr: &mut Expr,
) -> Result<(), String> {
    if session.contexts_by_entry.len() <= 1 {
        normalize_expr_query_capabilities(expr, session.cgs.as_ref()).map_err(|e| e.to_string())
    } else if let Some(exposure) = session.teaching_exposure.as_ref() {
        let fed = FederationDispatch::from_contexts_and_exposure(
            session.contexts_by_entry.clone(),
            exposure,
        );
        normalize_expr_query_capabilities_federated(expr, &fed, session.cgs.as_ref())
            .map_err(|e| e.to_string())
    } else {
        normalize_expr_query_capabilities(expr, session.cgs.as_ref()).map_err(|e| e.to_string())
    }
}

/// Parse one Plasm surface line with optional **program compile** context (in-scope node ids and
/// `for_each` row binding).
pub fn parse_plasm_surface_line_program(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    _pipeline: &PromptPipelineConfig,
    line: &str,
    program_nodes: Option<&BTreeSet<String>>,
    for_each_row_context: bool,
) -> Result<ParsedExpr, ParseError> {
    let surface = line.trim();
    let stack = session_cgs_layer_stack(session);
    let sym_map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let mut parsed = parse_with_cgs_layers_program(
        surface,
        &stack,
        sym_map,
        program_nodes,
        for_each_row_context,
    )?;
    normalize_query_capabilities_for_session(session, &mut parsed.expr).map_err(|message| {
        ParseError {
            kind: plasm_core::expr_parser::ParseErrorKind::Other { message },
            offset: 0,
        }
    })?;
    Ok(parsed)
}

/// Parse a program-context surface line and lower phrase idents (validate + normalize).
///
/// All DAG paths that pass `program_nodes` must use this instead of
/// [`parse_plasm_surface_line_program`] alone so binding-shadow / unknown-binding rules apply
/// consistently before plan emission.
pub fn parse_plasm_program_surface_for_dag(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    pipeline: &PromptPipelineConfig,
    line: &str,
    program_labels: &BTreeSet<String>,
    for_each_row_context: bool,
    node_id: Option<&str>,
) -> Result<ParsedExpr, String> {
    let mut parsed = parse_plasm_surface_line_program(
        session,
        symbol_map_cross_cache,
        pipeline,
        line,
        Some(program_labels),
        for_each_row_context,
    )
    .map_err(|e| {
        format_session_symbolic_parse_error(session, symbol_map_cross_cache, pipeline, line, &e)
    })?;
    lower_program_phrase_idents_in_parsed(session, &mut parsed, program_labels, node_id)?;
    Ok(parsed)
}

pub(crate) fn lower_program_phrase_idents_in_parsed(
    session: &ExecuteSession,
    parsed: &mut ParsedExpr,
    program_labels: &BTreeSet<String>,
    node_id: Option<&str>,
) -> Result<(), String> {
    let phrase_result = if session.contexts_by_entry.len() <= 1 {
        plasm_core::lower_program_phrase_idents_in_expr(
            &mut parsed.expr,
            program_labels,
            session.cgs.as_ref(),
        )
    } else if let Some(exposure) = session.teaching_exposure.as_ref() {
        let fed = FederationDispatch::from_contexts_and_exposure(
            session.contexts_by_entry.clone(),
            exposure,
        );
        plasm_core::lower_program_phrase_idents_in_expr_federated(
            &mut parsed.expr,
            program_labels,
            &fed,
            session.cgs.as_ref(),
        )
    } else {
        let fed = FederationDispatch::from_contexts_only(session.contexts_by_entry.clone());
        plasm_core::lower_program_phrase_idents_in_expr_federated(
            &mut parsed.expr,
            program_labels,
            &fed,
            session.cgs.as_ref(),
        )
    };
    phrase_result.map_err(|e| {
        let enriched = enrich_phrase_ident_program_error(session, &e);
        if let Some(id) = node_id {
            agent_program_error(
                format!("Plasm program `{id}`: {enriched}"),
                Some("Use a binding reference for program labels (`label` or `label.field`), or quote literal strings (`\"…\"`)."),
            )
        } else {
            agent_program_error(enriched, None::<&str>)
        }
    })
}

/// Add session `e#` / catalog context when phrase-ident validation fails on federated surfaces.
fn enrich_phrase_ident_program_error(session: &ExecuteSession, raw: &str) -> String {
    let Some(exposure) = session.teaching_exposure.as_ref() else {
        return raw.to_string();
    };
    let map = exposure.symbol_map_arc();

    if let Some(entity) = raw
        .strip_prefix("unknown entity `")
        .and_then(|tail| tail.strip_suffix('`'))
    {
        for (i, ent) in exposure.entities.iter().enumerate() {
            if ent.as_str() != entity {
                continue;
            }
            let Some(eid) = exposure.entity_catalog_entry_ids.get(i) else {
                continue;
            };
            let sym = map.entity_sym_for(eid.as_str(), entity);
            return format!(
                "unknown entity `{sym}` ({entity}) in catalog `{eid}` — use the session `e#` from the teaching table"
            );
        }
    }

    if let Some(cap) = raw
        .strip_prefix("unknown capability `")
        .and_then(|tail| tail.strip_suffix('`'))
    {
        let mut owners: Vec<String> = session
            .contexts_by_entry
            .iter()
            .filter(|(_eid, ctx)| ctx.cgs.get_capability(cap).is_some())
            .map(|(eid, _)| eid.clone())
            .collect();
        owners.sort();
        owners.dedup();
        if owners.is_empty() {
            return format!(
                "unknown capability `{cap}` — check ranked_capabilities or session seeds"
            );
        }
        if owners.len() == 1 {
            return format!(
                "unknown capability `{cap}` in catalog `{}` — use the session `e#` / `m#` from the teaching table for that catalog",
                owners[0]
            );
        }
        return format!(
            "unknown capability `{cap}` — loaded in catalogs {owners:?}; disambiguate with session `e#` / `m#` stamps"
        );
    }

    raw.to_string()
}

/// Program surface fragment for DAG lowering — **no** textual symbol expansion.
///
/// Opaque `e#` / `m#` / `p#` / `r#` resolve in the parser and per-token field helpers
/// ([`resolve_wire_field_token`], [`resolve_wire_field_list`]) against the session [`SymbolMap`].
pub fn expand_program_surface_for_session_lower(
    _session: &ExecuteSession,
    _pipeline: &PromptPipelineConfig,
    fragment: &str,
) -> String {
    fragment.trim().to_string()
}

/// Parse a Plasm line to [`ParsedExpr`] (surface IR + optional projection) for the active session.
///
/// Uses [`PromptPipelineConfig::default`] (TSV symbol tuning) and no cross-request symbol-map LRU.
/// Prefer [`parse_plasm_surface_line`] from HTTP/MCP with the process [`PromptPipelineConfig`] +
/// [`ExecuteSessionStore::symbol_map_cross_cache`].
pub fn parse_parsed_expr_for_session(
    session: &ExecuteSession,
    line: &str,
) -> Result<ParsedExpr, ParseError> {
    parse_plasm_surface_line(session, None, &PromptPipelineConfig::default(), line)
}

/// Type-check a parsed line against the session CGS (federated when multiple catalogs are loaded).
pub fn typecheck_parsed_for_session(
    session: &ExecuteSession,
    pe: &ParsedExpr,
) -> Result<(), TypeError> {
    if session.contexts_by_entry.len() <= 1 {
        return type_check_expr(&pe.expr, session.cgs.as_ref());
    }
    let fed = crate::catalog_ownership::federation_for_session(session);
    type_check_expr_federated(&pe.expr, &fed, session.cgs.as_ref())
}

pub(crate) fn entry_scoped_execute_session(
    session: &ExecuteSession,
    qualified_entity: Option<&QualifiedEntityKey>,
) -> Result<ExecuteSession, String> {
    let Some(q) = qualified_entity else {
        return Ok(session.clone());
    };
    if session.contexts_by_entry.len() <= 1 && session.entry_id == q.entry_id {
        return Ok(session.clone());
    }
    let ctx = session.contexts_by_entry.get(&q.entry_id).ok_or_else(|| {
        format!(
            "Plasm program node targets catalog {:?}, but that catalog is not loaded in this execute session",
            q.entry_id
        )
    })?;
    let mut scoped = session.clone();
    scoped.cgs = ctx.cgs.clone();
    scoped.contexts_by_entry = IndexMap::from([(q.entry_id.clone(), ctx.clone())]);
    scoped.entry_id = q.entry_id.clone();
    scoped.http_backend = Some(ctx.cgs.http_backend.clone());
    // Preserve the parent session symbol table — never mint fresh numbering for federated scoping.
    scoped.entities = session.entities.clone();
    Ok(scoped)
}

pub(crate) fn reference_for_row_identity(entity: &plasm_runtime::CachedEntity, cgs: &CGS) -> Ref {
    let primary = entity.reference.primary_slot_str();
    if !primary.is_empty() {
        return entity.reference.clone();
    }
    let id_name = cgs
        .get_entity(entity.reference.entity_type.as_str())
        .map(|e| e.id_field.as_str())
        .unwrap_or("id");
    if let Some(tf) = entity.get_field(id_name) {
        let v = tf.to_value();
        if let Value::String(s) = v {
            if !s.is_empty() {
                return Ref::new(entity.reference.entity_type.clone(), s);
            }
        }
    }
    entity.reference.clone()
}

pub(crate) fn row_identities_from_entities(
    es: &ExecuteSession,
    entity: &str,
    entities: &[plasm_runtime::CachedEntity],
) -> Vec<Option<plasm_core::RowIdentity>> {
    entities
        .iter()
        .map(|e| {
            let plan_qe = crate::catalog_ownership::resolve_qualified_entity_key(
                es,
                e.reference.entity_type.as_str(),
                None,
            )
            .or_else(|_| crate::catalog_ownership::resolve_qualified_entity_key(es, entity, None));
            let core_qe = match plan_qe {
                Ok(qe) => {
                    plasm_core::QualifiedEntityKey::new(qe.entry_id.clone(), qe.entity.clone())
                }
                Err(_) => return None,
            };
            let cgs = crate::catalog_ownership::resolve_cgs_for_entity(
                es,
                e.reference.entity_type.as_str(),
                None,
            )
            .unwrap_or(es.cgs.as_ref());
            let ent = cgs.get_entity(e.reference.entity_type.as_str())?;
            let reference = reference_for_row_identity(e, cgs);
            let key_vars = ent
                .key_vars
                .iter()
                .map(|k| k.as_str().to_string())
                .collect::<Vec<_>>();
            let mut identity = plasm_core::row_identity_from_parts(
                core_qe,
                reference,
                &e.relations,
                ent.id_field.as_str(),
                &key_vars,
            );
            for rel_name in ent.relations.keys() {
                if identity.ambient.contains_key(rel_name.as_str()) {
                    continue;
                }
                if let Some(tf) = e.get_field(rel_name.as_str()) {
                    if let plasm_core::Value::String(s) = tf.to_value() {
                        if !s.is_empty() {
                            identity.ambient.insert(rel_name.as_str().to_string(), s);
                        }
                    }
                }
            }
            Some(identity)
        })
        .collect()
}

pub(crate) fn propagate_row_identities(
    source: &PlanNodeId,
    op: &ComputeOp,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    out_len: usize,
) -> Result<Vec<Option<plasm_core::RowIdentity>>, String> {
    let mat = materialized.get(source).ok_or_else(|| {
        format!(
            "compute source node {:?} has not been materialized",
            source.as_str()
        )
    })?;
    match op {
        ComputeOp::Limit { count } => Ok(mat.row_identities.iter().take(*count).cloned().collect()),
        ComputeOp::Project { .. } => Ok(mat.row_identities.iter().take(out_len).cloned().collect()),
        ComputeOp::Filter { predicates } => {
            let Some(rows) = mat.row_source.inline_rows() else {
                return Ok(Vec::new());
            };
            Ok(mat
                .row_identities
                .iter()
                .zip(rows.iter())
                .filter(|(_, row)| predicates.iter().all(|p| predicate_matches(row, p)))
                .map(|(id, _)| id.clone())
                .collect())
        }
        _ => Ok(vec![None; out_len]),
    }
}

/// Simulated execution step: human **intent**, compact **il** (query `cap=` from schema), and **bindings** JSON, without HTTP or the `plasm` tool.
pub fn dry_run_simulation_for_session(
    session: &ExecuteSession,
    pe: &ParsedExpr,
) -> (String, String, serde_json::Value) {
    let intent = if session.contexts_by_entry.len() <= 1 {
        render_intent_with_projection(&pe.expr, pe.projection.as_deref(), session.cgs.as_ref())
    } else {
        match session.teaching_exposure.as_ref() {
            None => render_intent_with_projection(
                &pe.expr,
                pe.projection.as_deref(),
                session.cgs.as_ref(),
            ),
            Some(exposure) => {
                let fed = FederationDispatch::from_contexts_and_exposure(
                    session.contexts_by_entry.clone(),
                    exposure,
                );
                render_intent_with_projection_federated(
                    &pe.expr,
                    pe.projection.as_deref(),
                    &fed,
                    session.cgs.as_ref(),
                )
            }
        }
    };
    let il = if session.contexts_by_entry.len() <= 1 {
        expr_display_resolved(&pe.expr, session.cgs.as_ref())
    } else {
        match session.teaching_exposure.as_ref() {
            None => expr_display_resolved(&pe.expr, session.cgs.as_ref()),
            Some(exposure) => {
                let fed = FederationDispatch::from_contexts_and_exposure(
                    session.contexts_by_entry.clone(),
                    exposure,
                );
                expr_display_resolved_federated(&pe.expr, &fed, session.cgs.as_ref())
            }
        }
    };
    (intent, il, expr_simulation_bindings(&pe.expr))
}

/// Parse a single Plasm path expression string against the active execute session (federated or single).
pub fn parse_plasm_line_for_session(
    session: &ExecuteSession,
    line: &str,
) -> Result<(), ParseError> {
    parse_parsed_expr_for_session(session, line).map(|_| ())
}

/// Parser diagnostic plus SymbolicLlm correction (stamp lists, `e#` hints) for MCP/HTTP/DAG surfaces.
pub fn format_session_symbolic_parse_error(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    _pipeline: &PromptPipelineConfig,
    source_line: &str,
    err: &ParseError,
) -> String {
    let surface = source_line.trim();
    if surface.contains("=>")
        && matches!(
            err.kind,
            plasm_core::expr_parser::ParseErrorKind::ExpectedIdentifier
                | plasm_core::expr_parser::ParseErrorKind::ExpectedOperator
                | plasm_core::expr_parser::ParseErrorKind::ExpectedValue
        )
    {
        if let Err(msg) = crate::plasm_dag_surface_guards::reject_relation_arrow_trap(surface) {
            return msg;
        }
    }
    let sym_map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let step = render_parse_error_with_feedback(
        err,
        surface,
        surface,
        session.cgs.as_ref(),
        FeedbackStyle::SymbolicLlm {
            map: sym_map.as_ref(),
        },
    );
    let base = if step.correction.is_empty() {
        err.to_string()
    } else {
        step.correction
    };
    append_symbol_map_stability_context(session, &base, surface)
}

fn append_symbol_map_stability_context(
    session: &ExecuteSession,
    message: &str,
    source_line: &str,
) -> String {
    let needs_context = message.contains("not a mutator")
        || message.contains("compound constructor key")
        || message.contains("is not a row symbol")
        || message.contains("is not valid for");
    if !needs_context {
        return message.to_string();
    }
    let Some(exp) = session.teaching_exposure.as_ref() else {
        return message.to_string();
    };
    let fingerprint = plasm_core::symbol_map_fingerprint_hex(exp);
    let mut out = message.to_string();
    out.push_str(&format!(
        "\nsymbol_map_fingerprint={fingerprint}, domain_revision={}",
        session.domain_revision
    ));
    if let Some(m_token) = plasm_core::first_opaque_m_sym_in_expr(source_line) {
        if let Some((entry, domain, cap)) = exp
            .symbol_map_arc()
            .resolve_method_symbol_triple(m_token.as_str())
        {
            out.push_str(&format!("\n{m_token} → {entry}.{domain}.{cap}"));
        }
    }
    out
}
