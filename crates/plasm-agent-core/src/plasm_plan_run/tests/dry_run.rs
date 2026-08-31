use super::super::*;
use super::support::*;
use plasm_core::load_schema;
use plasm_core::CgsContext;
use plasm_core::TeachingExposureSession;
use std::path::PathBuf;
use std::sync::Arc;

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
    let program =
        format!("{e2}.{m_create}(team={e3}({p_key}=EVA), title=\"federation triage dry-run\")");
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
fn federated_linear_issue_create_dry_run_preflight_compiles_p_sym_tokens() {
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
    let p_team = map.ident_sym_cap_param_for("linear", "Issue", "issue_create", "team");
    let p_title = map.ident_sym_cap_param_for("linear", "Issue", "issue_create", "title");
    let p_key = map.ident_sym_entity_field_for("linear", "Team", "key");
    let program = format!(
        "{e2}.{m_create}({p_team}={e3}({p_key}=EVA), {p_title}=\"federation triage p# dry-run\")"
    );
    let plan = crate::plasm_dag::compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "fed-linear-create-p-sym",
        &program,
    )
    .expect("compile federated linear issue create with p# tokens");
    evaluate_plasm_plan_dry(&session, &plan).expect("dry-run federated linear issue create p#");
}

/// A2: `provides` is the authoritative row schema for create/mutation outputs. Projecting a field
/// inside `provides` (now including `description`) compiles; projecting one outside it must fail
/// closed (compile error) rather than silently decode to null.
#[test]
fn issue_create_projection_honors_provides_fail_closed() {
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
    let p_desc = map.ident_sym_entity_field_for("linear", "Issue", "description");
    let p_priority = map.ident_sym_entity_field_for("linear", "Issue", "priority");

    let ok = format!("{e2}.{m_create}(team={e3}({p_key}=EVA), title=\"a\")[{p_desc}]");
    crate::plasm_dag::compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "create-proj-desc",
        &ok,
    )
    .expect("description is in issue_create.provides and must project");

    let bad = format!("{e2}.{m_create}(team={e3}({p_key}=EVA), title=\"a\")[{p_priority}]");
    let err = crate::plasm_dag::compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "create-proj-priority",
        &bad,
    )
    .expect_err("priority is outside issue_create.provides and must fail closed");
    assert!(err.contains("not a row field"), "{err}");
}

fn for_each_write_plan(source_node: serde_json::Value, source_id: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "for-each-write",
        "nodes": [
            source_node,
            {
                "id": "label",
                "kind": "for_each",
                "effect_class": "side_effect",
                "result_shape": "side_effect_ack",
                "source": source_id,
                "item_binding": "product",
                "depends_on": [source_id],
                "uses_result": [{ "node": source_id, "as": "product" }],
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
        "return": { "kind": "node", "node": "label" }
    })
}

/// D1: singleton `get` source `=>` write is not fanout risk.
#[test]
fn for_each_write_over_singleton_source_is_not_fanout() {
    let get = serde_json::json!({
        "id": "product",
        "kind": "get",
        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
        "expr": "Product(\"p1\")",
        "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
        "effect_class": "read",
        "result_shape": "single"
    });
    let plan = for_each_write_plan(get, "product");
    let validated = crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
    let b = crate::plan_prepare::analyze_read_boundedness(validated.artifact());
    assert!(
        !b.has_foreach_fanout_risk,
        "singleton-source for_each write must not be fanout"
    );
}

/// D1: plural query source keeps fanout semantics.
#[test]
fn for_each_write_over_plural_source_is_fanout() {
    let query = serde_json::json!({
        "id": "product",
        "kind": "query",
        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
        "expr": "Product",
        "ir": { "expr": { "op": "query", "entity": "Product" } },
        "effect_class": "read",
        "result_shape": "list"
    });
    let plan = for_each_write_plan(query, "product");
    let validated = crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate");
    let b = crate::plan_prepare::analyze_read_boundedness(validated.artifact());
    assert!(
        b.has_foreach_fanout_risk,
        "plural-source for_each write must remain fanout risk"
    );
}

