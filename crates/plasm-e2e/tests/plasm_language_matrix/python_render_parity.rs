//! Original render/derive obligations expressed through typed Python nodes.
use super::python::Case;

pub(super) const CASES: &[Case] = &[
    Case { id: "render_parity_lang_cross_binding_render", plasm: "", python: "class Text(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"Item: {row.id}\"\n    def build(self):\n        a = E.get(\"i1\").select(\"id\", \"title\")\n        report = self.text(a)\n        return report\n", existing: Some("lang_cross_binding_render"), expect_live_error: None },
    Case { id: "render_parity_lang_ra4_apply_render_bind_cut", plasm: "", python: "class Text(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"# {row.title}\"\n    def build(self):\n        items = E.get(\"i1\").select(\"id\", \"title\")\n        hdr = self.text(items)\n        return hdr\n", existing: Some("lang_ra4_apply_render_bind_cut"), expect_live_error: None },
    Case { id: "render_parity_lang_render_content_into_create", plasm: "", python: "class Text(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return row.title\n    def build(self):\n        one = E.query().take(1).select(\"title\")\n        hdr = self.text(one)\n        return E.CREATE(title=hdr.content, score=0, owner=\"render-pipe-owner\")\n", existing: Some("lang_render_content_into_create"), expect_live_error: None },
    Case { id: "render_parity_lang_derive_map_parallel", plasm: "", python: "hits = E.search(q=\"Alpha\")\nsumry = hits.select(\"id\", \"title\")\ncards = sumry.select(t=\"title\")\nreturn sumry, cards", existing: Some("lang_derive_map_parallel"), expect_live_error: None },
    Case { id: "render_parity_lang_ra4_apply_monolith", plasm: "", python: "return E.query().where(lambda row: row.owner == \"alice\").take(3).select(t=\"title\", o=\"owner\")", existing: Some("lang_ra4_apply_monolith"), expect_live_error: None },
    Case { id: "render_parity_lang_ra4_apply_bind_cut", plasm: "", python: "rows = E.query().where(lambda row: row.owner == \"alice\").take(3)\ncards = rows.select(t=\"title\", o=\"owner\")\nreturn cards", existing: Some("lang_ra4_apply_bind_cut"), expect_live_error: None },
    Case { id: "render_parity_lang_ra4_apply_derive_message_field", plasm: "", python: "return E.query().where(lambda row: row.owner == \"alice\").take(2).select(t=\"title\", note=lambda row: \"_.message\")", existing: Some("lang_ra4_apply_derive_message_field"), expect_live_error: None },
    Case { id: "render_parity_lang_with_mul", plasm: "", python: "items = E.query()\nboosted = items.select(\"id\", boost=lambda row: row.score * 2).take(3)\nreturn boosted", existing: Some("lang_with_mul"), expect_live_error: None },
    Case { id: "render_parity_lang_with_div", plasm: "", python: "items = E.query()\nhalved = items.select(\"id\", half=lambda row: row.score / 2).take(3)\nreturn halved", existing: Some("lang_with_div"), expect_live_error: None },
    Case { id: "render_parity_lang_with_concat", plasm: "", python: "items = E.query().where(lambda row: row.owner == \"alice\")\ntagged = items.select(tag=lambda row: row.owner + row.owner).take(1)\nreturn tagged", existing: Some("lang_with_concat"), expect_live_error: None },
    Case { id: "render_parity_lang_with_when_len", plasm: "", python: "items = E.query().where(lambda row: row.owner == \"alice\")\nlabeled = items.select(label=lambda row: row.owner if len(row.owner) > 0 else row.title).take(1)\nreturn labeled", existing: Some("lang_with_when_len"), expect_live_error: None },
    Case { id: "render_parity_lang_heredoc_into_create", plasm: "", python: "body = \"hello-heredoc-string\\n\"\nreturn E.CREATE(title=body, score=0, owner=\"heredoc-string-owner\")", existing: Some("lang_heredoc_into_create"), expect_live_error: None },
    Case { id: "render_parity_lang_inline_heredoc_method_arg", plasm: "", python: "created = E.CREATE(title=\"line one\\n\", score=0, owner=\"inline-heredoc\")\nreturn created", existing: Some("lang_inline_heredoc_method_arg"), expect_live_error: None },
    Case { id: "render_parity_lang_inline_heredoc_method_arg_same_line", plasm: "", python: "created = E.CREATE(title=\"same-line body\\n\", score=0, owner=\"inline-same-line\")\nreturn created", existing: Some("lang_inline_heredoc_method_arg_same_line"), expect_live_error: None },
    Case { id: "render_parity_lang_inline_heredoc_method_arg_github_shape", plasm: "", python: "created = E.CREATE(title=\"## Problem\\nTesting mid-arg heredoc close.\\n\", score=0, owner=\"github-shape\", tags=[\"documentation\"])\nreturn created", existing: Some("lang_inline_heredoc_method_arg_github_shape"), expect_live_error: None },
    Case { id: "render_parity_lang_utf8_minijinja_content_stitch", plasm: "", python: "class Text(Program):\n    @compute\n    def type_text(self, row: Value[ENTITY]) -> str:\n        return f\"# Pokémon — {row.title}\"\n    @compute\n    def document(self, row: Row) -> str:\n        return f\"Featured Pokémon\\n{row.content}\"\n    def build(self):\n        one = E.query().take(1).select(\"title\")\n        type_md = self.type_text(one)\n        doc = self.document(type_md)\n        return E.CREATE(title=doc.content, score=0, owner=\"utf8-matrix-owner\")\n", existing: Some("lang_utf8_minijinja_content_stitch"), expect_live_error: None },
    Case { id: "render_parity_lang_per_row_arg_template", plasm: "", python: "items = E.query().take(2)\ndone = items.flat_map(lambda row: row.UPDATE(title=f\"{row.title} — {row.id}\", score=row.score, owner=row.owner))\nreturn done", existing: Some("lang_per_row_arg_template"), expect_live_error: None },
    Case { id: "render_parity_lang_render_aggregate_report", plasm: "", python: "class Text(Program):\n    @compute\n    def format_report(self, rows: list[Value[ENTITY]]) -> str:\n        return \"\".join(f\"\\n- {row.title}\\n\" for row in rows)\n    def build(self):\n        items = E.query().take(2)\n        done = items.flat_map(lambda row: row.UPDATE(title=f\"{row.title} — {row.id}\", score=row.score, owner=row.owner))\n        report = self.format_report(done)\n        return report.content\n", existing: Some("lang_render_aggregate_report"), expect_live_error: None },
    Case { id: "render_parity_lang_render_relation_shape", plasm: "", python: "class RelationText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"relation_count={len(row.lines)}\"\n    def build(self):\n        items = E.get(\"i1\")\n        return self.text(items)\n", existing: Some("lang_render_relation_shape"), expect_live_error: None },
    Case { id: "render_parity_lang_render_value_error_at_execution", plasm: "", python: "class Text(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return row.id.split('/')[99]\n    def build(self):\n        items = E.get(\"i1\")\n        return self.text(items)\n", existing: Some("lang_render_value_error_at_execution"), expect_live_error: Some("split_part") },
    Case { id: "render_parity_lang_render_undefined_field", plasm: "", python: "class Text(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return row.missing_field\n    def build(self):\n        items = E.get(\"i1\").select(\"id\")\n        return self.text(items)\n", existing: Some("lang_render_undefined_field"), expect_live_error: Some("not a current-row field") },
    Case { id: "render_parity_lang_render_content_plural_reject", plasm: "", python: "class Text(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return row.title\n    def build(self):\n        items = E.query().take(2).select(\"title\")\n        hdr = self.text(items)\n        return E.CREATE(title=hdr.content, score=0, owner=\"plural-content\")\n", existing: Some("lang_render_content_plural_reject"), expect_live_error: Some("not a singleton") },
    Case { id: "render_parity_lang_render_name_collision", plasm: "", python: "class Text(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return row.title\n    def build(self):\n        title = E.get(\"i1\").take(1)\n        items = E.query().take(2)\n        return self.text(items)\n", existing: Some("lang_render_name_collision"), expect_live_error: Some("both a row field and a program binding") },
];

pub(super) fn python_outcome(id: &str) -> super::python::PythonOutcome {
    use super::python::PythonOutcome::*;
    match id {
        "render_parity_lang_render_value_error_at_execution" => LiveError("IndexError"),
        "render_parity_lang_render_undefined_field" => CompileError("unknown value field"),
        "render_parity_lang_render_content_plural_reject" => CompileError("proven singleton"),
        "render_parity_lang_render_name_collision" => ExplicitQualification,
        _ => Equivalent,
    }
}

pub(super) fn assert_replacement(
    case: &Case,
    run: &plasm_agent::plasm_plan_run::PlasmPlanRunResult,
) {
    assert_eq!(case.existing, Some("lang_render_name_collision"));
    let rows = run
        .return_steps
        .iter()
        .flat_map(|step| step.result.entities.iter())
        .map(|entity| plasm_runtime::entity_to_agent_row_json(entity, None))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            serde_json::json!({"id":"synthetic-1", "content":"Alpha"}),
            serde_json::json!({"id":"synthetic-2", "content":"Beta"})
        ]
    );
}

