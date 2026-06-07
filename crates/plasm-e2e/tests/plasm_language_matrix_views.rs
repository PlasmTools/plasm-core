//! Hermit-backed conformance for **view transport** (`transport: view`), view-backed query/get,
//! `relation_outputs`, and `get_scoped_bindings` on the extended `plasm_language_matrix_views` CGS.
//! Shares OpenAPI with [`plasm_language_matrix`](./plasm_language_matrix.rs); keeps the main matrix
//! fixture stable.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix_views.rs"]
mod language_matrix_views;

use std::collections::BTreeSet;

use plasm_agent::plasm_dag::compile_plasm_dag_to_plan;
use plasm_agent::plasm_plan::{parse_plan_value, validate_plan_artifact};
use plasm_agent::plasm_plan_run::{
    evaluate_validated_plasm_plan_dry, run_validated_plasm_plan, DryPlasmPlanEvaluation,
    PlasmPlanRunResult,
};
use plasm_core::{
    ChainStep, CompOp, EntityKey, Expr, GetExpr, Predicate, PromptPipelineConfig, QueryExpr,
    TypedComparisonValue, Value,
};
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

/// Each tag must appear on at least one [`VIEW_MATRIX_ROWS`] entry (`features` column).
const REQUIRED_VIEW_FEATURE_TAGS: &[&str] = &[
    "view_query",
    "view_get",
    "view_relation_outputs",
    "view_computed",
    "get_scoped_bindings",
    "view_node_field",
    "view_node_computed_bind",
];

struct ViewMatrixRow {
    id: &'static str,
    program: &'static str,
    features: &'static [&'static str],
    min_node_results: usize,
    expect_markdown_substrings: &'static [&'static str],
}

fn surface_exprs(dry: &DryPlasmPlanEvaluation) -> Vec<Expr> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            let ev = nr.get("ir")?.get("expr")?;
            serde_json::from_value(ev.clone()).ok()
        })
        .collect()
}

fn relation_exprs(dry: &DryPlasmPlanEvaluation) -> Vec<Expr> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            let ev = nr.get("execution_contract")?.get("ir")?;
            serde_json::from_value(ev.clone()).ok()
        })
        .collect()
}

