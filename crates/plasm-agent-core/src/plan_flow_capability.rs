//! Capability name resolution for plan flow analysis.

use crate::approval_gate::operation_name_for_kind;
use crate::plan_flow::QualifiedCapabilityKey;
use crate::plasm_plan::{PlanNodeKind, PlanResultUse, ValidatedSurfaceNode};
use plasm_core::Expr;

pub fn capability_name_from_expr(expr: &serde_json::Value) -> Option<String> {
    expr.get("capability")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| expr.get("op").and_then(|v| v.as_str()).map(str::to_string))
}

pub fn resolved_mutation_capability_name(
    template_expr: Option<&serde_json::Value>,
    kind: PlanNodeKind,
) -> String {
    template_expr
        .and_then(capability_name_from_expr)
        .unwrap_or_else(|| operation_name_for_kind(kind).to_string())
}

pub fn surface_capability_key(surface: &ValidatedSurfaceNode) -> Option<QualifiedCapabilityKey> {
    let q = surface.qualified_entity.as_ref()?;
    let cap_name = surface
        .ir
        .as_ref()
        .and_then(|ir| capability_from_plasm_expr(&ir.expr))
        .or_else(|| {
            surface
                .ir_template
                .as_ref()
                .and_then(|t| capability_name_from_expr(&t.expr))
        })
        .unwrap_or_else(|| operation_name_for_kind(surface.kind).to_string());
    Some(QualifiedCapabilityKey::from_parts(
        q.entry_id.as_str(),
        q.entity.as_str(),
        cap_name.as_str(),
    ))
}

pub fn resolve_alias_node(uses_result: &[PlanResultUse], alias: &str) -> Option<String> {
    uses_result
        .iter()
        .find(|u| u.r#as == alias)
        .map(|u| u.node.clone())
}

fn capability_from_plasm_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Query(q) => q
            .capability_name
            .as_ref()
            .map(|c| c.as_str().to_string())
            .or_else(|| Some(format!("{}_query", pascal_to_snake(q.entity.as_str())))),
        Expr::Get(g) => g
            .capability_name
            .as_ref()
            .map(|c| c.as_str().to_string())
            .or_else(|| {
                Some(format!(
                    "{}_get",
                    pascal_to_snake(g.reference.entity_type.as_str())
                ))
            }),
        Expr::Invoke(i) => Some(i.capability.as_str().to_string()),
        Expr::Create(c) => Some(c.capability.as_str().to_string()),
        Expr::Delete(d) => Some(d.capability.as_str().to_string()),
        _ => None,
    }
}

/// PascalCase / camelCase entity wire names → snake_case capability prefixes
/// (`Issue` → `issue`, `PullRequest` → `pull_request`).
pub fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            for lower in c.to_lowercase() {
                out.push(lower);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_to_snake_matches_catalog_capability_prefixes() {
        assert_eq!(pascal_to_snake("Issue"), "issue");
        assert_eq!(pascal_to_snake("PullRequest"), "pull_request");
        assert_eq!(pascal_to_snake("IssueComment"), "issue_comment");
    }
}
