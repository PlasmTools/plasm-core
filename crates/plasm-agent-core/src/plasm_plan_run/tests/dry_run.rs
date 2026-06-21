use super::super::*;
use super::support::*;
use plasm_core::load_schema;
use plasm_core::CgsContext;
use plasm_core::TeachingExposureSession;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn cmp_json_sort_values_orders_multi_digit_numbers_numerically() {
    use std::cmp::Ordering;
    let n87 = serde_json::json!(87);
    let n300 = serde_json::json!(300);
    assert_eq!(
        cmp_json_sort_values(Some(&n87), Some(&n300)),
        Ordering::Less
    );
    let s87 = serde_json::json!("87");
    let s300 = serde_json::json!("300");
    assert_eq!(
        cmp_json_sort_values(Some(&s87), Some(&s300)),
        Ordering::Less
    );
}

#[test]
fn singleton_input_zero_row_error_is_actionable() {
    let err = singleton_input_row_count_error("src", "_", 0, "staged expression rendering");
    assert!(err.contains("zero rows"), "{err}");
    assert!(err.contains("not a Plasm syntax error"), "{err}");
    assert!(err.contains("branch around empty results"), "{err}");
}

#[test]
fn singleton_input_multi_row_error_mentions_ambiguity_remedy() {
    let err = singleton_input_row_count_error("src", "_", 2, "staged expression rendering");
    assert!(err.contains("2 rows"), "{err}");
    assert!(err.contains("make the source unique"), "{err}");
    assert!(err.contains(".singleton()"), "{err}");
}

#[test]
fn cmp_json_sort_values_string_collates_non_numeric_strings_lexically() {
    use std::cmp::Ordering;
    let apple = serde_json::json!("apple");
    let banana = serde_json::json!("banana");
    assert_eq!(
        cmp_json_sort_values(Some(&apple), Some(&banana)),
        Ordering::Less
    );
}

/// Regression: `.sort(score)` must not stringify numbers and compare lexicographically (where
/// `87` sorts after `300`). Keeps parity with [`eval_compute`] `ComputeOp::Sort` staging.
#[test]
fn plan_sort_compute_orders_integer_scores_numerically() {
    let key = FieldPath::from_dotted("score").expect("score path");
    let mut rows = [
        serde_json::json!({"id": "n300", "score": 300}),
        serde_json::json!({"id": "n87", "score": 87}),
        serde_json::json!({"id": "n100", "score": 100}),
    ];
    rows.sort_by(|a, b| cmp_json_sort_values(value_at_path(a, &key), value_at_path(b, &key)));
    assert_eq!(rows[0]["id"], "n87");
    assert_eq!(rows[1]["id"], "n100");
    assert_eq!(rows[2]["id"], "n300");

    rows.reverse();
    assert_eq!(rows[0]["id"], "n300");
    assert_eq!(rows[1]["id"], "n100");
    assert_eq!(rows[2]["id"], "n87");
}

