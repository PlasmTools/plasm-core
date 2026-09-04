//! Pack-time fail-closed path-env coverage proof.

use crate::path_env::names::{
    identity_env_var_names, identity_wire_names, is_identity_projectable, IdentityWireNames,
};
use crate::path_env::project::capability_declared_input_names;
use crate::schema::{CapabilitySchema, EntityDef, CGS};

/// Pack-time coverage failures (invented transport names, unknown domain).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathEnvProofError {
    UnknownDomain {
        capability: String,
        domain: String,
    },
    UncoveredPathVar {
        capability: String,
        entity: String,
        var: String,
        identity_names: IdentityWireNames,
    },
}

impl std::fmt::Display for PathEnvProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDomain { capability, domain } => {
                write!(
                    f,
                    "capability `{capability}`: unknown domain entity `{domain}`"
                )
            }
            Self::UncoveredPathVar {
                capability,
                entity,
                var,
                identity_names,
            } => {
                write!(
                    f,
                    "capability `{capability}`: CML path/identity var `{var}` is not projectable from entity `{entity}` identity {:?} and is not a declared capability input — invented `{{entity}}_id` / body-splat transport names are forbidden; rename the path var to `id_field`/`key_vars` or declare it as an input",
                    identity_names.as_sorted_vec()
                )
            }
        }
    }
}

impl std::error::Error for PathEnvProofError {}

/// Pack-time proof against a resolved domain entity.
pub fn prove_path_env_coverage(
    cap: &CapabilitySchema,
    ent: &EntityDef,
) -> Result<(), PathEnvProofError> {
    let Some(mapping) = &cap.mapping else {
        return Ok(());
    };
    let vars = identity_env_var_names(&mapping.template.0);
    if vars.is_empty() {
        return Ok(());
    }

    let declared = capability_declared_input_names(cap);
    let identity = identity_wire_names(ent);
    let policy = cap.kind.domain_path_env_alias_policy();

    for var in vars.iter() {
        if is_identity_projectable(ent, &vars, var, policy) {
            continue;
        }
        if declared.contains(var) {
            continue;
        }
        return Err(PathEnvProofError::UncoveredPathVar {
            capability: cap.name.to_string(),
            entity: ent.name.to_string(),
            var: var.to_string(),
            identity_names: identity,
        });
    }
    Ok(())
}

/// Resolve domain entity from `cgs` then prove — never vacuously Ok on missing domain.
pub fn prove_path_env_coverage_in_cgs(
    cgs: &CGS,
    cap: &CapabilitySchema,
) -> Result<(), PathEnvProofError> {
    let Some(ent) = cgs.get_entity(cap.domain.as_str()) else {
        return Err(PathEnvProofError::UnknownDomain {
            capability: cap.name.to_string(),
            domain: cap.domain.to_string(),
        });
    };
    prove_path_env_coverage(cap, ent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{CapabilityName, EntityFieldName, EntityName};
    use crate::path_env::names::SoleAliasPolicy;
    use crate::schema::{CapabilityMapping, CapabilitySchema, CapabilityTemplateJson};
    use crate::CapabilityKind;
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

    fn get_cap(path_vars: &[&str]) -> CapabilitySchema {
        let mut segments = vec![serde_json::json!({"type": "literal", "value": "things"})];
        for pv in path_vars {
            segments.push(serde_json::json!({"type": "var", "name": pv}));
        }
        let mut cap = CapabilitySchema::minimal_test();
        cap.name = CapabilityName::from("pet_get");
        cap.kind = CapabilityKind::Get;
        cap.domain = EntityName::from("Pet");
        cap.mapping = Some(CapabilityMapping {
            template: CapabilityTemplateJson(serde_json::json!({
                "method": "GET",
                "path": segments,
            })),
        });
        cap
    }

    #[test]
    fn rejects_multi_var_invent() {
        let err = prove_path_env_coverage(&get_cap(&["owner", "pet_id"]), &pet_entity())
            .expect_err("invent");
        assert!(err.to_string().contains("not projectable"));
    }

    #[test]
    fn accepts_id_field_and_single_alias() {
        prove_path_env_coverage(&get_cap(&["name"]), &pet_entity()).expect("id_field");
        let mut team = pet_entity();
        team.name = EntityName::from("Team");
        team.id_field = EntityFieldName::from("key");
        let mut cap = get_cap(&["slug"]);
        cap.domain = EntityName::from("Team");
        prove_path_env_coverage(&cap, &team).expect("alias");
        assert_eq!(
            CapabilityKind::Create.domain_path_env_alias_policy(),
            SoleAliasPolicy::Forbidden
        );
    }

    #[test]
    fn create_rejects_sole_alias() {
        let mut cap = get_cap(&["slug"]);
        cap.kind = CapabilityKind::Create;
        prove_path_env_coverage(&cap, &pet_entity()).expect_err("no alias");
    }
}
