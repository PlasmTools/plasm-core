//! Compile-time surface guards for Plasm programs: derive RHS traps, literal no-ops, `.content` policy.

use crate::plasm_plan::PlanValue;
use crate::program_binding::ContinuationCapability;

pub(crate) const DERIVE_MAP_RELATION_HOP_MSG: &str = "Plural relation reads use `child = source => _.r#` (the taught relation symbol from the active TSV), not a bare `r#` applicator. `=>` accepts only derive maps `{ … }`, renders `<<TAG`, per-row effects `Entity.m#(…, _)`, or row relations `_.r#`.";

/// Reject `source => rhs` when `rhs` looks like a relation hop (teaching `r#` or known wire), not derive/write.
pub(crate) fn reject_relation_arrow_trap(fragment: &str) -> Result<(), String> {
    let line = fragment.trim();
    if !line.contains("=>") {
        return Ok(());
    }
    let Ok(Some((_left, right))) = plasm_core::expr_parser::split_token_top_level(line, "=>")
    else {
        return Ok(());
    };
    let rhs = right.trim();
    if rhs.starts_with("_.") {
        return Ok(());
    }
    if rhs_text_looks_like_relation_hop_trap(rhs, &[]) {
        Err(derive_map_invalid_rhs_err(Some(rhs)))
    } else {
        Ok(())
    }
}

fn rhs_text_looks_like_relation_hop_trap(rhs: &str, source_relation_wires: &[String]) -> bool {
    let t = rhs.trim();
    if t.is_empty() || t.starts_with('{') || t.contains('(') {
        return false;
    }
    dotted_tail_looks_like_relation_hop(t, source_relation_wires)
}

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
        PlanValue::NodeSymbol { path, .. } | PlanValue::BindingSymbol { path, .. }
            if path.first().is_some_and(|seg| {
                path_segment_looks_like_relation_hop(seg.as_str(), source_relation_wires)
            }) =>
        {
            return Err(derive_map_relation_hop_err());
        }
        PlanValue::NodeSymbol { .. } | PlanValue::BindingSymbol { .. } => {}
        _ => {}
    }
    Ok(())
}

pub(crate) fn derive_map_invalid_rhs_err(sample: Option<&str>) -> String {
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
        (_, ContinuationCapability::RenderContentScalar) => agent_program_error(
            format!("Don't bind `{label}.content` as a surface expression."),
            Some(format!(
                "Pass `param={label}.content` into a capability string slot, or return `{label}` for the generated-text row."
            )),
        ),
        _ => agent_program_error(
            format!(
                "`.content` exists only on row-to-text template bindings (`label = source => <<TAG`) — `{label}` is not one."
            ),
            Some(format!(
                "Plain heredoc / string bindings are already strings — pass `param={label}` (not `{label}.content`). Entity field dots use the taught wire name when the binding is a row."
            )),
        ),
    }
}

/// True when `path` is a `.content` stitch that is lawful only on render bindings.
pub(crate) fn path_is_render_content_stitch(path: &[impl AsRef<str>]) -> bool {
    path.first().is_some_and(|s| s.as_ref() == "content")
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
    fn reject_relation_arrow_trap_bare_teaching_r_hash() {
        let err = reject_relation_arrow_trap("pika => e2.r3").unwrap_err();
        assert!(err.contains("Plural relation reads use"), "{err}");
    }

    #[test]
    fn reject_relation_arrow_trap_skips_non_relation_dotted_rhs() {
        reject_relation_arrow_trap("src => binding.field").unwrap();
    }

    #[test]
    fn reject_relation_arrow_trap_skips_write_effect_rhs() {
        reject_relation_arrow_trap("items => e1.m1(p1=1)").unwrap();
    }

    #[test]
    fn quoted_relation_text_remains_literal() {
        let wires: Vec<String> = Vec::new();
        let value = PlanValue::Literal {
            value: plasm_core::operand_binding::ResolvedValue::from_wire(serde_json::json!(
                "e2.r2"
            ))
            .expect("literal data"),
        };
        reject_derive_map_invalid_rhs(&value, &wires).unwrap();
    }

    #[test]
    fn rejects_wire_relation_on_node_symbol_path() {
        let wires = vec!["lines".to_string()];
        let value = PlanValue::NodeSymbol {
            node: "hits".into(),
            alias: "hits".into(),
            path: vec!["lines".into()],
        };
        reject_derive_map_invalid_rhs(&value, &wires).unwrap_err();
    }

    #[test]
    fn bare_json_root_is_literal_noop() {
        assert!(is_bare_literal_noop_root(r#"{"foo":"bar"}"#));
        reject_bare_literal_noop_root(r#"{"foo":"bar"}"#).unwrap_err();
    }
}