fn github_repository_commit_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(load_schema(&root.join("../../apis/github")).expect("load github"));
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "github".into(),
        Arc::new(CgsContext::entry("github", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["Repository", "Commit"]);
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "github".into(),
        String::new(),
        String::new(),
        None,
        vec!["Repository".into(), "Commit".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

#[test]
fn entry_scoped_surface_parse_preserves_typed_catalog_create() {
    let s = duplicate_product_create_session();
    let err = parse_parsed_expr_for_session(&s, "Product.create(name=\"bolt\")")
        .expect_err("unscoped federated create should be ambiguous")
        .to_string();
    assert!(
        err.contains("ambiguous entity `Product`")
            || err.contains("ambiguous capability label `create`"),
        "{err}"
    );

    let scoped = entry_scoped_execute_session(
        &s,
        Some(&QualifiedEntityKey {
            entry_id: "other".to_string(),
            entity: "Product".to_string(),
        }),
    )
    .expect("scope session");
    let parsed = parse_parsed_expr_for_session(&scoped, "Product.create(name=\"bolt\")")
        .expect("scoped create parses");
    typecheck_parsed_for_session(&scoped, &parsed).expect("scoped create typechecks");
}

#[test]
fn federated_e2_method_symbol_resolves_scoped_create() {
    let s = duplicate_product_create_session();
    let map = s
        .teaching_exposure
        .as_ref()
        .expect("exposure")
        .symbol_map_arc();
    let m_sym = map.method_sym_for("other", "Product", "create");
    assert!(
        m_sym.starts_with('m'),
        "expected opaque m# for other/Product.create, got {m_sym}"
    );
    let program = format!("e2.{m_sym}(name=\"bolt\")");
    let parsed = parse_parsed_expr_for_session(&s, &program).expect("e2.m# create parses");
    typecheck_parsed_for_session(&s, &parsed).expect("e2.m# create typechecks");
}

#[test]
fn federated_bare_entity_mutator_stays_ambiguous() {
    let s = duplicate_product_create_session();
    let err = parse_parsed_expr_for_session(&s, "Product.create(name=\"bolt\")")
        .expect_err("unscoped federated create should stay ambiguous")
        .to_string();
    assert!(
        err.contains("ambiguous entity `Product`")
            || err.contains("ambiguous capability label `create`"),
        "{err}"
    );
}

#[test]
fn federated_github_linear_issue_children_relation_dry_run() {
    let Some(session) = federated_github_linear_issue_session() else {
        return;
    };
    let map = session
        .teaching_exposure
        .as_ref()
        .expect("exposure")
        .symbol_map_arc();
    let e2 = map.entity_sym_for("linear", "Issue");
    let r_sym = map.ident_sym_relation_for("linear", "Issue", "children");
    let program = format!(
        r#"parent = {e2}("issue-id")
kids = parent.{r_sym}
kids"#
    );
    let plan = crate::plasm_dag::compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "fed-linear-children",
        &program,
    )
    .expect("compile federated linear children hop");
    evaluate_plasm_plan_dry(&session, &plan).expect("dry-run federated linear children");
}

#[test]
fn federated_linear_issue_create_dry_run_preflight_compiles() {
    let Some(session) = federated_github_linear_issue_team_session() else {
        return;
    };
    let map = session
        .teaching_exposure
        .as_ref()
        .expect("exposure")
        .symbol_map_arc();
    let e2 = map.entity_sym_for("linear", "Issue");
    let e3 = map.entity_sym_for("linear", "Team");
    let m_create = map.method_sym_for("linear", "Issue", "create");
    let p_key = map.ident_sym_entity_field_for("linear", "Team", "key");
    let program = format!(
        "{e2}.{m_create}(team={e3}({p_key}=EVA), title=\"federation triage dry-run\")"
    );
    let plan = crate::plasm_dag::compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "fed-linear-create",
        &program,
    )
    .expect("compile federated linear issue create");
    evaluate_plasm_plan_dry(&session, &plan).expect("dry-run federated linear issue create");
}

#[test]
fn plan_parses_product_query() {
    let s = test_session();
    let pe = parse_parsed_expr_for_session(&s, "Product").expect("parse");
    let v = serde_json::json!({ "expr": pe.expr, "projection": pe.projection });
    assert!(v.get("expr").is_some());
}

/// `e#` is session-local (teaching TSV); single-catalog + exposure must not parse `e1` as an entity *name*.
/// (`.page_size(n)` is Plasm program postfix sugar; the core line parser does not treat it as Plasm path syntax.)
#[test]
fn parse_resolves_e1_with_teaching_exposure() {
    let s = test_session();
    let _ = parse_parsed_expr_for_session(&s, "e1").expect("e1 => first taught entity (Product)");
}

#[test]
fn dry_run_typechecks_product_query() {
    let s = test_session();
    let pe = parse_parsed_expr_for_session(&s, "Product").expect("parse");
    typecheck_parsed_for_session(&s, &pe).expect("typecheck");
}

