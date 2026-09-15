//! Unit tests for program_surface.

use super::physical_lines::plp2_unterminated_heredoc_message;
use super::*;

#[test]
fn split_top_level_keeps_commas_inside_tagged_heredoc() {
    let parts = split_top_level("fn(<<T\na,b,c\nT\n), bar", ',').expect("split");
    assert_eq!(parts.len(), 2);
    assert!(parts[0].contains("a,b,c"));
    assert_eq!(parts[1].trim(), "bar");
}

#[test]
fn split_assignment_respects_heredoc_bodies_with_equals() {
    let stmt = "body = <<PLASM_EOF\nkey = value\nPLASM_EOF";
    let (label, rhs) = split_assignment_at_top_level(stmt).expect("assignment");
    assert_eq!(label, "body");
    assert!(rhs.starts_with("<<PLASM_EOF"));
    assert!(rhs.contains("key = value"));
}

#[test]
fn collect_program_binding_heredoc_sugar_without_equals() {
    let src = "body <<PLASM_LABEL_ISSUE_V1\n## Problem\nline two\nPLASM_LABEL_ISSUE_V1\ncreated = x()\nbody, created";
    let stmts = collect_program_statement_lines(src).expect("parse");
    assert_eq!(stmts.len(), 3);
    assert!(stmts[0].starts_with("body = <<PLASM_LABEL_ISSUE_V1"));
    assert!(stmts[0].contains("## Problem"));
    assert_eq!(stmts[1], "created = x()");
    assert_eq!(stmts[2], "body, created");
}

#[test]
fn collect_program_statement_lines_errors_on_squashed_heredoc_opener() {
    let err = collect_program_statement_lines("body = <<B # junk").expect_err("err");
    assert!(
        err.contains("PLP-2:") || err.contains("PLP-3:"),
        "unexpected err: {err}"
    );
    assert!(
        err.contains("tagged heredoc") || err.contains("<<"),
        "unexpected err: {err}"
    );
}

#[test]
fn collect_program_statement_lines_glued_heredoc_close() {
    let stmts = collect_program_statement_lines("x = m(<<H\none\nH)").expect("parse");
    assert_eq!(stmts.len(), 1);
    assert!(stmts[0].contains("<<H"));
    assert!(stmts[0].contains("one"));
}

#[test]
fn collect_program_statement_lines_waits_for_delimiters_after_heredoc_close() {
    let src = "x = m(v111{content=<<H\none\nH\n})\nx";
    let stmts = collect_program_statement_lines(src).expect("parse");
    assert_eq!(stmts, vec!["x = m(v111{content=<<H\none\nH\n})", "x"]);
}

#[test]
fn collect_mid_call_heredoc_same_line_trailing_args() {
    let src = r#"created = e1.m1(p86="title", p73=<<PLASM_BODY_7f3a
## Problem
Testing.
PLASM_BODY_7f3a, p28=["documentation"])
created"#;
    let stmts = collect_program_statement_lines(src).expect("parse");
    assert_eq!(stmts.len(), 2);
    assert!(stmts[0].contains("<<PLASM_BODY_7f3a"));
    assert!(stmts[0].contains("## Problem"));
    assert!(stmts[0].contains("p28=[\"documentation\"]"));
    assert_eq!(stmts[1], "created");
}

#[test]
fn collect_mid_call_heredoc_next_line_trailing_args() {
    let src = r#"created = LangItem.create(title=<<PLASM_INLINE_ARG
line one
PLASM_INLINE_ARG
, score=0, owner="inline-heredoc")
created"#;
    let stmts = collect_program_statement_lines(src).expect("parse");
    assert_eq!(stmts.len(), 2);
    assert!(stmts[0].contains("<<PLASM_INLINE_ARG"));
    assert!(stmts[0].contains(", score=0"));
    assert_eq!(stmts[1], "created");
}

#[test]
fn plp2_finish_hints_unrecognized_same_line_close() {
    let err = collect_program_statement_lines("x = m(body=<<BODY\nline\nBODYfoo, other=1)")
        .expect_err("staging should fail when close suffix is not delimiter-only");
    assert!(err.contains("PLP-2:"), "{err}");
    assert!(err.contains("close line not recognized"), "{err}");
}

#[test]
fn plp2_message_tag_collision_hint() {
    let msg = plp2_unterminated_heredoc_message("TAG", "opener <<TAG\nTAG\nbody\nnot_closed");
    assert!(msg.contains("PLP-2:"), "{msg}");
    assert!(
        msg.contains("body contains a line equal to close tag"),
        "{msg}"
    );
}

#[test]
fn collect_program_statement_lines_inline_heredoc_in_call() {
    let src = "created = e1.m1(p86=\"title\",\n  p73=<<PLASM_INLINE\n## Problem\nline\nPLASM_INLINE\n)\ncreated";
    let stmts = collect_program_statement_lines(src).expect("parse");
    assert_eq!(stmts.len(), 2);
    assert!(stmts[0].contains("e1.m1("));
    assert!(stmts[0].contains("<<PLASM_INLINE"));
    assert!(stmts[0].contains("## Problem"));
    assert_eq!(stmts[1], "created");
}

