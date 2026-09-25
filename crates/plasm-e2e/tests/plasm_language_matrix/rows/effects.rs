//! Effects, foreach, iterate, render matrix rows.

use super::super::row::MatrixRow;

pub(crate) const ROWS: &[MatrixRow] = &[
    MatrixRow {
        id: "lang_relation_empty_fanout",
        program: "items = LangItem | where id = \"missing\"\ntags = items => _.tags\ntags",
        surface_line: false,
        federated: false,
        features: &["relation_empty_fanout", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["tags"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_relation_one_chain",
        program:
            "summary = LangItem(\"i1\").summary\ndetail = summary.detail\ndetail | select id, body",
        surface_line: false,
        federated: false,
        features: &["relation_one_chain", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "body"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_derived_shape",
        program: r#"items = LangItem("i1")
mapped = items => { renamed: _.id }
out = mapped => <<ROW
value={{ renamed }}
ROW
out"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "per_row_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["value=i1"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_value_error_at_execution",
        program: r#"items = LangItem("i1")
out = items => <<ROW
{{ id | split_part('/', 99) }}
ROW
out"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "per_row_render"],
        min_node_results: 2,
        expect_markdown_substrings: &[],
        expect_live_error: Some("split_part"),
    },
    MatrixRow {
        id: "lang_render_projected_shape",
        program: r#"items = LangItem("i1") | select renamed = id
out = items => <<ROW
{{ renamed | split_part('1', 0) }}
ROW
out"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "per_row_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["i"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_relation_shape",
        program: r#"items = LangItem("i1")
out = items => <<ROW
relation_count={{ lines | length }}
ROW
out"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "per_row_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["relation_count="],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bindings_render",
        program: r#"items = LangItem("i1") | select id, title
hdr = items => <<MD
# {{ title }}
MD
hdr"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "bracket_render", "per_row_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["#", "```"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_split_part",
        program: r#"items = LangItem("i1") | select id
hdr = items => <<MD
split_part_ok={{ id | split_part('1', 0) }}
MD
hdr"#,
        surface_line: false,
        federated: false,
        features: &[
            "bindings_assignment",
            "bracket_render",
            "render_minijinja_split_part",
            "per_row_render",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["split_part_ok=i"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_cross_binding_render",
        program: r#"a = LangItem("i1") | select id, title
report = a => <<MD
Item: {{ id }}
MD
report"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "bracket_render", "per_row_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["Item:", "i1", "```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_content_into_create",
        program: r#"one = LangItem | take 1 | select title
hdr = one => <<PLASM_TITLE_PIPE
{{ title }}
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
        id: "lang_take_one_method_invoke",
        program: r#"items = LangItem
one = items | order by id | take 1
out = one.update(title="after-take1", score=2, owner="alice")
out"#,
        surface_line: false,
        federated: false,
        features: &[
            "bounded_singleton_method_invoke",
            "effect_update",
            "pipe_take",
        ],
        min_node_results: 3,
        expect_markdown_substrings: &["after-take1", "```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_take_one_method_invoke_empty",
        program: r#"items = LangItem
one = items | where id = "missing" | take 1
out = one.update(title="must-not-write", score=2, owner="alice")
out"#,
        surface_line: false,
        federated: false,
        features: &["bounded_singleton_method_invoke", "effect_update"],
        min_node_results: 3,
        expect_markdown_substrings: &[],
        expect_live_error: Some("zero rows"),
    },
    MatrixRow {
        id: "lang_rows_each_method_invoke",
        program: r#"items = LangItem
out = items => _.update(title="after-each", score=2, owner=_.owner)
out"#,
        surface_line: false,
        federated: false,
        features: &[
            "row_identity_method_invoke",
            "for_each_effect",
            "effect_update",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["after-each", "```tsv"],
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
        id: "lang_get_singleton_field_scalar",
        program: r#"title = LangItem("i1").title
title"#,
        surface_line: false,
        federated: false,
        features: &["get_singleton_field_scalar", "bind_singleton_field_scalar"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "title"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_get_singleton_field_argument",
        program: r#"out = LangItem("i1").update(title=LangItem("i2").title, score=1, owner="alice")
out"#,
        surface_line: false,
        federated: false,
        features: &["get_singleton_field_scalar", "effect_update"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_get_singleton_field_password",
        program: r#"pw = LangVault("venmo").password
pw"#,
        surface_line: false,
        federated: false,
        features: &["get_singleton_field_scalar"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "venmo-secret"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_get_singleton_field_empty",
        program: r#"LangVault("missing").password"#,
        surface_line: false,
        federated: false,
        features: &["get_singleton_field_scalar"],
        min_node_results: 1,
        expect_markdown_substrings: &[],
        expect_live_error: Some("zero rows"),
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
        id: "lang_bind_filter_continuation",
        program: r#"root = LangItem{owner="alice"}
filtered = root | where owner="alice"
tags = filtered => _.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "bind_pipe_where_continuation",
            "pipe_where",
            "binding_continuation",
        ],
        min_node_results: 3,
        expect_markdown_substrings: &["## tags (", "Result coverage:"],
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
        surface_line: true,
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
        surface_line: true,
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
    MatrixRow {
        id: "lang_per_row_render_zero",
        program: r#"items = LangItem | where id = "missing"
rendered = items => <<TEXT
{{ title }}
TEXT
rendered"#,
        surface_line: false,
        federated: false,
        features: &["per_row_render", "bracket_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_per_row_render_many",
        program: r#"items = LangItem | take 2 | select id, title, code
rendered = items => <<TEXT
{{ title }} — {{ code }}
TEXT
rendered"#,
        surface_line: false,
        federated: false,
        features: &["per_row_render", "bracket_render", "pipe_take"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "content"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_plain_template_foreach",
        program: r#"items = LangItem | take 2 | select id, title
report = <<REPORT
{% for item in items %}
- {{ item.title }}
{% endfor %}
REPORT
report"#,
        surface_line: false,
        federated: false,
        features: &[
            "plain_template_foreach",
            "static_heredoc_binding",
            "pipe_take",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["-"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_name_collision",
        program: r#"title = LangItem("i1") | take 1
items = LangItem | take 2
bad = items => <<TEXT
{{ title }}
TEXT
bad"#,
        surface_line: false,
        federated: false,
        features: &["render_name_collision", "per_row_render"],
        min_node_results: 1,
        expect_markdown_substrings: &[],
        expect_live_error: Some("both a row field and a program binding"),
    },
    MatrixRow {
        id: "lang_render_content_plural_reject",
        program: r#"items = LangItem | take 2 | select title
hdr = items => <<TEXT
{{ title }}
TEXT
LangItem.create(title=hdr.content, score=0, owner="plural-content")"#,
        surface_line: false,
        federated: false,
        features: &["render_content_plural_reject", "bracket_render_content_ref"],
        min_node_results: 2,
        expect_markdown_substrings: &[],
        expect_live_error: Some("not a singleton"),
    },
    MatrixRow {
        id: "lang_per_row_arg_template",
        program: r#"items = LangItem | take 2
done = items => _.update(title=<<TITLE
{{ title }} — {{ id }}
TITLE, score=_.score, owner=_.owner)
done"#,
        surface_line: false,
        federated: false,
        features: &[
            "per_row_arg_template",
            "for_each_effect",
            "effect_update",
            "row_identity_method_invoke",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_aggregate_report",
        program: r#"items = LangItem | take 2
done = items => _.update(title=<<TITLE
{{ title }} — {{ id }}
TITLE, score=_.score, owner=_.owner)
report = <<REPORT
{% for item in done %}
- {{ item.title }}
{% endfor %}
REPORT
report"#,
        surface_line: false,
        federated: false,
        features: &[
            "render_aggregate_report",
            "per_row_arg_template",
            "plain_template_foreach",
            "for_each_effect",
            "effect_update",
        ],
        min_node_results: 3,
        expect_markdown_substrings: &["-"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_render_undefined_field",
        program: r#"items = LangItem("i1") | select id
bad = items => <<TEXT
{{ missing_field }}
TEXT
bad"#,
        surface_line: false,
        federated: false,
        features: &["per_row_render"],
        min_node_results: 1,
        expect_markdown_substrings: &[],
        expect_live_error: Some("not a current-row field"),
    },
];
