//! List-backed keyed Gets (`derive:` on `kind: get`) — plan type and schema validation.

use crate::error::SchemaError;
use crate::identity::CapabilityName;
use crate::schema::{CapabilityKind, CapabilitySchema, CGS};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

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
            detail: "plan references unknown get capability".into(),
        })?;
    if get_cap.kind != CapabilityKind::Get {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            detail: format!(
                "outer capability must be kind: get (got {:?})",
                get_cap.kind
            ),
        });
    }
    if get_cap.mapping.is_some() {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            detail: "derived get must have mapping: None (no CML transport)".into(),
        });
    }
    if plan.get_capability.as_str() != cap_name {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            detail: format!(
                "plan get_capability `{}` must match capability name `{cap_name}`",
                plan.get_capability
            ),
        });
    }
    let source =
        cgs.capabilities
            .get(&plan.source_query)
            .ok_or_else(|| SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                detail: format!("unknown source query `{}`", plan.source_query),
            })?;
    if source.kind != CapabilityKind::Query {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            detail: format!(
                "source `{}` must be kind: query (got {:?})",
                plan.source_query, source.kind
            ),
        });
    }
    let target_ent =
        cgs.get_entity(get_cap.domain.as_str())
            .ok_or_else(|| SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                detail: format!("unknown get entity `{}`", get_cap.domain),
            })?;
    if plan.identity_field != target_ent.id_field.as_str() {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            detail: format!(
                "identity_field `{}` must equal entity id_field `{}`",
                plan.identity_field, target_ent.id_field
            ),
        });
    }
    let source_ent =
        cgs.get_entity(source.domain.as_str())
            .ok_or_else(|| SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                detail: format!("unknown source entity `{}`", source.domain),
            })?;
    if !source_ent.fields.contains_key(plan.match_field.as_str()) {
        return Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            detail: format!(
                "match_field `{}` is not a field on source entity `{}`",
                plan.match_field, source.domain
            ),
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
                detail: format!(
                    "projection missing target field `{target_field}` required by provides"
                ),
            });
        };
        if !target_ent.fields.contains_key(target_field.as_str()) {
            return Err(SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                detail: format!(
                    "projection target `{target_field}` is not a field on `{}`",
                    get_cap.domain
                ),
            });
        }
        if !source_ent.fields.contains_key(source_field.as_str()) {
            return Err(SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                detail: format!(
                    "projection source `{source_field}` is not a field on `{}`",
                    source.domain
                ),
            });
        }
    }
    for (target_field, _) in &plan.projection {
        if !provides.iter().any(|p| p == target_field) {
            return Err(SchemaError::DerivedGetInvalid {
                capability: cap_name.to_string(),
                detail: format!("projection includes `{target_field}` which is not in provides"),
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
            detail: "capability must not have both mapping and derived".into(),
        }),
        (None, None) => Err(SchemaError::DerivedGetInvalid {
            capability: cap_name.to_string(),
            detail: "capability must have exactly one of mapping or derived".into(),
        }),
    }
}
