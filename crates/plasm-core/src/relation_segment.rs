//! Unified relation-segment resolution for navigation (`.wire`, `r#`, homograph `p#`).

use crate::identity::RelationName;
use crate::schema::RelationSchema;
use crate::symbol_tuning::{SymbolMap, SymbolSession};
use indexmap::IndexMap;
use std::fmt;

/// Binding label on the LHS of `label = receiver.segment` (DAG compile context).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramBindingLabel<'a>(pub &'a str);

impl<'a> ProgramBindingLabel<'a> {
    #[inline]
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

/// Inputs for [`resolve_relation_segment`].
pub struct RelationSegmentContext<'a> {
    pub map: &'a dyn SymbolSession,
    pub entity: &'a str,
    pub relations: &'a IndexMap<RelationName, RelationSchema>,
    pub binding_label: Option<ProgramBindingLabel<'a>>,
    pub allow_lhs_coercion: bool,
}

/// Result of classifying a dotted relation segment on a receiver entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationSegmentOutcome {
    /// Resolved catalog relation wire name.
    Wire(String),
    /// Bare `p#` (or wire) names a field/param that homographs a relation — not allowed in nav.
    WrongRole {
        sym: String,
        wire: String,
    },
    NotFound,
}

/// Shared user-facing message for homograph `p#` in relation position.
pub fn relation_segment_wrong_role_message(sym: &str, wire: &str, entity: &str) -> String {
    format!(
        "'{sym}' is a query parameter or field ('{wire}'), not a relation on '{entity}'; use '.{wire}' or an r# symbol from the teaching table"
    )
}

/// Resolve a relation continuation segment: wire → session `r#` → optional LHS gate → homograph check.
pub fn resolve_relation_segment(
    ctx: &RelationSegmentContext<'_>,
    segment: &str,
) -> RelationSegmentOutcome {
    if ctx.relations.contains_key(segment) {
        return RelationSegmentOutcome::Wire(segment.to_string());
    }
    if SymbolMap::is_opaque_r_sym(segment) {
        if let Ok(binding) = ctx.map.resolve_session_relation(segment) {
            if binding.source_entity.as_str() == ctx.entity
                && ctx.relations.contains_key(binding.relation_wire.as_str())
            {
                return RelationSegmentOutcome::Wire(binding.relation_wire.to_string());
            }
        }
    }
    if ctx.allow_lhs_coercion {
        if let Some(label) = ctx.binding_label {
            if ctx.relations.contains_key(label.as_str()) {
                return RelationSegmentOutcome::Wire(label.as_str().to_string());
            }
        }
    }
    if SymbolMap::is_opaque_p_sym(segment) {
        return RelationSegmentOutcome::NotFound;
    }
    RelationSegmentOutcome::NotFound
}

impl fmt::Display for RelationSegmentOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wire(w) => write!(f, "wire:{w}"),
            Self::WrongRole { sym, wire } => write!(f, "wrong_role:{sym}→{wire}"),
            Self::NotFound => write!(f, "not_found"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;
    use crate::symbol_tuning::TeachingExposureSession;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn github_issue_label_map() -> (Arc<crate::CGS>, SymbolMap) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs =
            Arc::new(load_schema_dir(&root.join("../../apis/github")).expect("github apis/github"));
        let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["Issue"]);
        let map = exp.symbol_map_arc();
        let owned = (*map).clone();
        (cgs, owned)
    }

    fn issue_relations(cgs: &crate::CGS) -> &IndexMap<RelationName, RelationSchema> {
        &cgs.entities.get("Issue").expect("Issue").relations
    }

    #[test]
    fn wire_and_r_symbol_resolve() {
        let (cgs, map) = github_issue_label_map();
        let rels = issue_relations(&cgs);
        let ctx = RelationSegmentContext {
            map: &map,
            entity: "Issue",
            relations: rels,
            binding_label: None,
            allow_lhs_coercion: false,
        };
        assert!(matches!(
            resolve_relation_segment(&ctx, "labels"),
            RelationSegmentOutcome::Wire(w) if w == "labels"
        ));
        let r_sym = map.ident_sym_relation_for("github", "Issue", "labels");
        assert!(matches!(
            resolve_relation_segment(&ctx, r_sym.as_str()),
            RelationSegmentOutcome::Wire(w) if w == "labels"
        ));
    }

    #[test]
    fn lhs_coercion_ignores_wrong_p_token() {
        let (cgs, map) = github_issue_label_map();
        let rels = issue_relations(&cgs);
        let ctx = RelationSegmentContext {
            map: &map,
            entity: "Issue",
            relations: rels,
            binding_label: Some(ProgramBindingLabel("labels")),
            allow_lhs_coercion: true,
        };
        assert!(matches!(
            resolve_relation_segment(&ctx, "p99"),
            RelationSegmentOutcome::Wire(w) if w == "labels"
        ));
    }

    #[test]
    fn bare_homograph_p_without_lhs_is_wrong_role() {
        let (cgs, map) = github_issue_label_map();
        let rels = issue_relations(&cgs);
        let labels_wire = map.ident_sym_cap_param_for("github", "Issue", "issue_query", "labels");
        assert_eq!(
            labels_wire, "labels",
            "labels filter param teaches as catalog wire name"
        );
        let ctx = RelationSegmentContext {
            map: &map,
            entity: "Issue",
            relations: rels,
            binding_label: None,
            allow_lhs_coercion: false,
        };
        match resolve_relation_segment(&ctx, labels_wire.as_str()) {
            RelationSegmentOutcome::Wire(w) => {
                assert_eq!(w, "labels");
            }
            other => panic!("expected Wire(labels) for homograph filter wire, got {other:?}"),
        }
    }
}
