//! Federated / RA4 / pagination matrix rows.

use super::super::row::MatrixRow;

pub(crate) const ROWS: &[MatrixRow] = &[
    MatrixRow {
        id: "lang_ra4_apply_render_bind_cut",
        program: r#"items = LangItem("i1") | select id, title
hdr = items => <<RA4MDBIND
# {{ title }}
RA4MDBIND
hdr"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_apply_factor",
            "pipe_select",
            "bracket_render",
            "bindings_assignment",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "content", "#"],
        expect_live_error: None,
    },
    // RA-4 apply — for_each monolith vs bind-cut.
    MatrixRow {
        id: "lang_ra4_apply_foreach_monolith",
        program: r#"LangItem("i1") | select id, title, owner => LangItem("i1").update(score=3, title=_.title, owner=_.owner)"#,
        surface_line: false,
        federated: false,
        features: &["ra4_apply_factor", "pipe_select", "for_each_effect", "dry_live_parity"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_ra4_apply_foreach_bind_cut",
        program: r#"items = LangItem("i1") | select id, title, owner
sync = items => LangItem("i1").update(score=3, title=_.title, owner=_.owner)
sync"#,
        surface_line: false,
        federated: false,
        features: &[
            "ra4_apply_factor",
            "pipe_select",
            "for_each_effect",
            "bindings_assignment",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // Trap: derive body containing `.message` must stay derive (not for_each via `.m` substring).
    MatrixRow {
        id: "lang_ra4_apply_derive_message_field",
        program: r#"LangItem | where owner="alice" | take 2 => { t: _.title, note: "_.message" }"#,
        surface_line: true,
        federated: false,
        features: &["ra4_apply_factor", "derive_map", "pipe_where", "pipe_take", "dry_live_parity"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_group_by_first",
        program: "LangItem | summarize by owner title=first(title)",
        surface_line: true,
        federated: false,
        features: &["pipe_summarize_by", "agg_first_last"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_projection_then_relation",
        program: r#"root = LangItem("i1")
trimmed = root | select id, title
tags = trimmed.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &["bind_projection_then_relation", "pipe_select"],
        min_node_results: 3,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_relation_hop_one_one",
        program: r#"summary = LangItem("i1").summary
summary | select headline"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_one_opaque_r",
            "bind_relation_hop_one_one",
            "dry_live_parity",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_effect_create_literal",
        program: r#"LangItem.create(title="MatrixCreated", score=7, owner="bot")"#,
        surface_line: false,
        federated: false,
        features: &["effect_create"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "MatrixCreated"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_money_predicate_gt",
        program: r#"LangOffer | where price>10"#,
        surface_line: false,
        federated: false,
        features: &["money_predicate", "pipe_where"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_integer_where_gt_dry_coerce",
        program: r#"LangItem | where score > 0"#,
        surface_line: false,
        federated: false,
        features: &["catalog_directed_coerce", "pipe_where", "dry_live_parity"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_money_create_body",
        program: r#"LangOffer.create(price="9.25", quote_currency="USD")"#,
        surface_line: false,
        federated: false,
        features: &["money_create_body", "effect_create"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_effect_update",
        program: r#"LangItem("i1").update(title="MatrixPatch", score=42, owner="alice")"#,
        surface_line: false,
        federated: false,
        features: &["effect_update"],
        min_node_results: 1,
        expect_markdown_substrings: &["MatrixPatch", "42"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_effect_action_ping",
        program: r#"LangItem("i1").ping()"#,
        surface_line: false,
        federated: false,
        features: &["effect_action"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "i1", "operations:", "capability=`langitem_ping`"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_effect_delete",
        program: r#"LangItem("i2").delete()"#,
        surface_line: false,
        federated: false,
        features: &["effect_delete"],
        min_node_results: 1,
        expect_markdown_substrings: &[
            "(no results)",
            "operations:",
            "capability=`langitem_delete`",
            "completed=1",
        ],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_for_each_empty_ping",
        program: r#"items = LangItem | where owner="no-such-matrix-owner"
done = items => LangItem(_.id).ping()
done"#,
        surface_line: false,
        federated: false,
        features: &["for_each_effect", "effect_action", "pipe_where"],
        min_node_results: 2,
        expect_markdown_substrings: &[
            "(no results)",
            "operations:",
            "capability=`langitem_ping`",
            "invocations=0",
        ],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_apply_get_multirow",
        program: r#"items = LangItem | where owner="alice" | take 3
details = items => LangItem(_.id)
details"#,
        surface_line: false,
        federated: false,
        features: &["row_apply_get", "pipe_where", "pipe_take", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "details"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_apply_query_multirow",
        program: r#"items = LangItem | where owner="alice" | take 2
peers = items => LangItem{owner=_.owner}
peers"#,
        surface_line: false,
        federated: false,
        features: &["row_apply_query", "pipe_where", "pipe_take", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["peers"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_for_each_update",
        program: "items = LangItem(\"i1\") | select id, title, owner\nsync = items => LangItem(\"i1\").update(score=3, title=_.title, owner=_.owner)\nsync",
        surface_line: false,
        federated: false,
        features: &["for_each_effect"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // Multi-row live mutator fanout (list source → N distinct PATCH identities).
    MatrixRow {
        id: "lang_for_each_multirow_update",
        program: r#"items = LangItem | where owner="alice" | take 3
done = items => LangItem(_.id).update(score=9, title=_.title, owner=_.owner)
done"#,
        surface_line: false,
        federated: false,
        features: &[
            "for_each_effect",
            "for_each_multirow",
            "pipe_where",
            "pipe_take",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // Outer auth binding × multi-row secured mutator (Bearer from LangAuthSession.login).
    MatrixRow {
        id: "lang_for_each_auth_secured_touch",
        program: r#"auth = LangAuthSession.login(username="matrix", password="secret")
items = LangItem | where owner="alice" | take 3
done = items => LangItem(_.id).secured-touch(access_token=auth.access_token)
done"#,
        surface_line: false,
        federated: false,
        features: &[
            "for_each_effect",
            "for_each_multirow",
            "for_each_auth_outer_binding",
            "pipe_where",
            "pipe_take",
            "dry_live_parity",
        ],
        min_node_results: 3,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    // PLP-8: seed phase=open; each tick advances open→mid→done; until holds within take.
    MatrixRow {
        id: "lang_iterate_until_bound",
        program: r#"cur = LangCursor("c1")
done = iterate cur step LangCursor(_.id).tick() until phase = "done" take 4
done"#,
        surface_line: false,
        federated: false,
        features: &[
            "iterate_until_bound",
            "effect_action",
            "entity_get",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &[
            "```tsv",
            "c1",
            "phase",
            "operations:",
            "capability=`langcursor_tick`",
        ],
        expect_live_error: None,
    },
    // PLP-8: until already true on seed — zero steps.
    MatrixRow {
        id: "lang_iterate_until_zero_step",
        program: r#"cur = LangCursor("c_done")
done = iterate cur step LangCursor(_.id).tick() until phase = "done" take 3
done"#,
        surface_line: false,
        federated: false,
        features: &["iterate_until_zero_step", "entity_get", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "c_done", "phase"],
        expect_live_error: None,
    },
    // PLP-8: stuck cursor never reaches done — bound exhaustion is defined failure.
    MatrixRow {
        id: "lang_iterate_bound_exhausted",
        program: r#"cur = LangCursor("c_stuck")
done = iterate cur step LangCursor(_.id).tick() until phase = "done" take 2
done"#,
        surface_line: false,
        federated: false,
        features: &["iterate_bound_exhausted", "effect_action", "entity_get"],
        min_node_results: 1,
        expect_markdown_substrings: &[],
        expect_live_error: Some("iterate_bound_exhausted"),
    },
    MatrixRow {
        id: "lang_federated_relation_target_entry",
        program: "",
        surface_line: false,
        federated: true,
        features: &["federated_relation_target_entry", "relation_from_parent_get"],
        min_node_results: 2,
        expect_markdown_substrings: &["summary"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_e1_query",
        program: r#"e1{owner="alice"}"#,
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "domain_symbol_e1",
            "entity_query",
            "predicate_brace_equality",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_e2_search",
        program: "e2~$",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "entity_search",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_relation_r",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_duplicate_entity_relation_r",
            "relation_from_parent_get",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_mutator_m",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_duplicate_entity_mutator_m",
            "effect_create",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_pathless_action",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_duplicate_entity_pathless_action",
            "effect_action",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_auth_session_provides_mutation",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_auth_session_provides_mutation",
            "federated_dual_auth_bearer_holes",
            "summary_hydrate_capability_params",
            "effect_action",
            "entity_search",
            "entity_query",
            "entity_get",
        ],
        min_node_results: 4,
        expect_markdown_substrings: &["```tsv", "note_id", "group_id"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_parallel_roots",
        program: "e1{owner=\"alice\"}, e2~$",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_parallel_roots",
            "entity_query",
            "entity_search",
            "parallel_final_roots",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_federated_group_by_on_e1",
        program: "by = e1{owner=\"alice\"} | summarize by owner n=count()\nby",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_group_by_on_e1",
            "pipe_summarize_chain",
            "pipe_summarize_by",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bind_template_inline_on_e1",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "bracket_render_inline_on_e",
            "bracket_render",
            "bindings_assignment",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["#"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_domain_symbol_page_size",
        program: "e1.page_size(10)",
        surface_line: false,
        federated: false,
        features: &["domain_symbol_e1", "pagination_page_size"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_utf8_minijinja_content_stitch",
        program: r#"one = LangItem | take 1 | select title
type_md = one => <<UTF8_ROW_EOF
# Pokémon — {{ title }}
UTF8_ROW_EOF
LangItem.create(title=<<UTF8_DOC_EOF
Featured Pokémon
{{ type_md.content }}
UTF8_DOC_EOF
, score=0, owner="utf8-matrix-owner")"#,
        surface_line: false,
        federated: false,
        features: &[
            "utf8_minijinja_interpolate",
            "bracket_render",
            "bracket_render_content_ref",
            "effect_create",
            "bindings_assignment",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "utf8-matrix-owner", "Pokémon"],
        expect_live_error: None,
    },
];
