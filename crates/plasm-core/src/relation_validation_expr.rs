//! Standalone validation expressions for relation traversal (CLI validate, catalog gates).

use crate::catalog_id::CatalogEntryStamp;
use crate::expr::{ChainExpr, ChainStep, Expr, QueryExpr};
use crate::schema::{Cardinality, RelationMaterialization, RelationSchema, CGS};
use crate::{CapabilityName, EntityName, Predicate, RelationName, RelationScopedFallback, Value};

/// Build a dry/live-executable expression that exercises a many-relation hop, when possible.
pub fn relation_validation_expr(
    cgs: &CGS,
    entity_name: &EntityName,
    rel_name: &RelationName,
    rel: &RelationSchema,
) -> Option<Expr> {
    if rel.cardinality != Cardinality::Many {
        return None;
    }
    match rel
        .materialize
        .as_ref()
        .unwrap_or(&RelationMaterialization::Unavailable)
    {
        RelationMaterialization::QueryScoped { capability, param } => {
            let mut q = QueryExpr::filtered(
                rel.target_resource.clone(),
                Predicate::eq(param.as_str(), Value::String("1".into())),
            );
            q.capability_name = Some(capability.clone());
            Some(Expr::Query(q))
        }
        RelationMaterialization::QueryScopedBindings {
            capability,
            bindings,
        } => {
            let preds: Vec<Predicate> = bindings
                .keys()
                .map(|cap_param| Predicate::eq(cap_param.as_str(), Value::String("1".into())))
                .collect();
            let pred = if preds.len() == 1 {
                preds.into_iter().next().unwrap()
            } else {
                Predicate::and(preds)
            };
            let mut q = QueryExpr::filtered(rel.target_resource.clone(), pred);
            q.capability_name = Some(capability.clone());
            Some(Expr::Query(q))
        }
        RelationMaterialization::PreferFromParentGet { fallback, .. } => {
            query_expr_from_scoped_fallback(rel, fallback)
        }
        RelationMaterialization::FromParentGet { .. }
        | RelationMaterialization::GetScopedBindings { .. }
        | RelationMaterialization::Unavailable => None,
        RelationMaterialization::ViewEmbed { view } => {
            cgs.views.get(view.as_str()).map(|view_def| {
                let scope_pred = view_def
                    .scope
                    .first()
                    .map(|s| Predicate::eq(s.name.as_str(), Value::String("test-1".into())));
                let mut root_query = match scope_pred {
                    Some(p) => QueryExpr::filtered(entity_name.clone(), p),
                    None => QueryExpr::all(entity_name.clone()),
                };
                root_query.capability_name =
                    Some(CapabilityName::from(view_def.capability.as_str()));
                Expr::Chain(ChainExpr {
                    source: Box::new(Expr::Query(root_query)),
                    selector: rel_name.to_string(),
                    catalog_entry_id: CatalogEntryStamp::none(),
                    step: ChainStep::Explicit {
                        expr: Box::new(Expr::Query(QueryExpr::all(rel.target_resource.clone()))),
                    },
                })
            })
        }
    }
}

fn query_expr_from_scoped_fallback(
    rel: &RelationSchema,
    fallback: &RelationScopedFallback,
) -> Option<Expr> {
    match fallback {
        RelationScopedFallback::QueryScoped { capability, param } => {
            let mut q = QueryExpr::filtered(
                rel.target_resource.clone(),
                Predicate::eq(param.as_str(), Value::String("1".into())),
            );
            q.capability_name = Some(capability.clone());
            Some(Expr::Query(q))
        }
        RelationScopedFallback::QueryScopedBindings {
            capability,
            bindings,
        } => {
            let preds: Vec<Predicate> = bindings
                .keys()
                .map(|cap_param| Predicate::eq(cap_param.as_str(), Value::String("1".into())))
                .collect();
            let pred = if preds.len() == 1 {
                preds.into_iter().next().unwrap()
            } else {
                Predicate::and(preds)
            };
            let mut q = QueryExpr::filtered(rel.target_resource.clone(), pred);
            q.capability_name = Some(capability.clone());
            Some(Expr::Query(q))
        }
        RelationScopedFallback::HydrateFromEmbedPath { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::Expr;
    use std::path::Path;

    #[test]
    fn relation_validation_expr_builds_view_root_chain_for_view_embed() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.is_dir() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(&dir).expect("load matrix views");
        let entity = cgs.get_entity("LangTriageContext").expect("entity");
        let rel = entity.relations.get("tags").expect("tags");
        let entity_name = entity.name.clone();
        let rel_name = RelationName::from("tags");
        let expr = relation_validation_expr(&cgs, &entity_name, &rel_name, rel)
            .expect("view_embed validation expr");
        let Expr::Chain(chain) = expr else {
            panic!("expected view-root chain expr, got {expr:?}");
        };
        assert_eq!(chain.selector, "tags");
        assert!(matches!(chain.source.as_ref(), Expr::Query(_)));
    }

    #[test]
    fn relation_validation_expr_skips_from_parent_get_many_relations() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.is_dir() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(&dir).expect("load matrix views");
        let entity = cgs.get_entity("LangItem").expect("entity");
        let rel = entity.relations.get("lines").expect("lines");
        let expr = relation_validation_expr(&cgs, &entity.name, &RelationName::from("lines"), rel);
        assert!(
            expr.is_none(),
            "from_parent_get is not standalone-validatable"
        );
    }
}
