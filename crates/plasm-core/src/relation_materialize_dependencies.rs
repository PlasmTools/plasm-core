//! Static capability requirements shared by admission and runtime preflight.
//! This is the union of permitted read paths; row/cache-dependent selection stays at runtime.
use crate::{RelationMaterialization, RelationScopedFallback, CGS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Source,
    Target,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityDependency {
    pub owner: Owner,
    pub capability: String,
}
#[derive(Debug, thiserror::Error)]
pub enum DependencyError {
    #[error("relation capability `{capability}` is missing from {owner:?} catalogue")]
    MissingCapability { owner: Owner, capability: String },
    #[error("relation target `{entity}` has no get capability")]
    MissingGet { entity: String },
    #[error("relation view `{view}` is unavailable")]
    MissingView { view: String },
}

pub fn resolve(
    source: &CGS,
    target: &CGS,
    parent: &str,
    entity: &str,
    materialize: Option<&RelationMaterialization>,
) -> Result<Vec<CapabilityDependency>, DependencyError> {
    let mut result = Vec::new();
    let mut add = |owner: Owner, name: &str| {
        let cgs = match owner {
            Owner::Source => source,
            Owner::Target => target,
        };
        if cgs.get_capability(name).is_none() {
            return Err(DependencyError::MissingCapability {
                owner,
                capability: name.into(),
            });
        }
        result.push(CapabilityDependency {
            owner,
            capability: name.into(),
        });
        Ok(())
    };
    let mut get = |owner: Owner, name: &str, required: bool| {
        let cgs = match owner {
            Owner::Source => source,
            Owner::Target => target,
        };
        match cgs.primary_get_capability(name) {
            Some(cap) => add(owner, cap.name.as_str()),
            None if !required => Ok(()),
            None => Err(DependencyError::MissingGet {
                entity: name.into(),
            }),
        }
    };
    match materialize {
        Some(RelationMaterialization::FromParentGet { .. }) => {
            get(Owner::Source, parent, false)?;
            get(Owner::Target, entity, false)?;
        }
        Some(RelationMaterialization::PreferFromParentGet { fallback, .. }) => {
            get(Owner::Source, parent, false)?;
            let capability = match fallback {
                RelationScopedFallback::QueryScoped { capability, .. }
                | RelationScopedFallback::QueryScopedBindings { capability, .. } => capability,
                RelationScopedFallback::HydrateFromEmbedPath { get_capability, .. } => {
                    get_capability
                }
            };
            add(Owner::Target, capability.as_str())?;
        }
        Some(
            RelationMaterialization::QueryScoped { capability, .. }
            | RelationMaterialization::QueryScopedBindings { capability, .. }
            | RelationMaterialization::GetScopedBindings { capability, .. },
        ) => add(Owner::Target, capability.as_str())?,
        Some(RelationMaterialization::ViewEmbed { view }) => {
            let definition = source
                .views
                .get(view.as_str())
                .ok_or_else(|| DependencyError::MissingView { view: view.clone() })?;
            add(Owner::Source, &definition.capability)?;
        }
        None | Some(RelationMaterialization::Unavailable) => get(Owner::Target, entity, true)?,
    }
    Ok(result)
}
