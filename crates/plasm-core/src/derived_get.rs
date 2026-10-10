//! List-backed keyed Gets (`derive:` on `kind: get`) — plan type and schema validation.

use crate::error::SchemaError;
use crate::identity::CapabilityName;
use crate::schema::{CapabilityKind, CapabilitySchema, CGS};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, thiserror::Error)]
pub enum DerivedGetError {
    #[error("plan references unknown get capability")]
    UnknownGet,
    #[error("outer capability must be kind: get (got {actual})")]
    OuterKind { actual: CapabilityKind },
    #[error("derived get must have mapping: None (no CML transport)")]
    UnexpectedMapping,
    #[error("plan get_capability `{actual}` must match capability name `{expected}`")]
    CapabilityNameMismatch { actual: String, expected: String },
    #[error("unknown source query `{query}`")]
    UnknownSourceQuery { query: String },
    #[error("source `{query}` must be kind: query (got {actual})")]
    SourceKind {
        query: String,
        actual: CapabilityKind,
    },
    #[error("unknown {role} entity `{entity}`")]
    UnknownEntity {
        role: DerivedEntityRole,
        entity: String,
    },
    #[error("identity_field `{actual}` must equal entity id_field `{expected}`")]
    IdentityFieldMismatch { actual: String, expected: String },
    #[error("match_field `{field}` is not a field on source entity `{entity}`")]
    MatchFieldMissing { field: String, entity: String },
    #[error("projection missing target field `{field}` required by provides")]
    ProjectionMissing { field: String },
    #[error("projection {role} `{field}` is not a field on `{entity}`")]
    ProjectionFieldMissing {
        role: DerivedEntityRole,
        field: String,
        entity: String,
    },
    #[error("projection includes `{field}` which is not in provides")]
    ProjectionNotProvided { field: String },
    #[error("capability must not have both mapping and derived")]
    ConflictingBackends,
    #[error("capability must have exactly one of mapping or derived")]
    MissingBackend,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum DerivedEntityRole {
    #[error("get")]
    Target,
    #[error("source")]
    Source,
}

/// Validated list-backed Get plan (`derive:` on a `kind: get` capability).
///
/// Executes the source Query to completion, requires exactly one row whose `match_field`
/// equals the Get identity, then projects fields onto the Get entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedGetPlan {
    /// Outer Get capability id.
    pub get_capability: CapabilityName,
    /// Same-catalog Query capability that materializes candidate rows.
    pub source_query: CapabilityName,
    /// Field on the **source** query entity compared to the Get identity value.
    pub match_field: String,
    /// Target entity `id_field` (Get reference identity).
    pub identity_field: String,
    /// Target field → source field (must cover every `provides` on the Get).
    pub projection: IndexMap<String, String>,
}

/// Semantic validation for a derived Get plan attached to `cap_name`.
pub fn validate_derived_get(
    cgs: &CGS,
    cap_name: &str,
    plan: &DerivedGetPlan,
) -> Result<(), SchemaError> {
    let get_cap = cgs
        .capabilities
        .get(cap_name)
        .ok_or_else(|| SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::UnknownGet,
        })?;
    if get_cap.kind != CapabilityKind::Get {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::OuterKind {
                actual: get_cap.kind,
            },
        });
    }
    if get_cap.mapping.is_some() {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::UnexpectedMapping,
        });
    }
    if plan.get_capability.as_str() != cap_name {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::CapabilityNameMismatch {
                actual: plan.get_capability.to_string(),
                expected: cap_name.to_owned(),
            },
        });
    }
    let source =
        cgs.capabilities
            .get(&plan.source_query)
            .ok_or_else(|| SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                source: DerivedGetError::UnknownSourceQuery {
                    query: plan.source_query.to_string(),
                },
            })?;
    if source.kind != CapabilityKind::Query {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::SourceKind {
                query: plan.source_query.to_string(),
                actual: source.kind,
            },
        });
    }
    let target_ent =
        cgs.get_entity(get_cap.domain.as_str())
            .ok_or_else(|| SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                source: DerivedGetError::UnknownEntity {
                    role: DerivedEntityRole::Target,
                    entity: get_cap.domain.to_string(),
                },
            })?;
    if plan.identity_field != target_ent.id_field.as_str() {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::IdentityFieldMismatch {
                actual: plan.identity_field.clone(),
                expected: target_ent.id_field.to_string(),
            },
        });
    }
    let source_ent =
        cgs.get_entity(source.domain.as_str())
            .ok_or_else(|| SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                source: DerivedGetError::UnknownEntity {
                    role: DerivedEntityRole::Source,
                    entity: source.domain.to_string(),
                },
            })?;
    if !source_ent.fields.contains_key(plan.match_field.as_str()) {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::MatchFieldMissing {
                field: plan.match_field.clone(),
                entity: source.domain.to_string(),
            },
        });
    }
    let provides = if get_cap.provides.is_empty() {
        target_ent
            .fields
            .keys()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
    } else {
        get_cap.provides.clone()
    };
    for target_field in &provides {
        let Some(source_field) = plan.projection.get(target_field.as_str()) else {
            return Err(SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                source: DerivedGetError::ProjectionMissing {
                    field: target_field.clone(),
                },
            });
        };
        if !target_ent.fields.contains_key(target_field.as_str()) {
            return Err(SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                source: DerivedGetError::ProjectionFieldMissing {
                    role: DerivedEntityRole::Target,
                    field: target_field.clone(),
                    entity: get_cap.domain.to_string(),
                },
            });
        }
        if !source_ent.fields.contains_key(source_field.as_str()) {
            return Err(SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                source: DerivedGetError::ProjectionFieldMissing {
                    role: DerivedEntityRole::Source,
                    field: source_field.clone(),
                    entity: source.domain.to_string(),
                },
            });
        }
    }
    for (target_field, _) in &plan.projection {
        if !provides.iter().any(|p| p == target_field) {
            return Err(SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                source: DerivedGetError::ProjectionNotProvided {
                    field: target_field.clone(),
                },
            });
        }
    }
    Ok(())
}

/// Exactly one of [`CapabilitySchema::mapping`] / [`CapabilitySchema::derived`].
pub(crate) fn validate_capability_backend(
    cap_name: &str,
    cap: &CapabilitySchema,
) -> Result<(), SchemaError> {
    match (&cap.mapping, &cap.derived) {
        (Some(_), None) | (None, Some(_)) => Ok(()),
        (Some(_), Some(_)) => Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::ConflictingBackends,
        }),
        (None, None) => Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            source: DerivedGetError::MissingBackend,
        }),
    }
}
