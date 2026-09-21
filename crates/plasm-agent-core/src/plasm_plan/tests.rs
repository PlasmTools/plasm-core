use super::*;
use crate::plasm_plan::{
    enrich_uses_result_provenance, parse_plan_value, validate_plan_artifact, validate_plan_value,
};

#[test]
fn program_query_ok() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "read-products",
        "nodes": [{
            "id": "n1",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list",
            "projection": [],
            "predicates": [{
                "field_path": ["state"],
                "op": "eq",
                "value": { "kind": "literal", "value": "open" }
            }],
            "depends_on": [],
            "uses_result": []
        }],
        "return": { "kind": "node", "node": "n1" }
    });
    validate_plan_value(&v).expect("ok");
}

#[test]
fn enrich_uses_result_stamps_source_qualified_entity() {
    let plan: Plan = serde_json::from_value(serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "qe-uses",
        "nodes": [
            {
                "id": "sn",
                "kind": "action",
                "qualified_entity": { "entry_id": "langmatrix", "entity": "LangAuthSession" },
                "effect_class": "side_effect",
                "result_shape": "single",
                "depends_on": [],
                "uses_result": []
            },
            {
                "id": "notes",
                "kind": "search",
                "qualified_entity": { "entry_id": "langmatrix", "entity": "LangSecuredNote" },
                "effect_class": "read",
                "result_shape": "list",
                "depends_on": ["sn"],
                "uses_result": [{ "node": "sn", "as": "sn" }]
            }
        ],
        "return": { "kind": "node", "node": "notes" }
    }))
    .expect("plan serde");
    let uses =
        enrich_uses_result_provenance(&plan.nodes[1].uses_result, &plan, "notes").expect("enrich");
    assert_eq!(uses.len(), 1);
    let qe = uses[0].qualified_entity.as_ref().expect("stamped qe");
    assert_eq!(qe.entry_id, "langmatrix");
    assert_eq!(qe.entity, "LangAuthSession");
}

#[test]
fn legacy_expr_list_is_rejected() {
    let v = serde_json::json!({
        "nodes": [{ "expr": "x" }],
    });
    let err = parse_plan_value(&v).expect_err("legacy expression list rejected");
    assert!(err.contains("missing field"), "{err}");
}

#[test]
fn executable_text_without_ir_is_rejected() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "n1",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "n1" }
    });
    let err = validate_plan_value(&v).expect_err("text-only executable rejected");
    assert!(err.contains("ir or ir_template is required"), "{err}");
}

#[test]
fn legacy_untagged_returns_are_rejected() {
    let base_nodes = serde_json::json!([{
        "id": "n1",
        "kind": "data",
        "effect_class": "artifact_read",
        "result_shape": "artifact",
        "data": { "kind": "literal", "value": [{ "id": "i1" }] }
    }]);
    for return_value in [
        serde_json::json!("n1"),
        serde_json::json!({ "parallel": ["n1"] }),
        serde_json::json!({ "name": "n1" }),
    ] {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": base_nodes.clone(),
            "return": return_value
        });
        let err = parse_plan_value(&v).expect_err("untagged return rejected");
        assert!(err.contains("tag") || err.contains("kind"), "{err}");
    }
}

#[test]
fn reject_cycle() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "cycle",
        "nodes": [
            {
                "id": "a",
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "depends_on": ["b"]
            },
            {
                "id": "b",
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "depends_on": ["a"]
            }
        ],
        "return": { "kind": "node", "node": "a" }
    });
    assert!(validate_plan_value(&v).is_err());
}

#[test]
fn reject_missing_qualified_entity() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "n1",
            "kind": "query",
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "n1" }
    });
    assert!(validate_plan_value(&v).is_err());
}

#[test]
fn page_surface_node_may_omit_qualified_entity() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "pg",
            "kind": "query",
            "expr": "page(l_AAAAAAAAQACAAAAAAAAAAQ_pg1)",
            "ir": { "expr": { "op": "page", "handle": "l_AAAAAAAAQACAAAAAAAAAAQ_pg1" } },
            "effect_class": "read",
            "result_shape": "page"
        }],
        "return": { "kind": "node", "node": "pg" }
    });
    validate_plan_value(&v).expect("page node validates without CGS entity key");
}

