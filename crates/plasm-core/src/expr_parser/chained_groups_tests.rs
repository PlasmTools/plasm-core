//! Chained `{…}{…}` predicate groups — AND-conjoin vs syntax-tail rejection.

use super::*;
use crate::cgs_federation::CgsLayer;
use crate::schema::registry_test_util;
use crate::symbol_tuning::{SymbolMap, SymbolSession};
use crate::{
    CapabilityKind, CapabilityMapping, CapabilitySchema, FieldType, NamedValueSchema,
    ResourceSchema, CGS,
};
use std::sync::Arc;

fn ticket_query_fixture_cgs() -> CGS {
    let mut cgs = CGS::new();
    cgs.values.insert(
        "fx_str".into(),
        NamedValueSchema {
            description: String::new(),
            field_type: FieldType::String,
            value_format: None,
            allowed_values: None,
            string_semantics: None,
            array_items: None,
            currency: None,
        },
    );
    let f = |n: &str| registry_test_util::entity_field_from_values(&cgs, "fx_str", n, true, "");
    cgs.add_resource(ResourceSchema {
        name: "Ticket".into(),
        description: String::new(),
        id_field: "n".into(),
        id_format: None,
        id_from: None,
        fields: vec![f("owner"), f("repo"), f("n")],
        relations: vec![],
        expression_aliases: vec![],
        implicit_request_identity: false,
        key_vars: vec!["owner".into(), "repo".into(), "n".into()],
        abstract_entity: false,
        domain_projection_examples: false,
        primary_read: None,
        discovery: None,
    })
    .unwrap();
    cgs.add_capability(CapabilitySchema {
        name: "ticket_get".into(),
        description: String::new(),
        kind: CapabilityKind::Get,
        domain: "Ticket".into(),
        identity_key: None,
        mapping: CapabilityMapping {
            template: serde_json::json!({
                "method": "GET",
                "path": [
                    {"type": "var", "name": "owner"},
                    {"type": "var", "name": "repo"},
                    {"type": "var", "name": "n"}
                ]
            })
            .into(),
        },
        input_schema: None,
        output_schema: None,
        provides: vec![],
        scope_aggregate_key_policy: Default::default(),
        preflight: None,
        discovery: None,
        sanitizes: vec![],

        deterministic: None,
    })
    .unwrap();
    cgs.validate().unwrap();
    cgs
}

#[test]
fn parse_chained_brace_groups_and_conjoin() {
    let cgs = ticket_query_fixture_cgs();
    let parsed = parse(r#"Ticket{owner="a"}{repo="b"}"#, &cgs).expect("parse");
    let Expr::Query(q) = parsed.expr else {
        panic!("expected query");
    };
    let Some(pred) = q.predicate else {
        panic!("expected predicate");
    };
    let Predicate::And { args } = pred else {
        panic!("expected AND of chained groups, got {pred:?}");
    };
    assert_eq!(args.len(), 2);
}

#[test]
fn parse_with_cgs_layers_program_rejects_syntax_tail() {
    let cgs = ticket_query_fixture_cgs();
    let stack = [CgsLayer::unset(&cgs)];
    let sym_map: Arc<dyn SymbolSession> = Arc::new(SymbolMap::build(&cgs, &[]));
    let err = parse_with_cgs_layers_program(
        r#"Ticket{owner="a"} {repo="b"}"#,
        &stack,
        sym_map,
        None,
        false,
    )
    .expect_err("spaced second brace group must not be silently dropped");
    assert!(
        err.to_string().contains("unexpected trailing syntax"),
        "{}",
        err
    );
}