#[test]
fn python_projection_and_write_interpolation_preserve_admission_boundaries() {
    use plasm_core::symbol_tuning::SymbolRender;
    let es = super::language_matrix::matrix_execute_session(
        super::language_matrix::load_language_matrix_cgs(),
    );
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entry = super::language_matrix::MATRIX_ENTRY_ID;
    let entity = symbols.entity_sym_for(entry, "LangItem");
    let update = symbols.method_sym_for(entry, "LangItem", "update");
    for body in [
        "return E.query().select(x=lambda row: other.score * 2)",
        "return E.query().select(x=lambda row: row.absent * 2)",
        "return E.query().select(x=lambda row, other: row.score)",
        "return E.query().select(x=lambda row: len(7))",
        "return E.query().select(x=lambda row: row.title if row.score < 2 < 3 else row.owner)",
        "return E.query().flat_map(lambda row: row.UPDATE(title=f'{row.score}'))",
        "return E.query().flat_map(lambda row: row.UPDATE(title=f'{row.active}'))",
        "return E.query().flat_map(lambda row: row.UPDATE(title=f'{row.title!r}'))",
        "return E.query().flat_map(lambda row: row.UPDATE(title=f'{row.title:>20}'))",
        "return E.query().flat_map(lambda row: row.UPDATE(title=f'{other.title}'))",
    ] {
        let body = body
            .replace("E.", &format!("{entity}."))
            .replace("UPDATE", &update);
        let source = format!("class Invalid(Program):\n    def build(self):\n        {body}\n");
        assert!(
            plasm_agent::plasm_compile::compile_python_program(&es, &source).is_err(),
            "admitted {source}"
        );
    }
}