/// B4 — federated write smoke: pokeapi single GET `=>` linear issue_create. Locks the coherent
/// shape: exactly one pokeapi GET, one write effect, no relation fanout, a single (non-parallel)
/// return root, and (D1) no fanout risk on the singleton source.
#[test]
fn federated_pokeapi_linear_write_plan_is_coherent() {
    let Some(session) = federated_pokeapi_linear_write_session() else {
        return;
    };
    let map = session
        .teaching_exposure
        .as_ref()
        .expect("exposure")
        .symbol_map_arc();
    let e_mon = map.entity_sym_for("pokeapi", "Pokemon");
    let e_issue = map.entity_sym_for("linear", "Issue");
    let e_team = map.entity_sym_for("linear", "Team");
    let m_create = map.method_sym_for("linear", "Issue", "create");
    let p_team = map.ident_sym_cap_param_for("linear", "Issue", "issue_create", "team");
    let p_title = map.ident_sym_cap_param_for("linear", "Issue", "issue_create", "title");
    let p_description =
        map.ident_sym_cap_param_for("linear", "Issue", "issue_create", "description");

    let program = format!(
        "pika = {e_mon}(\"pikachu\")\nticket = pika => {e_issue}.{m_create}({p_team}={e_team}(\"EVA\"), {p_title}=\"Pokedex #025 Pikachu\", {p_description}=<<MD\n# Pikachu\n\nElectric-type Pokémon #025.\nMD\n)\nticket"
    );
    let plan = crate::plasm_dag::compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(),
        None,
        &session,
        "fed-poke-linear-write",
        &program,
    )
    .expect("compile federated pokeapi -> linear write");

    let nodes = plan["nodes"].as_array().expect("nodes array");
    let poke_gets = nodes
        .iter()
        .filter(|n| n["kind"] == "get" && n["qualified_entity"]["entry_id"] == "pokeapi")
        .count();
    assert_eq!(poke_gets, 1, "exactly one pokeapi GET: {plan:#}");
    let writes = nodes.iter().filter(|n| n["kind"] == "for_each").count();
    assert_eq!(writes, 1, "exactly one write effect: {plan:#}");
    let relations = nodes.iter().filter(|n| n["kind"] == "relation").count();
    assert_eq!(relations, 0, "no relation fanout: {plan:#}");
    assert_eq!(
        plan["return"]["kind"], "node",
        "single (non-parallel) return root: {plan:#}"
    );

    evaluate_plasm_plan_dry(&session, &plan).expect("dry-run federated write");

    let validated =
        crate::plasm_plan::parse_and_validate_plan_json(&plan).expect("validate federated write");
    let bounded = crate::plan_prepare::analyze_read_boundedness(validated.artifact());
    assert!(
        !bounded.has_foreach_fanout_risk,
        "singleton pokeapi GET `=>` write must not be fanout risk"
    );
}

#[test]
fn federated_ambiguous_entity_parse_includes_session_stamps() {
    let Some(session) = federated_github_linear_issue_session() else {
        return;
    };
    let err = parse_parsed_expr_for_session(&session, "Issue.create(title=\"x\")")
        .expect_err("ambiguous");
    let msg = format_session_symbolic_parse_error(
        &session,
        None,
        &PromptPipelineConfig::default(),
        "Issue.create(title=\"x\")",
        &err,
    );
    assert!(
        msg.contains("e1")
            && msg.contains("e2")
            && msg.contains("github")
            && msg.contains("linear"),
        "{msg}"
    );
}

#[test]
fn relation_arrow_trap_does_not_mask_broken_write_parse_error() {
    let Some(session) = federated_github_linear_issue_session() else {
        return;
    };
    let line = "hits => e1.m1(p1=";
    let err = parse_parsed_expr_for_session(&session, line).expect_err("broken write");
    let msg = format_session_symbolic_parse_error(
        &session,
        None,
        &PromptPipelineConfig::default(),
        line,
        &err,
    );
    assert!(
        !msg.contains("Relation reads use"),
        "write-effect parse errors must not be replaced by relation trap: {msg}"
    );
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
    super::support::matrix_views_session()
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
            assert!(
                err.contains("query/capability input")
                    || err.contains("not a row field")
                    || err.contains("not a row symbol"),
                "{err}"
            );
        }
        Ok(bundle) => {
            let dry_err = evaluate_plasm_comp_dry(&s, &bundle)
                .expect_err("dry must reject search input projection");
            assert!(
                dry_err.contains("query/capability input")
                    || dry_err.contains("not a row field")
                    || dry_err.contains("not a row symbol")
                    || dry_err.contains("postfix projection"),
                "{dry_err}"
            );
        }
    }
}

