//! Agent-ergonomics rewrites on parsed [`Expr`] trees.

use crate::cgs_federation::CgsLayer;
use crate::expr::{EntityKey, Expr, GetExpr, IdentitySlot, QueryExpr, Ref};
use crate::predicate::Predicate;
use crate::schema::{CapabilityKind, CGS};
use crate::typed_literal::TypedComparisonValue;
use crate::value::Value;
use crate::CompOp;

/// Identity brace → Get lowering failed for a recognized Get-identity form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityLoweringError {
    /// Exact `id_field` brace but multiple Get capabilities and no primary.
    AmbiguousGet { entity: String },
    /// Owning CGS could not be resolved for a stamped/ambiguous entity.
    UnresolvedCatalog { entity: String },
    /// Identity value could not be stringified.
    InvalidIdentity { entity: String, detail: String },
}

impl std::fmt::Display for IdentityLoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AmbiguousGet { entity } => write!(
                f,
                "identity brace on `{entity}` is ambiguous (multiple Gets; set primary_read)"
            ),
            Self::UnresolvedCatalog { entity } => write!(
                f,
                "identity brace on `{entity}`: cannot resolve owning catalog"
            ),
            Self::InvalidIdentity { entity, detail } => {
                write!(f, "identity brace on `{entity}`: {detail}")
            }
        }
    }
}

impl std::error::Error for IdentityLoweringError {}

/// When `Entity{id_field=value}` is a sole equality on the entity's `id_field`, lower to Get.
///
/// - Exact identity + owning Get → [`Expr::Get`] (catalog stamp preserved)
/// - Exact identity + ambiguous Gets → [`IdentityLoweringError`]
/// - Exact identity but **no** Get → left as Query (id_field may be a selection slot)
/// - Non-identity braces → left as Query (RA-2 applies later)
pub fn lower_id_field_brace_to_get(expr: Expr, cgs: &CGS) -> Result<Expr, IdentityLoweringError> {
    let layer = CgsLayer::new(cgs.entry_id.as_deref().unwrap_or_default(), cgs);
    lower_id_field_brace_to_get_federated(expr, &[layer])
}

/// Federated lowering: resolve owning CGS from stamp / unique entity across layers.
pub fn lower_id_field_brace_to_get_federated(
    expr: Expr,
    layers: &[CgsLayer<'_>],
) -> Result<Expr, IdentityLoweringError> {
    match expr {
        Expr::Query(q) => {
            let Some(cgs) = resolve_query_cgs(&q, layers)? else {
                return Ok(Expr::Query(q));
            };
            if let Some(get) = try_brace_query_to_get(&q, cgs)? {
                Ok(Expr::Get(get))
            } else {
                Ok(Expr::Query(q))
            }
        }
        Expr::Chain(ch) => {
            let source = lower_id_field_brace_to_get_federated(*ch.source, layers)?;
            Ok(Expr::Chain(crate::expr::ChainExpr {
                source: Box::new(source),
                selector: ch.selector,
                catalog_entry_id: ch.catalog_entry_id,
                step: ch.step,
            }))
        }
        other => Ok(other),
    }
}

fn resolve_query_cgs<'a>(
    q: &QueryExpr,
    layers: &'a [CgsLayer<'a>],
) -> Result<Option<&'a CGS>, IdentityLoweringError> {
    let entity = q.entity.as_str();
    if let Some(eid) = q.catalog_entry_id.as_deref() {
        for layer in layers {
            if layer.matches_forward_entry_id(eid) && layer.cgs().get_entity(entity).is_some() {
                return Ok(Some(layer.cgs()));
            }
        }
        return Err(IdentityLoweringError::UnresolvedCatalog {
            entity: entity.to_string(),
        });
    }
    let matches: Vec<_> = layers
        .iter()
        .map(CgsLayer::cgs)
        .filter(|c| c.get_entity(entity).is_some())
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        _ => Err(IdentityLoweringError::UnresolvedCatalog {
            entity: entity.to_string(),
        }),
    }
}

/// True when `pred` is an exact sole equality on `field` (optionally wrapped in a single-arg `And`).
pub fn predicate_is_sole_field_eq(pred: &Predicate, field: &str) -> bool {
    let Some((eq_field, _)) = single_eq_field(pred) else {
        return false;
    };
    eq_field == field && pred_references_only_field(pred, field)
}

