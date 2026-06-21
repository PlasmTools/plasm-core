//! Shared preflight gates for dry and live execute paths.

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{reject_domain_placeholder_in_executable, PreflightToken};

use crate::plasm_plan::ValidatedSurfaceNode;
use crate::plasm_plan_run::{dry_run_simulation_for_session, typecheck_parsed_for_session};

use super::dispatch;

/// Dry-run materialization bundle produced during preflight simulation.
#[derive(Debug, Clone, Default)]
pub struct SimulationBundle {
    pub node_count: usize,
}

/// Proof bundle that all preflight gates passed — required before live I/O.
#[derive(Debug, Clone)]
pub struct PreflightReport {
    pub typecheck: PreflightToken,
    pub placeholders: PreflightToken,
    pub projection: PreflightToken,
    pub simulation: SimulationBundle,
}

impl PreflightReport {
    pub fn token(&self) -> PreflightToken {
        self.typecheck
    }
}

/// Run federated/single-graph type-check and executable-surface gates once per expression.
pub struct PlasmPreflight;

impl PlasmPreflight {
    pub fn typecheck_parsed_for_session(
        session: &crate::execute_session::ExecuteSession,
        parsed: &ParsedExpr,
    ) -> Result<PreflightToken, String> {
        typecheck_parsed_for_session(session, parsed).map_err(|e| e.to_string())?;
        Ok(PreflightToken::VERIFIED)
    }

    pub fn validate_projection_fields(
        session: &crate::execute_session::ExecuteSession,
        parsed: &ParsedExpr,
    ) -> Result<PreflightToken, String> {
        let Some(fields) = parsed.projection.as_ref() else {
            return Ok(PreflightToken::VERIFIED);
        };
        if fields.is_empty() {
            return Ok(PreflightToken::VERIFIED);
        }
        for field in fields {
            let name = crate::plasm_plan_run::resolve_wire_field_token(session, None, None, field);
            let entity = parsed.expr.primary_entity();
            let cgs = crate::catalog_ownership::resolve_cgs_for_entity(session, entity, None)?;
            let Some(ent) = cgs.get_entity(entity) else {
                return Err(format!(
                    "entity `{entity}` is not defined in the resolved catalog"
                ));
            };
            if !ent.fields.contains_key(name.as_str()) && !ent.relations.contains_key(name.as_str())
            {
                return Err(format!(
                    "projection field `{field}` (wire `{name}`) is not declared on entity `{entity}`"
                ));
            }
        }
        Ok(PreflightToken::VERIFIED)
    }

    /// Full preflight chain shared by dry preview and live execute (dry ≡ live gates).
    pub fn preflight_parsed_line(
        session: &crate::execute_session::ExecuteSession,
        _source: &str,
        parsed: &ParsedExpr,
    ) -> Result<PreflightReport, String> {
        let typecheck = Self::typecheck_parsed_for_session(session, parsed)?;
        reject_domain_placeholder_in_executable(&parsed.expr).map_err(|e| e.to_string())?;
        let projection = Self::validate_projection_fields(session, parsed)?;
        Ok(PreflightReport {
            typecheck,
            placeholders: PreflightToken::VERIFIED,
            projection,
            simulation: SimulationBundle { node_count: 1 },
        })
    }

    /// Typecheck, normalize, plan-kind match, and CML compile — same gates as live line execute.
    pub fn preflight_node_compile_dispatch(
        es: &crate::execute_session::ExecuteSession,
        scoped_es: &crate::execute_session::ExecuteSession,
        surface: &ValidatedSurfaceNode,
        parsed: &ParsedExpr,
        step_idx: usize,
    ) -> Result<ParsedExpr, String> {
        let label = format!("plan.nodes[{step_idx}]");
        Self::preflight_parsed_line(scoped_es, &label, parsed)?;
        dispatch::preflight_surface_dispatch_after_typecheck(
            es, scoped_es, surface, parsed, step_idx,
        )
    }

    /// Dry preview: same gates as live, then intent/il/bindings simulation (no HTTP).
    pub fn dry_preview_for_line(
        session: &crate::execute_session::ExecuteSession,
        source: &str,
        parsed: &ParsedExpr,
    ) -> Result<(String, String, serde_json::Value), String> {
        Self::preflight_parsed_line(session, source, parsed)?;
        let normalized = dispatch::preflight_line_compile_dispatch(
            session,
            session,
            parsed,
            source,
            session.cgs.as_ref(),
        )?;
        Ok(dry_run_simulation_for_session(session, &normalized))
    }
}

#[cfg(test)]
mod tests {
    use crate::execute_session::ExecuteSession;
    use crate::plasm_plan_run::parse_parsed_expr_for_session;

    use super::PlasmPreflight;

    fn matrix_session() -> ExecuteSession {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = std::sync::Arc::new(
            plasm_core::load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix"))
                .expect("matrix"),
        );
        let exp = plasm_core::TeachingExposureSession::new(cgs.as_ref(), "default", &["LangItem"]);
        ExecuteSession::new(
            "ph".into(),
            "sess".into(),
            cgs.clone(),
            indexmap::IndexMap::from([(
                "default".into(),
                std::sync::Arc::new(plasm_core::CgsContext::entry("default", cgs.clone())),
            )]),
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn preflight_rejects_domain_dollar_in_get_identity() {
        let session = matrix_session();
        let parsed = parse_parsed_expr_for_session(&session, "e1($)").expect("parse");
        let err =
            PlasmPreflight::preflight_parsed_line(&session, "e1($)", &parsed).expect_err("dollar");
        assert!(err.contains('$'), "{err}");
    }

    #[test]
    fn dry_preview_matches_preflight_then_simulation() {
        let session = matrix_session();
        let pe = parse_parsed_expr_for_session(&session, "e1").expect("parse");
        let (intent, il, bindings) =
            PlasmPreflight::dry_preview_for_line(&session, "e1", &pe).expect("dry");
        assert!(!intent.is_empty());
        assert!(il.contains("LangItem"));
        assert!(bindings.is_object());
    }
}
