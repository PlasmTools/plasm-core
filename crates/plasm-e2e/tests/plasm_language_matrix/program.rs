//! Runtime program synthesis for rows with empty static programs.

use super::row::MatrixRow;
use crate::language_matrix;

pub(crate) fn matrix_program_for_row(
    row: &MatrixRow,
    es: &plasm_agent::execute_session::ExecuteSession,
) -> String {
    match row.id {
        "lang_relation_opaque_r_symbol" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("matrix session domain exposure");
            let map = exp.symbol_map_arc();
            let r_sym =
                map.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "tags");
            format!("items = LangItem | take 2\ntags = items => _.{r_sym}\ntags")
        }
        "lang_homograph_lhs_coercion" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("matrix session domain exposure");
            let map = exp.symbol_map_arc();
            let tags_wire = map.ident_sym_cap_param_for(
                language_matrix::MATRIX_ENTRY_ID,
                "LangItem",
                "langitem_query",
                "tags",
            );
            assert_eq!(
                tags_wire, "tags",
                "langitem_query.tags filter teaches as wire name"
            );
            format!("items = LangItem | take 2\ntags = items => _.{tags_wire}\ntags")
        }
        "lang_federated_duplicate_entity_relation_r" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("federated dup session exposure");
            let map = exp.symbol_map_arc();
            let r_sym = map.ident_sym_relation_for("linear", "LangItem", "children");
            format!("parent = e2(\"i1\")\nkids = parent.{r_sym}\nkids | select id, title")
        }
        "lang_federated_duplicate_entity_mutator_m" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("federated dup session exposure");
            let map = exp.symbol_map_arc();
            let m_sym = map.method_sym_for("linear", "LangItem", "create");
            format!("e2.{m_sym}(title=\"fed-mutator-matrix\", score=0, owner=\"matrix-fed-owner\")")
        }
        "lang_federated_duplicate_entity_pathless_action" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("federated dup session exposure");
            let map = exp.symbol_map_arc();
            let m_sym = map.method_sym_for("linear", "LangItem", "broadcast");
            format!(r#"e2.{m_sym}(message="fed-pathless-broadcast")"#)
        }
        "lang_federated_auth_session_provides_mutation" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("federated auth session exposure");
            let map = exp.symbol_map_arc();
            let e_sn_auth = map.entity_sym_for("linear", "LangAuthSession");
            let m_sn_login = map.method_sym_for("linear", "LangAuthSession", "login");
            let e_sw_auth = map.entity_sym_for("github", "LangAuthSession");
            let m_sw_login = map.method_sym_for("github", "LangAuthSession", "login");
            let e_note = map.entity_sym_for("linear", "LangSecuredNote");
            let e_group = map.entity_sym_for("github", "LangSecuredGroup");
            // CUGA AppWorld shape: two federated logins → two Bearer consumers in one plasm_run.
            format!(
                r#"sn_auth = {e_sn_auth}.{m_sn_login}(username="simple_note", password="secret")
sw_auth = {e_sw_auth}.{m_sw_login}(username="splitwise", password="secret")
sn_tok = sn_auth.access_token
sw_tok = sw_auth.access_token
notes = {e_note}~"trip"{{access_token=sn_tok}}
groups = {e_group}{{access_token=sw_tok}}
notes, groups"#
            )
        }
        "lang_federated_relation_target_entry" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("federated relation target session exposure");
            let map = exp.symbol_map_arc();
            let e_poke = map.entity_sym_for("pokeapi", "LangItem");
            format!("item = {e_poke}(\"i1\")\nsummary = item.summary\nsummary")
        }
        "lang_federated_duplicate_entity_e2_search" => r#"e2~"Alpha""#.to_string(),
        "lang_federated_parallel_roots" => r#"e1{owner="alice"}, e2~"Alpha""#.to_string(),
        "lang_bind_template_inline_on_e1" => r#"rows = e1{owner="alice"} | select title
report = rows => <<INLINE_E1
# {{ rows | length }} row(s)
INLINE_E1
report"#
            .to_string(),
        _ => row.program.to_string(),
    }
}