fn tcv_string(v: &TypedComparisonValue) -> Option<String> {
    match v.to_value() {
        Value::String(s) => Some(s),
        Value::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

fn first_query(exprs: &[Expr]) -> Result<&QueryExpr, String> {
    for e in exprs {
        if let Expr::Query(q) = e {
            return Ok(q);
        }
    }
    Err("expected a Query IR node".into())
}

fn get_simple_id(g: &GetExpr) -> Option<&str> {
    match &g.reference.key {
        EntityKey::Simple(id) => Some(id.as_str()),
        EntityKey::Compound(_) => None,
    }
}

fn expr_contains_get_digest(e: &Expr, want_id: Option<&str>) -> bool {
    match e {
        Expr::Get(g) if g.reference.entity_type == "LangDigest" => {
            want_id.is_none_or(|id| get_simple_id(g) == Some(id))
        }
        Expr::Chain(c) => expr_contains_get_digest(&c.source, want_id),
        _ => false,
    }
}

fn expr_chain_selects_item_snapshot(e: &Expr) -> bool {
    match e {
        Expr::Chain(c) if c.selector == "item_snapshot" => true,
        Expr::Chain(c) => {
            expr_chain_selects_item_snapshot(&c.source)
                || matches!(
                    &c.step,
                    ChainStep::Explicit { expr } if expr_chain_selects_item_snapshot(expr)
                )
        }
        _ => false,
    }
}

fn expr_chain_selects_self_via_bindings(e: &Expr) -> bool {
    match e {
        Expr::Chain(c) if c.selector == "self_via_bindings" => true,
        Expr::Chain(c) => {
            expr_chain_selects_self_via_bindings(&c.source)
                || matches!(
                    &c.step,
                    ChainStep::Explicit { expr } if expr_chain_selects_self_via_bindings(expr)
                )
        }
        _ => false,
    }
}

fn plan_has_relation_named(plan: &serde_json::Value, relation: &str) -> bool {
    let Some(nodes) = plan.get("nodes").and_then(|n| n.as_array()) else {
        return false;
    };
    nodes.iter().any(|n| {
        n.get("kind").and_then(|k| k.as_str()) == Some("relation")
            && n.pointer("/relation/relation").and_then(|x| x.as_str()) == Some(relation)
    })
}

fn assert_view_planning_ir(
    row: &ViewMatrixRow,
    dry: &DryPlasmPlanEvaluation,
    plan: &serde_json::Value,
) -> Result<(), String> {
    let surfaces = surface_exprs(dry);
    let rel = relation_exprs(dry);

    match row.id {
        "views_digest_query_filter" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangDigest" {
                return Err(format!("expected LangDigest query, got {:?}", q.entity));
            }
            if let Some(cap) = q.capability_name.as_ref() {
                if cap.as_str() != "lang_digest_query" {
                    return Err(format!(
                        "expected lang_digest_query capability when pinned, got {cap}"
                    ));
                }
            }
            let Some(pred) = q.predicate.as_ref() else {
                return Err("expected item_id filter predicate".into());
            };
            let Predicate::Comparison {
                field,
                op: CompOp::Eq,
                value,
            } = pred
            else {
                return Err(format!("expected item_id eq predicate, got {pred:?}"));
            };
            if field != "item_id" {
                return Err(format!("expected field item_id, got {field}"));
            }
            // Hermit fixture ids are plain strings; brace equality may lower to string or entity_ref.
            if tcv_string(value).as_deref() != Some("i1") {
                return Err(format!("expected i1 filter, got {:?}", tcv_string(value)));
            }
        }
        "views_digest_get" => {
            if !surfaces
                .iter()
                .any(|e| expr_contains_get_digest(e, Some("i1")))
            {
                return Err(format!(
                    "expected LangDigest(i1) Get IR, got {:?}",
                    surfaces.first()
                ));
            }
        }
        "views_digest_relation_outputs" => {
            if !surfaces
                .iter()
                .any(|e| expr_contains_get_digest(e, Some("i1")))
            {
                return Err(format!(
                    "expected LangDigest(i1) anchor in surface IR, got {:?}",
                    surfaces.first()
                ));
            }
            let pool: Vec<&Expr> = surfaces.iter().chain(rel.iter()).collect();
            if !pool.iter().copied().any(expr_chain_selects_item_snapshot) {
                return Err(format!(
                    "expected `.item_snapshot` chain in IR, surfaces={surfaces:?} rel={rel:?}"
                ));
            }
            if !plan_has_relation_named(plan, "item_snapshot") {
                return Err("compiled plan missing relation node item_snapshot".into());
            }
        }
        "views_digest_computed_slug" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangDigest" {
                return Err(format!("expected LangDigest query, got {:?}", q.entity));
            }
            if let Some(cap) = q.capability_name.as_ref() {
                if cap.as_str() != "lang_digest_query" {
                    return Err(format!(
                        "expected lang_digest_query capability when pinned, got {cap}"
                    ));
                }
            }
        }
        "views_langitem_get_scoped_bindings" => {
            if !surfaces
                .iter()
                .any(|e| expr_contains_get_langitem(e, Some("i1")))
            {
                return Err(format!(
                    "expected LangItem(i1) anchor, got {:?}",
                    surfaces.first()
                ));
            }
            let pool: Vec<&Expr> = surfaces.iter().chain(rel.iter()).collect();
            if !pool
                .iter()
                .copied()
                .any(expr_chain_selects_self_via_bindings)
            {
                return Err(format!(
                    "expected `.self_via_bindings` chain in IR, surfaces={surfaces:?} rel={rel:?}"
                ));
            }
        }
        "views_triage_context_node_field" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangTriageContext" {
                return Err(format!(
                    "expected LangTriageContext query, got {:?}",
                    q.entity
                ));
            }
        }
        "views_owner_filter_computed_bind" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangOwnerFilterDemo" {
                return Err(format!(
                    "expected LangOwnerFilterDemo query, got {:?}",
                    q.entity
                ));
            }
        }
        other => return Err(format!("unknown view matrix row id {other}")),
    }
    Ok(())
}

fn expr_contains_get_langitem(e: &Expr, want_id: Option<&str>) -> bool {
    match e {
        Expr::Get(g) if g.reference.entity_type == "LangItem" => {
            want_id.is_none_or(|id| get_simple_id(g) == Some(id))
        }
        Expr::Chain(c) => expr_contains_get_langitem(&c.source, want_id),
        _ => false,
    }
}