fn try_brace_query_to_get(
    q: &QueryExpr,
    cgs: &CGS,
) -> Result<Option<GetExpr>, IdentityLoweringError> {
    if q.capability_name.is_some() {
        return Ok(None);
    }
    let Some(pred) = q.predicate.as_ref() else {
        return Ok(None);
    };
    let Some(ent) = cgs.get_entity(q.entity.as_str()) else {
        return Ok(None);
    };
    if !predicate_is_sole_field_eq(pred, ent.id_field.as_str()) {
        return Ok(None);
    }
    let Some((_, value)) = single_eq_field(pred) else {
        return Ok(None);
    };

    let gets = cgs.find_capabilities(&q.entity, CapabilityKind::Get);
    // No Get: `id_field` may still be a Query selection wire (e.g. Spotify Profile{email=…}).
    // Fail closed only when a Get exists but cannot be chosen (ambiguous) or identity is invalid.
    if gets.is_empty() {
        return Ok(None);
    }
    if gets.len() > 1 && ent.primary_read.is_none() {
        return Err(IdentityLoweringError::AmbiguousGet {
            entity: q.entity.to_string(),
        });
    }

    let slot = identity_slot_from_predicate_value(&value).ok_or_else(|| {
        IdentityLoweringError::InvalidIdentity {
            entity: q.entity.to_string(),
            detail: "identity value must be a scalar".into(),
        }
    })?;

    let mut get = GetExpr::from_ref(Ref {
        entity_type: q.entity.clone(),
        key: EntityKey::Simple(slot),
    });
    get.catalog_entry_id = q.catalog_entry_id.clone();
    if let Some(pid) = ent.primary_read.as_ref() {
        get.capability_name = Some(crate::identity::CapabilityName::from(pid.as_str()));
    }
    Ok(Some(get))
}

fn pred_references_only_field(pred: &Predicate, field: &str) -> bool {
    match pred {
        Predicate::Comparison { field: f, .. } => f == field,
        Predicate::And { args } => args.iter().all(|p| pred_references_only_field(p, field)),
        Predicate::Or { args } => args.len() == 1 && pred_references_only_field(&args[0], field),
        Predicate::Not { .. } => false,
        Predicate::True | Predicate::False => false,
        Predicate::ExistsRelation { .. } => false,
    }
}

fn single_eq_field(pred: &Predicate) -> Option<(&str, Value)> {
    match pred {
        Predicate::Comparison {
            field,
            op: CompOp::Eq,
            value,
        } => Some((field.as_str(), typed_to_value(value))),
        Predicate::And { args } if args.len() == 1 => single_eq_field(&args[0]),
        _ => None,
    }
}

fn typed_to_value(v: &TypedComparisonValue) -> Value {
    v.to_value()
}