#[test]
fn for_each_write_template_does_not_trust_agent_authored_approval() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "find",
                "kind": "query",
                "qualified_entity": { "entry_id": "github", "entity": "Issue" },
                "expr": "Issue{state=open}",
                "ir": { "expr": { "op": "query", "entity": "Issue", "predicate": { "type": "comparison", "field": "state", "op": "=", "value": "open" } } },
                "effect_class": "read",
                "result_shape": "list"
            },
            {
                "id": "label",
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": "find",
                "item_binding": "issue",
                "depends_on": ["find"],
                "uses_result": [{ "node": "find", "as": "issue" }],
                "effect_template": {
                    "kind": "action",
                    "qualified_entity": { "entry_id": "github", "entity": "Issue" },
                    "expr_template": "Issue({{ issue.id }}).add-label(label=\"stale\")",
                    "ir_template": {
                        "expr": {
                            "op": "invoke",
                            "capability": "add_label",
                            "target": { "entity_type": "Issue", "key": { "__plasm_hole": { "kind": "binding", "binding": "issue", "path": ["id"] } } },
                            "input": { "label": "stale" }
                        },
                        "input_bindings": [{ "from": "issue.id", "to": "id" }]
                    },
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack"
                }
            }
        ],
        "return": { "kind": "parallel", "nodes": ["find", "label"] }
    });
    validate_plan_value(&v).expect("host infers approval gates during dry-run");
}

#[test]
fn display_is_inert_while_value_templates_validate() {
    let bad_surface = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "n1",
            "kind": "get",
            "qualified_entity": { "entry_id": "acme", "entity": "Item" },
            "expr": "Item(\"[object Object]\")",
            "ir": { "expr": { "op": "get", "ref": { "entity_type": "Item", "key": "x" } } },
            "effect_class": "read",
            "result_shape": "single"
        }],
        "return": { "kind": "node", "node": "n1" }
    });
    validate_plan_value(&bad_surface).expect("display text is inert");

    let bad_template = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "rows",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "literal", "value": [{ "id": "i1" }] }
            },
            {
                "id": "mapped",
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "depends_on": ["rows"],
                "uses_result": [{ "node": "rows", "as": "item" }],
                "derive_template": {
                    "kind": "map",
                    "source": "rows",
                    "item_binding": "item",
                    "inputs": [],
                    "value": { "kind": "template", "template": "Item(\"[object Object]\")" }
                }
            }
        ],
        "return": { "kind": "node", "node": "mapped" }
    });
    let err = validate_plan_value(&bad_template).expect_err("bad template rejected");
    assert!(err.contains("[object Object]"), "{err}");
}

#[test]
fn reject_malformed_template_substitutions() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "rows",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "literal", "value": [{ "id": "i1" }] }
            },
            {
                "id": "mapped",
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "depends_on": ["rows"],
                "uses_result": [{ "node": "rows", "as": "item" }],
                "derive_template": {
                    "kind": "map",
                    "source": "rows",
                    "item_binding": "item",
                    "inputs": [],
                    "value": { "kind": "template", "template": "Item(${})" }
                }
            }
        ],
        "return": { "kind": "node", "node": "mapped" }
    });
    let err = validate_plan_value(&v).expect_err("empty/dollar substitution rejected");
    assert!(
        err.contains("abolished") || (err.contains("empty") && err.contains("substitution")),
        "{err}"
    );
}

