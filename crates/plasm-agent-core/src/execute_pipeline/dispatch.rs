//! Normalize + plan-kind + CML compile gates shared by dry preview and live line execute.

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{
    normalize_expr_query_capabilities, normalize_expr_query_capabilities_federated, CapabilityKind,
    Expr,
};
use plasm_runtime::preflight_compile_expr;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{PlanNodeKind, ValidatedSurfaceNode};

/// Semantic failures at normalization, plan-kind admission, and compile preflight.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("query capability resolution failed: {0}")]
    QueryResolution(#[from] plasm_core::QueryCapabilityResolveError),
    #[error("plan.nodes[{index}] search requires a query expression")]
    SearchRequiresQuery { index: usize },
    #[error("plan.nodes[{index}] search requires a resolved capability")]
    SearchCapabilityMissing { index: usize },
    #[error("plan.nodes[{index}] references unknown capability `{capability}`")]
    CapabilityMissing { index: usize, capability: String },
    #[error("plan.nodes[{index}] {plan_kind:?} cannot dispatch capability `{capability}` of kind {capability_kind:?}")]
    PlanKindMismatch {
        index: usize,
        plan_kind: PlanNodeKind,
        capability: String,
        capability_kind: CapabilityKind,
    },
    #[error("session graph is locked during compile preflight")]
    SessionGraphLocked,
    #[error("compiled catalog lookup failed: {0}")]
    CompiledCatalogLookup(#[from] crate::execute_session::CompiledCatalogLookupError),
    #[error("CML preflight failed: {0}")]
    Cml(#[source] plasm_compile::CmlError),
    #[error("runtime compile preflight failed: {0}")]
    RuntimePreflight(#[source] Box<plasm_runtime::RuntimeError>),
}

impl From<plasm_runtime::RuntimeError> for DispatchError {
    fn from(error: plasm_runtime::RuntimeError) -> Self {
        match error {
            plasm_runtime::RuntimeError::CmlError { source } => Self::Cml(source),
            error => Self::RuntimePreflight(Box::new(error)),
        }
    }
}

/// Normalize query capabilities the same way as HTTP `parse_plasm_line_for_session`.
pub fn prepare_parsed_expr_for_dispatch(
    federation_es: &ExecuteSession,
    scoped_es: &ExecuteSession,
    parsed: &ParsedExpr,
) -> Result<ParsedExpr, DispatchError> {
    let mut expr = parsed.expr.clone();
    if let Some(ref fed) = federation_es.federation_dispatch() {
        normalize_expr_query_capabilities_federated(&mut expr, fed.as_ref(), scoped_es.cgs.as_ref())
    } else {
        normalize_expr_query_capabilities(&mut expr, scoped_es.cgs.as_ref())
    }
    .map_err(DispatchError::QueryResolution)?;
    Ok(ParsedExpr {
        expr,
        projection: parsed.projection.clone(),
        field_dot_extract: None,
    })
}

pub fn ensure_surface_expr_matches_plan_kind(
    es: &ExecuteSession,
    surface: &ValidatedSurfaceNode,
    pe: &ParsedExpr,
    index: usize,
) -> Result<(), DispatchError> {
    let Expr::Query(query) = &pe.expr else {
        if surface.kind == PlanNodeKind::Search {
            return Err(DispatchError::SearchRequiresQuery { index });
        }
        return Ok(());
    };
    let Some(name) = query.capability_name.as_deref() else {
        if surface.kind == PlanNodeKind::Search {
            return Err(DispatchError::SearchCapabilityMissing { index });
        }
        return Ok(());
    };
    let cgs = es
        .contexts_by_entry
        .get(
            surface
                .qualified_entity
                .as_ref()
                .map(|q| q.entry_id.as_str())
                .unwrap_or(es.entry_id.as_str()),
        )
        .map(|ctx| ctx.cgs.as_ref())
        .unwrap_or(es.cgs.as_ref());
    let Some(cap) = cgs.get_capability(name) else {
        return Err(DispatchError::CapabilityMissing {
            index,
            capability: name.to_owned(),
        });
    };
    match (surface.kind, cap.kind) {
        (PlanNodeKind::Search, CapabilityKind::Search) => Ok(()),
        (PlanNodeKind::Search, _) | (PlanNodeKind::Query, CapabilityKind::Search) => {
            Err(DispatchError::PlanKindMismatch {
                index,
                plan_kind: surface.kind,
                capability: name.to_owned(),
                capability_kind: cap.kind,
            })
        }
        _ => Ok(()),
    }
}

fn compile_dispatch_cgs<'a>(
    federation_es: &'a ExecuteSession,
    scoped_es: &'a ExecuteSession,
    surface: &'a ValidatedSurfaceNode,
) -> &'a plasm_core::CGS {
    surface
        .qualified_entity
        .as_ref()
        .and_then(|q| federation_es.contexts_by_entry.get(q.entry_id.as_str()))
        .map(|ctx| ctx.cgs.as_ref())
        .unwrap_or(scoped_es.cgs.as_ref())
}

fn compile_expr_with_session_mat(
    federation_es: &ExecuteSession,
    expr: &Expr,
    cgs: &plasm_core::CGS,
) -> Result<(), DispatchError> {
    let ambient = federation_es.view_ambient();
    let guard = federation_es
        .graph_cache
        .try_lock()
        .map_err(|_| DispatchError::SessionGraphLocked)?;
    let compiled = federation_es.compiled_catalog_for_cgs(cgs)?;
    preflight_compile_expr(expr, cgs, &compiled, &ambient, &guard).map_err(DispatchError::from)
}

/// Normalize, plan-kind match, and CML compile after [`PlasmPreflight::preflight_parsed_line`].
pub fn preflight_surface_dispatch_after_typecheck(
    federation_es: &ExecuteSession,
    scoped_es: &ExecuteSession,
    surface: &ValidatedSurfaceNode,
    parsed: &ParsedExpr,
    step_idx: usize,
) -> Result<ParsedExpr, DispatchError> {
    let normalized = prepare_parsed_expr_for_dispatch(federation_es, scoped_es, parsed)?;
    ensure_surface_expr_matches_plan_kind(scoped_es, surface, &normalized, step_idx)?;
    let cgs = compile_dispatch_cgs(federation_es, scoped_es, surface);
    compile_expr_with_session_mat(federation_es, &normalized.expr, cgs)?;
    Ok(normalized)
}

/// CML compile gate for a single parsed line (after typecheck / placeholder / projection gates).
pub fn preflight_line_compile_dispatch(
    federation_es: &ExecuteSession,
    scoped_es: &ExecuteSession,
    parsed: &ParsedExpr,
    _label: &str,
    cgs: &plasm_core::CGS,
) -> Result<ParsedExpr, DispatchError> {
    let normalized = prepare_parsed_expr_for_dispatch(federation_es, scoped_es, parsed)?;
    compile_expr_with_session_mat(federation_es, &normalized.expr, cgs)?;
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program_diagnostic::{ProgramErrorCategory, ProgramStageError};
    use std::error::Error;

    #[test]
    fn compiled_catalog_lookup_cause_survives_stage_conversion() {
        let lookup = crate::execute_session::CompiledCatalogLookupError::HashMissing {
            catalog_hash: "missing-hash".into(),
        };
        let stage = ProgramStageError::from(DispatchError::from(lookup));
        let dispatch = stage.source().expect("dispatch cause");
        assert!(matches!(
            dispatch.source().and_then(|source| {
                source.downcast_ref::<crate::execute_session::CompiledCatalogLookupError>()
            }),
            Some(crate::execute_session::CompiledCatalogLookupError::HashMissing { catalog_hash })
                if catalog_hash == "missing-hash"
        ));
    }

    #[test]
    fn cml_preflight_cause_survives_stage_conversion_and_clone() {
        let error = DispatchError::from(plasm_runtime::RuntimeError::CmlError {
            source: plasm_compile::CmlError::VariableNotFound {
                name: "input".into(),
            },
        });
        let stage = ProgramStageError::from(error).clone();
        assert_eq!(stage.category(), ProgramErrorCategory::Plan);
        let dispatch = stage.source().expect("dispatch cause");
        assert!(matches!(
            dispatch.downcast_ref::<DispatchError>(),
            Some(DispatchError::Cml(_))
        ));
        assert!(matches!(
            dispatch.source().and_then(|source| source.downcast_ref::<plasm_compile::CmlError>()),
            Some(plasm_compile::CmlError::VariableNotFound { name }) if name == "input"
        ));
    }

    #[test]
    fn query_resolution_cause_survives_stage_conversion() {
        let stage = ProgramStageError::from(DispatchError::QueryResolution(
            plasm_core::QueryCapabilityResolveError::CapabilityNotFound {
                capability: "missing".into(),
                entity: "item".into(),
            },
        ));
        let dispatch = stage.source().expect("dispatch cause");
        assert!(matches!(
                dispatch.source().and_then(
                    |source| source.downcast_ref::<plasm_core::QueryCapabilityResolveError>()
                ),
                Some(plasm_core::QueryCapabilityResolveError::CapabilityNotFound { .. })
            ));
    }
}
