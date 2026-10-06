//! Surface parse and typecheck.

use super::*;
use plasm_core::error_render::{render_parse_error_with_feedback, FeedbackStyle};

use plasm_core::cgs_federation::{cgs_layer_stack_from_contexts, CgsLayer};
use plasm_core::symbol_tuning::CatalogScope;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProgramSurfaceParseError {
    #[error("invalid program surface: {0}")]
    Parse(#[from] Box<ParseError>),
    #[error(transparent)]
    PhraseIdent(#[from] plasm_core::phrase_ident::PhraseIdentError),
}

#[derive(Debug, Error)]
pub enum WireFieldTokenError {
    #[error(transparent)]
    CatalogOwnership(#[from] crate::catalog_ownership::CatalogOwnershipError),
    #[error("entity `{entity}` is not defined in catalog `{entry_id}`")]
    EntityNotFound { entity: String, entry_id: String },
    #[error("{0}")]
    Symbol(#[source] Box<plasm_core::symbol_tuning::SymbolResolveError>),
    #[error("field token `{token}` requires a row binding context")]
    MissingBindingContext { token: String },
}

impl From<ParseError> for ProgramSurfaceParseError {
    fn from(error: ParseError) -> Self {
        Self::Parse(Box::new(error))
    }
}

impl From<plasm_core::symbol_tuning::SymbolResolveError> for WireFieldTokenError {
    fn from(error: plasm_core::symbol_tuning::SymbolResolveError) -> Self {
        Self::Symbol(Box::new(error))
    }
}

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

/// Resolve a teaching `p#` token (or pass through a wire name) for a known row entity.
pub fn resolve_wire_field_token(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    token: &str,
) -> Result<String, WireFieldTokenError> {
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
        .map_err(WireFieldTokenError::CatalogOwnership)?;
        let ent = cgs.get_entity(qe.entity.as_str()).ok_or_else(|| {
            WireFieldTokenError::EntityNotFound {
                entity: qe.entity.to_string(),
                entry_id: qe.entry_id.to_string(),
            }
        })?;
        return map
            .resolve_entity_field(
                CatalogScope::qualified(qe.entry_id.as_str()),
                qe.entity.as_str(),
                ent,
                t,
            )
            .map_err(WireFieldTokenError::from);
    }
    if plasm_core::symbol_tuning::SymbolMap::is_opaque_p_sym(t) {
        return Err(WireFieldTokenError::MissingBindingContext {
            token: t.to_string(),
        });
    }
    Ok(t.to_string())
}

/// Resolve optional projection / postfix field list tokens to wire names.
pub fn resolve_wire_field_list(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    fields: &[String],
) -> Result<Vec<String>, WireFieldTokenError> {
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
) -> Result<(), plasm_core::QueryCapabilityResolveError> {
    if session.contexts_by_entry.len() <= 1 {
        normalize_expr_query_capabilities(expr, session.cgs.as_ref())
    } else if let Some(exposure) = session.teaching_exposure.as_ref() {
        let fed = FederationDispatch::from_contexts_and_exposure(
            session.contexts_by_entry.clone(),
            exposure,
        );
        normalize_expr_query_capabilities_federated(expr, &fed, session.cgs.as_ref())
    } else {
        normalize_expr_query_capabilities(expr, session.cgs.as_ref())
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
    normalize_query_capabilities_for_session(session, &mut parsed.expr).map_err(|source| {
        ParseError {
            kind: plasm_core::expr_parser::ParseErrorKind::QueryResolution { source },
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
) -> Result<ParsedExpr, ProgramSurfaceParseError> {
    let mut parsed = parse_plasm_surface_line_program(
        session,
        symbol_map_cross_cache,
        pipeline,
        line,
        Some(program_labels),
        for_each_row_context,
    )?;
    lower_program_phrase_idents_in_parsed(session, &mut parsed, program_labels, node_id)?;
    Ok(parsed)
}

pub(crate) fn lower_program_phrase_idents_in_parsed(
    session: &ExecuteSession,
    parsed: &mut ParsedExpr,
    program_labels: &BTreeSet<String>,
    _node_id: Option<&str>,
) -> Result<(), plasm_core::phrase_ident::PhraseIdentError> {
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
    phrase_result
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Plasm program node targets catalog {entry_id:?}, but that catalog is not loaded in this execute session")]
pub struct SessionCatalogNotLoaded {
    pub entry_id: String,
}

pub(crate) fn entry_scoped_execute_session(
    session: &ExecuteSession,
    qualified_entity: Option<&QualifiedEntityKey>,
) -> Result<ExecuteSession, SessionCatalogNotLoaded> {
    let Some(q) = qualified_entity else {
        return Ok(session.clone());
    };
    if session.contexts_by_entry.len() <= 1 && session.entry_id == q.entry_id {
        return Ok(session.clone());
    }
    let ctx =
        session
            .contexts_by_entry
            .get(&q.entry_id)
            .ok_or_else(|| SessionCatalogNotLoaded {
                entry_id: q.entry_id.clone(),
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

pub(crate) fn row_identities_from_entities<'a>(
    es: &ExecuteSession,
    entity: &str,
    entities: impl IntoIterator<Item = &'a plasm_runtime::CachedEntity>,
) -> Vec<Option<plasm_core::RowIdentity>> {
    entities
        .into_iter()
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum RowIdentityPropagationError {
    #[error("row correspondence length {correspondences} does not match output rows {rows}")]
    CorrespondenceLengthMismatch { correspondences: usize, rows: usize },
    #[error("compute source `{source_id}` is not materialized")]
    SourceNotMaterialized { source_id: String },
    #[error("union source `{source_id}` is not materialized")]
    UnionSourceNotMaterialized { source_id: String },
    #[error(transparent)]
    InvalidSourceId(#[from] crate::plasm_plan::PlanAtomError),
    #[error("row correspondence {index} has no source identity slot (available {slots})")]
    MissingIdentitySlot { index: usize, slots: usize },
}

pub(crate) fn propagate_row_identities(
    source: &PlanNodeId,
    op: &ComputeOp,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    occurrences: &[Option<usize>],
    output_len: usize,
) -> Result<Vec<Option<plasm_core::RowIdentity>>, RowIdentityPropagationError> {
    if occurrences.len() != output_len {
        return Err(RowIdentityPropagationError::CorrespondenceLengthMismatch {
            correspondences: occurrences.len(),
            rows: output_len,
        });
    }
    let left = materialized.get(source).ok_or_else(|| {
        RowIdentityPropagationError::SourceNotMaterialized {
            source_id: source.to_string(),
        }
    })?;
    let right = if let ComputeOp::Union { other } = op {
        Some(
            materialized
                .get(&PlanNodeId::new(other.as_str())?)
                .ok_or_else(|| RowIdentityPropagationError::UnionSourceNotMaterialized {
                    source_id: other.to_string(),
                })?,
        )
    } else {
        None
    };
    if right.is_some_and(|right| right.qualified_entity != left.qualified_entity) {
        return Ok(vec![None; occurrences.len()]);
    }
    let identities: Vec<_> = left
        .row_identities
        .iter()
        .chain(
            right
                .into_iter()
                .flat_map(|node| node.row_identities.iter()),
        )
        .collect();
    occurrences
        .iter()
        .map(|occurrence| match occurrence {
            None => Ok(None),
            Some(index) => identities
                .get(*index)
                .map(|identity| (*identity).clone())
                .ok_or(RowIdentityPropagationError::MissingIdentitySlot {
                    index: *index,
                    slots: identities.len(),
                }),
        })
        .collect()
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
            return msg.to_string();
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
    if step.correction.is_empty() {
        err.to_string()
    } else {
        step.correction
    }
}

#[cfg(test)]
mod footprint_tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn boxed_parse_source_preserves_position_and_concrete_cause() {
        assert!(std::mem::size_of::<ProgramSurfaceParseError>() < 128);
        let source = ParseError {
            kind: plasm_core::expr_parser::ParseErrorKind::ExpectedIdentifier,
            offset: 7,
        };
        let expected_display = format!("invalid program surface: {source}");
        let error = ProgramSurfaceParseError::from(source);
        assert_eq!(error.to_string(), expected_display);
        let cause = error
            .source()
            .unwrap()
            .downcast_ref::<Box<ParseError>>()
            .unwrap()
            .as_ref();
        assert_eq!(cause.offset, 7);
        assert!(matches!(
            cause.kind,
            plasm_core::expr_parser::ParseErrorKind::ExpectedIdentifier
        ));
    }

    #[test]
    fn boxed_symbol_source_preserves_metadata_and_display() {
        use plasm_core::symbol_tuning::SymbolResolveError;
        assert!(std::mem::size_of::<WireFieldTokenError>() < 128);
        let source = SymbolResolveError::UnknownEntityPSym {
            catalog_entry_id: "fixture".into(),
            entity: "FixtureEntity".into(),
            token: "p1".into(),
        };
        let expected_display = source.to_string();
        let error = WireFieldTokenError::from(source);
        assert_eq!(error.to_string(), expected_display);
        assert!(matches!(
            error.source().unwrap().downcast_ref::<Box<SymbolResolveError>>().map(Box::as_ref),
            Some(SymbolResolveError::UnknownEntityPSym { catalog_entry_id, entity, token })
                if catalog_entry_id == "fixture" && entity == "FixtureEntity" && token == "p1"
        ));
    }
}
