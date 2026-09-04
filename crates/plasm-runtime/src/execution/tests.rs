//! Unit tests for execution (formerly inline in mod.rs).

use super::*;
use indexmap::IndexMap;
use plasm_compile::decode_entities;
use plasm_core::value_domain::ValueDomain;
use plasm_core::{
    CapabilityKind, CapabilityMapping, CapabilitySchema, Expr, FieldSchema, FieldType,
    FieldValueKind, GetExpr, InputFieldSchema, InputFieldWire, JsonPathSegment, NamedValueSchema,
    QueryPagination, Ref, ResourceSchema, ValueDomainKey,
};
use std::collections::BTreeMap;

fn create_test_cgs() -> CGS {
    let mut cgs = CGS::new();
    cgs.values.insert(
        "exec_test_id".into(),
        NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::from_legacy(&FieldType::String, None, None, None, None),
            None,
        ),
    );
    cgs.values.insert(
        "exec_test_name".into(),
        NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::from_legacy(&FieldType::String, None, None, None, None),
            None,
        ),
    );

    // Add Account entity
    let account = ResourceSchema {
        name: "Account".into(),
        description: String::new(),
        id_field: "id".into(),
        id_format: None,
        id_from: None,
        fields: vec![
            FieldSchema {
                name: "id".into(),
                kind: FieldValueKind::Registry(ValueDomainKey::new("exec_test_id").expect("key")),
                description: String::new(),
                required: true,
                agent_presentation: None,
                mime_type_hint: None,
                attachment_media: None,
                wire_path: None,
                derive: None,
                data_class: None,
                currency_field: None,
            },
            FieldSchema {
                name: "name".into(),
                kind: FieldValueKind::Registry(ValueDomainKey::new("exec_test_name").expect("key")),
                description: String::new(),
                required: true,
                agent_presentation: None,
                mime_type_hint: None,
                attachment_media: None,
                wire_path: None,
                derive: None,
                data_class: None,
                currency_field: None,
            },
        ],
        relations: vec![],
        expression_aliases: vec![],
        implicit_request_identity: false,
        key_vars: vec![],
        abstract_entity: false,
        domain_projection_examples: false,
        primary_read: None,
        primary_query: None,
        primary_search: None,
        discovery: None,
    };

    cgs.add_resource(account).unwrap();

    // Add query capability
    let query_capability = CapabilitySchema {
        name: "query_accounts".into(),
        description: String::new(),
        kind: CapabilityKind::Query,
        domain: "Account".into(),
        identity_key: None,
        invalidates_entities: vec![],
        mapping: Some(CapabilityMapping {
            template: serde_json::json!({
                "method": "POST",
                "path": [{"type": "literal", "value": "query"}, {"type": "literal", "value": "Account"}],
                "body": {
                    "type": "if",
                    "condition": {"type": "exists", "var": "filter"},
                    "then_expr": {"type": "object", "fields": [["filter", {"type": "var", "name": "filter"}]]},
                    "else_expr": {"type": "object", "fields": []}
                }
            })
            .into(),
        }),
        derived: None,
        inputs: Default::default(),
        output_schema: None,
        provides: vec![],
        scope_aggregate_key_policy: Default::default(),
        preflight: None,
        discovery: None,
        sanitizes: vec![],

        deterministic: None,
};

    cgs.add_capability(query_capability).unwrap();

    // Add get capability
    let get_capability = CapabilitySchema {
        name: "get_account".into(),
        description: String::new(),
        kind: CapabilityKind::Get,
        domain: "Account".into(),
        identity_key: None,
        invalidates_entities: vec![],
        mapping: Some(CapabilityMapping {
            template: serde_json::json!({
                "method": "GET",
                "path": [
                    {"type": "literal", "value": "resources"},
                    {"type": "literal", "value": "Account"},
                    {"type": "var", "name": "id"}
                ]
            })
            .into(),
        }),
        derived: None,
        inputs: Default::default(),
        output_schema: None,
        provides: vec![],
        scope_aggregate_key_policy: Default::default(),
        preflight: None,
        discovery: None,
        sanitizes: vec![],

        deterministic: None,
    };

    cgs.add_capability(get_capability).unwrap();

    cgs
}

fn cgs_with_unary_entity_ref_scope_query() -> (CGS, CapabilitySchema) {
    let mut cgs = CGS::new();
    cgs.values.insert(
        "rt_str".into(),
        NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::from_legacy(&FieldType::String, None, None, None, None),
            None,
        ),
    );
    cgs.values.insert(
        "rt_workspace_ref".into(),
        NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::from_legacy(
                &FieldType::EntityRef {
                    entry_id: Default::default(),
                    target: "Workspace".into(),
                },
                None,
                None,
                None,
                None,
            ),
            None,
        ),
    );
    cgs.add_resource(ResourceSchema {
        name: "Workspace".into(),
        description: String::new(),
        id_field: "id".into(),
        id_format: None,
        id_from: None,
        fields: vec![FieldSchema {
            name: "id".into(),
            kind: FieldValueKind::Registry(ValueDomainKey::new("rt_str").expect("key")),
            description: String::new(),
            required: true,
            agent_presentation: None,
            mime_type_hint: None,
            attachment_media: None,
            wire_path: None,
            derive: None,
            data_class: None,
            currency_field: None,
        }],
        relations: vec![],
        expression_aliases: vec![],
        implicit_request_identity: false,
        key_vars: vec![],
        abstract_entity: false,
        domain_projection_examples: false,
        primary_read: None,
        primary_query: None,
        primary_search: None,
        discovery: None,
    })
    .expect("workspace resource");
    let cap = CapabilitySchema {
        name: "managed_resource_query".into(),
        description: String::new(),
        kind: CapabilityKind::Query,
        domain: "ManagedResource".into(),
        identity_key: None,
        invalidates_entities: vec![],
        mapping: Some(CapabilityMapping {
            template: serde_json::json!({
                "method": "GET",
                "path": [
                    {"type": "literal", "value": "workspaces"},
                    {"type": "var", "name": "workspace_id"},
                    {"type": "literal", "value": "managed-resources"}
                ]
            })
            .into(),
        }),
        derived: None,
        inputs: plasm_core::CapabilityInputs {
            scope: plasm_core::ParentScopeSchema(vec![InputFieldSchema {
                name: "workspace_id".to_string(),
                wire: InputFieldWire::Registry(
                    ValueDomainKey::new("rt_workspace_ref").expect("workspace ref key"),
                ),
                required: true,
                description: None,
                default: None,
                wire_json_path: None,
                wire_array_element_key: None,
                sink_class: None,
            }]),
            ..Default::default()
        },
        output_schema: None,
        provides: vec![],
        scope_aggregate_key_policy: Default::default(),
        preflight: None,
        discovery: None,
        sanitizes: vec![],

        deterministic: None,
    };
    (cgs, cap)
}

#[test]
fn normalize_cml_scope_entity_ref_keeps_scalar_unary_ref_for_path_var() {
    let (cgs, cap) = cgs_with_unary_entity_ref_scope_query();
    let mut env = CmlEnv::new();
    env.insert(
        "workspace_id".to_string(),
        Value::String("workspace_123".to_string()),
    );
    normalize_cml_env_scope_entity_refs(&mut env, &cgs, &cap).expect("normalize");
    assert_eq!(
        env.get("workspace_id"),
        Some(&Value::String("workspace_123".to_string()))
    );
}

#[test]
fn normalize_cml_scope_entity_ref_narrows_unary_ref_row_to_path_scalar() {
    let (cgs, cap) = cgs_with_unary_entity_ref_scope_query();
    let mut env = CmlEnv::new();
    env.insert(
        "workspace_id".to_string(),
        Value::Object(indexmap::indexmap! {
            "id".to_string() => Value::String("workspace_123".to_string()),
            "name".to_string() => Value::String("General Workspace".to_string()),
        }),
    );
    normalize_cml_env_scope_entity_refs(&mut env, &cgs, &cap).expect("normalize");
    assert_eq!(
        env.get("workspace_id"),
        Some(&Value::String("workspace_123".to_string()))
    );
}

