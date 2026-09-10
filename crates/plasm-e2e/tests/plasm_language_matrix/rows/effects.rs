//! Effects, foreach, iterate, render matrix rows.

use super::super::row::MatrixRow;

pub(crate) const ROWS: &[MatrixRow] = &[
    MatrixRow {
        id: "lang_bindings_render",
        program: r#"rows = LangItem("i1") | select id, title
hdr = rows => <<MD
# {{ rows | length }} row(s): {% for r in rows %}{{ r.id }}{% endfor %}
MD
hdr"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "bracket_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["row(s)", "```"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_cross_binding_render",
        program: r#"a = LangItem("i1") | select id, title
report = a => <<MD
Item: {{ a.id }}
MD
report"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "bracket_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["Item:", "i1", "```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_content_into_create",
        program: r#"one = LangItem | take 1 | select title
hdr = one => <<PLASM_TITLE_PIPE
{{ rows[0].title }}
PLASM_TITLE_PIPE
LangItem.create(title=hdr.content, score=0, owner="render-pipe-owner")"#,
        surface_line: false,
        federated: false,
        features: &[
            "bracket_render_content_ref",
            "effect_create",
            "bracket_render",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "render-pipe-owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_heredoc_binding",
        program: r#"note = <<PLASM_LANG_MATRIX_EOF
hello-matrix
PLASM_LANG_MATRIX_EOF
one = LangItem | take 1 | select title
one, note"#,
        surface_line: false,
        federated: false,
        features: &["static_heredoc_binding", "parallel_final_roots"],
        min_node_results: 2,
        expect_markdown_substrings: &["# Results", "hello-matrix", "```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_heredoc_into_create",
        program: r#"body = <<PLASM_HEREDOC_STR
hello-heredoc-string
PLASM_HEREDOC_STR
LangItem.create(title=body, score=0, owner="heredoc-string-owner")"#,
        surface_line: false,
        federated: false,
        features: &[
            "heredoc_binding_string_param",
            "static_heredoc_binding",
            "effect_create",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["heredoc-string-owner", "```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_heredoc_body_with_equals",
        program: r#"body = <<PLASM_EQ_BODY
key = value
PLASM_EQ_BODY
one = LangItem | take 1 | select title
one, body"#,
        surface_line: false,
        federated: false,
        features: &[
            "heredoc_body_with_equals",
            "static_heredoc_binding",
            "parallel_final_roots",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["# Results", "key = value", "```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_inline_heredoc_method_arg",
        program: r#"created = LangItem.create(title=<<PLASM_INLINE_ARG
line one
PLASM_INLINE_ARG
, score=0, owner="inline-heredoc")
created"#,
        surface_line: false,
        federated: false,
        features: &["inline_heredoc_method_arg", "effect_create"],
        min_node_results: 1,
        expect_markdown_substrings: &["inline-heredoc"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_inline_heredoc_method_arg_same_line",
        program: r#"created = LangItem.create(title=<<PLASM_INLINE_SAME
same-line body
PLASM_INLINE_SAME, score=0, owner="inline-same-line")
created"#,
        surface_line: false,
        federated: false,
        features: &["inline_heredoc_method_arg_same_line", "effect_create"],
        min_node_results: 1,
        expect_markdown_substrings: &["inline-same-line"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_inline_heredoc_method_arg_github_shape",
        program: r#"created = LangItem.create(title=<<PLASM_GH_BODY_7f3a
## Problem
Testing mid-arg heredoc close.
PLASM_GH_BODY_7f3a, score=0, owner="github-shape", tags=["documentation"])
created"#,
        surface_line: false,
        federated: false,
        features: &["inline_heredoc_method_arg_github_shape", "effect_create"],
        min_node_results: 1,
        expect_markdown_substrings: &["## Problem", "documentation"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_method_invoke_field_ref",
        program: r#"item = LangItem("i1")
peer = LangItem("i2")
out = item.update(title=peer.title, score=42, owner="alice")
out"#,
        surface_line: false,
        federated: false,
        features: &["bind_method_invoke_field_ref", "effect_update"],
        min_node_results: 3,
        expect_markdown_substrings: &["alice", "42", "```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_singleton_field_scalar",
        program: r#"item = LangItem("i1")
title = item.title
title"#,
        surface_line: false,
        federated: false,
        features: &["bind_singleton_field_scalar", "binding_continuation"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "title"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_derive_map_parallel",
        program: r#"hits = LangItem~"Alpha"
sumry = hits | select id, title
cards = sumry => { t: _.title }
sumry, cards"#,
        surface_line: false,
        federated: false,
        features: &["derive_map", "parallel_final_roots"],
        min_node_results: 3,
        expect_markdown_substrings: &["# Results", "```tsv", "t"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_binding_continuation",
        program: r#"root = LangItem("i1")
tags = root.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &["binding_continuation"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_pipe_select_row_fields",
        program: r#"root = LangItem("i1")
root | select title"#,
        surface_line: false,
        federated: false,
        features: &[
            "pipe_select_row_fields",
            "binding_continuation",
            "pipe_select",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "title"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_limit1_continuation",
        program: r#"root = LangItem{owner="alice"}
one = root | take 1
tags = one => _.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &["bind_pipe_take_continuation", "pipe_take"],
        min_node_results: 3,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_relation_many_from_plural_query",
        program: r#"items = LangItem | take 2
tags = items => _.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "relation_prefer_embed_miss",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_relation_prefer_embed_hit",
        program: r#"item = LangItem("i1")
tags = item.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_prefer_embed_hit",
            "relation_prefer_from_parent_get",
            "relation_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "item_id", "label"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_relation_prefer_embed_miss",
        program: r#"items = LangItem{owner="bob"} | take 2
tags = items => _.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_prefer_embed_miss",
            "relation_prefer_from_parent_get",
            "relation_many_from_plural",
            "relation_query_scoped",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_plural_relation_opaque_p",
        program: r#"items = LangItem | take 2
tags = items => _.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "binding_opaque_relation_ref",
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_relation_opaque_r_symbol",
        program: "",
        surface_line: false,
        federated: false,
        features: &[
            "relation_opaque_r_symbol",
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_homograph_lhs_coercion",
        program: "",
        surface_line: false,
        federated: false,
        features: &[
            "homograph_lhs_coercion",
            "repair_sugar_homograph",
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_relation_integer_scoped_bindings",
        program: r#"items = LangItem | take 2
tags = items => _.tags_by_score
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_many_from_plural",
            "relation_query_scoped_bindings",
            "relation_binding_proof",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_group_by_then_sort_agg_column",
        program: "LangItem | summarize by owner n=count() | order by n desc",
        surface_line: true,
        federated: false,
        features: &[
            "pipe_summarize_by",
            "pipe_summarize_order_by",
            "pipe_order_by",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_dedupe",
        program: "LangItem | distinct by owner | take 20",
        surface_line: true,
        federated: false,
        features: &["pipe_distinct", "pipe_take"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_dedupe",
        program: "rows = LangItem~\"matrix\"\nrows | distinct by owner",
        surface_line: false,
        federated: false,
        features: &["pipe_distinct", "entity_search", "search_then_group_by"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // RA-4 pipe factorization — monolith (inline stages).
    MatrixRow {
        id: "lang_ra4_pipe_monolith",
        program: r#"LangItem | where owner="alice" | order by title | take 5 | select title, owner"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_pipe_factor",
            "pipe_where",
            "pipe_order_by",
            "pipe_take",
            "pipe_select",
            "dry_live_parity",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "alice"],
        expect_live_error: None,
    },
    // RA-4 pipe factorization — bind cuts of the same stage spine.
    MatrixRow {
        id: "lang_ra4_pipe_bind_cut",
        program: r#"h = LangItem
w = h | where owner="alice"
o = w | order by title
t = o | take 5
t | select title, owner"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_pipe_factor",
            "pipe_where",
            "pipe_order_by",
            "pipe_take",
            "pipe_select",
            "bindings_assignment",
            "dry_live_parity",
        ],
        min_node_results: 5,
        expect_markdown_substrings: &["```tsv", "alice"],
        expect_live_error: None,
    },
    // RA-4 apply factorization — inline `=>` derive.
    MatrixRow {
        id: "lang_ra4_apply_monolith",
        program: r#"LangItem | where owner="alice" | take 3 => { t: _.title, o: _.owner }"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_apply_factor",
            "pipe_where",
            "pipe_take",
            "derive_map",
            "dry_live_parity",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // RA-4 apply factorization — bind left then `=>`.
    MatrixRow {
        id: "lang_ra4_apply_bind_cut",
        program: r#"rows = LangItem | where owner="alice" | take 3
cards = rows => { t: _.title, o: _.owner }
cards"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_apply_factor",
            "pipe_where",
            "pipe_take",
            "derive_map",
            "bindings_assignment",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // RA-4 apply — relation fanout monolith vs bind-cut.
    MatrixRow {
        id: "lang_ra4_apply_relation_monolith",
        program: r#"LangItem | take 2 => _.tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_apply_factor",
            "pipe_take",
            "relation_many_from_plural",
            "dry_live_parity",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_ra4_apply_relation_bind_cut",
        program: r#"items = LangItem | take 2
tags = items => _.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_apply_factor",
            "pipe_take",
            "relation_many_from_plural",
            "bindings_assignment",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // RA-4 apply — render bind-cut (pipe⇒render monolith needs named collection alias; sealed via bind).
];