#[test]
fn reject_unnormalized_entity_ref_wrapper_predicate_values() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "commits",
            "kind": "query",
            "qualified_entity": { "entry_id": "github", "entity": "Commit" },
            "expr": "Commit{repository=\"ryan-s-roberts/plasm-core\"}",
            "ir": {
                "expr": {
                    "op": "query",
                    "entity": "Commit",
                    "predicate": {
                        "type": "comparison",
                        "field": "repository",
                        "op": "=",
                        "value": "ryan-s-roberts/plasm-core"
                    }
                }
            },
            "effect_class": "read",
            "result_shape": "list",
            "predicates": [{
                "field_path": ["repository"],
                "op": "eq",
                "value": {
                    "kind": "object",
                    "fields": {
                        "api": { "kind": "literal", "value": "github" },
                        "entity": { "kind": "literal", "value": "Repository" },
                        "key": { "kind": "literal", "value": "ryan-s-roberts/plasm-core" }
                    }
                }
            }]
        }],
        "return": { "kind": "node", "node": "commits" }
    });
    let err = validate_plan_value(&v).expect_err("unnormalized wrapper rejected");
    assert!(err.contains("unnormalized entity_ref wrapper"), "{err}");
}

#[test]
fn explicit_entity_ref_key_predicate_values_validate() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "commits",
            "kind": "query",
            "qualified_entity": { "entry_id": "github", "entity": "Commit" },
            "expr": "Commit{repository=\"ryan-s-roberts/plasm-core\"}",
            "ir": {
                "expr": {
                    "op": "query",
                    "entity": "Commit",
                    "predicate": {
                        "type": "comparison",
                        "field": "repository",
                        "op": "=",
                        "value": "ryan-s-roberts/plasm-core"
                    }
                }
            },
            "effect_class": "read",
            "result_shape": "list",
            "predicates": [{
                "field_path": ["repository"],
                "op": "eq",
                "value": {
                    "kind": "entity_ref_key",
                    "api": "github",
                    "entity": "Repository",
                    "key": { "kind": "literal", "value": "ryan-s-roberts/plasm-core" }
                }
            }]
        }],
        "return": { "kind": "node", "node": "commits" }
    });
    validate_plan_value(&v).expect("explicit entity_ref_key is valid");
}

#[test]
fn predicate_helper_values_are_rejected() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "n1",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product{updated_at>30d}",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list",
            "predicates": [{
                "field_path": ["updated_at"],
                "op": "gt",
                "value": {
                    "kind": "helper",
                    "name": "daysAgo",
                    "args": [30],
                    "display": "30d"
                }
            }]
        }],
        "return": { "kind": "node", "node": "n1" }
    });
    let err = validate_plan_value(&v).expect_err("helper predicate rejected");
    assert!(err.contains("unknown variant `helper`"), "{err}");
}

#[test]
fn compound_entity_ref_key_predicate_values_validate() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "n1",
            "kind": "query",
            "qualified_entity": { "entry_id": "github", "entity": "Commit" },
            "expr": "Commit.query(repository=Repository{owner=\"ryan-s-roberts\", repo=\"plasm-core\"})",
            "ir": {
                "expr": {
                    "op": "query",
                    "entity": "Commit",
                    "predicate": {
                        "type": "comparison",
                        "field": "repository",
                        "op": "=",
                        "value": {
                            "owner": "ryan-s-roberts",
                            "repo": "plasm-core"
                        }
                    }
                }
            },
            "effect_class": "read",
            "result_shape": "list",
            "predicates": [{
                "field_path": ["repository"],
                "op": "eq",
                "value": {
                    "kind": "entity_ref_key",
                    "api": "github",
                    "entity": "Repository",
                    "key": {
                        "kind": "object",
                        "fields": {
                            "owner": { "kind": "literal", "value": "ryan-s-roberts" },
                            "repo": { "kind": "literal", "value": "plasm-core" }
                        }
                    }
                }
            }]
        }],
        "return": { "kind": "node", "node": "n1" }
    });
    validate_plan_value(&v).expect("compound entity_ref_key is valid");
}

#[test]
fn compound_get_ref_key_validates() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "repo",
            "kind": "get",
            "qualified_entity": { "entry_id": "github", "entity": "Repository" },
            "expr": "Repository{owner=\"ryan-s-roberts\", repo=\"plasm-core\"}",
            "ir": {
                "expr": {
                    "op": "get",
                    "ref": {
                        "entity_type": "Repository",
                        "key": {
                            "owner": "ryan-s-roberts",
                            "repo": "plasm-core"
                        }
                    }
                }
            },
            "effect_class": "read",
            "result_shape": "single"
        }],
        "return": { "kind": "node", "node": "repo" }
    });
    validate_plan_value(&v).expect("compound get ref key is valid");
}

