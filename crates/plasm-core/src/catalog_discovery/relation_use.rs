//! Typed projection of explicit graph declarations. No entity-name similarity,
//! parameter-name inference, graph traversal or operational JSON is involved.
use crate::schema::{RelationMaterialization, RelationScopedFallback};
use crate::CapabilityName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RelationReadRole {
    Resolve,
    ResolveWhenEmbeddedIncomplete,
    HydrateIdentifiedWhenEmbeddedIncomplete,
}
impl RelationReadRole {
    pub(super) fn meaning(self) -> &'static str {
        match self {
            Self::Resolve => "Resolves related records",
            Self::ResolveWhenEmbeddedIncomplete => "Resolves related records when embedded targets are absent or incomplete",
            Self::HydrateIdentifiedWhenEmbeddedIncomplete => "Hydrates already identified related records when embedded targets are absent or incomplete",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CapabilityUse<'a> {
    pub capability: &'a CapabilityName,
    pub role: RelationReadRole,
}

/// Only model variants that name a capability can establish a use of it.
/// Exhaustive implementations force new materialization forms to declare their semantics.
pub(super) trait DeclaredCapabilityUse {
    fn declared_capability_use(&self) -> Option<CapabilityUse<'_>>;
}
impl DeclaredCapabilityUse for RelationMaterialization {
    fn declared_capability_use(&self) -> Option<CapabilityUse<'_>> {
        match self {
            Self::QueryScoped { capability, .. }
            | Self::QueryScopedBindings { capability, .. }
            | Self::GetScopedBindings { capability, .. } => Some(CapabilityUse {
                capability,
                role: RelationReadRole::Resolve,
            }),
            Self::PreferFromParentGet { fallback, .. } => fallback.declared_capability_use(),
            Self::Unavailable | Self::FromParentGet { .. } | Self::ViewEmbed { .. } => None,
        }
    }
}
impl DeclaredCapabilityUse for RelationScopedFallback {
    fn declared_capability_use(&self) -> Option<CapabilityUse<'_>> {
        Some(match self {
            Self::QueryScoped { capability, .. } | Self::QueryScopedBindings { capability, .. } => {
                CapabilityUse {
                    capability,
                    role: RelationReadRole::ResolveWhenEmbeddedIncomplete,
                }
            }
            Self::HydrateFromEmbedPath { get_capability, .. } => CapabilityUse {
                capability: get_capability,
                role: RelationReadRole::HydrateIdentifiedWhenEmbeddedIncomplete,
            },
        })
    }
}
