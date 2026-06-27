//! Compile-time surface guards for Plasm programs: derive RHS traps, literal no-ops, `.content` policy.

use crate::plasm_plan::PlanValue;
use crate::program_binding::ContinuationCapability;

const DERIVE_MAP_RELATION_HOP_MSG: &str = "Relation reads use `child = source.r#` (the taught relation symbol from the active TSV), not `source => …`. `=>` is for per-row derive maps `{ … }` or write effects `source => e#.m#(…)`.";

/// JSON/array/string/heredoc shapes that lower to [`DagNodeSource::Data`] when bound — not valid bare roots.
pub(crate) fn looks_like_data_literal(rhs: &str) -> bool {
    let t = rhs.trim_start();
    t.starts_with('{') || t.starts_with('[') || t.starts_with('"') || t.starts_with("<<")
}

pub(crate) fn is_bare_literal_noop_root(expr: &str) -> bool {
    looks_like_data_literal(expr.trim())
}

pub(crate) fn literal_noop_program_error() -> String {
    agent_program_error(
        "Program is a JSON/data literal only — that is a literal no-op.",
        Some(
            "Rewrite as Plasm source: bindings, entity gets, relation hops, or transforms — not a bare object/array/string literal.",
        ),
    )
}

pub(crate) fn reject_bare_literal_noop_root(expr: &str) -> Result<(), String> {
    if is_bare_literal_noop_root(expr) {
        Err(literal_noop_program_error())
    } else {
        Ok(())
    }
}

pub(crate) fn reject_derive_map_invalid_rhs(
    value: &PlanValue,
    source_relation_wires: &[String],
) -> Result<(), String> {
    match value {
        PlanValue::Literal { value } => {
            let Some(s) = value.as_str() else {
                return Ok(());
            };
            let t = s.trim();
            if derive_rhs_literal_looks_like_surface_call(t)
                || dotted_tail_looks_like_relation_hop(t, source_relation_wires)
            {
                return Err(derive_map_invalid_rhs_err(Some(t)));
            }
        }
        PlanValue::NodeSymbol { path, .. } | PlanValue::BindingSymbol { path, .. } => {
            if path.first().is_some_and(|seg| {
                path_segment_looks_like_relation_hop(seg.as_str(), source_relation_wires)
            }) {
                return Err(derive_map_relation_hop_err());
            }
        }
        _ => {}
    }
    Ok(())
}

fn derive_map_invalid_rhs_err(sample: Option<&str>) -> String {
    match sample {
        Some(t) => format!("`=>` derive map does not accept `{t}`; {DERIVE_MAP_RELATION_HOP_MSG}"),
        None => derive_map_relation_hop_err(),
    }
}

fn derive_map_relation_hop_err() -> String {
    DERIVE_MAP_RELATION_HOP_MSG.to_string()
}

fn dotted_tail_looks_like_relation_hop(s: &str, source_relation_wires: &[String]) -> bool {
    let t = s.trim();
    let Some((_, right)) = t.split_once('.') else {
        return false;
    };
    let seg = right.split('.').next().unwrap_or("").trim();
    path_segment_looks_like_relation_hop(seg, source_relation_wires)
}

fn path_segment_looks_like_relation_hop(seg: &str, source_relation_wires: &[String]) -> bool {
    teaching_relation_symbol(seg) || source_relation_wires.iter().any(|wire| wire == seg)
}

fn teaching_relation_symbol(seg: &str) -> bool {
    seg.len() > 1 && seg.starts_with('r') && seg[1..].chars().all(|c| c.is_ascii_digit())
}

fn derive_rhs_literal_looks_like_surface_call(s: &str) -> bool {
    if !s.contains('(') {
        return false;
    }
    let head = s.split('(').next().unwrap_or("").trim();
    if head.is_empty() {
        return false;
    }
    let mut chars = head.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first == 'e' && chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return true;
    }
    first.is_ascii_uppercase() && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentReferenceSite {
    ProgramRoot,
    Continuation,
}

pub(crate) fn content_reference_error(
    label: &str,
    site: ContentReferenceSite,
    continuation: ContinuationCapability,
) -> String {
    match (site, continuation) {
        (ContentReferenceSite::ProgramRoot, ContinuationCapability::RenderContentScalar) => {
            agent_program_error(
                format!("Don't return `{label}.content` as the program root."),
                Some(format!(
                    "Return `{label}` for the generated-text row, or use `{label}.content` only inside params/heredocs."
                )),
            )
        }
        _ => agent_program_error(
            format!(
                "`.content` exists only on row-to-text template bindings — `{label}` is not one."
            ),
            Some(format!(
                "Use `{label}` for row fields, or add a row-to-text template binding (`{label} = source <<TAG …`) before `.content`."
            )),
        ),
    }
}

fn agent_program_error(head: impl AsRef<str>, help: Option<impl AsRef<str>>) -> String {
    if let Some(h) = help {
        format!("{}\nhelp: {}", head.as_ref(), h.as_ref())
    } else {
        head.as_ref().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::PlanValue;

    #[test]
    fn rejects_teaching_relation_symbol_in_literal_rhs() {
        let wires: Vec<String> = Vec::new();
        let value = PlanValue::Literal {
            value: serde_json::json!("e2.r2"),
        };
        let err = reject_derive_map_invalid_rhs(&value, &wires).expect_err("r# hop");
        assert!(err.contains(DERIVE_MAP_RELATION_HOP_MSG), "{err}");
    }

    #[test]
    fn rejects_wire_relation_on_node_symbol_path() {
        let wires = vec!["lines".to_string()];
        let value = PlanValue::NodeSymbol {
            node: "hits".into(),
            alias: "hits".into(),
            path: vec!["lines".into()],
        };
        reject_derive_map_invalid_rhs(&value, &wires).expect_err("wire relation hop");
    }

    #[test]
    fn bare_json_root_is_literal_noop() {
        assert!(is_bare_literal_noop_root(r#"{"foo":"bar"}"#));
        reject_bare_literal_noop_root(r#"{"foo":"bar"}"#).expect_err("noop");
    }
}
