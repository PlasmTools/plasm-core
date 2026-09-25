//! Exact counterparts of the original federated matrix rows.
use super::python::Case;
use crate::language_matrix;
use plasm_core::symbol_tuning::SymbolRender;

pub(super) const CASES: &[Case] = &[
    Case { id: "cert_federated_e1_query", plasm: "", python: "return e1.query(owner=\"alice\")", existing: Some("lang_federated_duplicate_entity_e1_query"), expect_live_error: None },
    Case { id: "cert_federated_e2_search", plasm: "", python: "return e2.search(q=\"Alpha\")", existing: Some("lang_federated_duplicate_entity_e2_search"), expect_live_error: None },
    Case { id: "cert_federated_relation_r", plasm: "", python: "parent = e2.get(\"i1\")\nkids = parent.FED_CHILDREN\nreturn kids.select(\"id\", \"title\")", existing: Some("lang_federated_duplicate_entity_relation_r"), expect_live_error: None },
    Case { id: "cert_federated_mutator_m", plasm: "", python: "return e2.FED_CREATE(title=\"fed-mutator-matrix\", score=0, owner=\"matrix-fed-owner\")", existing: Some("lang_federated_duplicate_entity_mutator_m"), expect_live_error: None },
    Case { id: "cert_federated_pathless_action", plasm: "", python: "return e2.FED_BROADCAST(message=\"fed-pathless-broadcast\")", existing: Some("lang_federated_duplicate_entity_pathless_action"), expect_live_error: None },
    Case { id: "cert_federated_auth_sessions", plasm: "", python: "sn_auth = FED_AUTH_B.FED_LOGIN_B(username=\"simple_note\", password=\"secret\")\nsw_auth = FED_AUTH_A.FED_LOGIN_A(username=\"splitwise\", password=\"secret\")\nsn_tok = sn_auth.access_token\nsw_tok = sw_auth.access_token\nnotes = FED_NOTE.search(query=\"trip\", access_token=sn_tok)\ngroups = FED_GROUP.query(access_token=sw_tok)\nreturn notes, groups", existing: Some("lang_federated_auth_session_provides_mutation"), expect_live_error: None },
    Case { id: "cert_federated_parallel_roots", plasm: "", python: "return e1.query(owner=\"alice\"), e2.search(q=\"Alpha\")", existing: Some("lang_federated_parallel_roots"), expect_live_error: None },
    Case { id: "cert_federated_group_by", plasm: "", python: "by = e1.query(owner=\"alice\").group_by(\"owner\", n=agg.count())\nreturn by", existing: Some("lang_federated_group_by_on_e1"), expect_live_error: None },
    Case { id: "cert_federated_target_entry", plasm: "", python: "item = FED_ITEM_B.get(\"i1\")\nsummary = item.summary\nreturn summary", existing: Some("lang_federated_relation_target_entry"), expect_live_error: None },
    Case { id: "cert_federated_bound_render", plasm: "", python: "class MatrixProgram(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return f\"# {row.title}\"\n    def build(self):\n        rows = e1.query(owner=\"alice\").select(\"title\")\n        report = self.text(rows)\n        return report\n", existing: Some("lang_bind_template_inline_on_e1"), expect_live_error: None },
];

pub(super) fn source(case: &Case, es: &plasm_agent::execute_session::ExecuteSession) -> String {
    let mut source = case.python.to_owned();
    if !case.id.starts_with("cert_federated_") {
        return source;
    }
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    for (token, entry, entity) in [
        (
            "FED_AUTH_A",
            language_matrix::MATRIX_FED_A,
            "LangAuthSession",
        ),
        (
            "FED_AUTH_B",
            language_matrix::MATRIX_FED_B,
            "LangAuthSession",
        ),
        ("FED_NOTE", language_matrix::MATRIX_FED_B, "LangSecuredNote"),
        (
            "FED_GROUP",
            language_matrix::MATRIX_FED_A,
            "LangSecuredGroup",
        ),
        ("FED_ITEM_B", language_matrix::MATRIX_FED_B, "LangItem"),
    ] {
        if source.contains(token) {
            source = source.replace(token, &symbols.entity_sym_for(entry, entity));
        }
    }
    for (token, entry, entity, method) in [
        (
            "FED_CREATE",
            language_matrix::MATRIX_FED_B,
            "LangItem",
            "create",
        ),
        (
            "FED_BROADCAST",
            language_matrix::MATRIX_FED_B,
            "LangItem",
            "broadcast",
        ),
        (
            "FED_LOGIN_A",
            language_matrix::MATRIX_FED_A,
            "LangAuthSession",
            "login",
        ),
        (
            "FED_LOGIN_B",
            language_matrix::MATRIX_FED_B,
            "LangAuthSession",
            "login",
        ),
    ] {
        if source.contains(token) {
            source = source.replace(token, &symbols.method_sym_for(entry, entity, method));
        }
    }
    if source.contains("FED_CHILDREN") {
        source = source.replace(
            "FED_CHILDREN",
            &symbols.ident_sym_relation_for(language_matrix::MATRIX_FED_B, "LangItem", "children"),
        );
    }
    source
}
