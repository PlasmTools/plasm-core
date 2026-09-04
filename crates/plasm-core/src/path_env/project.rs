//! Project [`Ref`] identity onto identity-env var names.

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::expr::{EntityKey, IdentitySlot, Ref};
use crate::path_env::names::{
    identity_env_var_names, is_identity_projectable, IdentityEnvVars, SoleAliasPolicy,
};
use crate::resolved_identity::{IdentityProjectionCtx, ResolvedIdentity};
use crate::schema::{CapabilitySchema, EntityDef};
use crate::CapabilityKind;
use crate::Value;

/// Slots projected from identity onto identity-env var names.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CmlIdentityEnv {
    pub slots: IndexMap<String, Value>,
}

/// Full identity materialization + path/GQL identity-env projection (one `from_ref`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CapabilityIdentityProjection {
    pub identity: ResolvedIdentity,
    pub path_env: CmlIdentityEnv,
}

/// Projection failures when a required var cannot be filled from identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathEnvProjectionError {
    MissingPathVar { capability: String, var: String },
    /// Var is neither identity-projectable nor declared — invent / body-splat heresy at project time.
    UncoveredPathVar { capability: String, var: String },
}

impl std::fmt::Display for PathEnvProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingPathVar { capability, var } => {
                write!(
                    f,
                    "capability `{capability}` path var `{var}` unresolved from entity identity"
                )
            }
            Self::UncoveredPathVar { capability, var } => {
                write!(
                    f,
                    "capability `{capability}`: CML path/identity var `{var}` is not identity-projectable and not a declared input"
                )
            }
        }
    }
}

impl std::error::Error for PathEnvProjectionError {}

/// Project `vars` from a precomputed [`ResolvedIdentity`] under `policy`.
///
/// - **Identity-projectable** vars must resolve from the Ref (or sole-alias), else [`MissingPathVar`]
///   (pathless nullary may skip).
/// - **Declared** inputs are omitted from slots (session/overlay fills them).
/// - Anything else is [`UncoveredPathVar`] (fail closed — same law as pack prove).
pub fn project_identity_onto_vars(
    reference: &Ref,
    ctx: IdentityProjectionCtx<'_>,
    resolved: &ResolvedIdentity,
    vars: &IdentityEnvVars,
    policy: SoleAliasPolicy,
    declared_inputs: &HashSet<String>,
    capability_name: &str,
) -> Result<CmlIdentityEnv, PathEnvProjectionError> {
    if vars.is_empty() {
        return Ok(CmlIdentityEnv::default());
    }

    let ent = match ctx {
        IdentityProjectionCtx::Entity(e) => Some(e),
        IdentityProjectionCtx::LegacyIdOnly => None,
    };
    let primary = reference.primary_slot_str();
    let id_val = if reference.is_pathless_nullary() {
        None
    } else if matches!(&reference.key, EntityKey::Simple(IdentitySlot::Lit(_))) {
        Some(Value::String(primary.clone()))
    } else {
        resolved.get_value("id").cloned()
    };

    let allow_alias = matches!(policy, SoleAliasPolicy::AllowedOnSimpleKey);
    let single_alias = (allow_alias && vars.len() == 1).then(|| vars.as_slice()[0].clone());
    let mut slots = IndexMap::new();

    for var_name in vars.iter() {
        let resolved_v = if var_name == "id" {
            id_val.clone().or_else(|| resolved.get_value("id").cloned())
        } else {
            resolved.get_value(var_name).cloned().or_else(|| {
                single_alias
                    .as_ref()
                    .filter(|name| name.as_str() == var_name)
                    .and_then(|_| id_val.clone())
            })
        };
        if let Some(v) = resolved_v {
            slots.insert(var_name.to_string(), v);
            continue;
        }

        if reference.is_pathless_nullary() {
            continue;
        }
        if declared_inputs.contains(var_name) {
            continue;
        }

        let projectable = match ent {
            Some(e) => is_identity_projectable(e, vars, var_name, policy),
            None => false,
        };
        if projectable {
            return Err(PathEnvProjectionError::MissingPathVar {
                capability: capability_name.to_string(),
                var: var_name.to_string(),
            });
        }
        return Err(PathEnvProjectionError::UncoveredPathVar {
            capability: capability_name.to_string(),
            var: var_name.to_string(),
        });
    }

    Ok(CmlIdentityEnv { slots })
}

/// Declared capability input names (scope / selection / controls / arguments / payload).
#[must_use]
pub fn capability_declared_input_names(cap: &CapabilitySchema) -> HashSet<String> {
    cap.input_fields().map(|f| f.name.to_string()).collect()
}