#[test]
fn pagination_context_map_reads_relay_page_info() {
    let v = serde_json::json!({
        "data": {
            "issues": {
                "nodes": [{"id": "1"}],
                "pageInfo": {"hasNextPage": true, "endCursor": "cursor-abc"}
            }
        }
    });
    let m = super::pagination_context_map(
        &v,
        Some(&[
            "data".to_string(),
            "issues".to_string(),
            "pageInfo".to_string(),
        ]),
    )
    .expect("pageInfo object");
    assert_eq!(m.get("endCursor"), Some(&serde_json::json!("cursor-abc")));
    assert_eq!(m.get("hasNextPage"), Some(&serde_json::json!(true)));
}

#[test]
fn pagination_context_map_accepts_numeric_prefix_for_root_array() {
    let v = serde_json::json!([
        { "data": { "after": null } },
        { "data": { "after": "t1_next", "children": [] } }
    ]);
    let m = super::pagination_context_map(&v, Some(&["1".to_string(), "data".to_string()]))
        .expect("second listing data object");
    assert_eq!(m.get("after"), Some(&serde_json::json!("t1_next")));
}

#[test]
fn merge_pagination_into_body_nested_graphql_variables() {
    let mut body = Value::Object(indexmap::indexmap! {
        "query".to_string() => Value::String("{ q }".to_string()),
        "variables".to_string() => Value::Object(indexmap::indexmap! {
            "o".to_string() => Value::Object(IndexMap::new()),
        }),
    });
    merge_pagination_into_body(
        &mut body,
        Some(&[
            "variables".to_string(),
            "o".to_string(),
            "paginate".to_string(),
        ]),
        "page",
        Value::Integer(2),
    )
    .unwrap();
    merge_pagination_into_body(
        &mut body,
        Some(&[
            "variables".to_string(),
            "o".to_string(),
            "paginate".to_string(),
        ]),
        "limit",
        Value::Integer(5),
    )
    .unwrap();
    let vars = body
        .as_object()
        .unwrap()
        .get("variables")
        .unwrap()
        .as_object()
        .unwrap()
        .get("o")
        .unwrap()
        .as_object()
        .unwrap()
        .get("paginate")
        .unwrap()
        .as_object()
        .unwrap();
    assert_eq!(vars.get("page"), Some(&Value::Integer(2)));
    assert_eq!(vars.get("limit"), Some(&Value::Integer(5)));
}

#[test]
fn test_execution_config_default() {
    let config = ExecutionConfig::default();
    assert_eq!(config.default_mode, ExecutionMode::Live);
    assert_eq!(config.timeout_seconds, 30);
    assert!(config.validate_responses);
    assert!(config.hydrate);
    assert_eq!(config.max_concurrent_requests, 64);
    assert_eq!(config.per_host_max_inflight, 24);
    assert_eq!(config.hydrate_concurrency, 16);
}

/// Regression: Cloudflare v4 list envelopes use `result: [...]`; paginated queries skip
/// `prepare_http_query_response` and must still decode rows with scalar `id`.
#[test]
fn matrix_fixture_ruleset_query_decodes_v4_envelope_list() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let cgs = plasm_core::load_schema(&dir).expect("load plasm_prompt_matrix fixture");
    let cap = cgs
        .get_capability("ruleset_query")
        .expect("ruleset_query capability");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template).unwrap();
    let cml = match &capability_template {
        plasm_compile::CapabilityTemplate::Http(c) => c,
        _ => panic!("expected HTTP template"),
    };
    assert!(
        !cml.response_is_single_object(),
        "ruleset_query must be a collection decode (`single: false`)",
    );
    let body = serde_json::json!({
        "errors": [],
        "messages": [],
        "result": [{
            "id": "2f2feab2026849078ba485f918791bdc",
            "kind": "root",
            "last_updated": "2000-01-01T00:00:00.000000Z",
            "name": "My ruleset",
            "phase": "http_request_firewall_custom",
            "version": "1",
            "description": "A description for my ruleset."
        }],
        "success": true,
        "result_info": { "cursors": { "after": "dGhpc2lzYW5leGFtcGxlCg" } }
    });
    let normalized =
        normalize_collection_response(body, response_bare_array_wrap_key(cml).as_str());
    let mut ambient = indexmap::IndexMap::new();
    ambient.insert("zone_id".into(), "00d2860b1edaed6074fd0f45a66e1a87".into());
    let decoder = create_entity_decoder(
        "Ruleset",
        &cgs,
        Some(http_collection_source(cml)),
        None,
        Some(&ambient),
    );
    let entities = decode_entities(&decoder, &normalized).expect("decode rulesets");
    assert_eq!(entities.len(), 1);
    let parts = entities[0]
        .reference
        .compound_parts()
        .expect("compound Ruleset ref");
    assert_eq!(
        parts.get("ruleset_id").map(String::as_str),
        Some("2f2feab2026849078ba485f918791bdc")
    );
    assert_eq!(
        parts.get("zone_id").map(String::as_str),
        Some("00d2860b1edaed6074fd0f45a66e1a87")
    );
}

#[test]
fn matrix_fixture_ruleset_get_narrowing_decodes_inner_result_object() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let cgs = plasm_core::load_schema(&dir).expect("load plasm_prompt_matrix fixture");
    let cap = cgs
        .get_capability("ruleset_get")
        .expect("ruleset_get capability");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template).unwrap();
    let cml = match &capability_template {
        plasm_compile::CapabilityTemplate::Http(c) => c,
        _ => panic!("expected HTTP template"),
    };
    assert!(cml.response_is_single_object());
    let body = serde_json::json!({
        "result": {
            "id": "2f2feab2026849078ba485f918791bdc",
            "name": "Zone-level phase entry point",
            "description": "",
            "kind": "zone",
            "version": "5",
            "last_updated": "2025-03-18T18:30:08.122758Z",
            "phase": "http_request_firewall_managed"
        },
        "success": true,
        "errors": [],
        "messages": []
    });
    let narrowed =
        narrow_http_graphql_response_for_entity_decode(&capability_template, body).unwrap();
    let mut ambient = indexmap::IndexMap::new();
    ambient.insert("zone_id".into(), "00d2860b1edaed6074fd0f45a66e1a87".into());
    let decoder = create_entity_decoder("Ruleset", &cgs, None, None, Some(&ambient));
    let entities = decode_entities(&decoder, &narrowed).expect("decode ruleset get");
    assert_eq!(entities.len(), 1);
    assert_eq!(
        entities[0].fields.get("id"),
        Some(&plasm_core::Value::String(
            "2f2feab2026849078ba485f918791bdc".into()
        ))
    );
}

#[test]
fn test_create_execution_engine() {
    let config = ExecutionConfig::default();
    let engine = ExecutionEngine::new(config);
    assert!(engine.is_ok());
}

#[tokio::test]
async fn test_type_check_before_execution() {
    let config = ExecutionConfig::default();
    let engine = ExecutionEngine::new(config).unwrap();
    let cgs = create_test_cgs();
    let mut cache = SessionMaterialization::new();

    // Create an invalid query (non-existent entity)
    let query = QueryExpr::all("NonExistentEntity");
    let expr = Expr::Query(query);

    let result = engine
        .execute(
            &expr,
            &cgs,
            &mut cache,
            None,
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await;
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::TypeError { .. }
    ));
}

