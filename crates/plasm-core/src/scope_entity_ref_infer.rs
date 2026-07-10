//! Type-level inference for same-entity [`FieldType::EntityRef`] **scope** parameters on dotted
//! invoke/create: when the receiver is already `EntityRef(T)` and the scope slot targets `T`,
//! omit explicit scope args if [`normalize_entity_ref_value_for_target`] succeeds on the receiver
//! identity. Explicit authored keys always win.

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::entity_ref_value::normalize_entity_ref_value_for_target;
use crate::expr::{CreateExpr, EntityKey, Expr, InvokeExpr, Ref};
use crate::schema::{CapabilitySchema, EntityDef, InputFieldSchema, InputType, ParameterRole};
use crate::value::Value;
use crate::{FieldType, CGS};

/// How a scope input field is supplied on dotted invoke/create.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScopeParamSupply {
    /// Must appear in `(…)` — not path-bound, not same-entity inferable.
    Explicit,
    /// Bound by CML path template / unary `{entity}_id` inject.
    PathTemplate,
    /// Same-entity EntityRef scope — omit from teaching; infer at runtime when normalizable.
    ReceiverEntityRef,
}

/// Classify how a scope parameter is supplied relative to a dotted receiver.
pub fn classify_scope_param_supply(
    receiver_entity: &EntityDef,
    cap: &CapabilitySchema,
    field: &InputFieldSchema,
    cgs: &CGS,
) -> ScopeParamSupply {
    if !matches!(field.role, Some(ParameterRole::Scope)) {
        return ScopeParamSupply::Explicit;
    }
    if field_omitted_from_path_inject(receiver_entity, cap, field.name.as_str()) {
        return ScopeParamSupply::PathTemplate;
    }
    let Ok(nv) = field.named_value(cgs) else {
        return ScopeParamSupply::Explicit;
    };
    let FieldType::EntityRef { target } = &nv.field_type else {
        return ScopeParamSupply::Explicit;
    };
    if receiver_entity.name == *target {
        ScopeParamSupply::ReceiverEntityRef
    } else {
        ScopeParamSupply::Explicit
    }
}

/// Teaching exemplar: omit scope args already path-bound or same-entity EntityRef inferable.
pub fn should_omit_invoke_teaching_arg(
    receiver_entity: &EntityDef,
    cap: &CapabilitySchema,
    field: &InputFieldSchema,
    cgs: &CGS,
) -> bool {
    matches!(
        classify_scope_param_supply(receiver_entity, cap, field, cgs),
        ScopeParamSupply::PathTemplate | ScopeParamSupply::ReceiverEntityRef
    )
}