#[test]
fn compute_rejects_unknown_source_and_bad_aggregate() {
    let unknown_source = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "by_state",
            "kind": "compute",
            "effect_class": "artifact_read",
            "result_shape": "list",
            "compute": {
                "source": "missing",
                "op": { "kind": "group_by", "keys": ["state"], "aggregates": [{ "name": "count", "function": "count" }] },
                "schema": { "fields": [{ "name": "key", "value_kind": "string" }, { "name": "count", "value_kind": "integer" }] }
            }
        }],
        "return": { "kind": "node", "node": "by_state" }
    });
    assert!(validate_plan_value(&unknown_source).is_err());

    let bad_aggregate = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "rows",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "literal", "value": [{ "points": 1 }] }
            },
            {
                "id": "totals",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "compute": {
                    "source": "rows",
                    "op": { "kind": "aggregate", "aggregates": [{ "name": "total", "function": "sum" }] },
                    "schema": { "fields": [{ "name": "total", "value_kind": "number" }] }
                }
            }
        ],
        "return": { "kind": "node", "node": "totals" }
    });
    assert!(validate_plan_value(&bad_aggregate).is_err());
}

#[test]
fn search_requires_read_list_shape() {
    let bad_shape = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "search",
            "kind": "search",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product~\"bolt\"",
            "ir": { "expr": { "op": "query", "entity": "Product", "predicate": { "type": "comparison", "field": "q", "op": "=", "value": "bolt" }, "capability_name": "product_search" } },
            "effect_class": "read",
            "result_shape": "single"
        }],
        "return": { "kind": "node", "node": "search" }
    });
    let err = validate_plan_value(&bad_shape).expect_err("bad search shape rejected");
    assert!(err.contains("search result_shape must be list"), "{err}");
}

#[test]
fn relation_traversal_carries_validated_proof() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "product",
                "kind": "get",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product(\"p1\")",
                "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                "effect_class": "read",
                "result_shape": "single"
            },
            {
                "id": "category",
                "kind": "relation",
                "effect_class": "read",
                "result_shape": "single",
                "relation": {
                    "source": "product",
                    "relation": "category",
                    "target": { "entry_id": "acme", "entity": "Category" },
                    "cardinality": "one",
                    "source_cardinality": "single",
                    "expr": "Product(\"p1\").category",
                    "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                },
                "depends_on": ["product"],
                "uses_result": [{ "node": "product", "as": "source" }]
            }
        ],
        "return": { "kind": "node", "node": "category" }
    });
    let plan = parse_plan_value(&v).expect("parse");
    let validated = validate_plan_artifact(&plan).expect("validate");
    assert_eq!(validated.topological_order()[0].as_str(), "product");
    assert_eq!(validated.topological_order()[1].as_str(), "category");
    assert!(matches!(
        &validated.nodes()[1],
        ValidatedPlanNode::RelationTraversal(node)
            if node.relation.target.entity == "Category"
    ));
}

#[test]
fn relation_one_from_plural_source_lowers_to_fanout() {
    // A one-cardinality relation over a plural source is a valid 1:1 flat-map
    // (one target per parent); it must validate and lower to a relation traversal
    // node — no `Plan.singleton(...)` required. See the cardinality lattice in
    // `docs/plasm-language-definition.md`.
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "products",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            },
            {
                "id": "category",
                "kind": "relation",
                "effect_class": "read",
                "result_shape": "list",
                "relation": {
                    "source": "products",
                    "relation": "category",
                    "target": { "entry_id": "acme", "entity": "Category" },
                    "cardinality": "one",
                    "source_cardinality": "many",
                    "expr": "Product.category",
                    "ir": { "expr": { "op": "chain", "source": { "op": "query", "entity": "Product" }, "selector": "category", "step": { "type": "auto_get" } } }
                },
                "depends_on": ["products"]
            }
        ],
        "return": { "kind": "node", "node": "category" }
    });
    validate_plan_value(&v).expect("plural one relation validates as fanout");
    let plan = parse_plan_value(&v).expect("parse");
    let validated = validate_plan_artifact(&plan).expect("validate");
    assert!(matches!(
        &validated.nodes()[1],
        ValidatedPlanNode::RelationTraversal(node)
            if node.relation.cardinality == RelationCardinality::One
                && node.relation.source_cardinality == RelationSourceCardinality::Many
                && node.relation.target.entity == "Category"
    ));
}