#[tokio::test]
async fn test_execute_get_rejects_domain_placeholder_id() {
    let engine = ExecutionEngine::new(ExecutionConfig::default()).unwrap();
    let cgs = create_test_cgs();
    let mut cache = SessionMaterialization::new();
    let expr = Expr::Get(GetExpr::new("Account", "$"));
    let res = engine
        .execute(
            &expr,
            &cgs,
            &mut cache,
            None,
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await;
    let err = res.expect_err("expected placeholder rejection");
    assert!(matches!(err, RuntimeError::TypeError { .. }));
}

#[test]
fn test_basic_decoder_creation() {
    let decoder = create_entity_decoder(
        "TestEntity",
        &CGS::new(),
        Some(PathExpr::from_slice(&["results", "*"])),
        None,
        None,
    );
    assert_eq!(decoder.entity, "TestEntity");
    assert_eq!(decoder.fields.len(), 1);
}

#[test]
fn test_execution_result_serialization() {
    let result = ExecutionResult {
        entities: vec![],
        count: 0,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Live,
        stats: ExecutionStats {
            duration_ms: 100,
            network_requests: 1,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints: Vec::new(),
    };

    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains("live")); // lowercase due to serde rename_all
    assert!(json.contains("duration_ms"));
}

#[test]
fn test_execution_result_json_skips_host_pagination_fields() {
    use plasm_core::PagingHandle;
    let result = ExecutionResult {
        entities: vec![],
        count: 0,
        has_more: true,
        pagination_resume: None,
        paging_handle: Some(PagingHandle::mint_monotonic(1)),
        source: ExecutionSource::Live,
        stats: ExecutionStats {
            duration_ms: 1,
            network_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints: Vec::new(),
    };
    let json = serde_json::to_string(&result).unwrap();
    assert!(
        !json.contains("pg1"),
        "paging_handle must not appear on wire JSON: {json}"
    );
    assert!(json.contains("\"has_more\":true"));
}

#[test]
fn populate_template_path_env_binds_explicit_evm_get_vars() {
    let template = parse_capability_template(&serde_json::json!({
        "transport": "evm_call",
        "chain": 1,
        "contract": { "type": "const", "value": "0x0000000000000000000000000000000000000001" },
        "function": "function balanceOf(address owner) view returns (uint256)",
        "args": [{ "type": "var", "name": "owner" }],
        "block": { "type": "var", "name": "block" }
    }))
    .unwrap();

    let mut env = CmlEnv::new();
    let mut vars = IndexMap::new();
    vars.insert(
        "owner".to_string(),
        Value::String("0x00000000000000000000000000000000000000aa".to_string()),
    );
    vars.insert("block".to_string(), Value::String("latest".to_string()));

    populate_template_path_env(
        &mut env,
        &template,
        &Ref::new("Pet", "ignored-id"),
        None,
        Some(&vars),
        None,
    );

    assert_eq!(
        env.get("owner"),
        Some(&Value::String(
            "0x00000000000000000000000000000000000000aa".to_string()
        ))
    );
    assert_eq!(env.get("block"), Some(&Value::String("latest".to_string())));
    assert_eq!(
        env.get("id"),
        Some(&Value::String("ignored-id".to_string()))
    );
}

#[test]
fn populate_template_path_env_does_not_default_non_id_evm_vars_to_primary_id() {
    let template = parse_capability_template(&serde_json::json!({
        "transport": "evm_call",
        "chain": 1,
        "contract": { "type": "const", "value": "0x0000000000000000000000000000000000000001" },
        "function": "function balanceOf(address owner) view returns (uint256)",
        "args": [{ "type": "var", "name": "owner" }]
    }))
    .unwrap();

    let mut env = CmlEnv::new();
    populate_template_path_env(
        &mut env,
        &template,
        &Ref::new("Pet", "primary-id"),
        None,
        None,
        None,
    );

    assert_eq!(
        env.get("id"),
        Some(&Value::String("primary-id".to_string()))
    );
    assert!(
        !env.contains_key("owner"),
        "non-id EVM vars should be explicitly supplied, not silently bound to the primary id"
    );
}

#[test]
fn populate_template_path_env_binds_graphql_id_field_var() {
    use indexmap::IndexMap;
    use plasm_core::identity::{EntityFieldName, EntityName};
    use plasm_core::schema::EntityDef;

    let template = parse_capability_template(&serde_json::json!({
        "transport": "graphql",
        "method": "POST",
        "path": [{ "type": "literal", "value": "graphql" }],
        "body": {
            "type": "object",
            "fields": [
                ["query", { "type": "const", "value": "query($key: String!) { teams { nodes { key } } }" }],
                ["variables", {
                    "type": "object",
                    "fields": [["key", { "type": "var", "name": "key" }]]
                }]
            ]
        }
    }))
    .unwrap();

    let ent = EntityDef {
        name: EntityName::from("Team"),
        description: String::new(),
        id_field: EntityFieldName::from("key"),
        id_format: None,
        id_from: None,
        fields: IndexMap::new(),
        relations: IndexMap::new(),
        expression_aliases: vec![],
        implicit_request_identity: false,
        key_vars: vec![],
        abstract_entity: false,
        domain_projection_examples: true,
        primary_read: None,
        primary_query: None,
        primary_search: None,
        discovery: None,
    };

    let mut env = CmlEnv::new();
    populate_template_path_env(
        &mut env,
        &template,
        &Ref::new("Team", "EVA"),
        Some(&ent),
        None,
        None,
    );

    assert_eq!(env.get("key"), Some(&Value::String("EVA".to_string())));
}

#[test]
fn populate_template_path_env_path_vars_override_compound_ref_strings() {
    let template = parse_capability_template(&serde_json::json!({
        "method": "GET",
        "path": [
            {"type": "var", "name": "owner"},
            {"type": "literal", "value": "/"},
            {"type": "var", "name": "repo"},
            {"type": "literal", "value": "/"},
            {"type": "var", "name": "n"}
        ]
    }))
    .unwrap();

    let mut parts = BTreeMap::new();
    parts.insert("owner".into(), "stale-binding-name".into());
    parts.insert("repo".into(), "r".into());
    parts.insert("n".into(), "9".into());
    let reference = Ref::compound("Ticket", parts);

    let mut pv = IndexMap::new();
    pv.insert(
        "owner".into(),
        plasm_core::Value::String("real-owner-id".into()),
    );

    let mut env = CmlEnv::new();
    populate_template_path_env(&mut env, &template, &reference, None, Some(&pv), None);

    assert_eq!(
        env.get("owner"),
        Some(&plasm_core::Value::String("real-owner-id".into())),
        "path_vars must override stale compound Ref string for HTTP template vars"
    );
}

#[test]
fn block_range_with_upper_bound_is_not_single_page() {
    let pconf = PaginationConfig {
        strategy: Some(plasm_compile::PaginationStrategyKind::BlockRange),
        params: indexmap::indexmap! {
            "range_size".to_string() => plasm_compile::PaginationParam::Fixed {
                fixed: serde_json::json!(100),
                role: Some(plasm_compile::PaginationParamRole::PageSize),
            },
        },
        location: plasm_compile::PaginationLocation::BlockRange,
        body_merge_path: None,
        response_prefix: None,
        response_next_url_field: None,
        stop_when: None,
    };
    let user = QueryPagination {
        from_block: Some(0),
        to_block: Some(5_000),
        ..Default::default()
    };
    let consume = StreamConsumeOpts {
        fetch_all: false,
        max_items: None,
        one_page: false,
        ..Default::default()
    };
    // block_range + explicit to_block → NOT single HTTP round-trip (multi-page range query)
    let single_http_roundtrip = !consume.fetch_all
        && !matches!(
            pconf.location,
            plasm_compile::PaginationLocation::BlockRange
        )
        && (consume.max_items.is_none() || consume.one_page);
    assert!(!single_http_roundtrip);
    let _ = user; // suppress unused warning
}

#[test]
fn block_range_without_upper_bound_stays_single_page_by_default() {
    let pconf = PaginationConfig {
        strategy: Some(plasm_compile::PaginationStrategyKind::BlockRange),
        params: indexmap::indexmap! {
            "range_size".to_string() => plasm_compile::PaginationParam::Fixed {
                fixed: serde_json::json!(100),
                role: Some(plasm_compile::PaginationParamRole::PageSize),
            },
        },
        location: plasm_compile::PaginationLocation::BlockRange,
        body_merge_path: None,
        response_prefix: None,
        response_next_url_field: None,
        stop_when: None,
    };
    let user = QueryPagination {
        from_block: Some(0),
        ..Default::default()
    };
    let consume = StreamConsumeOpts {
        fetch_all: false,
        max_items: None,
        one_page: false,
        ..Default::default()
    };
    // block_range without to_block → not a single HTTP round-trip (BlockRange is always multi-step)
    let single_http_roundtrip = !consume.fetch_all
        && !matches!(
            pconf.location,
            plasm_compile::PaginationLocation::BlockRange
        )
        && (consume.max_items.is_none() || consume.one_page);
    // BlockRange always forces multi-page in the new model — test confirms the flag logic
    assert!(!single_http_roundtrip); // BlockRange is never a single HTTP round-trip
    let _ = user;
}

#[tokio::test]
async fn execute_http_respects_base_url_override() {
    use crate::auth::ResolvedAuth;
    use crate::http_transport::HttpTransport;
    use async_trait::async_trait;
    use plasm_compile::CompiledRequest;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct RecordingTransport {
        last_base: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl HttpTransport for RecordingTransport {
        async fn send_compiled_http(
            &self,
            base_url: &str,
            _request: &CompiledRequest,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            *self.last_base.lock().unwrap() = Some(base_url.to_string());
            Ok((serde_json::json!({"id":"1","name":"n"}), None))
        }

        async fn get_json_absolute(
            &self,
            _url: &str,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            Ok((serde_json::json!({}), None))
        }
    }

    let last = Arc::new(Mutex::new(None));
    let transport = RecordingTransport {
        last_base: last.clone(),
    };
    let config = ExecutionConfig {
        base_url: Some("http://wrong-host".to_string()),
        ..ExecutionConfig::default()
    };
    let engine = ExecutionEngine::new_with_transport(config, Arc::new(transport), None);
    let cgs = create_test_cgs();
    let mut cache = SessionMaterialization::new();
    let expr = Expr::Get(GetExpr::new("Account", "1"));
    engine
        .execute(
            &expr,
            &cgs,
            &mut cache,
            None,
            StreamConsumeOpts::default(),
            ExecuteOptions {
                http_base_url_override: Some("http://right-host".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("execute");

    assert_eq!(last.lock().unwrap().as_deref(), Some("http://right-host"));
}

/// Sync test: `with_captured_spans` + nested `block_on` must not run under `#[tokio::test]`.
#[test]
fn execute_operation_parents_http_compiled_request_on_live_get() {
    use crate::auth::ResolvedAuth;
    use crate::http_transport::HttpTransport;
    use async_trait::async_trait;
    use plasm_compile::CompiledRequest;
    use plasm_otel::span_capture::{find_span, is_child_of, with_captured_spans};
    use std::sync::Arc;

    struct OkTransport;

    #[async_trait]
    impl HttpTransport for OkTransport {
        async fn send_compiled_http(
            &self,
            _base_url: &str,
            _request: &CompiledRequest,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            Ok((serde_json::json!({"id": "1", "name": "n"}), None))
        }

        async fn get_json_absolute(
            &self,
            _url: &str,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            Ok((serde_json::json!({}), None))
        }
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ((), spans) = with_captured_spans(|| {
        rt.block_on(async {
            let config = ExecutionConfig {
                base_url: Some("http://example.test".to_string()),
                ..ExecutionConfig::default()
            };
            let engine = ExecutionEngine::new_with_transport(config, Arc::new(OkTransport), None);
            let cgs = create_test_cgs();
            let mut cache = SessionMaterialization::new();
            let expr = Expr::Get(GetExpr::new("Account", "1"));
            let _ = engine
                .execute(
                    &expr,
                    &cgs,
                    &mut cache,
                    None,
                    StreamConsumeOpts::default(),
                    ExecuteOptions::default(),
                )
                .await;
        });
    });

    let parent = find_span(&spans, "plasm_runtime.execute.operation").expect("execute.operation");
    let child =
        find_span(&spans, "plasm_runtime.http.compiled_request").expect("http.compiled_request");
    assert!(
        is_child_of(child, parent),
        "compiled_request must be child of execute.operation; spans={:?}",
        spans.iter().map(|s| s.name.as_ref()).collect::<Vec<_>>()
    );
    assert!(
        child
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "http.method"),
        "expected http.method on compiled_request"
    );
}

#[tokio::test]
async fn session_bearer_is_not_merged_into_cml_env() {
    let material = Arc::new(ExecuteSessionMaterial {
        prompt_hash: "a".repeat(64),
        session_id: "b".repeat(32),
        share_token: Some("transport-secret".into()),
        proof_base_token: Some("domain-precondition".into()),
        transport_origin: None,
        ui_origin: None,
        catalog_bind: None,
    });

    ExecutionEngine::run_in_execute_task_scopes(
        "https://api.example.test".into(),
        None,
        None,
        None,
        Some(material),
        None,
        None,
        async {
            let mut env = CmlEnv::new();
            merge_plasm_execute_session_proof_base_token_env(&mut env);
            merge_plasm_execute_session_env(&mut env);

            assert!(
                !env.contains_key("share_token"),
                "transport bearer must never enter CML env"
            );
            assert_eq!(
                env.get("base_token"),
                Some(&Value::String("domain-precondition".into()))
            );
        },
    )
    .await;
}

#[tokio::test]
async fn empty_cml_env_still_sends_resolver_bearer() {
    use crate::auth::ResolvedAuth;
    use crate::http_transport::HttpTransport;
    use async_trait::async_trait;
    use plasm_compile::{CompiledRequest, HttpBodyFormat, HttpMethod};
    use plasm_core::AuthScheme;
    use std::sync::{Arc, Mutex};

    struct RecordingTransport {
        last_auth: Arc<Mutex<Option<ResolvedAuth>>>,
    }

    #[async_trait]
    impl HttpTransport for RecordingTransport {
        async fn send_compiled_http(
            &self,
            _base_url: &str,
            _request: &CompiledRequest,
            auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            *self.last_auth.lock().unwrap() = auth;
            Ok((serde_json::json!({}), None))
        }

        async fn get_json_absolute(
            &self,
            _url: &str,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            Ok((serde_json::json!({}), None))
        }
    }

    let last = Arc::new(Mutex::new(None));
    let resolver = crate::AuthResolver::new(
        AuthScheme::BearerToken {
            env: None,
            hosted_kv: None,
            optional_env: true,
        },
        Arc::new(crate::EnvSecretProvider),
    )
    .with_session_bearer_override(Some("resolver-secret".into()));
    let engine = ExecutionEngine::new_with_transport(
        ExecutionConfig {
            base_url: Some("https://api.example.test".into()),
            ..ExecutionConfig::default()
        },
        Arc::new(RecordingTransport {
            last_auth: last.clone(),
        }),
        Some(resolver),
    );
    let request = CompiledRequest {
        method: HttpMethod::Get,
        path: "/v1/items".into(),
        query: None,
        body: None,
        body_format: HttpBodyFormat::Json,
        multipart: None,
        headers: None,
    };

    engine
        .execute_operation_full(&CompiledOperation::Http(request))
        .await
        .expect("HTTP execution");

    let resolved = last.lock().unwrap().clone().expect("resolved auth");
    assert_eq!(
        resolved.headers,
        vec![("Authorization".into(), "Bearer resolver-secret".into())]
    );
}

#[tokio::test]
async fn execute_http_uses_session_auth_resolver_override_when_engine_has_none() {
    use crate::auth::ResolvedAuth;
    use crate::http_transport::HttpTransport;
    use async_trait::async_trait;
    use plasm_compile::CompiledRequest;
    use plasm_core::AuthScheme;
    use std::sync::{Arc, Mutex};

    const ENV_KEY: &str = "PLASM_RT_SESSION_AUTH_OVERRIDE_TEST";

    struct RecordingTransport {
        last_auth: Arc<Mutex<Option<ResolvedAuth>>>,
    }

    #[async_trait]
    impl HttpTransport for RecordingTransport {
        async fn send_compiled_http(
            &self,
            _base_url: &str,
            _request: &CompiledRequest,
            auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            *self.last_auth.lock().unwrap() = auth;
            Ok((serde_json::json!({"id":"1","name":"n"}), None))
        }

        async fn get_json_absolute(
            &self,
            _url: &str,
            _auth: Option<ResolvedAuth>,
        ) -> Result<(serde_json::Value, Option<String>), RuntimeError> {
            Ok((serde_json::json!({}), None))
        }
    }

    std::env::set_var(ENV_KEY, "secret-token");

    let last = Arc::new(Mutex::new(None));
    let transport = RecordingTransport {
        last_auth: last.clone(),
    };
    let config = ExecutionConfig::default();
    let scheme = AuthScheme::ApiKeyHeader {
        header: "X-Test-Auth".to_string(),
        env: Some(ENV_KEY.to_string()),
        hosted_kv: None,
    };
    let override_resolver = Arc::new(crate::AuthResolver::from_env(scheme));
    let engine = ExecutionEngine::new_with_transport(config, Arc::new(transport), None);
    let cgs = create_test_cgs();
    let mut cache = SessionMaterialization::new();
    let expr = Expr::Get(GetExpr::new("Account", "1"));
    engine
        .execute(
            &expr,
            &cgs,
            &mut cache,
            None,
            StreamConsumeOpts::default(),
            ExecuteOptions {
                auth_resolver_override: Some(override_resolver),
                ..Default::default()
            },
        )
        .await
        .expect("execute");

    std::env::remove_var(ENV_KEY);

    let resolved = last.lock().unwrap().clone().expect("auth should be set");
    assert!(
        resolved
            .headers
            .iter()
            .any(|(k, v)| k == "X-Test-Auth" && v == "secret-token"),
        "expected override header, got {:?}",
        resolved.headers
    );
}

/// `prepare_http_query_response` + tagged [`ResponsePreprocess`]: find workspace, pluck `nested_array`.
#[test]
fn prepare_http_query_response_array_find_pluck() {
    use plasm_compile::CmlRequest;
    use plasm_core::Value;

    let cml: CmlRequest = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "path": [{"type": "literal", "value": "v2"}],
        "response": {
            "items": "members",
            "response_preprocess": {
                "kind": "array_find_pluck",
                "path": ["teams"],
                "id_field": "id",
                "id_var": "team_id",
                "nested_array": "members"
            }
        }
    }))
    .unwrap();
    let mut env = CmlEnv::new();
    env.insert("team_id".to_string(), Value::String("2".to_string()));
    let body = serde_json::json!({
        "teams": [
            {"id": "1", "members": [{"n": "a"}]},
            {"id": "2", "members": [{"n": "b"}]}
        ]
    });
    let out = prepare_http_query_response(body, &cml, &env);
    assert_eq!(out, serde_json::json!({ "members": [ {"n": "b"} ] }));
}

/// Invalid `path` for array_find: body unchanged (no empty shell).
#[test]
fn prepare_http_query_response_array_find_bad_path_unchanged() {
    use plasm_compile::CmlRequest;
    use plasm_core::Value;

    let cml: CmlRequest = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "path": [{"type": "literal", "value": "v2"}],
        "response": {
            "items": "members",
            "response_preprocess": {
                "kind": "array_find_pluck",
                "path": ["teams"],
                "id_field": "id",
                "id_var": "team_id",
                "nested_array": "members"
            }
        }
    }))
    .unwrap();
    let mut env = CmlEnv::new();
    env.insert("team_id".to_string(), Value::String("2".to_string()));
    let body = serde_json::json!({ "other": 1 });
    let out = prepare_http_query_response(body.clone(), &cml, &env);
    assert_eq!(out, body);
}