/// Materialize identity once and project the capability's identity-env vars.
pub fn project_capability_identity_env(
    cap: &CapabilitySchema,
    reference: &Ref,
    ctx: IdentityProjectionCtx<'_>,
) -> Result<CapabilityIdentityProjection, PathEnvProjectionError> {
    let vars = cap
        .mapping
        .as_ref()
        .map(|m| identity_env_var_names(&m.template.0))
        .unwrap_or_default();
    let declared = capability_declared_input_names(cap);
    let policy = cap.kind.domain_path_env_alias_policy();
    let identity = ResolvedIdentity::from_ref(reference, ctx);
    let path_env = project_identity_onto_vars(
        reference,
        ctx,
        &identity,
        &vars,
        policy,
        &declared,
        cap.name.as_str(),
    )?;
    Ok(CapabilityIdentityProjection { identity, path_env })
}

/// True when Create identity-env vars are fully projectable from the anchor entity.
///
/// `anchor_ref`: `None` = schema index (name-set only); `Some` = parse-time projection.
/// Both paths share the same identity-projectable predicate — no invent skip asymmetry.
#[must_use]
pub fn create_binds_from_anchor_identity(
    cap: &CapabilitySchema,
    anchor: &EntityDef,
    anchor_ref: Option<&Ref>,
) -> bool {
    if cap.kind != CapabilityKind::Create {
        return false;
    }
    let Some(mapping) = &cap.mapping else {
        return false;
    };
    let vars = identity_env_var_names(&mapping.template.0);
    if vars.is_empty() {
        return false;
    }
    // Same law as pack prove for Create: no invent sole-alias — wire names only.
    let policy = SoleAliasPolicy::Forbidden;
    if !vars
        .iter()
        .all(|pv| is_identity_projectable(anchor, &vars, pv, policy))
    {
        return false;
    }
    match anchor_ref {
        None => true,
        Some(reference) => {
            let empty = HashSet::new();
            let ctx = IdentityProjectionCtx::Entity(anchor);
            let identity = ResolvedIdentity::from_ref(reference, ctx);
            project_identity_onto_vars(
                reference,
                ctx,
                &identity,
                &vars,
                policy,
                &empty,
                cap.name.as_str(),
            )
            .is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{CapabilityName, EntityFieldName, EntityName};
    use crate::schema::{CapabilityMapping, CapabilityTemplateJson};
    use indexmap::IndexMap as Im;

    fn pet_entity() -> EntityDef {
        EntityDef {
            name: EntityName::from("Pet"),
            description: String::new(),
            id_field: EntityFieldName::from("name"),
            id_format: None,
            id_from: None,
            fields: Im::new(),
            relations: Im::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        }
    }

    fn create_cap(path_vars: &[&str]) -> CapabilitySchema {
        let mut segments = vec![serde_json::json!({"type": "literal", "value": "things"})];
        for pv in path_vars {
            segments.push(serde_json::json!({"type": "var", "name": pv}));
        }
        let mut cap = CapabilitySchema::minimal_test();
        cap.name = CapabilityName::from("toy_create");
        cap.kind = CapabilityKind::Create;
        cap.domain = EntityName::from("Toy");
        cap.mapping = Some(CapabilityMapping {
            template: CapabilityTemplateJson(serde_json::json!({
                "method": "POST",
                "path": segments,
            })),
        });
        cap
    }

    #[test]
    fn create_bind_rejects_invent_on_both_index_and_parse() {
        let anchor = pet_entity();
        let cap = create_cap(&["owner", "pet_id"]);
        assert!(!create_binds_from_anchor_identity(&cap, &anchor, None));
        let reference = Ref::new("Pet", "fluffy");
        assert!(!create_binds_from_anchor_identity(
            &cap,
            &anchor,
            Some(&reference)
        ));
    }

    #[test]
    fn create_bind_accepts_id_field_path() {
        let anchor = pet_entity();
        let cap = create_cap(&["name"]);
        assert!(create_binds_from_anchor_identity(&cap, &anchor, None));
        let reference = Ref::new("Pet", "fluffy");
        assert!(create_binds_from_anchor_identity(
            &cap,
            &anchor,
            Some(&reference)
        ));
    }

    #[test]
    fn create_bind_rejects_sole_invent_alias() {
        let anchor = pet_entity();
        let cap = create_cap(&["pet_id"]);
        assert!(!create_binds_from_anchor_identity(&cap, &anchor, None));
    }

    #[test]
    fn project_fails_closed_on_uncovered_var() {
        let ent = pet_entity();
        let vars = IdentityEnvVars::from(vec!["pet_id".into(), "extra".into()]);
        let empty = HashSet::new();
        let reference = Ref::new("Pet", "x");
        let ctx = IdentityProjectionCtx::Entity(&ent);
        let identity = ResolvedIdentity::from_ref(&reference, ctx);
        let err = project_identity_onto_vars(
            &reference,
            ctx,
            &identity,
            &vars,
            SoleAliasPolicy::AllowedOnSimpleKey,
            &empty,
            "pet_get",
        )
        .expect_err("uncovered");
        assert!(matches!(
            err,
            PathEnvProjectionError::UncoveredPathVar { .. }
        ));
    }
}