/// Omit path-bound scope keys from explicit dotted-call `(…)` when they are already supplied by the
/// receiver: unary `Entity($)` / symbolic unary `e#(p#)` identity injects `{entity}_id`, and compound
/// `Entity(k1=$, k2=$)` injects each `key_vars` slot that also appears as a path template variable.
pub fn field_omitted_from_path_inject(
    ent: &EntityDef,
    cap: &CapabilitySchema,
    field_name: &str,
) -> bool {
    let path_vars = crate::schema::path_var_names_from_mapping_json(&cap.mapping.template.0);
    if !path_vars.iter().any(|pv| pv == field_name) {
        return false;
    }
    let unary_anchor_id = format!("{}_id", ent.name.to_lowercase());
    if field_name == unary_anchor_id {
        return true;
    }
    if ent.key_vars.len() > 1 {
        if let Some(is) = cap.input_schema.as_ref() {
            if let InputType::Object { fields, .. } = &is.input_type {
                let required_scope: HashSet<&str> = fields
                    .iter()
                    .filter(|f| f.required && matches!(f.role, Some(ParameterRole::Scope)))
                    .map(|f| f.name.as_str())
                    .collect();
                let path_set: HashSet<&str> = path_vars.iter().map(|s| s.as_str()).collect();
                let every_path_bound_key_declared = ent.key_vars.iter().all(|kv| {
                    let k = kv.as_str();
                    !path_set.contains(k) || required_scope.contains(k)
                });
                if every_path_bound_key_declared
                    && ent.key_vars.iter().any(|kv| kv.as_str() == field_name)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Build a normalized EntityRef(scope) value from a same-entity receiver [`Ref`].
#[must_use]
pub fn entity_ref_scope_value_from_receiver_ref(
    receiver_ref: &Ref,
    target_entity: &EntityDef,
) -> Option<Value> {
    if receiver_ref.entity_type != target_entity.name {
        return None;
    }
    let candidate = ref_to_scope_candidate_value(receiver_ref, target_entity);
    normalize_entity_ref_value_for_target(&candidate, target_entity)
}

fn ref_to_scope_candidate_value(receiver_ref: &Ref, target_entity: &EntityDef) -> Value {
    match &receiver_ref.key {
        EntityKey::Compound(parts) => Value::Object(
            parts
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        ),
        EntityKey::Simple(id) => {
            if target_entity.key_vars.len() == 1 {
                Value::Object(IndexMap::from([(
                    target_entity.key_vars[0].to_string(),
                    Value::String(id.to_string()),
                )]))
            } else {
                Value::String(id.to_string())
            }
        }
    }
}

/// Merge inferred same-entity scope EntityRef params into `input` (explicit keys win).
#[must_use]
pub fn effective_capability_input(
    cap: &CapabilitySchema,
    receiver_entity: &EntityDef,
    receiver_ref: &Ref,
    input: Value,
    cgs: &CGS,
) -> Value {
    let Some(is) = cap.input_schema.as_ref() else {
        return input;
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return input;
    };

    let mut map = match input {
        Value::Object(m) => m,
        Value::Null => IndexMap::new(),
        other => return other,
    };

    for field in fields {
        if map.contains_key(field.name.as_str()) {
            continue;
        }
        match classify_scope_param_supply(receiver_entity, cap, field, cgs) {
            ScopeParamSupply::PathTemplate | ScopeParamSupply::Explicit => continue,
            ScopeParamSupply::ReceiverEntityRef => {
                let Ok(nv) = field.named_value(cgs) else {
                    continue;
                };
                let FieldType::EntityRef { target } = &nv.field_type else {
                    continue;
                };
                let Some(target_ent) = cgs.get_entity(target) else {
                    continue;
                };
                if let Some(v) = entity_ref_scope_value_from_receiver_ref(receiver_ref, target_ent)
                {
                    map.insert(field.name.to_string(), v);
                }
            }
        }
    }

    Value::Object(map)
}

/// Lift + normalize + same-entity scope inference for invoke (type-check, preflight, live).
#[must_use]
pub fn prepare_invoke_capability_input(
    cap: &CapabilitySchema,
    invoke: &InvokeExpr,
    input: Value,
    cgs: &CGS,
) -> Value {
    let Some(receiver_ent) = cgs.get_entity(&invoke.target.entity_type) else {
        return input;
    };
    effective_capability_input(cap, receiver_ent, &invoke.target, input, cgs)
}

/// Lift + normalize + same-entity scope inference for create with dotted `Get` receiver.
#[must_use]
pub fn prepare_create_capability_input(
    cap: &CapabilitySchema,
    create: &CreateExpr,
    input: Value,
    cgs: &CGS,
) -> Value {
    create
        .dotted_receiver
        .as_deref()
        .and_then(|expr| match expr {
            Expr::Get(get) => cgs
                .get_entity(&get.reference.entity_type)
                .map(|receiver_ent| {
                    effective_capability_input(
                        cap,
                        receiver_ent,
                        &get.reference,
                        input.clone(),
                        cgs,
                    )
                }),
            _ => None,
        })
        .unwrap_or(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::Ref;
    use crate::identity::{CapabilityName, EntityFieldName, EntityName};
    use crate::schema::registry_test_util;
    use crate::schema::{
        CapabilityMapping, CapabilitySchema, CapabilityTemplateJson, InputSchema, InputValidation,
        NamedValueSchema, ParameterRole, ResourceSchema, ScopeAggregateKeyPolicy,
    };
    use crate::CapabilityKind;

    fn seed_repository_entity_ref(cgs: &mut CGS) {
        cgs.values.insert(
            "fx_str".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(crate::StringSemantics::Short),
                array_items: None,
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
            },
        );
    }

    fn repository_resource(cgs: &CGS) -> ResourceSchema {
        ResourceSchema {
            name: "Repository".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                registry_test_util::entity_field_from_values(cgs, "fx_str", "id", true, ""),
                registry_test_util::entity_field_from_values(cgs, "fx_str", "owner", true, ""),
                registry_test_util::entity_field_from_values(cgs, "fx_str", "repo", true, ""),
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec!["owner".into(), "repo".into()],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        }
    }

    fn repo_branch_create_cap(cgs: &CGS) -> CapabilitySchema {
        let mut repository = registry_test_util::object_input_field_from_values(
            cgs,
            "fx_repo_ref",
            "repository",
            true,
        );
        repository.role = Some(ParameterRole::Scope);
        let name = registry_test_util::object_input_field_from_values(cgs, "fx_str", "name", true);
        let sha = registry_test_util::object_input_field_from_values(cgs, "fx_str", "sha", true);
        CapabilitySchema {
            name: CapabilityName::from("repo_branch_create"),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: EntityName::from("Repository"),
            identity_key: None,
            mapping: CapabilityMapping {
                template: CapabilityTemplateJson(serde_json::json!({
                    "method": "POST",
                    "path": [
                        {"type": "literal", "value": "repos"},
                        {"type": "var", "name": "owner"},
                        {"type": "var", "name": "repo"},
                        {"type": "literal", "value": "git/refs"},
                    ],
                })),
            },
            input_schema: Some(InputSchema {
                input_type: InputType::Object {
                    fields: vec![repository, name, sha],
                    additional_fields: false,
                },
                validation: InputValidation::default(),
                description: None,
                examples: vec![],
            }),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: ScopeAggregateKeyPolicy::OmitWhenRedundant,
            preflight: None,
            discovery: None,
            sanitizes: vec![],
            deterministic: None,
        }
    }

    #[test]
    fn infers_repository_scope_from_compound_receiver_ref() {
        let mut cgs = CGS::new();
        seed_repository_entity_ref(&mut cgs);
        cgs.add_resource(repository_resource(&cgs)).unwrap();
        let cap = repo_branch_create_cap(&cgs);
        cgs.add_capability(cap.clone()).unwrap();
        cgs.validate().expect("fixture");

        let receiver = Ref::compound(
            "Repository",
            [
                ("owner".into(), "octo".into()),
                ("repo".into(), "hello".into()),
            ]
            .into_iter()
            .collect(),
        );
        let ent = cgs.get_entity("Repository").expect("entity");
        let input = Value::Object(IndexMap::from([
            ("name".into(), Value::String("feat/x".into())),
            ("sha".into(), Value::String("abc".into())),
        ]));
        let effective = effective_capability_input(&cap, ent, &receiver, input, &cgs);
        let obj = effective.as_object().expect("object");
        assert_eq!(
            obj.get("repository"),
            Some(&Value::Object(IndexMap::from([
                ("owner".into(), Value::String("octo".into())),
                ("repo".into(), Value::String("hello".into())),
            ])))
        );
    }

    #[test]
    fn explicit_repository_scope_wins_over_inference() {
        let mut cgs = CGS::new();
        seed_repository_entity_ref(&mut cgs);
        cgs.add_resource(repository_resource(&cgs)).unwrap();
        let cap = repo_branch_create_cap(&cgs);
        let receiver = Ref::compound(
            "Repository",
            [("owner".into(), "a".into()), ("repo".into(), "b".into())]
                .into_iter()
                .collect(),
        );
        let ent = cgs.get_entity("Repository").expect("entity");
        let explicit = Value::Object(IndexMap::from([
            ("owner".into(), Value::String("other".into())),
            ("repo".into(), Value::String("repo".into())),
        ]));
        let input = Value::Object(IndexMap::from([
            ("repository".into(), explicit.clone()),
            ("name".into(), Value::String("n".into())),
            ("sha".into(), Value::String("s".into())),
        ]));
        let effective = effective_capability_input(&cap, ent, &receiver, input, &cgs);
        assert_eq!(
            effective.as_object().unwrap().get("repository"),
            Some(&explicit)
        );
    }

    #[test]
    fn classify_same_entity_scope_supply() {
        let mut cgs = CGS::new();
        seed_repository_entity_ref(&mut cgs);
        let cap = repo_branch_create_cap(&cgs);
        let ent = EntityDef {
            name: EntityName::from("Repository"),
            description: String::new(),
            id_field: EntityFieldName::from("id"),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![
                EntityFieldName::from("owner"),
                EntityFieldName::from("repo"),
            ],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            discovery: None,
        };
        let repo_field = cap
            .input_schema
            .as_ref()
            .and_then(|s| match &s.input_type {
                InputType::Object { fields, .. } => fields.first(),
                _ => None,
            })
            .expect("repository field");
        assert_eq!(
            classify_scope_param_supply(&ent, &cap, repo_field, &cgs),
            ScopeParamSupply::ReceiverEntityRef
        );
        assert!(should_omit_invoke_teaching_arg(
            &ent, &cap, repo_field, &cgs
        ));
    }
}
