//! Render parsed [`Expr`] trees back to Plasm wire surface syntax (inverse of parse).
//!
//! Display / teaching validation only — **never** call before [`crate::expr_parser::parse`] ingress.

mod predicates;
mod render;
mod values;

use crate::cgs_federation::FederationDispatch;
use crate::expr::Expr;
use crate::schema::CGS;
use crate::symbol_tuning::{
    strip_prompt_expression_annotations, SymbolMap, TeachingExposureSession,
};
use std::sync::Arc;

use render::render_expr_wire;

/// Render one expression to canonical wire surface (entity/field/method wire names from IR).
pub fn render_expr_surface(expr: &Expr, cgs: &CGS) -> String {
    render_expr_wire(expr, cgs, None, cgs)
}

/// Federated sessions: resolve per-entity [`CGS`] for capability/search rendering.
pub fn render_expr_surface_federated(
    expr: &Expr,
    fed: &FederationDispatch,
    fallback: &CGS,
) -> String {
    render_expr_wire(expr, fallback, Some(fed), fallback)
}

/// Parse opaque teaching line with session map, render wire surface. `None` when parse fails.
pub fn wire_surface_from_teaching_line(
    line: &str,
    cgs: &CGS,
    map: Arc<SymbolMap>,
) -> Option<String> {
    let stripped = strip_prompt_expression_annotations(line);
    let layers = [cgs];
    let parsed = match crate::expr_parser::parse_with_cgs_layers(&stripped, &layers, map) {
        Ok(p) => p,
        Err(_) => return None,
    };
    Some(render_expr_surface(&parsed.expr, cgs))
}

/// Session-aware wire surface from opaque line. `None` when parse fails.
pub fn wire_surface_from_teaching_session_line(
    line: &str,
    session: &TeachingExposureSession,
) -> Option<String> {
    let stripped = strip_prompt_expression_annotations(line.trim());
    let map = session.symbol_map_arc();
    let layers = session_cgs_layers(session);
    if layers.is_empty() {
        return None;
    }
    match crate::expr_parser::parse_with_cgs_layers(&stripped, &layers, map) {
        Ok(parsed) => Some(render_expr_wire(&parsed.expr, layers[0], None, layers[0])),
        Err(_) => None,
    }
}

pub(crate) fn session_cgs_layers(session: &TeachingExposureSession) -> Vec<&CGS> {
    use std::collections::HashSet;
    let mut out: Vec<&CGS> = Vec::new();
    let mut seen = HashSet::new();
    for eid in &session.entity_catalog_entry_ids {
        if seen.insert(eid.as_str()) {
            if let Some(cgs) = session.catalog_cgs_for_entry(eid) {
                out.push(cgs);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr_parser::parse;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::{
        entity_slices_for_render, FocusSpec, SymbolMap, TeachingExposureSession,
    };

    #[test]
    fn wire_render_roundtrips_petstore_get() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let wire = "Pet(42)";
        let parsed = parse(wire, &cgs).unwrap();
        let back = render_expr_surface(&parsed.expr, &cgs);
        assert_eq!(back, wire);
    }

    #[test]
    fn wire_surface_from_opaque_petstore() {
        let dir = std::path::Path::new("../../fixtures/schemas/petstore");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
        let map = SymbolMap::build(&cgs, &full);
        let pet = map.entity_sym("Pet");
        let opaque = format!("{pet}(42)", pet = pet);
        let wire = wire_surface_from_teaching_line(&opaque, &cgs, Arc::new(map)).expect("wire");
        assert_eq!(wire, "Pet(42)");
    }

    #[test]
    fn wire_surface_dotted_create_clickup() {
        let dir = std::path::Path::new("../../apis/clickup");
        if !dir.exists() {
            return;
        }
        let cgs = match load_schema_dir(dir) {
            Ok(c) => c,
            Err(_) => return,
        };
        let surface = "Team(11111).team-create-space(name=\"Sprint Sandbox\")";
        let parsed = parse(surface, &cgs).unwrap();
        let back = render_expr_surface(&parsed.expr, &cgs);
        assert_eq!(back, surface);
    }

    #[test]
    fn wire_surface_opaque_dotted_invoke_proof() {
        let dir = std::path::Path::new("../../apis/proof");
        if !dir.is_dir() {
            return;
        }
        let cgs = match load_schema_dir(dir) {
            Ok(c) => c,
            Err(_) => return,
        };
        let session = TeachingExposureSession::new(&cgs, "proof", &["Document"]);
        let map = session.symbol_map_arc();
        let e_sym = map.entity_sym_for("proof", "Document");
        let cap = cgs
            .get_capability("annotation_suggestion_insert")
            .expect("cap");
        let kebab = crate::schema::capability_method_label_kebab(cap);
        let m_sym = map.method_sym_for("proof", "Document", &kebab);
        let slug_sym = map.ident_sym_entity_field_for("proof", "Document", "slug");
        let agent_sym = map.ident_sym_cap_param_for(
            "proof",
            "Document",
            "annotation_suggestion_insert",
            "agent_id",
        );
        let opaque = format!(
            "{e}({slug}=\"acme\").{m}({agent}=\"bot\")",
            e = e_sym,
            slug = slug_sym,
            m = m_sym,
            agent = agent_sym,
        );
        let wire = wire_surface_from_teaching_session_line(&opaque, &session).expect("wire");
        assert!(wire.contains("slug"));
        assert!(wire.contains("agent_id"));
        assert!(wire.contains("bot"));
    }
}