#[test]
fn dry_run_simulation_includes_intent_il_and_bindings() {
    let s = test_session();
    let pe = parse_parsed_expr_for_session(&s, "Product").expect("parse");
    let (intent, il, bindings) = dry_run_simulation_for_session(&s, &pe);
    assert!(
        intent.contains("Query") && intent.contains("Product"),
        "{intent}"
    );
    assert!(il.contains("cap=product_list"), "il must resolve cap: {il}");
    let m = bindings.as_object().expect("object");
    assert_eq!(m.get("op").and_then(|v| v.as_str()), Some("query"));
}

#[test]
fn evaluate_plasm_plan_dry_matches_single_node() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "read-products",
        "nodes": [{
            "id": "n0",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "n0" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert_eq!(dry.node_results.len(), 1);
    assert!(dry.parallel_root_surfaces_only);
}

#[test]
fn evaluate_plasm_plan_dry_accepts_search_node() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "search-products",
        "nodes": [{
            "id": "search",
            "kind": "search",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "Product~\"bolt\"",
            "ir": { "expr": { "op": "query", "entity": "Product", "predicate": { "type": "comparison", "field": "q", "op": "=", "value": "bolt" }, "capability_name": "product_search" } },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "search" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert!(dry.parallel_root_surfaces_only);
    assert_eq!(dry.node_results[0]["kind"], "search");
}

fn langmatrix_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix"))
            .expect("load plasm_language_matrix"),
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix".into(),
        Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem"]);
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "langmatrix".into(),
        String::new(),
        String::new(),
        None,
        vec!["LangItem".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

fn matrix_views_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix_views"))
            .expect("load plasm_language_matrix_views"),
    );
    let mut ctxs = indexmap::IndexMap::new();
    ctxs.insert(
        "langmatrix_views".into(),
        Arc::new(CgsContext::entry("langmatrix_views", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(
        cgs.as_ref(),
        "langmatrix_views",
        &["LangDigest", "LangItem"],
    );
    ExecuteSession::new(
        "ph".into(),
        "p".into(),
        cgs.clone(),
        ctxs,
        "langmatrix_views".into(),
        String::new(),
        String::new(),
        None,
        vec!["LangDigest".into(), "LangItem".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
        None,
    )
}

#[test]
fn dry_run_rejects_query_node_resolving_search_capability() {
    let s = langmatrix_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "brace-search-mismatch",
        "nodes": [{
            "id": "n0",
            "kind": "query",
            "qualified_entity": { "entry_id": "langmatrix", "entity": "LangItem" },
            "expr": "LangItem{team_key=ENG}",
            "ir": {
                "expr": {
                    "op": "query",
                    "entity": "LangItem",
                    "predicate": {
                        "type": "comparison",
                        "field": "team_key",
                        "op": "=",
                        "value": "ENG"
                    },
                    "capability_name": "langitem_search"
                }
            },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "n0" }
    });
    let err = evaluate_plasm_plan_dry(&s, &plan).expect_err("query vs search dispatch");
    assert!(
        err.contains("kind query but expression resolved search capability"),
        "{err}"
    );
}

#[test]
fn dry_run_preflight_runs_query_capability_normalize_gates() {
    use crate::execute_pipeline::PlasmPreflight;

    let s = langmatrix_session();
    let pe = parse_parsed_expr_for_session(&s, "LangItem{owner=acme}").expect("parse brace owner");
    PlasmPreflight::dry_preview_for_line(&s, "LangItem{owner=acme}", &pe)
        .expect("normalize + compile dispatch for scoped query filter");
}

#[test]
fn dry_run_view_backed_matrix_views_preflight() {
    use crate::execute_pipeline::PlasmPreflight;

    let s = matrix_views_session();
    let lines = [
        r#"LangDigest{item_id="item-1"}"#,
        r#"LangTriageContext{item_id="item-1"}"#,
        r#"LangItemLink{item_id="item-1"}"#,
        r#"LangOwnerFilterDemo{item_id="item-1"}"#,
    ];
    for line in lines {
        let pe = parse_parsed_expr_for_session(&s, line)
            .unwrap_or_else(|err| panic!("parse `{line}`: {err}"));
        PlasmPreflight::dry_preview_for_line(&s, line, &pe).unwrap_or_else(|err| {
            panic!("view-backed dry preflight for `{line}`: {err}");
        });
    }
}

#[test]
fn dry_run_compiled_search_projection_rejects_filter_input_param() {
    use crate::plasm_compile::compile_plasm_expression;
    use plasm_core::PromptPipelineConfig;

    let s = langmatrix_session();
    let pipeline = PromptPipelineConfig::default();
    let source = r#"rows = LangItem~"probe"{team_key="eng"}[q]
rows"#;
    match compile_plasm_expression(&pipeline, None, &s, "search-proj-input", source) {
        Err(err) => {
            assert!(err.contains("is an input on langitem_search"), "{err}");
        }
        Ok(bundle) => {
            let dry_err = evaluate_plasm_comp_dry(&s, &bundle)
                .expect_err("dry must reject search input projection");
            assert!(
                dry_err.contains("is an input on langitem_search")
                    || dry_err.contains("postfix projection"),
                "{dry_err}"
            );
        }
    }
}

#[test]
fn evaluate_plasm_plan_dry_typechecks_ir_not_display_text() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "read-products",
        "nodes": [{
            "id": "n0",
            "kind": "query",
            "qualified_entity": { "entry_id": "acme", "entity": "Product" },
            "expr": "WrongEntity",
            "ir": { "expr": { "op": "query", "entity": "Product" } },
            "effect_class": "read",
            "result_shape": "list"
        }],
        "return": { "kind": "node", "node": "n0" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert_eq!(
        dry.node_results[0]["execution_contract"]["ir"]["entity"],
        "Product"
    );
}

#[test]
fn evaluate_plasm_plan_dry_rejects_relation_target_mismatch() {
    let s = test_session();
    let plan = serde_json::json!({
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
                "id": "bad_relation",
                "kind": "relation",
                "effect_class": "read",
                "result_shape": "single",
                "relation": {
                    "source": "product",
                    "relation": "category",
                    "target": { "entry_id": "acme", "entity": "Product" },
                    "cardinality": "one",
                    "source_cardinality": "single",
                    "expr": "Product(\"p1\").category",
                    "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                },
                "depends_on": ["product"],
                "uses_result": [{ "node": "product", "as": "source" }]
            }
        ],
        "return": { "kind": "node", "node": "bad_relation" }
    });
    let err = evaluate_plasm_plan_dry(&s, &plan).expect_err("relation target mismatch rejected");
    assert!(err.contains("does not match CGS target"), "{err}");
}

#[test]
fn evaluate_plasm_plan_dry_typechecks_relation_node() {
    let s = test_session();
    let plan = serde_json::json!({
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
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert!(!dry.parallel_root_surfaces_only);
    assert_eq!(
        dry.node_results[1]["simulation"]["kind"],
        "relation_traversal"
    );
}

#[test]
fn evaluate_plasm_plan_dry_accepts_runtime_checked_singleton_relation() {
    let s = test_session();
    let plan = serde_json::json!({
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
                "result_shape": "single",
                "relation": {
                    "source": "products",
                    "relation": "category",
                    "target": { "entry_id": "acme", "entity": "Category" },
                    "cardinality": "one",
                    "source_cardinality": "runtime_checked_singleton",
                    "expr": "Product.category",
                    "ir": { "expr": { "op": "chain", "source": { "op": "query", "entity": "Product" }, "selector": "category", "step": { "type": "auto_get" } } }
                },
                "depends_on": ["products"],
                "uses_result": [{ "node": "products", "as": "source" }]
            }
        ],
        "return": { "kind": "node", "node": "category" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert_eq!(
        dry.node_results[1]["simulation"]["kind"],
        "relation_traversal"
    );
}

#[test]
fn evaluate_plasm_plan_dry_accepts_github_relation_limit_aggregate() {
    let s = github_repository_commit_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "github-repo-commits-aggregate",
        "nodes": [
            {
                "id": "repo",
                "kind": "get",
                "qualified_entity": { "entry_id": "github", "entity": "Repository" },
                "expr": "Repository({owner=\"ryan-s-roberts\", repo=\"plasm-core\"})",
                "ir": { "expr": { "op": "get", "ref": { "entity_type": "Repository", "key": { "owner": "ryan-s-roberts", "repo": "plasm-core" } } } },
                "effect_class": "read",
                "result_shape": "single"
            },
            {
                "id": "commits",
                "kind": "relation",
                "effect_class": "read",
                "result_shape": "list",
                "relation": {
                    "source": "repo",
                    "relation": "commits",
                    "target": { "entry_id": "github", "entity": "Commit" },
                    "cardinality": "many",
                    "source_cardinality": "single",
                    "expr": "Repository({owner=\"ryan-s-roberts\", repo=\"plasm-core\"}).commits[sha,message]",
                    "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Repository", "key": { "owner": "ryan-s-roberts", "repo": "plasm-core" } } }, "selector": "commits", "step": { "type": "auto_get" } }, "projection": ["sha", "message"] }
                },
                "qualified_entity": { "entry_id": "github", "entity": "Commit" },
                "projection": ["sha", "message"],
                "depends_on": ["repo"],
                "uses_result": [{ "node": "repo", "as": "source" }]
            },
            {
                "id": "limited",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "compute": {
                    "source": "commits",
                    "op": { "kind": "limit", "count": 2000 },
                    "schema": {
                        "entity": "PlanLimit",
                        "fields": [
                            { "name": "sha", "value_kind": "string", "source": ["sha"] },
                            { "name": "message", "value_kind": "string", "source": ["message"] }
                        ]
                    }
                },
                "depends_on": ["commits"]
            },
            {
                "id": "n_commits",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "compute": {
                    "source": "limited",
                    "op": { "kind": "aggregate", "aggregates": [{ "name": "n", "function": "count" }] },
                    "schema": { "entity": "PlanAggregate", "fields": [{ "name": "n", "value_kind": "integer" }] }
                },
                "depends_on": ["limited"]
            }
        ],
        "return": { "kind": "node", "node": "n_commits" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert_eq!(
        dry.node_results[1]["simulation"]["kind"],
        "relation_traversal"
    );
    assert_eq!(dry.node_results[2]["kind"], "compute");
    assert_eq!(dry.node_results[3]["kind"], "compute");
}

#[test]
fn dry_run_text_renders_dependency_dag_snapshot() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "product-summary",
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
                "id": "summary",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "compute": {
                    "source": "products",
                    "op": { "kind": "project", "fields": { "sku": ["id"], "name": ["name"] } },
                    "schema": {
                        "entity": "PlanProject",
                        "fields": [
                            { "name": "sku", "value_kind": "unknown", "source": ["id"] },
                            { "name": "name", "value_kind": "unknown", "source": ["name"] }
                        ]
                    }
                }
            },
            {
                "id": "cards",
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "derive_template": {
                    "kind": "map",
                    "source": "summary",
                    "item_binding": "product",
                    "value": {
                        "kind": "object",
                        "fields": {
                            "title": { "kind": "template", "template": "${product.name}", "input_bindings": [{ "from": "product.name", "to": "" }] }
                        }
                    }
                },
                "depends_on": ["summary"],
                "uses_result": [{ "node": "summary", "as": "product" }]
            }
        ],
        "return": { "kind": "parallel", "nodes": ["summary", "cards"] }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    let comp = crate::plasm_comp_wire::trace_comp_wire_from_dry(&dry);
    let comp_json = comp.to_json_value();
    assert!(comp_json
        .get("steps")
        .and_then(|s| s.get("products"))
        .is_some());
    assert_eq!(comp_json["bind"]["deps"]["summary"][0], "products");
    assert_eq!(comp_json["bind"]["deps"]["cards"][0], "summary");
    let text = render_plasm_plan_dry_text(
        &dry,
        Some(PlasmPlanDryRunTextMeta {
            plan_name: None,
            plan_handle: "p7",
            plan_uri: "plasm://session/s0/p/7",
            canonical_plan_uri: "plasm://execute/ph/s/plan/uuid",
            plan_hash: "abc123",
        }),
    );
    insta::assert_snapshot!(
        text,
        @"
        plan ok · 3n 1r → parallel(2) · p7

        01 products     query Query(Product all)
        02 summary      project name, sku ← products
        03 cards        derive map summary as product → {1} ← summary
        "
    );
    assert!(!text.contains("node_results"));
    assert!(!text.contains("\"dry_run\""));
}