#[test]
fn validated_plan_exposes_topology_and_typed_return_refs() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "rows",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "literal", "value": [{ "state": "open" }] }
            },
            {
                "id": "limited",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "compute": {
                    "source": "rows",
                    "op": { "kind": "limit", "count": 1 },
                    "schema": { "fields": [{ "name": "state", "value_kind": "string", "source": ["state"] }] },
                    "page_size": 1
                }
            }
        ],
        "return": { "kind": "node", "node": "limited" }
    });
    let plan = parse_plan_value(&v).expect("parse");
    let validated = validate_plan_artifact(&plan).expect("validate");
    assert_eq!(validated.topological_order()[0].as_str(), "rows");
    assert_eq!(validated.topological_order()[1].as_str(), "limited");
    assert_eq!(validated.return_value().refs()[0].as_str(), "limited");
}

fn render_plan(columns: serde_json::Value, template: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "rows",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "literal", "value": [{ "name": "bolt" }] }
            },
            {
                "id": "doc",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "compute": {
                    "source": "rows",
                    "op": { "kind": "render", "columns": columns, "template": template },
                    "schema": {
                        "entity": "PlanRender",
                        "fields": [{ "name": "content", "value_kind": "string" }]
                    }
                },
                "depends_on": ["rows"],
                "uses_result": [{ "node": "rows", "as": "source" }]
            }
        ],
        "return": { "kind": "node", "node": "doc" }
    })
}

#[test]
fn validate_render_rejects_empty_columns() {
    let err = validate_plan_value(&render_plan(serde_json::json!([]), serde_json::json!("ok")))
        .expect_err("empty columns rejected");
    assert!(
        err.contains("compute.render.columns must be non-empty"),
        "{err}"
    );
}

#[test]
fn validate_render_rejects_empty_template() {
    let err = validate_plan_value(&render_plan(
        serde_json::json!(["name"]),
        serde_json::json!(" "),
    ))
    .expect_err("empty template rejected");
    assert!(
        err.contains("compute.render.template must be non-empty"),
        "{err}"
    );
}

#[test]
fn validate_render_rejects_duplicate_columns() {
    let err = validate_plan_value(&render_plan(
        serde_json::json!(["name", "name"]),
        serde_json::json!("{{ rows }}"),
    ))
    .expect_err("duplicate columns rejected");
    assert!(
        err.contains("compute.render.columns has duplicate"),
        "{err}"
    );
}

#[test]
fn validate_render_rejects_empty_column_name() {
    let err = validate_plan_value(&render_plan(
        serde_json::json!([""]),
        serde_json::json!("{{ rows }}"),
    ))
    .expect_err("empty column name rejected");
    assert!(err.contains("OutputName must be non-empty"), "{err}");
}

#[test]
fn validate_render_rejects_non_content_schema() {
    let mut plan = render_plan(serde_json::json!(["name"]), serde_json::json!("{{ rows }}"));
    plan["nodes"][1]["compute"]["schema"] = serde_json::json!({
        "entity": "PlanRender",
        "fields": [{ "name": "body", "value_kind": "string" }]
    });
    let err = validate_plan_value(&plan).expect_err("bad schema rejected");
    assert!(err.contains("single string field named 'content'"), "{err}");
}

#[test]
fn validate_render_rejects_template_syntax_errors() {
    let err = validate_plan_value(&render_plan(
        serde_json::json!(["name"]),
        serde_json::json!("{{"),
    ))
    .expect_err("bad minijinja syntax rejected");
    assert!(err.contains("compute.render.template"), "{err}");
}

