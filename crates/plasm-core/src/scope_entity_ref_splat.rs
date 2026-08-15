//! Expand compound [`crate::schema::FieldType::EntityRef`] **scope** parameters into per-key
//! CML env entries (object map or `a/b`-style phrase for two-part keys) before HTTP templates compile.
//!
//! Existing predicate / path keys win over splatted keys (**explicit wins**).

use crate::entity_ref_value::{
    normalize_entity_ref_value_for_target, EntityRefPayload, ScopeEntityRefNormalizeError,
};
use crate::identity::EntityFieldName;
use crate::schema::{CapabilitySchema, InputType, ParameterRole, ScopeAggregateKeyPolicy};
use crate::value::Value;
use crate::FieldType;
use crate::CGS;
use indexmap::IndexMap;

/// Apply scope splat + optional aggregate-key removal for one capability.
pub fn apply_entity_ref_scope_splat(
    env: &mut IndexMap<String, Value>,
    cgs: &CGS,
    cap: &CapabilitySchema,
) -> Result<(), ScopeEntityRefNormalizeError> {
    let Some(InputType::Object { fields, .. }) = cap.input_schema.as_ref().map(|s| &s.input_type)
    else {
        return Ok(());
    };

    for param in fields {
        if !matches!(param.role, Some(ParameterRole::Scope)) {
            continue;
        }
        let Ok(nv) = param.named_value(cgs) else {
            continue;
        };
        let FieldType::EntityRef { target } = &nv.field_type else {
            continue;
        };
        let Some(ent) = cgs.get_entity(target) else {
            continue;
        };
        if ent.key_vars.len() <= 1 {
            continue;
        }

        let aggregate_name = param.name.as_str();
        let Some(aggregate_val) = scope_aggregate_lookup(env, aggregate_name) else {
            continue;
        };

        let normalized = normalize_entity_ref_value_for_target(&aggregate_val, ent).ok_or_else(|| {
            ScopeEntityRefNormalizeError {
                param_name: aggregate_name.to_string(),
                target_entity: target.to_string(),
                message: "cannot normalize entity_ref scope value to target key_vars — supply compound keys, identifiable row fields, full_name owner/repo, or owner/repo string".into(),
            }
        })?;
        set_scope_aggregate_value(env, aggregate_name, normalized.clone());

        splat_aggregate_into_env(env, &ent.key_vars, &normalized);

        if cap.scope_aggregate_key_policy == ScopeAggregateKeyPolicy::OmitWhenRedundant
            && splat_redundant(ent.key_vars.as_slice(), env)
        {
            remove_scope_aggregate(env, aggregate_name);
        }
    }
    Ok(())
}

fn set_scope_aggregate_value(env: &mut IndexMap<String, Value>, name: &str, v: Value) {
    env.insert(name.to_string(), v.clone());
    if let Some(Value::Object(m)) = env.get_mut("input") {
        m.insert(name.to_string(), v);
    }
}

fn scope_aggregate_lookup(env: &IndexMap<String, Value>, param_name: &str) -> Option<Value> {
    env.get(param_name).cloned().or_else(|| {
        env.get("input")
            .and_then(|v| v.as_object()?.get(param_name).cloned())
    })
}

fn splat_redundant(key_vars: &[EntityFieldName], env: &IndexMap<String, Value>) -> bool {
    key_vars.iter().all(|k| env.contains_key(k.as_str()))
}

fn remove_scope_aggregate(env: &mut IndexMap<String, Value>, aggregate_name: &str) {
    env.shift_remove(aggregate_name);
    if let Some(Value::Object(m)) = env.get_mut("input") {
        m.shift_remove(aggregate_name);
    }
}

