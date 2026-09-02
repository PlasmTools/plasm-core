use clap::ArgMatches;
use indexmap::IndexMap;
use plasm_core::{CapabilitySchema, Value, CGS};

use crate::input_field_cli::{build_field_args, field_value_for_invoke, FieldArgHelp};

/// Generate typed clap `Arg`s from a capability's InputSchema.
///
/// Each InputFieldSchema becomes a named, typed flag:
///   --name (string), --revenue (f64), --priority (select with PossibleValues)
pub fn build_invoke_args(cap: &CapabilitySchema, cgs: &CGS) -> Vec<clap::Arg> {
    build_field_args(cap, FieldArgHelp::Invoke, cgs)
}

/// Extract matched invoke arguments into a `Value::Object` for `InvokeExpr::input`.
pub fn args_to_input(matches: &ArgMatches, cap: &CapabilitySchema, cgs: &CGS) -> Option<Value> {
    let fields: Vec<_> = cap.invocation_object_fields().collect();
    if fields.is_empty() {
        return None;
    }

    let mut obj = IndexMap::new();

    for field in fields {
        if let Some(v) = field_value_for_invoke(matches, field, cgs) {
            obj.insert(field.name.clone(), v);
        }
    }

    if obj.is_empty() {
        None
    } else {
        Some(Value::Object(obj))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, Command};
    use plasm_core::value_domain::ValueDomain;
    use plasm_core::{
        CapabilityKind, CapabilityMapping, FieldType, InputFieldSchema, InputFieldWire,
        InputSchema, InputType, InputValidation, NamedValueSchema, ValueDomainKey, CGS,
    };

    fn invoke_test_cgs() -> CGS {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "invoke_upd_name".into(),
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::String, None, None, None, None),
                None,
            ),
        );
        cgs.values.insert(
            "invoke_upd_revenue".into(),
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::Number, None, None, None, None),
                None,
            ),
        );
        cgs.values.insert(
            "invoke_upd_priority".into(),
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(
                    &FieldType::Select,
                    None,
                    None,
                    Some(vec!["low".into(), "medium".into(), "high".into()]).clone(),
                    None,
                ),
                None,
            ),
        );
        cgs
    }

    fn test_capability() -> CapabilitySchema {
        CapabilitySchema {
            name: "update_account".into(),
            description: String::new(),
            kind: CapabilityKind::Update,
            domain: "Account".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: CapabilityMapping {
                template: serde_json::json!({}).into(),
            },
            inputs: plasm_core::CapabilityInputs {
                payload: Some(InputSchema {
                    input_type: InputType::Object {
                        fields: vec![
                            InputFieldSchema {
                                name: "name".into(),
                                wire: InputFieldWire::Registry(
                                    ValueDomainKey::new("invoke_upd_name").expect("key"),
                                ),
                                required: false,
                                description: Some("Account name".into()),
                                default: None,
                                sink_class: None,
                                wire_json_path: None,
                                wire_array_element_key: None,
                            },
                            InputFieldSchema {
                                name: "revenue".into(),
                                wire: InputFieldWire::Registry(
                                    ValueDomainKey::new("invoke_upd_revenue").expect("key"),
                                ),
                                required: false,
                                description: Some("Annual revenue".into()),
                                default: None,
                                sink_class: None,
                                wire_json_path: None,
                                wire_array_element_key: None,
                            },
                            InputFieldSchema {
                                name: "priority".into(),
                                wire: InputFieldWire::Registry(
                                    ValueDomainKey::new("invoke_upd_priority").expect("key"),
                                ),
                                required: false,
                                description: Some("Priority level".into()),
                                default: None,
                                sink_class: None,
                                wire_json_path: None,
                                wire_array_element_key: None,
                            },
                        ],
                        additional_fields: false,
                    },
                    validation: InputValidation {
                        allow_null: false,
                        cross_field_rules: vec![],
                    },
                    description: None,
                    examples: vec![],
                }),
                ..Default::default()
            },
            output_schema: None,
            provides: vec![],
            sanitizes: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            deterministic: None,
        }
    }

    fn parse_invoke(args: &[&str], cap: &CapabilitySchema, cgs: &CGS) -> ArgMatches {
        let mut cmd = Command::new("test").arg(Arg::new("id").required(true));
        for arg in build_invoke_args(cap, cgs) {
            cmd = cmd.arg(arg);
        }
        cmd.try_get_matches_from(args).unwrap()
    }

    #[test]
    fn typed_invoke_args() {
        let cgs = invoke_test_cgs();
        let cap = test_capability();
        let matches = parse_invoke(
            &[
                "test",
                "acc-1",
                "--name",
                "New Corp",
                "--revenue",
                "2000",
                "--priority",
                "high",
            ],
            &cap,
            &cgs,
        );
        let input = args_to_input(&matches, &cap, &cgs).unwrap();
        if let Value::Object(obj) = input {
            assert_eq!(obj.get("name"), Some(&Value::String("New Corp".into())));
            assert_eq!(obj.get("revenue"), Some(&Value::Float(2000.0)));
            assert_eq!(obj.get("priority"), Some(&Value::String("high".into())));
        } else {
            panic!("expected Object");
        }
    }

    #[test]
    fn rejects_invalid_priority() {
        let cgs = invoke_test_cgs();
        let cap = test_capability();
        let mut cmd = Command::new("test").arg(Arg::new("id").required(true));
        for arg in build_invoke_args(&cap, &cgs) {
            cmd = cmd.arg(arg);
        }
        let result = cmd.try_get_matches_from(["test", "acc-1", "--priority", "INVALID"]);
        assert!(result.is_err());
    }

    #[test]
    fn partial_fields() {
        let cgs = invoke_test_cgs();
        let cap = test_capability();
        let matches = parse_invoke(&["test", "acc-1", "--revenue", "500"], &cap, &cgs);
        let input = args_to_input(&matches, &cap, &cgs).unwrap();
        if let Value::Object(obj) = input {
            assert_eq!(obj.len(), 1);
            assert_eq!(obj.get("revenue"), Some(&Value::Float(500.0)));
        } else {
            panic!("expected Object");
        }
    }

    #[test]
    fn no_args_returns_none() {
        let cgs = invoke_test_cgs();
        let cap = test_capability();
        let matches = parse_invoke(&["test", "acc-1"], &cap, &cgs);
        assert!(args_to_input(&matches, &cap, &cgs).is_none());
    }
}