#[test]
fn for_each_effect_template_rejects_undeclared_interpolation_alias() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "bad-for-each",
        "nodes": [
            {
                "id": "find",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "data": { "kind": "literal", "value": [{ "id": "p1" }] }
            },
            {
                "id": "label",
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": "find",
                "item_binding": "_",
                "depends_on": ["find"],
                "uses_result": [{ "node": "find", "as": "_" }],
                "effect_template": {
                    "kind": "action",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr_template": "Product.create(title=<<T\n{{ missing.content }}\nT\n)",
                    "ir_template": {
                        "expr": {
                            "op": "create",
                            "capability": "product_create",
                            "entity": "Product",
                            "input": { "title": {"__plasm_string_template": "{{ missing.content }}"} }
                        },
                        "input_bindings": []
                    },
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack"
                }
            }
        ],
        "return": { "kind": "node", "node": "label" }
    });
    let plan = parse_plan_value(&v).expect("parse");
    let err = validate_plan_artifact(&plan).expect_err("undeclared alias rejected");
    assert!(
        err.contains("undeclared alias") || err.contains("missing"),
        "{err}"
    );
}

#[test]
fn render_template_rejects_dollar_interpolation_with_actionable_copy() {
    let v = serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [
            {
                "id": "items",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "literal", "value": [{ "name": "a" }] },
                "depends_on": [],
                "uses_result": []
            },
            {
                "id": "report",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "single",
                "compute": {
                    "source": "items",
                    "op": {
                        "kind": "render",
                        "columns": ["name"],
                        "template": "- ${items.name}\n",
                        "column_aliases": {}
                    },
                    "schema": {
                        "entity": "PlanRender",
                        "fields": [{ "name": "content", "value_kind": "string" }]
                    }
                },
                "depends_on": ["items"],
                "uses_result": [{ "node": "items", "as": "source" }]
            }
        ],
        "return": { "kind": "node", "node": "report" }
    });
    let plan = parse_plan_value(&v).expect("parse");
    let err = validate_plan_artifact(&plan).expect_err("dollar interpolation rejected");
    assert!(err.contains("abolished") || err.contains("${"), "{err}");
    assert!(err.contains("Minijinja"), "{err}");
}

#[test]
fn typed_operand_scope_is_checked_before_plan_admission() {
    let mut plan: Plan = serde_json::from_value(serde_json::json!({
        "version": 1, "kind": "program",
        "nodes": [{
            "id": "n1", "kind": "get",
            "qualified_entity": {"entry_id": "matrix", "entity": "Item"},
            "ir": {"expr": {"op": "get", "ref": {"entity_type": "Item", "key": "fixed"}}},
            "effect_class": "read", "result_shape": "single"
        }], "return": {"kind": "node", "node": "n1"}
    }))
    .unwrap();
    for reference in [
        plasm_core::PlasmInputRef::node_output("missing", vec!["id".into()]),
        plasm_core::PlasmInputRef::row_binding("_", vec!["id".into()]),
    ] {
        let plasm_core::Expr::Get(get) = &mut plan.nodes[0].ir.as_mut().unwrap().expr else {
            panic!("get")
        };
        get.reference.key =
            plasm_core::EntityKey::Simple(plasm_core::IdentitySlot::binding(reference));
        let error = validate_plan_artifact(&plan).expect_err("unbound operand rejected");
        assert!(
            error.contains("undeclared input alias") || error.contains("outside its scope"),
            "{error}"
        );
    }
}

#[test]
fn display_text_does_not_affect_executable_admission() {
    let mut plan: Plan = serde_json::from_value(serde_json::json!({
        "version": 1, "kind": "program",
        "nodes": [{
            "id": "n1", "kind": "query",
            "qualified_entity": {"entry_id": "matrix", "entity": "Item"},
            "ir": {"expr": {"op": "query", "entity": "Item"}},
            "effect_class": "read", "result_shape": "list"
        }], "return": {"kind": "node", "node": "n1"}
    }))
    .unwrap();
    for display in ["Item", "[object Object]", "{{ missing }}", ""] {
        plan.nodes[0].expr = Some(display.into());
        plan.nodes[0].ir.as_mut().unwrap().display_expr = Some(display.into());
        validate_plan_artifact(&plan).expect("display is inert");
    }
}