fn value_to_leaf_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) if f.is_finite() => {
            if f.fract() == 0.0 {
                Some(format!("{}", *f as i64))
            } else {
                Some(f.to_string())
            }
        }
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn splat_aggregate_into_env(
    env: &mut IndexMap<String, Value>,
    key_vars: &[EntityFieldName],
    aggregate_val: &Value,
) {
    match aggregate_val {
        Value::Object(_) => {
            let Ok(EntityRefPayload::Compound(map)) =
                EntityRefPayload::try_from_value(aggregate_val)
            else {
                return;
            };
            for k in key_vars {
                let ks = k.as_str();
                if env.contains_key(ks) {
                    continue;
                }
                if let Some(child) = map.get(ks) {
                    if let Some(s) = value_to_leaf_string(&child.to_value()) {
                        env.insert(ks.to_string(), Value::String(s));
                    }
                }
            }
        }
        Value::String(s) => {
            splat_from_string_phrase(env, key_vars, s);
        }
        Value::Integer(n) if key_vars.len() == 1 && !env.contains_key(key_vars[0].as_str()) => {
            env.insert(
                key_vars[0].as_str().to_string(),
                Value::String(n.to_string()),
            );
        }
        Value::Float(f)
            if key_vars.len() == 1
                && !env.contains_key(key_vars[0].as_str())
                && f.is_finite()
                && value_to_leaf_string(aggregate_val).is_some() =>
        {
            if let Some(s) = value_to_leaf_string(aggregate_val) {
                env.insert(key_vars[0].as_str().to_string(), Value::String(s));
            }
        }
        _ => {}
    }
}