#[test]
fn validation_rejects_ambiguous_auto_cross_node_input() {
    let plan = serde_json::json!({
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
                "id": "tags",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "literal", "value": [{ "tag": "a" }] }
            },
            {
                "id": "cards",
                "kind": "derive",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "derive_template": {
                    "kind": "map",
                    "source": "tags",
                    "item_binding": "tag",
                    "inputs": [{ "node": "products", "alias": "products", "cardinality": "auto" }],
                    "value": { "kind": "node_symbol", "node": "products", "alias": "products", "path": ["name"] }
                }
            }
        ],
        "return": { "kind": "node", "node": "cards" }
    });
    let err = crate::plasm_plan::validate_plan_value(&plan).expect_err("ambiguous input");
    assert!(err.contains("not statically singleton"), "{err}");
}

#[test]
fn evaluate_plasm_plan_dry_reports_for_each_stage() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "label-products",
        "nodes": [
            {
                "id": "find",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            },
            {
                "id": "label",
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": "find",
                "item_binding": "product",
                "depends_on": ["find"],
                "uses_result": [{ "node": "find", "as": "product" }],
                "approval": "label_products",
                "effect_template": {
                    "kind": "action",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr_template": "Product(${product.id}).label(label=\"stale\")",
                    "ir_template": {
                        "expr": {
                            "op": "invoke",
                            "capability": "product_label",
                            "target": { "entity_type": "Product", "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } } },
                            "input": { "label": "stale" }
                        },
                        "input_bindings": [{ "from": "product.id", "to": "id" }]
                    },
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack"
                }
            }
        ],
        "return": { "kind": "parallel", "nodes": ["find", "label"] }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert!(!dry.parallel_root_surfaces_only);
    assert_eq!(dry.node_results.len(), 2);
    assert_eq!(dry.node_results[1]["simulation"]["kind"], "template_stage");
    assert_eq!(
        dry.node_results[1]["approval_gate"]["policy_key"],
        "acme.Product.label"
    );
}

#[test]
fn for_each_templates_render_concrete_row_bound_plasm_calls() {
    let plan = parse_plan_value(&serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "label-products",
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
                    "item_binding": "product",
                    "depends_on": ["find"],
                    "uses_result": [{ "node": "find", "as": "product" }],
                    "effect_template": {
                        "kind": "action",
                        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                        "expr_template": "Product(${product.id}).label(label=\"stale\")",
                        "ir_template": {
                            "expr": {
                                "op": "invoke",
                                "capability": "product_label",
                                "target": { "entity_type": "Product", "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } } },
                                "input": { "label": "stale" }
                            },
                            "input_bindings": [{ "from": "product.id", "to": "id" }]
                        },
                        "effect_class": "side_effect",
                        "result_shape": "side_effect_ack"
                    }
                }
            ],
            "return": { "kind": "node", "node": "label" }
        }))
        .expect("parse plan");
    let validated = validate_plan_artifact(&plan).expect("validate plan");
    let for_each = validated
        .nodes()
        .iter()
        .find_map(|node| match node {
            ValidatedPlanNode::ForEach(node) => Some(node),
            _ => None,
        })
        .expect("for_each node");
    let expressions = render_for_each_expressions(
        for_each,
        &[serde_json::json!({ "id": "p1", "name": "Bolt" })],
        None,
    )
    .expect("render expressions");

    assert_eq!(
        expressions,
        vec!["Product(\"p1\").label(label=\"stale\")".to_string()]
    );
}