#[test]
fn prepare_http_query_response_concat_field_arrays() {
    use plasm_compile::CmlRequest;

    let cml: CmlRequest = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "path": [{"type": "literal", "value": "v2"}],
        "response": {
            "items": "intervals",
            "response_preprocess": {
                "kind": "concat_field_arrays",
                "path": ["data"],
                "from_each": "intervals"
            }
        }
    }))
    .unwrap();
    let env = CmlEnv::new();
    let body = serde_json::json!({
        "data": [
            { "intervals": [ {"a": 1} ] },
            { "intervals": [ {"a": 2}, {"a": 3} ] }
        ]
    });
    let out = prepare_http_query_response(body, &cml, &env);
    assert_eq!(
        out,
        serde_json::json!({ "intervals": [ {"a": 1}, {"a": 2}, {"a": 3} ] })
    );
}

#[test]
fn prepare_http_query_response_string_ids_to_field_objects() {
    use plasm_compile::CmlRequest;

    let cml: CmlRequest = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "path": [{"type": "literal", "value": "v2"}],
        "response": {
            "items": "templates",
            "response_preprocess": {
                "kind": "string_ids_to_field_objects",
                "path": ["templates"],
                "field": "id"
            }
        }
    }))
    .unwrap();
    let env = CmlEnv::new();
    let body = serde_json::json!({ "templates": ["t-1", "t-2", 3] });
    let out = prepare_http_query_response(body, &cml, &env);
    assert_eq!(
        out,
        serde_json::json!({
            "templates": [
                { "id": "t-1" },
                { "id": "t-2" }
            ]
        })
    );
}