#[test]
fn python_compute_after_fanout_preserves_output_ownership_and_projection() {
    use plasm_core::symbol_tuning::SymbolRender;
    let es = super::language_matrix::matrix_execute_session(
        super::language_matrix::load_language_matrix_cgs(),
    );
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entry = super::language_matrix::MATRIX_ENTRY_ID;
    let item = symbols.entity_sym_for(entry, "LangItem");
    let line = symbols.entity_sym_for(entry, "LangLine");
    let update = symbols.method_sym_for(entry, "LangItem", "update");
    for (annotation, projection, expected_error) in [
        (item.as_str(), ".select(\"id\")", "projection"),
        (line.as_str(), "", "annotation does not match source entity"),
    ] {
        let source = format!("class Invalid(Program):\n    @compute\n    def text(self, rows: list[Value[{annotation}]]) -> str:\n        return ''.join(row.id for row in rows)\n    def build(self):\n        done = {item}.query().flat_map(lambda row: row.{update}(title=row.title, score=row.score, owner=row.owner)){projection}\n        return self.text(done)\n");
        // Use a consumed field excluded by the projection, while both entities
        // expose id for the independent catalog/entity ownership check.
        let source = if projection.is_empty() {
            source
        } else {
            source.replace("row.id for", "row.title for")
        };
        let error = plasm_agent::plasm_compile::compile_python_program(&es, &source)
            .expect_err("fanout must not erase its output schema");
        assert!(error.contains(expected_error), "{error}");
    }
}