#[test]
fn dry_run_text_default_page_bounds_bare_list_root() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "unbounded-products",
        "nodes": [
            {
                "id": "products",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            }
        ],
        "return": { "kind": "node", "node": "products" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    let text = render_plasm_plan_dry_text(&dry, None);
    assert!(
        !dry.review.has_unbounded_read_root,
        "default host page bounds bare query roots: {:?}",
        dry.review
    );
    assert!(
        !text.contains("unbounded read"),
        "default page should avoid unbounded warning: {text}"
    );
}

#[test]
fn dry_run_text_review_omits_broad_list_nudge_for_bounded_single_get() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "one-product",
        "nodes": [
            {
                "id": "one",
                "kind": "get",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product(\"p1\")",
                "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                "effect_class": "read",
                "result_shape": "single"
            }
        ],
        "return": { "kind": "node", "node": "one" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    let text = render_plasm_plan_dry_text(&dry, None);
    assert!(text.starts_with("plan ok"), "{text}");
    assert!(
        !text.contains("unbounded read"),
        "bounded get should not get unbounded risk: {text}"
    );
    assert!(
        dry.review
            .unused_seeds
            .iter()
            .any(|s| s.contains("Category")),
        "unused seeds remain session advisory: {:?}",
        dry.review.unused_seeds
    );
    assert!(
        !text.contains("unused seed"),
        "unused seeds belong in session notes, not dry-run warn line: {text}"
    );
}

#[test]
fn dry_run_text_renders_staged_read_map_body() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "dependent-product-read",
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
                "id": "details",
                "kind": "for_each",
                "effect_class": "read",
                "result_shape": "single",
                "source": "products",
                "item_binding": "product",
                "depends_on": ["products"],
                "uses_result": [{ "node": "products", "as": "product" }],
                "effect_template": {
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr_template": "Product(${product.id})",
                    "ir_template": {
                        "expr": {
                            "op": "get",
                            "ref": {
                                "entity_type": "Product",
                                "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } }
                            }
                        },
                        "input_bindings": [{ "from": "product.id", "to": "id" }]
                    },
                    "effect_class": "read",
                    "result_shape": "single"
                }
            }
        ],
        "return": { "kind": "node", "node": "details" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert_eq!(dry.staged_nodes, vec!["details (ForEach)"]);
    assert!(dry.execution_unsupported.is_empty());
    let text = render_plasm_plan_dry_text(&dry, None);
    assert!(
        text.contains("for_each products as product => Product(${product.id})"),
        "{text}"
    );
    assert!(!text.contains("=> {}"), "{text}");
    assert!(
        text.starts_with("plan ok"),
        "root list read is bounded by the default host page (paged-by-default): {text}"
    );
    assert!(
        !text.contains("unbounded read"),
        "paged-by-default reads are not unbounded: {text}"
    );
}

#[test]
fn dry_run_text_renders_empty_literals_explicitly() {
    let plan = parse_plan_value(&serde_json::json!({
        "version": 1,
        "kind": "program",
        "nodes": [{
            "id": "empty",
            "kind": "data",
            "effect_class": "artifact_read",
            "result_shape": "artifact",
            "data": { "kind": "object", "fields": {} }
        }],
        "return": { "kind": "node", "node": "empty" }
    }))
    .expect("parse plan");
    let validated = validate_plan_artifact(&plan).expect("validate");
    let operation = render_node_operation(&validated.nodes()[0]);
    assert_eq!(operation, "data {0 fields}");
    assert!(!operation.contains("{}"), "{operation}");
}