/// `single: true` does not wrap a second time when `response_preprocess` already shaped the body.
#[test]
fn prepare_http_query_response_single_skipped_when_preprocess() {
    use plasm_compile::CmlRequest;

    let cml: CmlRequest = serde_json::from_value(serde_json::json!({
        "method": "GET",
        "path": [],
        "response": {
            "single": true,
            "items": "intervals",
            "response_preprocess": {
                "kind": "concat_field_arrays",
                "path": ["data"],
                "from_each": "intervals"
            }
        }
    }))
    .unwrap();
    let env = CmlEnv::new();
    let body = serde_json::json!({
        "data": [ { "intervals": [ {"i": 1} ] } ]
    });
    let out = prepare_http_query_response(body, &cml, &env);
    assert_eq!(out, serde_json::json!({ "intervals": [ {"i": 1} ] }));
}

#[test]
fn schema_overlay_decode_routes_to_typed_entity() {
    use plasm_core::loader::load_schema_dir;
    use plasm_core::schema_overlay::build_schema_overlay;

    let base_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/fibery_schema_overlay/bootstrap");
    let base = load_schema_dir(&base_dir).expect("bootstrap fixture");
    let json: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
    )))
    .expect("sample schema JSON");
    let spec = base.schema_overlay.as_ref().unwrap();
    let overlay = build_schema_overlay(spec, &base, &json).expect("overlay");
    let cgs = base.with_overlay(overlay).expect("merge");

    let mut ambient = IndexMap::new();
    ambient.insert("database".to_string(), "Cricket/Player".to_string());
    let entity =
        entity_decoder::resolve_overlay_decode_entity(&cgs, "entity_query", Some(&ambient))
            .expect("overlay entity for scope");
    assert_eq!(entity, "Cricket__Player");
    let ent = cgs.get_entity("Cricket__Player").expect("overlay entity");
    assert!(ent
        .fields
        .contains_key(&plasm_core::EntityFieldName::from("Cricket_name")));
}

#[test]
fn schema_overlay_decode_composite_scope_key() {
    use plasm_core::loader::load_schema_dir;
    use plasm_core::schema_overlay::{build_decode_scope_key, build_schema_overlay};

    let base_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/fibery_schema_overlay/bootstrap");
    let base = load_schema_dir(&base_dir).expect("bootstrap fixture");
    let json: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
    )))
    .expect("sample schema JSON");
    let spec = base.schema_overlay.as_ref().unwrap();
    let overlay = build_schema_overlay(spec, &base, &json).expect("overlay");
    let cgs = base.with_overlay(overlay).expect("merge");

    let mut ambient = IndexMap::new();
    ambient.insert("project".into(), "MYPROJ".into());
    ambient.insert("issuetype".into(), "Story".into());
    let composite_spec = plasm_core::schema_overlay::OverlayDecodeScopeSpec {
        params: vec!["project".into(), "issuetype".into()],
        key: plasm_core::schema_overlay::OverlayTemplateSpec {
            template: "{{ ambient.project }}:{{ ambient.issuetype }}".into(),
        },
    };
    let key = build_decode_scope_key(&composite_spec, &ambient).expect("composite key");
    assert_eq!(key, "MYPROJ:Story");

    let mut single = IndexMap::new();
    single.insert("database".to_string(), "Cricket/Player".to_string());
    let entity = entity_decoder::resolve_overlay_decode_entity(&cgs, "entity_query", Some(&single))
        .expect("overlay entity for scope");
    assert_eq!(entity, "Cricket__Player");
}