/// Best-effort split for `owner/slug` style phrases when the target has exactly two key vars.
fn splat_from_string_phrase(
    env: &mut IndexMap<String, Value>,
    key_vars: &[EntityFieldName],
    s: &str,
) {
    if key_vars.len() != 2 {
        return;
    }
    let (a, b) = (key_vars[0].as_str(), key_vars[1].as_str());
    if env.contains_key(a) && env.contains_key(b) {
        return;
    }
    let idx = s.find('/');
    let Some(pos) = idx else {
        return;
    };
    let left = s[..pos].trim();
    let right = s[pos + 1..].trim();
    if left.is_empty() || right.is_empty() {
        return;
    }
    if !env.contains_key(a) {
        env.insert(a.to_string(), Value::String(left.to_string()));
    }
    if !env.contains_key(b) {
        env.insert(b.to_string(), Value::String(right.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{CapabilityName, EntityName};
    use crate::schema::{
        registry_test_util, CapabilityKind, CapabilityMapping, CapabilitySchema,
        CapabilityTemplateJson, FieldSchema, InputSchema, InputType, InputValidation,
        NamedValueSchema, ResourceSchema, StringSemantics,
    };
    use crate::FieldType;

    fn seed_repository_fixture(cgs: &mut CGS) {
        cgs.values.insert(
            "fx_str".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
                currency: None,
            },
        );
        cgs.values.insert(
            "fx_repo_ref".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::EntityRef {
                    target: EntityName::from("Repository"),
                },
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
                currency: None,
            },
        );
    }

    fn string_field(cgs: &CGS, name: &str) -> FieldSchema {
        registry_test_util::entity_field_from_values(cgs, "fx_str", name, true, "")
    }

    fn repo_scope_cap(cgs: &CGS, policy: ScopeAggregateKeyPolicy) -> CapabilitySchema {
        let mut repository_param = registry_test_util::object_input_field_from_values(
            cgs,
            "fx_repo_ref",
            "repository",
            true,
        );
        repository_param.role = Some(ParameterRole::Scope);
        CapabilitySchema {
            name: CapabilityName::from("repo_forks_query"),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: EntityName::from("Repository"),
            identity_key: None,
            mapping: CapabilityMapping {
                template: CapabilityTemplateJson(serde_json::json!({})),
            },
            input_schema: Some(InputSchema {
                input_type: InputType::Object {
                    fields: vec![repository_param],
                    additional_fields: true,
                },
                validation: InputValidation::default(),
                description: None,
                examples: vec![],
            }),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: policy,
            preflight: None,
            discovery: None,
            sanitizes: vec![],
            deterministic: None,
        }
    }

    #[test]
    fn splat_object_and_omit_aggregate() {
        let mut cgs = CGS::new();
        seed_repository_fixture(&mut cgs);
        cgs.add_resource(ResourceSchema {
            name: "Repository".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                string_field(&cgs, "id"),
                string_field(&cgs, "owner"),
                string_field(&cgs, "name"),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["owner".into(), "name".into()],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        let cap = repo_scope_cap(&cgs, ScopeAggregateKeyPolicy::OmitWhenRedundant);
        let mut env = IndexMap::new();
        env.insert(
            "repository".into(),
            Value::Object(
                vec![
                    ("owner".into(), Value::String("octo".into())),
                    ("name".into(), Value::String("Hello-World".into())),
                ]
                .into_iter()
                .collect(),
            ),
        );

        apply_entity_ref_scope_splat(&mut env, &cgs, &cap).expect("splat");
        assert_eq!(env.get("owner"), Some(&Value::String("octo".into())));
        assert_eq!(env.get("name"), Some(&Value::String("Hello-World".into())));
        assert!(!env.contains_key("repository"));
    }

    #[test]
    fn explicit_wins_no_overwrite() {
        let mut cgs = CGS::new();
        seed_repository_fixture(&mut cgs);
        cgs.add_resource(ResourceSchema {
            name: "Repository".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                string_field(&cgs, "id"),
                string_field(&cgs, "owner"),
                string_field(&cgs, "name"),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["owner".into(), "name".into()],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        let cap = repo_scope_cap(&cgs, ScopeAggregateKeyPolicy::Retain);
        let mut env = IndexMap::new();
        env.insert("owner".into(), Value::String("pred".into()));
        env.insert(
            "repository".into(),
            Value::Object(
                vec![
                    ("owner".into(), Value::String("octo".into())),
                    ("name".into(), Value::String("Hello-World".into())),
                ]
                .into_iter()
                .collect(),
            ),
        );

        apply_entity_ref_scope_splat(&mut env, &cgs, &cap).expect("splat");
        assert_eq!(env.get("owner"), Some(&Value::String("pred".into())));
        assert_eq!(env.get("name"), Some(&Value::String("Hello-World".into())));
        assert!(env.contains_key("repository"));
    }

    #[test]
    fn splat_full_name_object_normalizes_before_splat() {
        let mut cgs = CGS::new();
        seed_repository_fixture(&mut cgs);
        cgs.add_resource(ResourceSchema {
            name: "Repository".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                string_field(&cgs, "id"),
                string_field(&cgs, "owner"),
                string_field(&cgs, "repo"),
                string_field(&cgs, "full_name"),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["owner".into(), "repo".into()],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        let cap = repo_scope_cap(&cgs, ScopeAggregateKeyPolicy::OmitWhenRedundant);
        let mut env = IndexMap::new();
        env.insert(
            "repository".into(),
            Value::Object(
                vec![(
                    "full_name".into(),
                    Value::String("ryan-s-roberts/plasm-core".into()),
                )]
                .into_iter()
                .collect(),
            ),
        );
        apply_entity_ref_scope_splat(&mut env, &cgs, &cap).expect("splat");
        assert_eq!(
            env.get("owner"),
            Some(&Value::String("ryan-s-roberts".into()))
        );
        assert_eq!(env.get("repo"), Some(&Value::String("plasm-core".into())));
        assert!(!env.contains_key("repository"));
    }

    #[test]
    fn splat_owner_slash_name_string() {
        let mut cgs = CGS::new();
        seed_repository_fixture(&mut cgs);
        cgs.add_resource(ResourceSchema {
            name: "Repository".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                string_field(&cgs, "id"),
                string_field(&cgs, "owner"),
                string_field(&cgs, "name"),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["owner".into(), "name".into()],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        let cap = repo_scope_cap(&cgs, ScopeAggregateKeyPolicy::OmitWhenRedundant);
        let mut env = IndexMap::new();
        env.insert(
            "repository".into(),
            Value::String("octocat/Hello-World".into()),
        );
        apply_entity_ref_scope_splat(&mut env, &cgs, &cap).expect("splat");
        assert_eq!(env.get("owner"), Some(&Value::String("octocat".into())));
        assert_eq!(env.get("name"), Some(&Value::String("Hello-World".into())));
        assert!(
            !env.contains_key("repo"),
            "scope splat must not invent non-schema env keys"
        );
    }
}
