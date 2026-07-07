//! Relation chain navigation admissibility (type-check, parser alignment, teaching emit).

use crate::schema::{Cardinality, RelationMaterialization, RelationSchema, CGS};

/// Whether a declared relation supports `AutoGet` chain nav without requiring target Get.
///
/// Must stay aligned with [`crate::expr_parser`] many-relation AutoGet allowance
/// (`FromParentGet`, `PreferFromParentGet`, `QueryScoped`, `QueryScopedBindings`).
/// `GetScopedBindings` is excluded (parse rejects bare relation nav on that materialization).
pub(crate) fn relation_chain_nav_admissible(
    rel: &RelationSchema,
    target_entity: &str,
    cgs: &CGS,
) -> bool {
    if cgs
        .find_capability(target_entity, crate::CapabilityKind::Get)
        .is_some()
    {
        return true;
    }
    if rel.cardinality != Cardinality::Many {
        return false;
    }
    matches!(
        rel.materialize.as_ref(),
        Some(RelationMaterialization::FromParentGet { .. })
            | Some(RelationMaterialization::PreferFromParentGet { .. })
            | Some(RelationMaterialization::QueryScoped { .. })
            | Some(RelationMaterialization::QueryScopedBindings { .. })
    )
}

/// Whether relation-nav teaching rows should emit for this relation slot.
pub(crate) fn relation_nav_admissible(rel: &RelationSchema, cgs: &CGS) -> bool {
    if rel.cardinality != Cardinality::Many {
        return true;
    }
    relation_chain_nav_admissible(rel, rel.target_resource.as_str(), cgs)
}
