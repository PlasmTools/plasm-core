//! Sealed lowering helpers for plan⇒run GET IR parity (digest witness + hydrate entry points).
//!
//! Hydration must preserve compound [`Ref`] keys via [`GetExpr::from_ref`] — never
//! [`Ref::primary_slot_str`] into a simple-id [`GetExpr`].

use plasm_core::{Expr, GetExpr};
use sha2::{Digest, Sha256};

/// Content-addressed digest of all lowered GET IR in a validated plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoweredIrDigest(String);

impl LoweredIrDigest {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn from_get_exprs(gets: &[GetExpr]) -> Self {
        let mut canonical: Vec<serde_json::Value> = gets
            .iter()
            .filter_map(|g| serde_json::to_value(g).ok())
            .collect();
        canonical.sort_by(|a, b| {
            serde_json::to_string(a)
                .unwrap_or_default()
                .cmp(&serde_json::to_string(b).unwrap_or_default())
        });
        let payload = serde_json::to_string(&canonical).unwrap_or_default();
        let digest = Sha256::digest(payload.as_bytes());
        Self(hex::encode(digest))
    }
}

fn collect_get_exprs(expr: &Expr, out: &mut Vec<GetExpr>) {
    match expr {
        Expr::Get(g) => out.push(g.clone()),
        Expr::Chain(c) => collect_get_exprs(&c.source, out),
        _ => {}
    }
}

/// Walk a validated program plan and hash all static GET IR (plan⇒run parity witness).
pub fn lowered_ir_digest_from_validated_plan(
    plan: &crate::plasm_plan::Plan<crate::plasm_plan::ValidatedPlanState>,
) -> LoweredIrDigest {
    use crate::plasm_plan::ValidatedPlanNode;
    let mut gets = Vec::new();
    for node in &plan.nodes {
        if let ValidatedPlanNode::Surface(surface) = node {
            if let Some(ir) = &surface.ir {
                collect_get_exprs(&ir.expr, &mut gets);
            }
            if let Some(t) = &surface.ir_template {
                if let Ok(expr) = serde_json::from_value::<Expr>(t.expr.clone()) {
                    collect_get_exprs(&expr, &mut gets);
                }
            }
        }
    }
    LoweredIrDigest::from_get_exprs(&gets)
}

#[cfg(test)]
mod tests {
    use plasm_core::{EntityKey, GetExpr, Ref};
    use std::collections::BTreeMap;

    #[test]
    fn get_expr_from_ref_preserves_compound_key() {
        let mut parts = BTreeMap::new();
        parts.insert("owner".to_string(), "ryan-s-roberts".to_string());
        parts.insert("repo".to_string(), "tool-test".to_string());
        parts.insert("name".to_string(), "bug".to_string());
        let reference = Ref::compound("Label", parts);
        let get = GetExpr::from_ref(reference.clone());
        assert!(matches!(get.reference.key, EntityKey::Compound(_)));
        assert_eq!(get.reference.entity_type.as_str(), "Label");
    }
}
