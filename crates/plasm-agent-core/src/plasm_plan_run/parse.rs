//! Surface parse and typecheck.

use super::*;
use plasm_core::error_render::{render_parse_error_with_feedback, FeedbackStyle};

pub fn session_cgs_layers(session: &ExecuteSession) -> Vec<&CGS> {
    if session.contexts_by_entry.is_empty() {
        vec![session.cgs.as_ref()]
    } else {
        session
            .contexts_by_entry
            .values()
            .map(|c| c.cgs.as_ref())
            .collect()
    }
}

pub(crate) fn session_layer_catalog_entry_ids(session: &ExecuteSession) -> Vec<Option<&str>> {
    if session.contexts_by_entry.is_empty() {
        vec![None]
    } else {
        session
            .contexts_by_entry
            .keys()
            .map(|k| Some(k.as_str()))
            .collect()
    }
}

/// Resolve a teaching `p#` teaching symbol (or pass through an already-canonical wire name).
pub fn resolve_wire_field_token(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    token: &str,
) -> String {
    let t = token.trim();
    if t.is_empty() {
        return String::new();
    }
    let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    if let Some(wire) = map.resolve_ident(t) {
        return wire.to_string();
    }
    if let Some(qe) = qe {
        if let Ok(cgs) =
            crate::catalog_ownership::resolve_cgs_for_entity(session, qe.entity.as_str(), None)
        {
            if let Some(ent) = cgs.get_entity(qe.entity.as_str()) {
                if ent.fields.contains_key(t) || ent.relations.contains_key(t) {
                    return t.to_string();
                }
                let sym = map.ident_sym_entity_field(qe.entity.as_str(), t);
                if sym != t {
                    if let Some(wire) = map.resolve_ident(&sym) {
                        return wire.to_string();
                    }
                }
            }
        }
    }
    t.to_string()
}

/// Resolve optional projection / postfix field list tokens to wire names.
pub fn resolve_wire_field_list(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    fields: &[String],
) -> Vec<String> {
    fields
        .iter()
        .map(|f| resolve_wire_field_token(session, symbol_map_cross_cache, qe, f))
        .collect()
}

/// Symbol map aligned with [`PromptPipelineConfig::expand_expr_for_session_with_optional_exposure`]
/// and HTTP execute (`symbol_map_cross_cache` when available).
pub fn symbol_map_for_plasm_surface_parse(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
) -> Arc<SymbolMap> {
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
    pipeline: &PromptPipelineConfig,
    line: &str,
    program_nodes: Option<&BTreeSet<String>>,
    for_each_row_context: bool,
) -> Result<ParsedExpr, ParseError> {
    let expanded = pipeline.expand_expr_for_session_with_optional_exposure(
        line,
        session.cgs.as_ref(),
        &session.entities,
        session.teaching_exposure.as_ref(),
    );
    let layers = session_cgs_layers(session);
    let layer_entry_ids = session_layer_catalog_entry_ids(session);
    let sym_map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let mut parsed = parse_with_cgs_layers_program(
        &expanded,
        &layers,
        sym_map,
        program_nodes,
        for_each_row_context,
        Some(&layer_entry_ids),
    )?;
    normalize_query_capabilities_for_session(session, &mut parsed.expr).map_err(|message| {
        ParseError {
            kind: plasm_core::expr_parser::ParseErrorKind::Other { message },
            offset: 0,
        }
    })?;
    Ok(parsed)
}

/// Expand teaching gloss tokens the same way as [`parse_plasm_surface_line_program`], for program
/// lowering paths that peel postfix or slice field lists before building [`Plan`](crate::plasm_plan::Plan) IR.
///
/// Call this **before** any step that turns comma-separated field names into
/// [`FieldPath`](crate::plasm_plan::FieldPath) literals. The DAG compiler wraps the expanded text
/// in [`crate::plasm_dag::ExpandedProgramSurface`] at the program-node boundary.
pub fn expand_program_surface_for_session_lower(
    session: &ExecuteSession,
    pipeline: &PromptPipelineConfig,
    fragment: &str,
) -> String {
    pipeline.expand_expr_for_session_with_optional_exposure(
        fragment,
        session.cgs.as_ref(),
        &session.entities,
        session.teaching_exposure.as_ref(),
    )
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
    scoped.entities = ctx
        .cgs
        .entities
        .keys()
        .map(|name| name.as_str().to_string())
        .collect();
    let focus = [q.entity.as_str()];
    scoped.teaching_exposure = Some(TeachingExposureSession::new(
        ctx.cgs.as_ref(),
        q.entry_id.as_str(),
        &focus,
    ));
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
                Err(_) => plasm_core::QualifiedEntityKey::new(
                    es.entry_id.clone(),
                    e.reference.entity_type.to_string(),
                ),
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
    pipeline: &PromptPipelineConfig,
    source_line: &str,
    err: &ParseError,
) -> String {
    let expanded = expand_program_surface_for_session_lower(session, pipeline, source_line);
    let sym_map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let step = render_parse_error_with_feedback(
        err,
        &expanded,
        source_line.trim(),
        session.cgs.as_ref(),
        FeedbackStyle::SymbolicLlm {
            map: sym_map.as_ref(),
        },
    );
    format!("{err}\n\n{}", step.correction)
}
