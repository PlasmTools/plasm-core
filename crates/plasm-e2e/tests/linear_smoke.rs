//! Linear schema loads and key task-oriented surface forms parse (no network).

use std::path::PathBuf;
use std::sync::Arc;

use plasm_core::entity_slices_for_render;
use plasm_core::expr_parser::parse_with_cgs_layers;
use plasm_core::loader::load_schema_dir;
use plasm_core::normalize_expr_query_capabilities;
use plasm_core::resolve_query_capability;
use plasm_core::Expr;
use plasm_core::FocusSpec;
use plasm_core::Predicate;
use plasm_core::QueryExpr;
use plasm_core::CgsLayer;
use plasm_core::SymbolMap;

fn linear_cgs() -> plasm_core::CGS {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../apis/linear");
    load_schema_dir(&dir).expect("load apis/linear")
}

#[test]
fn linear_mappings_parse() {
    let cgs = linear_cgs();
    cgs.validate().expect("CGS validate");
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability templates");
}

#[test]
fn linear_issue_search_and_views_parse() {
    let cgs = linear_cgs();
    let (full, _) = entity_slices_for_render(&cgs, FocusSpec::All);
    let sym_map = Arc::new(SymbolMap::build(&cgs, &full));
    let stack = [CgsLayer::new("linear", &cgs)];

    let search = parse_with_cgs_layers(
        r#"Issue.search(q="bug", team_key="ENG")"#,
        &stack,
        sym_map.clone(),
    )
    .expect("Issue.search");
    let Expr::Query(q) = &search.expr else {
        panic!("expected query");
    };
    assert_eq!(q.capability_name.as_deref(), Some("issue_search"));

    parse_with_cgs_layers("IssueContext(ENG-42)", &stack, sym_map.clone())
        .expect("IssueContext get");
    parse_with_cgs_layers("MyWorkSnapshot", &stack, sym_map.clone()).expect("MyWorkSnapshot view");
    parse_with_cgs_layers("Team(ENG)", &stack, sym_map).expect("Team get");
}

#[test]
fn linear_issue_brace_filters_resolve_search() {
    let cgs = linear_cgs();
    let q = QueryExpr::filtered(
        "Issue",
        Predicate::and(vec![
            Predicate::eq("team_key", "ENG"),
            Predicate::eq("state_name", "Todo"),
        ]),
    );
    let cap = resolve_query_capability(&q, &cgs).expect("resolve");
    assert_eq!(cap.name.as_str(), "issue_search");

    let mut expr = Expr::Query(q);
    normalize_expr_query_capabilities(&mut expr, &cgs).expect("normalize");
    let Expr::Query(q2) = expr else {
        panic!("expected query");
    };
    assert_eq!(q2.capability_name.as_deref(), Some("issue_search"));
}