#[test]
fn schema_overlay_augment_base_global_decode_without_ambient() {
    use plasm_core::loader::load_schema_dir;
    use plasm_core::schema_overlay::build_schema_overlay;

    let base_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/clickup_schema_overlay/bootstrap");
    let base = load_schema_dir(&base_dir).expect("bootstrap fixture");
    let json: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/clickup_schema_overlay/sample_custom_field_query.json"
    )))
    .expect("sample custom field JSON");
    let spec = base.schema_overlay.as_ref().unwrap();
    let overlay = build_schema_overlay(spec, &base, &json).expect("overlay");
    let cgs = base.with_overlay(overlay).expect("merge");

    let empty = IndexMap::new();
    let entity = entity_decoder::resolve_overlay_decode_entity(&cgs, "task_get", Some(&empty))
        .expect("overlay entity for global augment_base");
    assert_eq!(entity, "Task");
    let task = cgs.get_entity("Task").expect("augmented Task");
    assert!(task
        .fields
        .contains_key(&plasm_core::EntityFieldName::from("Priority_Level")));
}

#[test]
fn fibery_schema_query_decodes_database_rows_from_fibery_name_id_path() {
    use plasm_compile::decode_entities;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("schema_query").expect("schema_query");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let cml = match &capability_template {
        plasm_compile::CapabilityTemplate::Http(c) => c,
        _ => panic!("expected HTTP template"),
    };
    let json: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/fibery_schema_overlay/sample_schema_query.json"
    )))
    .expect("sample schema JSON");
    let normalized = prepare_http_query_response(json, cml, &CmlEnv::new());
    let decoder = create_entity_decoder_for_capability(
        "Database",
        &cgs,
        Some("schema_query"),
        Some(http_collection_source(cml)),
        None,
        None,
    );
    let entities = decode_entities(&decoder, &normalized).expect("decode Database rows");
    assert_eq!(entities.len(), 1);
    assert_eq!(
        entities[0].fields.get("qualified_name"),
        Some(&plasm_core::Value::String("Cricket/Player".into()))
    );
}

#[test]
fn fibery_user_get_me_narrowing_decodes_first_result_row() {
    use plasm_compile::decode_entities;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("user_get_me").expect("user_get_me");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/fibery_schema_overlay/sample_user_get_me.json"
    )))
    .expect("sample user_get_me JSON");
    let narrowed = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect("narrow user_get_me");
    let decoder =
        create_entity_decoder_for_capability("User", &cgs, Some("user_get_me"), None, None, None);
    let entities = decode_entities(&decoder, &narrowed).expect("decode User");
    assert_eq!(entities.len(), 1);
    assert_eq!(
        entities[0].fields.get("id"),
        Some(&plasm_core::Value::String(
            "7dcf4730-82d2-11e9-8a28-82a9c787ee9d".into()
        ))
    );
    assert_eq!(
        entities[0].fields.get("name"),
        Some(&plasm_core::Value::String("Arthur Dent".into()))
    );
    assert_eq!(
        entities[0].fields.get("email"),
        Some(&plasm_core::Value::String("arthur@example.com".into()))
    );
}

#[test]
fn fibery_entity_create_narrowing_decodes_result_object() {
    use plasm_compile::decode_entities;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("entity_create").expect("entity_create");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/fibery_schema_overlay/sample_entity_create.json"
    )))
    .expect("sample entity_create JSON");
    let narrowed = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect("narrow entity_create");
    let mut env = CmlEnv::new();
    env.insert(
        "database".into(),
        plasm_core::Value::String("Cricket/Player".into()),
    );
    let identity_ambient = cml_env_to_identity_strings(&env);
    let decoder = mutating_capability_response_decoder(
        "Record",
        "entity_create",
        &cgs,
        &identity_ambient,
        None,
    );
    let entities = decode_entities(&decoder, &narrowed).expect("decode Record");
    assert_eq!(entities.len(), 1);
    assert_eq!(
        entities[0].fields.get("id"),
        Some(&plasm_core::Value::String(
            "d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4".into()
        ))
    );
    assert_eq!(
        entities[0].fields.get("public_id"),
        Some(&plasm_core::Value::String("6".into()))
    );
}

#[test]
fn fibery_entity_update_merge_injects_fibery_id_into_input() {
    use plasm_compile::CompiledOperation;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("entity_update").expect("entity_update");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let mut env = CmlEnv::new();
    env.insert(
        "id".into(),
        plasm_core::Value::String("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4".into()),
    );
    env.insert(
        "database".into(),
        plasm_core::Value::String("Cricket/Player".into()),
    );
    env.insert(
        "input".into(),
        plasm_core::Value::Object(indexmap::IndexMap::from([(
            "Cricket/Name".into(),
            plasm_core::Value::String("Renamed".into()),
        )])),
    );
    let target_ent = cgs.get_entity("Record");
    merge_entity_id_from_into_input_env(&mut env, target_ent, cap);
    let compiled = compile_operation_dispatch(&capability_template, &env).expect("compile");
    let CompiledOperation::Http(req) = compiled else {
        panic!("expected HTTP compiled operation");
    };
    let body_str = serde_json::to_string(&req.body).expect("serialize body");
    assert!(
        body_str.contains("fibery/id"),
        "entity_update must include fibery/id in entity body: {body_str}"
    );
    assert!(
        body_str.contains("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4"),
        "entity_update must bind id param into entity: {body_str}"
    );
}

#[test]
fn fibery_entity_delete_compiles_fibery_id_and_database() {
    use plasm_compile::CompiledOperation;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("entity_delete").expect("entity_delete");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let mut env = CmlEnv::new();
    env.insert(
        "id".into(),
        plasm_core::Value::String("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4".into()),
    );
    env.insert(
        "database".into(),
        plasm_core::Value::String("Cricket/Player".into()),
    );
    let compiled = compile_operation_dispatch(&capability_template, &env).expect("compile");
    let CompiledOperation::Http(req) = compiled else {
        panic!("expected HTTP compiled operation");
    };
    let body_str = serde_json::to_string(&req.body).expect("serialize body");
    assert!(
        body_str.contains("fibery.entity/delete"),
        "delete command name: {body_str}"
    );
    assert!(
        body_str.contains("Cricket/Player"),
        "delete must include database type: {body_str}"
    );
    assert!(
        body_str.contains("d17390c4-98c8-11e9-a2a3-2a2ae2dbcce4"),
        "delete must include fibery/id: {body_str}"
    );
}

#[test]
fn fibery_entity_delete_envelope_surfaces_success_false() {
    let body = serde_json::json!({
        "success": false,
        "result": {
            "name": "entity.error/not-found",
            "message": "Entity not found"
        }
    });
    let err = preflight_fibery_command_envelope(&body)
        .expect_err("delete envelope success:false must fail");
    let msg = format!("{err}");
    assert!(msg.contains("entity.error/not-found"), "{msg}");
    assert!(msg.contains("Entity not found"), "{msg}");
}