fn assert_view_row(row: &ViewMatrixRow, out: &PlasmPlanRunResult) -> Result<(), String> {
    if out.node_results.len() < row.min_node_results {
        return Err(format!(
            "row {}: expected at least {} node_results, got {}",
            row.id,
            row.min_node_results,
            out.node_results.len()
        ));
    }
    let md = out.run_markdown.as_deref().unwrap_or("");
    for sub in row.expect_markdown_substrings {
        if !md.contains(sub) {
            return Err(format!(
                "row {}: run_markdown missing substring {sub:?} (len {}):\n{md}",
                row.id,
                md.len()
            ));
        }
    }
    Ok(())
}

const VIEW_MATRIX_ROWS: &[ViewMatrixRow] = &[
    ViewMatrixRow {
        id: "views_digest_query_filter",
        program: r#"LangDigest{item_id="i1"}[item_id,echo_title]"#,
        features: &["view_query"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "echo_title", "item_id", "i1"],
    },
    ViewMatrixRow {
        id: "views_digest_get",
        program: r#"LangDigest("i1")[item_id,echo_title]"#,
        features: &["view_get"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "echo_title", "item_id", "i1"],
    },
    ViewMatrixRow {
        id: "views_digest_computed_slug",
        program: r#"LangDigest{item_id="i1"}[item_id,echo_title,echo_slug]"#,
        features: &["view_computed"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "echo_slug", "i1", "echo_title"],
    },
    ViewMatrixRow {
        id: "views_digest_relation_outputs",
        program: r#"d = LangDigest("i1")
snap = d.item_snapshot[id,title]
snap"#,
        features: &["view_relation_outputs"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "i1"],
    },
    ViewMatrixRow {
        id: "views_langitem_get_scoped_bindings",
        program: r#"LangItem("i1").self_via_bindings[id,title]"#,
        features: &["get_scoped_bindings"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "i1"],
    },
    ViewMatrixRow {
        id: "views_triage_context_node_field",
        program: r#"LangTriageContext{item_id="i1"}[item_id,echo_title,tag_count]"#,
        features: &["view_node_field"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "tag_count", "i1"],
    },
    ViewMatrixRow {
        id: "views_owner_filter_computed_bind",
        program: r#"LangOwnerFilterDemo{item_id="i1"}[item_id,owner,matching_item_count]"#,
        features: &["view_node_computed_bind"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "matching_item_count", "i1"],
    },
];

#[tokio::test]
async fn plasm_language_matrix_views_cgs_templates_validate() {
    let cgs = language_matrix_views::load_language_matrix_views_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability CML templates");
}

#[tokio::test]
async fn plasm_language_matrix_views_live_runs() {
    let base = hermit_lang_matrix::language_matrix_hermit_base_url().await;
    let cgs = language_matrix_views::load_language_matrix_views_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");

    let es = language_matrix_views::views_execute_session(cgs.clone());
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base.clone()),
        ..Default::default()
    })
    .expect("ExecutionEngine");
    let st = language_matrix_views::views_matrix_host_state(engine, cgs);

    let mut tags_seen: BTreeSet<String> = BTreeSet::new();

    for row in VIEW_MATRIX_ROWS {
        let plan_json = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(),
            None,
            &es,
            row.id,
            row.program,
        )
        .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));

        let plan = parse_plan_value(&plan_json)
            .unwrap_or_else(|e| panic!("row {} parse_plan_value: {e}", row.id));
        let validated = validate_plan_artifact(&plan)
            .unwrap_or_else(|e| panic!("row {} validate_plan_artifact: {e}", row.id));

        let dry = evaluate_validated_plasm_plan_dry(&es, &validated)
            .unwrap_or_else(|e| panic!("row {} evaluate_validated_plasm_plan_dry: {e}", row.id));
        assert_view_planning_ir(row, &dry, &plan_json)
            .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));

        let live = run_validated_plasm_plan(
            &es,
            &st,
            es.prompt_hash.as_str(),
            "matrix_views_sess",
            &validated,
            true,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("row {} run_validated_plasm_plan: {e}", row.id));

        assert_view_row(row, &live).unwrap_or_else(|e| panic!("row {} assertion: {e}", row.id));
        for t in row.features {
            tags_seen.insert((*t).to_string());
        }
    }

    let required: BTreeSet<String> = REQUIRED_VIEW_FEATURE_TAGS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let missing: Vec<_> = required.difference(&tags_seen).cloned().collect();
    assert!(
        missing.is_empty(),
        "missing required view feature tag coverage: {missing:?}"
    );
}
