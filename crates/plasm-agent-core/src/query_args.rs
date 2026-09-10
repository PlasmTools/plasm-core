use clap::ArgMatches;
use plasm_core::{CapabilitySchema, Predicate, CGS};

use crate::input_field_cli::{build_field_args, extend_query_predicates_for_field, FieldArgHelp};

/// Generate typed clap `Arg`s for a query capability's declared `parameters:`.
///
/// Each `InputFieldSchema` becomes exactly **one** flag — no operator suffixes.
/// APIs that need range queries (`due_date_gt`, `due_date_lt`) declare them as
/// separate parameters in domain.yaml.
///
/// No `parameters:` on the capability → empty vec (no filter flags; pagination
/// flags are added separately by the CLI builder).
pub fn build_query_param_args(cap: &CapabilitySchema, cgs: &CGS) -> Vec<clap::Arg> {
    build_field_args(cap, FieldArgHelp::Query, cgs)
}

/// Extract matched query arguments back into a typed `Predicate`.
///
/// Walks the capability's declared parameters, reads the clap match for each,
/// and assembles `Predicate::Eq` comparisons. Range/inequality semantics are
/// left to APIs that expose distinct `_gt` / `_lt` parameters — each is an
/// equality comparison against its own named flag.
pub fn args_to_query_predicate(
    matches: &ArgMatches,
    cap: &CapabilitySchema,
    cgs: &CGS,
) -> Option<Predicate> {
    let fields: Vec<_> = cap.query_surface_fields().collect();
    if fields.is_empty() {
        return None;
    }

    let mut comparisons = Vec::new();

    let registered_ids: std::collections::HashSet<String> =
        matches.ids().map(|id| id.as_str().to_string()).collect();

    for field in fields {
        if !registered_ids.contains(&field.name) {
            continue;
        }
        extend_query_predicates_for_field(matches, field, &mut comparisons, cgs);
    }

    match comparisons.len() {
        0 => None,
        1 => Some(comparisons.remove(0)),
        _ => Some(Predicate::and(comparisons)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Command;
    use plasm_core::value_domain::ValueDomain;
    use plasm_core::{
        CapabilityKind, CapabilityMapping, CompOp, FieldType, InputFieldSchema, InputFieldWire,
        NamedValueSchema, Value, ValueDomainKey, CGS,
    };

    fn query_test_cgs() -> CGS {
        let mut cgs = CGS::new();
        let mut add = |k: &str, nv: NamedValueSchema| {
            cgs.values.insert(k.to_string(), nv);
        };
        add(
            "qa_status_req",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(
                    &FieldType::Select,
                    None,
                    None,
                    Some(vec!["available".into(), "pending".into(), "sold".into()]).clone(),
                    None,
                ),
                None,
            ),
        );
        add(
            "qa_status_rej",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(
                    &FieldType::Select,
                    None,
                    None,
                    Some(vec!["available".into()]).clone(),
                    None,
                ),
                None,
            ),
        );
        add(
            "qa_team_id",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::String, None, None, None, None),
                None,
            ),
        );
        add(
            "qa_team_id_multi",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::String, None, None, None, None),
                None,
            ),
        );
        add(
            "qa_archived",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::Boolean, None, None, None, None),
                None,
            ),
        );
        add(
            "qa_status_none",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(
                    &FieldType::Select,
                    None,
                    None,
                    Some(vec!["available".into()]).clone(),
                    None,
                ),
                None,
            ),
        );
        add(
            "qa_region_gen",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(
                    &FieldType::Select,
                    None,
                    None,
                    Some(vec!["EMEA".into(), "APAC".into(), "AMER".into()]).clone(),
                    None,
                ),
                None,
            ),
        );
        add(
            "qa_revenue",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(&FieldType::Number, None, None, None, None),
                None,
            ),
        );
        add(
            "qa_region_nf",
            NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::from_legacy(
                    &FieldType::Select,
                    None,
                    None,
                    Some(vec!["EMEA".into()]).clone(),
                    None,
                ),
                None,
            ),
        );
        cgs
    }

    fn make_query_cap(params: Vec<InputFieldSchema>) -> CapabilitySchema {
        CapabilitySchema {
            name: "thing_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Thing".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({}).into(),
            }),
            derived: None,
            inputs: plasm_core::CapabilityInputs {
                selection: plasm_core::BackendSelectionSchema(params),
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

    fn no_params_cap() -> CapabilitySchema {
        CapabilitySchema {
            name: "index_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Thing".into(),
            identity_key: None,
            invalidates_entities: vec![],
            mapping: Some(CapabilityMapping {
                template: serde_json::json!({}).into(),
            }),
            derived: None,
            inputs: Default::default(),
            output_schema: None,
            provides: vec![],
            sanitizes: vec![],
            scope_aggregate_key_policy: Default::default(),
            preflight: None,
            discovery: None,
            deterministic: None,
        }
    }

    fn parse_query(args: &[&str], cap: &CapabilitySchema, cgs: &CGS) -> ArgMatches {
        let mut cmd = Command::new("test");
        for arg in build_query_param_args(cap, cgs) {
            cmd = cmd.arg(arg);
        }
        cmd.try_get_matches_from(args).unwrap()
    }

    #[test]
    fn no_params_gives_empty_flags() {
        let cgs = CGS::new();
        let cap = no_params_cap();
        let flags = build_query_param_args(&cap, &cgs);
        assert!(flags.is_empty());
    }

    #[test]
    fn no_params_gives_no_predicate() {
        let cgs = CGS::new();
        let cap = no_params_cap();
        let cmd = Command::new("test");
        let matches = cmd.try_get_matches_from(["test"]).unwrap();
        let pred = args_to_query_predicate(&matches, &cap, &cgs);
        assert!(pred.is_none());
    }

    #[test]
    fn select_param_generates_typed_flag() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![InputFieldSchema {
            name: "status".into(),
            wire: InputFieldWire::Registry(ValueDomainKey::new("qa_status_req").expect("key")),
            required: true,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        }]);
        let flags = build_query_param_args(&cap, &cgs);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].get_id(), "status");

        let matches = parse_query(&["test", "--status", "available"], &cap, &cgs);
        let pred = args_to_query_predicate(&matches, &cap, &cgs).unwrap();
        if let Predicate::Comparison { field, op, value } = pred {
            assert_eq!(field, "status");
            assert_eq!(op, CompOp::Eq);
            assert_eq!(value.to_value(), Value::String("available".into()));
        } else {
            panic!("expected Comparison");
        }
    }

    #[test]
    fn rejects_invalid_select_value() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![InputFieldSchema {
            name: "status".into(),
            wire: InputFieldWire::Registry(ValueDomainKey::new("qa_status_rej").expect("key")),
            required: false,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        }]);
        let mut cmd = Command::new("test");
        for arg in build_query_param_args(&cap, &cgs) {
            cmd = cmd.arg(arg);
        }
        assert!(cmd
            .try_get_matches_from(["test", "--status", "BOGUS"])
            .is_err());
    }

    #[test]
    fn required_param_enforced() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![InputFieldSchema {
            name: "team_id".into(),
            wire: InputFieldWire::Registry(ValueDomainKey::new("qa_team_id").expect("key")),
            required: true,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        }]);
        let mut cmd = Command::new("test");
        for arg in build_query_param_args(&cap, &cgs) {
            cmd = cmd.arg(arg);
        }
        assert!(cmd.try_get_matches_from(["test"]).is_err());
    }

    #[test]
    fn multiple_params_become_and_predicate() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![
            InputFieldSchema {
                name: "archived".into(),
                wire: InputFieldWire::Registry(ValueDomainKey::new("qa_archived").expect("key")),
                required: false,
                description: None,
                default: None,
                sink_class: None,
                wire_json_path: None,
                wire_array_element_key: None,
            },
            InputFieldSchema {
                name: "team_id".into(),
                wire: InputFieldWire::Registry(
                    ValueDomainKey::new("qa_team_id_multi").expect("key"),
                ),
                required: false,
                description: None,
                default: None,
                sink_class: None,
                wire_json_path: None,
                wire_array_element_key: None,
            },
        ]);
        let matches = parse_query(&["test", "--archived", "--team_id", "abc"], &cap, &cgs);
        let pred = args_to_query_predicate(&matches, &cap, &cgs).unwrap();
        assert!(matches!(pred, Predicate::And { .. }));
    }

    #[test]
    fn no_flags_given_returns_none_predicate() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![InputFieldSchema {
            name: "status".into(),
            wire: InputFieldWire::Registry(ValueDomainKey::new("qa_status_none").expect("key")),
            required: false,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        }]);
        let matches = parse_query(&["test"], &cap, &cgs);
        assert!(args_to_query_predicate(&matches, &cap, &cgs).is_none());
    }

    #[test]
    fn generates_select_with_possible_values() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![InputFieldSchema {
            name: "region".into(),
            wire: InputFieldWire::Registry(ValueDomainKey::new("qa_region_gen").expect("key")),
            required: false,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        }]);
        let matches = parse_query(&["test", "--region", "EMEA"], &cap, &cgs);
        let pred = args_to_query_predicate(&matches, &cap, &cgs).unwrap();
        if let Predicate::Comparison { field, op, value } = &pred {
            assert_eq!(field, "region");
            assert_eq!(*op, CompOp::Eq);
            assert_eq!(value.to_value(), Value::String("EMEA".into()));
        } else {
            panic!("expected Comparison, got {:?}", pred);
        }
    }

    #[test]
    fn number_param_flag() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![InputFieldSchema {
            name: "revenue".into(),
            wire: InputFieldWire::Registry(ValueDomainKey::new("qa_revenue").expect("key")),
            required: false,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        }]);
        let matches = parse_query(&["test", "--revenue", "1000"], &cap, &cgs);
        let pred = args_to_query_predicate(&matches, &cap, &cgs).unwrap();
        if let Predicate::Comparison { field, op, value } = &pred {
            assert_eq!(field, "revenue");
            assert_eq!(*op, CompOp::Eq);
            assert_eq!(value.to_value(), Value::Float(1000.0));
        } else {
            panic!("expected Comparison, got {:?}", pred);
        }
    }

    #[test]
    fn no_filters_returns_none() {
        let cgs = query_test_cgs();
        let cap = make_query_cap(vec![InputFieldSchema {
            name: "region".into(),
            wire: InputFieldWire::Registry(ValueDomainKey::new("qa_region_nf").expect("key")),
            required: false,
            description: None,
            default: None,
            sink_class: None,
            wire_json_path: None,
            wire_array_element_key: None,
        }]);
        let matches = parse_query(&["test"], &cap, &cgs);
        assert!(args_to_query_predicate(&matches, &cap, &cgs).is_none());
    }
}