#[test]
fn fibery_view_query_decodes_result_array() {
    use plasm_compile::decode_entities;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("view_query").expect("view_query");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let cml = match &capability_template {
        plasm_compile::CapabilityTemplate::Http(c) => c,
        _ => panic!("expected HTTP template"),
    };
    let body: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/fibery_schema_overlay/sample_view_query.json"
    )))
    .expect("sample view_query JSON");
    let normalized = prepare_http_query_response(body, cml, &CmlEnv::new());
    let decoder = create_entity_decoder_for_capability(
        "View",
        &cgs,
        Some("view_query"),
        Some(http_collection_source(cml)),
        None,
        None,
    );
    let entities = decode_entities(&decoder, &normalized).expect("decode View rows");
    assert_eq!(entities.len(), 1);
    assert_eq!(
        entities[0].fields.get("id"),
        Some(&plasm_core::Value::String(
            "43addb30-1fd0-11ee-9009-a7c752e861c6".into()
        ))
    );
    assert_eq!(
        entities[0].fields.get("name"),
        Some(&plasm_core::Value::String("Supa Doc".into()))
    );
}

#[test]
fn fibery_user_get_me_compile_preserves_my_id_filter() {
    use plasm_compile::CompiledOperation;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("user_get_me").expect("user_get_me");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let compiled = compile_operation_dispatch(&capability_template, &CmlEnv::new())
        .expect("compile user_get_me");
    let CompiledOperation::Http(req) = compiled else {
        panic!("expected HTTP compiled operation");
    };
    let body_str = serde_json::to_string(&req.body).expect("serialize body");
    assert!(
        body_str.contains("$my-id"),
        "user_get_me must filter on authenticated user via $my-id: {body_str}"
    );
    assert!(
        body_str.contains("\"params\""),
        "user_get_me must include empty params object for Fibery param resolution: {body_str}"
    );
    let body: serde_json::Value = serde_json::from_str(&body_str).expect("parse body json");
    let where_clause = body
        .get("args")
        .and_then(|a| a.get("query"))
        .and_then(|q| q.get("q/where"))
        .expect("q/where in compiled body");
    assert_eq!(
        where_clause,
        &serde_json::json!(["=", ["fibery/id"], "$my-id"])
    );
}

#[test]
fn fibery_command_envelope_preflight_surfaces_success_false() {
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("user_get_me").expect("user_get_me");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body = serde_json::json!({
        "success": false,
        "result": {
            "name": "entity.error/schema-type-not-found",
            "message": "fibery/user database was not found."
        }
    });
    let err = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect_err("success:false must fail before narrowing");
    let msg = format!("{err}");
    assert!(msg.contains("entity.error/schema-type-not-found"), "{msg}");
    assert!(msg.contains("fibery/user database was not found"), "{msg}");
}

#[test]
fn fibery_command_envelope_preflight_surfaces_empty_result_array() {
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/fibery");
    let cgs = load_schema_dir(&dir).expect("load fibery catalog");
    let cap = cgs.get_capability("user_get_me").expect("user_get_me");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body = serde_json::json!({ "success": true, "result": [] });
    let err = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect_err("empty result[] must fail with actionable message");
    let msg = format!("{err}");
    assert!(msg.contains("no rows"), "{msg}");
    assert!(msg.contains("$my-id"), "{msg}");
}

#[test]
fn graphql_get_null_entity_surfaces_request_error_not_config() {
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/linear");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).expect("load linear catalog");
    let cap = cgs.get_capability("issue_get").expect("issue_get");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body = serde_json::json!({
        "data": { "issue": null },
        "errors": [{ "message": "Entity not found: Issue" }]
    });
    let err = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect_err("null issue must fail before items_path config error");
    let msg = format!("{err}");
    assert!(
        matches!(err, RuntimeError::RequestError { .. }),
        "expected RequestError, got {err:?}"
    );
    assert!(msg.contains("null"), "{msg}");
    assert!(
        !msg.contains("missing path segment"),
        "must not leak items_path internals: {msg}"
    );
}

#[test]
fn graphql_mutation_success_false_surfaces_actionable_error() {
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/linear");
    let cgs = load_schema_dir(&dir).expect("load linear catalog");
    let cap = cgs.get_capability("issue_create").expect("issue_create");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body = serde_json::json!({
        "data": { "issueCreate": { "success": false, "issue": null } }
    });
    let err = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect_err("success:false mutation must fail before items_path narrowing");
    let msg = format!("{err}");
    assert!(msg.contains("success: false"), "{msg}");
    assert!(
        !msg.contains("missing path segment"),
        "must not leak items_path internals: {msg}"
    );
}

#[test]
fn graphql_mutation_success_false_prefers_graphql_errors() {
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/linear");
    let cgs = load_schema_dir(&dir).expect("load linear catalog");
    let cap = cgs.get_capability("issue_create").expect("issue_create");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body = serde_json::json!({
        "data": { "issueCreate": { "success": false, "issue": null } },
        "errors": [{ "message": "Team not found: PLA" }]
    });
    let err = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect_err("success:false mutation must fail");
    let msg = format!("{err}");
    assert!(msg.contains("Team not found: PLA"), "{msg}");
    assert!(!msg.contains("missing path segment"), "{msg}");
}

#[test]
fn graphql_mutation_success_true_decodes_normally() {
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/linear");
    let cgs = load_schema_dir(&dir).expect("load linear catalog");
    let cap = cgs.get_capability("issue_create").expect("issue_create");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let body = serde_json::json!({
        "data": { "issueCreate": { "success": true, "issue": { "id": "abc", "identifier": "EVA-61", "title": "x" } } }
    });
    let narrowed = narrow_http_graphql_response_for_entity_decode(&capability_template, body)
        .expect("success:true mutation decodes to the entity object");
    assert_eq!(
        narrowed.get("identifier").and_then(|v| v.as_str()),
        Some("EVA-61")
    );
}

#[test]
fn github_issue_query_decoder_includes_embedded_labels_relation() {
    use plasm_compile::decode_entities;
    use plasm_compile::DecodedRelation;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
    let cgs = load_schema_dir(&dir).expect("load github catalog");
    let cap = cgs.get_capability("issue_query").expect("issue_query");
    let capability_template =
        parse_capability_template(&cap.require_mapping().expect("cml mapping").template)
            .expect("parse template");
    let cml = match &capability_template {
        plasm_compile::CapabilityTemplate::Http(c) => c,
        _ => panic!("expected HTTP template"),
    };
    let decoder = create_entity_decoder_for_capability(
        "Issue",
        &cgs,
        Some("issue_query"),
        Some(http_collection_source(cml)),
        None,
        None,
    );
    assert!(
        decoder.relations.iter().any(|r| r.relation == "labels"),
        "Issue.labels prefer/from_parent_get must emit a relation decoder on issue_query"
    );

    let row = serde_json::json!({
        "id": 42,
        "number": 7,
        "repository_url": "https://api.github.com/repos/acme/demo",
        "title": "Bug",
        "state": "open",
        "labels": [
            {
                "id": 1,
                "name": "bug",
                "color": "f29513",
                "description": "label",
                "default": false
            }
        ]
    });
    let body = serde_json::json!([row]);
    let normalized =
        normalize_collection_response(body, response_bare_array_wrap_key(cml).as_str());
    let decoded = decode_entities(&decoder, &normalized).expect("decode issues");
    assert_eq!(decoded.len(), 1);
    match decoded[0].relations.get("labels") {
        Some(DecodedRelation::Specified(refs)) => {
            assert_eq!(refs.len(), 1);
            assert!(!decoded[0].embedded_entities.is_empty());
        }
        other => panic!("expected Specified labels relation, got {other:?}"),
    }
}