#[test]
fn split_token_top_level_respects_nesting() {
    let got1 = split_token_top_level("src => Effect(x)", "=>").expect("ok");
    assert_eq!(
        got1.map(|(a, b)| (a.trim(), b.trim())),
        Some(("src", "Effect(x)"))
    );
    let got2 = split_token_top_level("(a=>b) => c", "=>").expect("ok");
    assert_eq!(
        got2.map(|(a, b)| (a.trim(), b.trim())),
        Some(("(a=>b)", "c"))
    );
}

#[test]
fn split_assignment_skips_effect_arrow() {
    assert!(split_assignment_at_top_level("pika => e2.r3").is_none());
    assert!(split_assignment_for_binding("sync = items => e1.m1(p1=1)").is_some());
}

#[test]
fn rejects_domain_symbol_labels_for_assignment_split() {
    assert!(split_assignment_for_binding("e1 = foo").is_none());
    assert!(split_assignment_for_binding("repo = x").is_some());
    assert!(
        split_assignment_for_binding(r#"LangItem | where owner="alice" | take 1"#).is_none(),
        "where-equality is not a program binding"
    );
    assert_eq!(
        classify_top_level_assignment(r#"LangItem | where owner="alice" | take 1"#),
        None
    );
    assert_eq!(
        classify_top_level_assignment("e1 = foo()"),
        Some(TopLevelAssignment::InvalidLabel { label: "e1" })
    );
    assert!(matches!(
        classify_top_level_assignment("items = LangItem"),
        Some(TopLevelAssignment::Binding { label: "items", .. })
    ));
}

#[test]
fn split_flattened_program_line_preserves_for_each_effect_binding() {
    let line = "sync = items => LangItem(\"i1\").update(score=3, title=_.title, owner=_.owner)";
    let split = split_flattened_program_line(line);
    assert_eq!(split.statements.len(), 1);
    assert_eq!(split.statements[0], line);
    assert!(split.coerced_default_return.is_none());
}

#[test]
fn split_flattened_program_line_bindings_and_primary_return() {
    let split = split_flattened_program_line("issues = e1{p1=open} labels = issues.labels labels");
    assert_eq!(split.statements.len(), 3);
    assert!(split.statements[0].starts_with("issues = "));
    assert!(split.statements[1].starts_with("labels = "));
    assert_eq!(split.statements[2], "issues");
    assert_eq!(split.coerced_default_return.as_deref(), Some("issues"));
}

#[test]
fn expand_flattened_program_surfaces_coerced_return() {
    let expanded = expand_flattened_program_statements(&[
        "repo = e1 issues = e2 labels = issues.labels labels".to_string(),
    ]);
    assert_eq!(expanded.coerced_default_return.as_deref(), Some("repo"));
    assert!(expanded.statements.iter().any(|s| s.starts_with("repo = ")));
}

#[test]
fn expand_binding_only_newline_separated_coerces_last_binding_return() {
    let expanded = expand_flattened_program_statements(&[
        "hits = e4(p1=\"sha\")".to_string(),
        "labels = hits.p5".to_string(),
    ]);
    assert_eq!(expanded.coerced_default_return.as_deref(), Some("labels"));
    assert_eq!(
        expanded.statements.last().map(String::as_str),
        Some("labels")
    );
}

#[test]
fn expand_single_binding_line_coerces_default_return() {
    let expanded = expand_flattened_program_statements(&["hits = e4(p1=\"sha\")".to_string()]);
    assert_eq!(expanded.coerced_default_return.as_deref(), Some("hits"));
    assert_eq!(expanded.statements.last().map(String::as_str), Some("hits"));
}

#[test]
fn expand_multiline_explicit_non_first_root_preserved() {
    let expanded = expand_flattened_program_statements(&[
        "issue = e2(p4=\"PLA-1\")".to_string(),
        "comments = issue.r2".to_string(),
        "limited = comments.limit(5)".to_string(),
        "limited[p2,p14]".to_string(),
    ]);
    assert!(expanded.coerced_default_return.is_none());
    assert_eq!(
        expanded.statements.last().map(String::as_str),
        Some("limited[p2,p14]")
    );
}

#[test]
fn expand_multiline_explicit_side_label_root_preserved() {
    let expanded = expand_flattened_program_statements(&[
        "repo = e1".to_string(),
        "labels = e2".to_string(),
        "labels".to_string(),
    ]);
    assert!(expanded.coerced_default_return.is_none());
    assert_eq!(
        expanded.statements.last().map(String::as_str),
        Some("labels")
    );
}

#[test]
fn split_flattened_line_keeps_projection_on_in_scope_binding() {
    let split = split_flattened_program_line(
        "issue = e2(p4=\"PLA-1\") comments = issue.r2 comments[p2,p14]",
    );
    assert_eq!(
        split.statements.last().map(String::as_str),
        Some("comments[p2,p14]")
    );
    assert!(split.coerced_default_return.is_none());
}

#[test]
fn split_flattened_line_keeps_postfix_projection_on_in_scope_binding() {
    let split = split_flattened_program_line(
        "issue = e2(p4=\"PLA-1\") comments = issue.r2 comments.limit(5)[p2,p14]",
    );
    assert_eq!(
        split.statements.last().map(String::as_str),
        Some("comments.limit(5)[p2,p14]")
    );
    assert!(split.coerced_default_return.is_none());
}

#[test]
fn split_flattened_line_keeps_projection_on_first_binding() {
    let split =
        split_flattened_program_line("issue = e2(p4=\"PLA-1\") comments = issue.r2 issue[p4,p19]");
    assert_eq!(
        split.statements.last().map(String::as_str),
        Some("issue[p4,p19]")
    );
    assert!(split.coerced_default_return.is_none());
}

#[test]
fn split_flattened_line_fresh_trailing_query_still_coerces() {
    let split =
        split_flattened_program_line("item = LangItem(\"i1\") LangItem.sort(score, desc).limit(2)");
    assert_eq!(split.statements.last().map(String::as_str), Some("item"));
    assert_eq!(split.coerced_default_return.as_deref(), Some("item"));
}

#[test]
fn validate_rejects_intermediate_postfix_without_binding() {
    let err = validate_program_statement_order(&[
        "comments = issue.r2".to_string(),
        "comments.filter{p14=\"a\"}".to_string(),
        "comments[p2,p14]".to_string(),
    ])
    .expect_err("must bind intermediate postfix");
    assert!(
        err.contains("binding") || err.contains("Intermediate"),
        "{err}"
    );
}

#[test]
fn intermediate_return_error_does_not_echo_literal_unroll() {
    let lit = r#"e2.m14(source_file_path="/zone/a/work/x.dat", destination_file_path="/zone/a/archive/x.dat")"#;
    let err = program_intermediate_return_error(lit);
    assert!(err.contains("rows =>"), "{err}");
    assert!(!err.contains("/zone/"), "{err}");
    assert!(!err.contains("source_file_path"), "{err}");
    let err2 = program_intermediate_return_must_be_binding_error(lit);
    assert!(!err2.contains("/zone/"), "{err2}");
    assert!(!err2.contains("source_file_path"), "{err2}");
}

#[test]
fn validate_rejects_multiple_bare_root_lines() {
    let err = validate_program_statement_order(&[
        "a = e1".to_string(),
        "b = e2".to_string(),
        "a".to_string(),
        "b".to_string(),
        "c".to_string(),
    ])
    .expect_err("multiple return lines");
    assert!(err.contains("comma-separated"), "{err}");
    assert!(err.contains("one return line"), "{err}");
}

#[test]
fn validate_allows_comma_separated_final_roots() {
    validate_program_statement_order(&[
        "a = e1".to_string(),
        "b = e2".to_string(),
        "a, b".to_string(),
    ])
    .expect("taught multi-root return line");
}

#[test]
fn validate_domain_symbol_assignment_is_label_reject_not_return_root() {
    let err = validate_program_statement_order(&[
        "tok = sess.access_token".to_string(),
        "p1 = e4{access_token=tok, query=\"a@x.com\"}".to_string(),
        "p2 = e4{access_token=tok, query=\"b@x.com\"}".to_string(),
        "p1, p2".to_string(),
    ])
    .expect_err("p# binding name");
    assert!(
        err.contains("Binding names must be labels") && err.contains("p1"),
        "{err}"
    );
    assert!(
        !err.contains("Only one return line"),
        "must name the reserved label, not the return seat: {err}"
    );
}

#[test]
fn validate_rejects_binding_after_return_line() {
    let err = validate_program_statement_order(&[
        "limited[p2,p14]".to_string(),
        "comments = issue.r2".to_string(),
    ])
    .expect_err("binding after return");
    assert!(err.contains("Return must be last"), "{err}");
}

#[test]
fn validate_allows_bare_label_then_projection() {
    validate_program_statement_order(&[
        "comments = issue.r2".to_string(),
        "comments".to_string(),
        "comments[p2,p14]".to_string(),
    ])
    .expect("bare label before projection on last line");
}

#[test]
fn pipe_head_catalog_surface_syntax() {
    assert!(pipe_head_has_catalog_surface_syntax(
        "e3{access_token=\"x\"}"
    ));
    assert!(pipe_head_has_catalog_surface_syntax(
        "LangItem(\"i1\").lines"
    ));
    assert!(pipe_head_has_catalog_surface_syntax("e1~\"query\""));
    assert!(!pipe_head_has_catalog_surface_syntax("rows"));
    assert!(!pipe_head_has_catalog_surface_syntax("items"));
}

#[test]
fn validate_pipe_head_syntax_accepts_catalog_and_binding_labels() {
    validate_pipe_head_syntax("e3{state=\"open\"}").expect("e# brace");
    validate_pipe_head_syntax("LangItem").expect("plain wire entity label");
    validate_pipe_head_syntax("rows").expect("binding label");
}

#[test]
fn validate_pipe_head_syntax_rejects_empty_and_garbage() {
    assert!(validate_pipe_head_syntax("").is_err());
    assert!(validate_pipe_head_syntax("123bad").is_err());
}