/// PEC (Plan Executability Closure): a staged surface whose input is a literal `data` binding
/// must dry-run cleanly. The dry preflight materializes the pure `data` node identically to live
/// execution (same `plan_value_to_rows`), so `uses_result` resolves. This is the minimal shape of
/// the heredoc-body `=>` mutator failure ("input node `body` … has not been materialized"): a
/// surface consuming a prior pure binding.
#[test]
fn evaluate_plasm_plan_dry_materializes_data_binding_for_staged_surface() {
    let s = duplicate_product_create_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "staged-create-from-data-body",
        "nodes": [
            {
                "id": "body",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "single",
                "data": { "kind": "literal", "value": { "name": "Bolt" } }
            },
            {
                "id": "make",
                "kind": "create",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr_template": "Product.create(name=${body.name})",
                "ir_template": {
                    "expr": {
                        "op": "create",
                        "capability": "product_create",
                        "entity": "Product",
                        "input": {
                            "name": "${body.name}"
                        }
                    },
                    "input_bindings": []
                },
                "effect_class": "write",
                "result_shape": "single",
                "depends_on": ["body"],
                "uses_result": [{ "node": "body", "as": "body" }]
            }
        ],
        "return": { "kind": "node", "node": "make" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan)
        .expect("dry-run must materialize the `data` body feeding the staged create");
    assert_eq!(dry.node_results.len(), 2);
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
    assert_eq!(dry.node_results[0]["ir"]["expr"]["entity"], "Product");
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
        plan ok · 3n 1r → returns: summary, cards · p7

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
    let mut s = test_session();
    // Active policy: require review for product_label so the approval_gate is emitted.
    s.flow_policy = crate::FlowPolicySnapshot::Active {
        revision: crate::PolicyRevision(1),
        policy: crate::FlowPolicy {
            capability_gates: vec![crate::plan_flow_policy::CapabilityGateRule {
                pattern: crate::plan_flow_policy::CapabilityGatePattern {
                    entry_id: Some("acme".into()),
                    entity: Some("Product".into()),
                    capability: "product_label".into(),
                },
                enforcement: crate::plan_flow_policy::OperatorDisposition::Approve,
            }],
            ..crate::FlowPolicy::default()
        },
    };
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
        "acme.Product.product_label"
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
fn dry_run_text_default_page_keeps_advisory_for_bare_list_root() {
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
        !dry.review.execution_is_expensive(),
        "default host page keeps first-page sync: {:?}",
        dry.review
    );
    assert!(
        dry.review.has_unbounded_read_root,
        "unnarrowed root is advisory: {:?}",
        dry.review
    );
    assert!(
        dry.review.has_unprojected_multi_row_read,
        "unprojected list is advisory: {:?}",
        dry.review
    );
    assert!(
        dry.review.needs_review(true),
        "advisory must gate MCP fuse / return plan: {:?}",
        dry.review
    );
    assert!(
        text.contains("unbounded read") || text.contains("project list"),
        "dry text should surface advisory: {text}"
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

#[test]
fn dry_run_flags_unused_binding_not_consumed_or_returned() {
    let s = test_session();
    let plan = serde_json::json!({
        "version": 1,
        "kind": "program",
        "name": "dead-repo-read",
        "nodes": [
            {
                "id": "mon",
                "kind": "get",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product(\"p1\")",
                "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                "effect_class": "read",
                "result_shape": "single"
            },
            {
                "id": "repo",
                "kind": "get",
                "qualified_entity": { "entry_id": "acme", "entity": "Category" },
                "expr": "Category(\"c1\")",
                "ir": { "expr": { "op": "get", "ref": { "entity_type": "Category", "key": "c1" } } },
                "effect_class": "read",
                "result_shape": "single"
            }
        ],
        "return": { "kind": "node", "node": "mon" }
    });
    let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
    assert!(
        dry.review.unused_bindings.iter().any(|b| b == "repo"),
        "repo executes but is not consumed or returned: {:?}",
        dry.review.unused_bindings
    );
    assert!(
        !dry.review.unused_bindings.iter().any(|b| b == "mon"),
        "mon is the return root: {:?}",
        dry.review.unused_bindings
    );
}

#[test]
fn consecutive_writes_program_order_in_comp_metadata_and_graph_summary() {
    use crate::compile_plasm_program;
    use plasm_core::PromptPipelineConfig;

    let es = language_matrix_session();
    let program = r#"newbranch = LangItem.create(title="branch-a", score=1, owner="witness")
newfile = LangItem.create(title="file-b", score=2, owner="witness")
newbranch, newfile"#;
    let bundle = compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &es,
        "consecutive-writes",
        program,
    )
    .expect("compile consecutive writes");
    let meta = bundle
        .artifact()
        .comp
        .metadata
        .get("program_order_write_deps")
        .expect("program_order_write_deps metadata");
    assert_eq!(meta, &serde_json::json!([["newbranch", "newfile"]]));
    let dry = evaluate_plasm_comp_dry(&es, &bundle).expect("dry");
    assert_eq!(
        dry.graph_summary.get("execution_layers"),
        Some(&serde_json::json!([["newbranch"], ["newfile"]]))
    );
    assert_eq!(
        dry.graph_summary.get("parallelizable_roots"),
        Some(&serde_json::json!(["newbranch"]))
    );
}