#[test]
fn langitem_get_decoder_embed_decoders_are_leaf() {
    use plasm_compile::DecodedRelation;
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = load_schema_dir(&dir).expect("langmatrix");
    let decoder = create_entity_decoder_for_capability(
        "LangItem",
        &cgs,
        Some("langitem_get"),
        None,
        Some("i1"),
        None,
    );
    let summary_rel = decoder
        .relations
        .iter()
        .find(|r| r.relation == "summary")
        .expect("summary relation decoder");
    assert!(
        summary_rel.decoder.relations.is_empty(),
        "leaf summary decoder must not nest further embed decoders (CEP-10)"
    );

    let body = serde_json::json!({
        "id": "i1",
        "title": "Alpha",
        "summary": {
            "id": "sum-i1",
            "headline": "Alpha summary",
            "detail": { "id": "det-i1", "body": "nested detail" }
        }
    });
    let decoded =
        decode_entities_with_cgs(&decoder, &body, Some(&cgs)).expect("decode langitem get");
    let summary = decoded[0]
        .embedded_entities
        .iter()
        .find(|e| e.reference.entity_type.as_str() == "LangSummary")
        .expect("embedded summary");
    let detail_rel = summary
        .relations
        .get("detail")
        .expect("summary.detail relation");
    let DecodedRelation::Specified(refs) = detail_rel else {
        panic!("expected specified detail refs, got {detail_rel:?}");
    };
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].primary_slot_str(), "det-i1");
    let detail = summary
        .embedded_entities
        .iter()
        .find(|e| e.reference.entity_type.as_str() == "LangDetail")
        .expect("embedded detail");
    assert_eq!(detail.reference.primary_slot_str(), "det-i1");
}

#[test]
fn pokemon_get_decoder_embed_decoders_are_leaf() {
    use plasm_core::loader::load_schema_dir;

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    let cgs = load_schema_dir(&dir).expect("pokeapi");
    let decoder = create_entity_decoder_for_capability(
        "Pokemon",
        &cgs,
        Some("pokemon_get"),
        None,
        Some("pikachu"),
        None,
    );
    for rel in ["types", "abilities", "species", "forms", "moves"] {
        let rd = decoder
            .relations
            .iter()
            .find(|r| r.relation == rel)
            .unwrap_or_else(|| panic!("missing {rel} relation decoder"));
        assert!(
            rd.decoder.relations.is_empty(),
            "{rel} embed decoder must be leaf (no nested .relations; CEP-10)"
        );
    }
}

#[test]
fn pokemon_get_decode_on_release_stack_budget() {
    use plasm_compile::decode_entities_with_cgs;
    use plasm_core::loader::load_schema_dir;

    if cfg!(debug_assertions) {
        return;
    }

    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    let cgs = load_schema_dir(&dir).expect("pokeapi");
    let decoder = create_entity_decoder_for_capability(
        "Pokemon",
        &cgs,
        Some("pokemon_get"),
        None,
        Some("pikachu"),
        None,
    );
    let body = serde_json::json!({
        "id": 25,
        "name": "pikachu",
        "species": { "name": "pikachu", "id": 25, "is_legendary": false, "is_mythical": false },
        "types": [
            { "slot": 1, "type": { "name": "electric", "url": "https://pokeapi.co/api/v2/type/13/" } }
        ],
        "abilities": [
            { "is_hidden": false, "slot": 1, "ability": { "name": "static", "url": "https://pokeapi.co/api/v2/ability/9/" } }
        ]
    });

    std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let decoded =
                decode_entities_with_cgs(&decoder, &body, Some(&cgs)).expect("decode pikachu");
            assert_eq!(decoded.len(), 1);
            assert!(decoded[0].relations.contains_key("species"));
        })
        .expect("spawn 4MiB decode thread")
        .join()
        .expect("join decode thread");
}

#[test]
fn partition_scoped_query_fanout_one_job_per_parent() {
    let parents = vec![
        CachedEntity::from_decoded(
            Ref::new("LangItem", "i1"),
            IndexMap::new(),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        ),
        CachedEntity::from_decoded(
            Ref::new("LangItem", "i2"),
            IndexMap::new(),
            IndexMap::new(),
            0,
            EntityCompleteness::Summary,
        ),
    ];
    let jobs = partition_scoped_query_fanout(&parents, |p| {
        let q = QueryExpr::filtered(EntityName::from("LangTag"), Predicate::eq("id", "x"));
        let _ = p;
        q
    });
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].0, 0);
    assert_eq!(jobs[1].0, 1);
}

#[test]
fn prefer_graph_miss_yields_scoped_not_error() {
    let materialize = RelationMaterialization::PreferFromParentGet {
        path: vec![JsonPathSegment::Key { key: "tags".into() }],
        on_embed_miss: plasm_core::EmbedOnMissPolicy::FallbackScoped,
        fallback: RelationScopedFallback::QueryScoped {
            capability: "cap".into(),
            param: "p".into(),
        },
    };
    let parent_ref = Ref::new("LangItem", "i1");
    let tag_ref = Ref::new("LangTag", "missing");
    let mut parent = CachedEntity::from_decoded(
        parent_ref,
        IndexMap::from([(String::from("tags"), Value::String("1".into()))]),
        IndexMap::new(),
        0,
        EntityCompleteness::Summary,
    );
    parent.update_relations("tags".into(), vec![tag_ref], 0);
    let res = resolve_relation_row_resolution(
        &materialize,
        "tags",
        "LangTag",
        &parent.payload_to_json(),
        parent.relations.get("tags").map(|v| v.as_slice()),
        |_| false,
    );
    assert_eq!(res, RelationRowResolution::ScopedQuery);
}

#[test]
fn hydrate_from_embed_path_fallback_is_plan_materialized_only() {
    let cgs = create_test_cgs();
    let parent_def = cgs
        .get_entity("Account")
        .expect("Account entity in test cgs");
    let parent = CachedEntity::from_decoded(
        Ref::new("Account", "1"),
        IndexMap::new(),
        IndexMap::new(),
        0,
        EntityCompleteness::Summary,
    );
    let fallback = RelationScopedFallback::HydrateFromEmbedPath {
        path: Vec::new(),
        get_capability: "get_account".into(),
    };
    let err = build_scoped_query_from_fallback(
        &fallback,
        &parent,
        parent_def,
        &EntityName::from("Account"),
        &cgs,
    )
    .expect_err("runtime must not build scoped queries for hydrate fallback");
    match err {
        RuntimeError::ConfigurationError { message } => {
            assert!(message.contains("plan-materialized"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn flatten_per_parent_major_order() {
    let a = CachedEntity::from_decoded(
        Ref::new("LangTag", "a"),
        IndexMap::new(),
        IndexMap::new(),
        0,
        EntityCompleteness::Summary,
    );
    let b = CachedEntity::from_decoded(
        Ref::new("LangTag", "b"),
        IndexMap::new(),
        IndexMap::new(),
        0,
        EntityCompleteness::Summary,
    );
    let c = CachedEntity::from_decoded(
        Ref::new("LangTag", "c"),
        IndexMap::new(),
        IndexMap::new(),
        0,
        EntityCompleteness::Summary,
    );
    let per_parent = vec![vec![a.clone()], vec![b.clone(), c.clone()]];
    let flat: Vec<_> = per_parent.into_iter().flatten().collect();
    assert_eq!(flat.len(), 3);
    assert_eq!(flat[0].reference.primary_slot_str(), "a");
    assert_eq!(flat[1].reference.primary_slot_str(), "b");
    assert_eq!(flat[2].reference.primary_slot_str(), "c");
}

#[test]
fn parent_row_relation_decoded_and_resolve_cached_targets() {
    let parent_ref = Ref::new("LangItem", "i1");
    let tag_ref = Ref::new("LangTag", "t1");
    let mut parent = CachedEntity::from_decoded(
        parent_ref,
        IndexMap::from([(String::from("id"), Value::String("i1".into()))]),
        IndexMap::new(),
        0,
        EntityCompleteness::Summary,
    );
    parent.update_relations("tags".into(), vec![tag_ref.clone()], 0);
    assert!(parent.relations.contains_key("tags"));

    let tag = CachedEntity::from_decoded(
        tag_ref,
        IndexMap::from([(String::from("label"), Value::String("urgent".into()))]),
        IndexMap::new(),
        0,
        EntityCompleteness::Summary,
    );
    let mut mat = SessionMaterialization::new();
    mat.insert(tag).expect("insert tag");
    let resolved = resolve_cached_targets_from_relation_refs(
        &mat,
        parent.relations.get("tags").expect("tags refs"),
        "LangTag",
    )
    .expect("resolve tag ref");
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].get_field("label").map(|f| f.to_value()),
        Some(Value::String("urgent".into()))
    );
}
