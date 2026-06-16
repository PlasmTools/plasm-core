//! Normalize + plan-kind + CML compile gates shared by dry preview and live line execute.

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{
    normalize_expr_query_capabilities, normalize_expr_query_capabilities_federated, CapabilityKind,
    Expr,
};
use plasm_runtime::preflight_compile_expr;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{PlanNodeKind, ValidatedSurfaceNode};

/// Normalize query capabilities the same way as HTTP `parse_plasm_line_for_session`.
pub fn prepare_parsed_expr_for_dispatch(
    federation_es: &ExecuteSession,
    scoped_es: &ExecuteSession,
    parsed: &ParsedExpr,
) -> Result<ParsedExpr, String> {
    let mut expr = parsed.expr.clone();
    if let Some(ref fed) = federation_es.federation_dispatch() {
        normalize_expr_query_capabilities_federated(&mut expr, fed.as_ref(), scoped_es.cgs.as_ref())
    } else {
        normalize_expr_query_capabilities(&mut expr, scoped_es.cgs.as_ref())
    }
    .map_err(|e| e.to_string())?;
    Ok(ParsedExpr {
        expr,
        projection: parsed.projection.clone(),
    })
}

pub fn ensure_surface_expr_matches_plan_kind(
    es: &ExecuteSession,
    surface: &ValidatedSurfaceNode,
    pe: &ParsedExpr,
    index: usize,
) -> Result<(), String> {
    let Expr::Query(query) = &pe.expr else {
        if surface.kind == PlanNodeKind::Search {
            return Err(format!(
                "plan.nodes[{index}] is kind search but did not parse to a search query expression"
            ));
        }
        return Ok(());
    };
    let Some(name) = query.capability_name.as_deref() else {
        if surface.kind == PlanNodeKind::Search {
            return Err(format!(
                "plan.nodes[{index}] is kind search but expression did not resolve a search capability"
            ));
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
        return Err(format!(
            "plan.nodes[{index}] references unknown capability {name:?}"
        ));
    };
    match (surface.kind, cap.kind) {
        (PlanNodeKind::Search, CapabilityKind::Search) => Ok(()),
        (PlanNodeKind::Search, other) => Err(format!(
            "plan.nodes[{index}] is kind search but expression resolved capability {name:?} with kind {other:?}"
        )),
        (PlanNodeKind::Query, CapabilityKind::Search) => Err(format!(
            "plan.nodes[{index}] is kind query but expression resolved search capability {name:?}; use a `search` plan node (kind `search`) or a non-search query per teaching table"
        )),
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

/// Normalize, plan-kind match, and CML compile after [`PlasmPreflight::preflight_parsed_line`].
pub fn preflight_surface_dispatch_after_typecheck(
    federation_es: &ExecuteSession,
    scoped_es: &ExecuteSession,
    surface: &ValidatedSurfaceNode,
    parsed: &ParsedExpr,
    step_idx: usize,
) -> Result<ParsedExpr, String> {
    let label = format!("plan.nodes[{step_idx}]");
    let normalized = prepare_parsed_expr_for_dispatch(federation_es, scoped_es, parsed)?;
    ensure_surface_expr_matches_plan_kind(scoped_es, surface, &normalized, step_idx)?;
    let cgs = compile_dispatch_cgs(federation_es, scoped_es, surface);
    let ambient = federation_es.view_ambient();
    preflight_compile_expr(&normalized.expr, cgs, &ambient).map_err(|e| format!("{label}: {e}"))?;
    Ok(normalized)
}

/// CML compile gate for a single parsed line (after typecheck / placeholder / projection gates).
pub fn preflight_line_compile_dispatch(
    federation_es: &ExecuteSession,
    scoped_es: &ExecuteSession,
    parsed: &ParsedExpr,
    label: &str,
    cgs: &plasm_core::CGS,
) -> Result<ParsedExpr, String> {
    let normalized = prepare_parsed_expr_for_dispatch(federation_es, scoped_es, parsed)?;
    let ambient = federation_es.view_ambient();
    preflight_compile_expr(&normalized.expr, cgs, &ambient).map_err(|e| format!("{label}: {e}"))?;
    Ok(normalized)
}