fn identity_slot_from_predicate_value(v: &Value) -> Option<IdentitySlot> {
    match v {
        Value::String(s) => Some(IdentitySlot::lit(s.clone())),
        Value::Integer(i) => Some(IdentitySlot::lit(i.to_string())),
        Value::Bool(b) => Some(IdentitySlot::lit(b.to_string())),
        Value::Float(f) => Some(IdentitySlot::lit(f.to_string())),
        Value::PlasmInputRef(r) => Some(IdentitySlot::binding(r.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgs_federation::CgsLayer;
    use crate::loader::load_schema_dir;
    use crate::CatalogEntryStamp;

    #[test]
    fn single_catalog_parse_preserves_bound_catalog_for_brace_lowering() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix_views");
        let mut cgs = load_schema_dir(&dir).unwrap();
        cgs.bind_registry_entry_id("bound-matrix");
        let parsed = crate::expr_parser::parse(r#"LangKeyPick{key="k1"}"#, &cgs).unwrap();
        let Expr::Get(get) = parsed.expr else {
            panic!("expected Get")
        };
        assert_eq!(get.reference.primary_slot_str(), "k1");
        assert_eq!(get.catalog_entry_id.as_deref(), Some("bound-matrix"));
    }

    #[test]
    fn rewrite_issue_identifier_brace_to_get() {
        let dir = std::path::Path::new("../../apis/linear");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let q = QueryExpr::filtered("Issue", Predicate::eq("identifier", "EVA-60"));
        let expr = lower_id_field_brace_to_get(Expr::Query(q), &cgs).unwrap();
        match expr {
            Expr::Get(g) => assert_eq!(g.reference.primary_slot_str(), "EVA-60"),
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn account_password_brace_lowers_to_get() {
        let dir = std::path::Path::new("../../apis/appworld/supervisor");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let q = QueryExpr::filtered("AccountPassword", Predicate::eq("account_name", "alice"));
        let expr = lower_id_field_brace_to_get(Expr::Query(q), &cgs).unwrap();
        match expr {
            Expr::Get(g) => assert_eq!(g.reference.primary_slot_str(), "alice"),
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn lang_key_pick_brace_and_paren_reach_same_get_ir() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let q = QueryExpr::filtered("LangKeyPick", Predicate::eq("key", "k1"));
        let braced = lower_id_field_brace_to_get(Expr::Query(q), &cgs).unwrap();
        match braced {
            Expr::Get(g) => assert_eq!(g.reference.primary_slot_str(), "k1"),
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn federated_identity_brace_preserves_non_layer0_stamp() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let stamp = CatalogEntryStamp::from_opt_str(Some("layer1"));
        let mut q = QueryExpr::filtered("LangKeyPick", Predicate::eq("key", "k1"));
        q.catalog_entry_id = stamp.clone();
        let layers = [CgsLayer::new("layer1", &cgs)];
        let expr = lower_id_field_brace_to_get_federated(Expr::Query(q), &layers).unwrap();
        match expr {
            Expr::Get(g) => {
                assert_eq!(g.reference.primary_slot_str(), "k1");
                assert_eq!(g.catalog_entry_id, stamp);
            }
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn identity_brace_without_get_stays_query() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_prompt_matrix");
        if !dir.exists() {
            return;
        }
        let Ok(cgs) = load_schema_dir(dir) else {
            return;
        };
        let Some(ent) = cgs.entities.values().find(|e| {
            !e.id_field.as_str().is_empty()
                && cgs
                    .find_capabilities(&e.name, CapabilityKind::Get)
                    .is_empty()
        }) else {
            return;
        };
        let id_field = ent.id_field.as_str();
        let q = QueryExpr::filtered(ent.name.as_str(), Predicate::eq(id_field, "x"));
        let expr = lower_id_field_brace_to_get(Expr::Query(q), &cgs).unwrap();
        assert!(
            matches!(expr, Expr::Query(_)),
            "without Get, sole id_field brace must remain Query, got {expr:?}"
        );
    }

    #[test]
    fn federated_parse_identity_brace_on_layer1_is_get() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let layers = [CgsLayer::new("layer1", &cgs)];
        let mut q = QueryExpr::filtered("LangKeyPick", Predicate::eq("key", "k1"));
        q.catalog_entry_id = CatalogEntryStamp::from_opt_str(Some("layer1"));
        let expr = lower_id_field_brace_to_get_federated(Expr::Query(q), &layers).unwrap();
        assert!(matches!(expr, Expr::Get(_)));
    }

    #[test]
    fn identity_brace_field_path_lowers_to_binding_get() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let hole = crate::PlasmInputRef::node_output("sess", vec!["key".into()]);
        let q = QueryExpr::filtered(
            "LangKeyPick",
            Predicate::eq("key", Value::PlasmInputRef(hole.clone())),
        );
        let expr = lower_id_field_brace_to_get(Expr::Query(q), &cgs).unwrap();
        match expr {
            Expr::Get(g) => {
                assert!(
                    matches!(
                        &g.reference.key,
                        EntityKey::Simple(IdentitySlot::Binding(crate::PlasmInputRef::NodeInput { node, path }))
                            if node == "sess" && path.as_slice() == ["key"]
                    ),
                    "named identity brace with a field path must be Get Binding, got {:?}",
                    g.reference.key
                );
            }
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn matrix_views_loads_derived_lang_key_pick_get() {
        let dir = std::path::Path::new("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.exists() {
            return;
        }
        let cgs = load_schema_dir(dir).unwrap();
        let cap = cgs
            .capabilities
            .get("lang_key_pick_get")
            .expect("lang_key_pick_get");
        assert!(
            cap.derived.is_some(),
            "expected derived Get plan for lang_key_pick_get"
        );
        assert!(cap.mapping.is_none());
        let plan = cap.derived.as_ref().unwrap();
        assert_eq!(plan.source_query.as_str(), "langitem_query");
        assert_eq!(plan.match_field.as_str(), "id");
    }
}
