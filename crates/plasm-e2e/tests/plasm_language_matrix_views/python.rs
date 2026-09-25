//! Explicit Python counterparts; the original suite runs its assertions on both frontends.
use plasm_agent::execute_session::ExecuteSession;
use plasm_agent::plasm_compile::PlasmCompBundle;
use plasm_core::symbol_tuning::SymbolRender;

pub(super) fn compile_views_program(
    python: bool,
    es: &ExecuteSession,
    label: &str,
    native: &str,
) -> Result<PlasmCompBundle, String> {
    if !python {
        return plasm_agent::plasm_compile::compile_plasm_program(
            &Default::default(),
            None,
            es,
            label,
            native,
        )
        .map_err(String::from);
    }
    let body: String = match label {
        "matrix_views_wire_body_render" => return render(es, false, "row", "id, title", "f\"- {row.id}: {row.title}\\n\""),
        "matrix_views_alias_render" => return render(es, true, "r", "id, title, score", "f\"- {r.id}: {r.title} (score: {r.score or '—'})\\n\""),
        "matrix_views_named_cursor_render" => return render(es, true, "entry", "id, title", "f\"- {entry.id}: {entry.title or '—'}\\n\""),
        "matrix_views_view_embed_tags" => "return LangTriageContext.get(\"i1\").tags".into(),
        "matrix_views_empty_tags_dry" => "return LangTriageContext.get(\"i2\").tags".into(),
        "matrix_views_work_snapshot_items" => "return LangWorkSnapshot.get().items".into(),
        "matrix_views_work_snapshot_empty_items" => "return LangWorkSnapshotEmpty.get().items".into(),
        "view_parent_fanout" => match native {
            "parent = LangTriageContext{item_id=\"i1\"}\ntags = parent => _.tags\ntags" => "parent = LangTriageContext.query(item_id=\"i1\")\ntags = parent.flat_map(lambda row: row.tags)\nreturn tags",
            "parent = LangTriageContext{item_id=\"i1\"}\ntags = parent.tags\ntags" => "parent = LangTriageContext.query(item_id=\"i1\")\ntags = parent.tags\nreturn tags",
            "parent = LangTriageContext{item_id=\"i1\"} | take 1\ntags = parent => _.tags\ntags" => "parent = LangTriageContext.query(item_id=\"i1\").take(1)\ntags = parent.flat_map(lambda row: row.tags)\nreturn tags",
            _ => panic!("unmapped original view program: {native}"),
        }.into(),
        "rowset_view" => match native {
            "items = Library{access_token=\"test-token\"}.items\nitems" => "items = Library.query(access_token=\"test-token\").items\nreturn items",
            "library = Library{access_token=\"test-token\"}\nitems = library.items\nitems" => "library = Library.query(access_token=\"test-token\")\nitems = library.items\nreturn items",
            _ => panic!("unmapped original view program: {native}"),
        }.into(),
        "query_only_view" => match native {
            "library = Library{access_token=\"test-token\"}\nitems = library => _.items\nitems" => "library = Library.query(access_token=\"test-token\")\nitems = library.flat_map(lambda row: row.items)\nreturn items",
            "library = Library{access_token=\"test-token\"}\nitems = library.items\nitems" => "library = Library.query(access_token=\"test-token\")\nitems = library.items\nreturn items",
            "library = Library{access_token=\"test-token\"} | take 1\nitems = library => _.items\nitems" => "library = Library.query(access_token=\"test-token\").take(1)\nitems = library.flat_map(lambda row: row.items)\nreturn items",
            "library = Library{access_token=\"test-token\"} | take 1\nitems = library.items\nitems" => "library = Library.query(access_token=\"test-token\").take(1)\nitems = library.items\nreturn items",
            "library = Library{access_token=\"test-token\"}\ncopy = library | select access_token\nitems = copy => _.items\nitems" => "library = Library.query(access_token=\"test-token\")\ncopy = library.select(\"access_token\")\nitems = copy.flat_map(lambda row: row.items)\nreturn items",
            "library = Library{access_token=\"test-token\"} | select access_token\nitems = library => _.items\nitems" => "library = Library.query(access_token=\"test-token\").select(\"access_token\")\nitems = library.flat_map(lambda row: row.items)\nreturn items",
            _ => panic!("unmapped original view program: {native}"),
        }.into(),
        "empty_query_view" => "library = Library.query(access_token=\"test-token\").where(lambda row: row.access_token == \"absent\")\nitems = library.flat_map(lambda row: row.items)\nreturn items".into(),
        "empty_singleton_view" => "library = Library.query(access_token=\"test-token\").where(lambda row: row.access_token == \"absent\").take(1)\nitems = library.items\nreturn items".into(),
        _ => panic!("missing Python counterpart for original views obligation {label}"),
    };
    let source = format!(
        "class Views(Program):\n    def build(self):\n{}\n",
        body.lines()
            .map(|line| format!("        {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    compile(es, source)
}

fn render(
    es: &ExecuteSession,
    collection: bool,
    variable: &str,
    fields: &str,
    expression: &str,
) -> Result<PlasmCompBundle, String> {
    let annotation = if collection { "list[Row]" } else { "Row" };
    let expression = if collection {
        format!("''.join({expression} for {variable} in rows)")
    } else {
        expression.into()
    };
    let parameter = if collection { "rows" } else { variable };
    let fields = fields
        .split(", ")
        .map(|field| format!("\"{field}\""))
        .collect::<Vec<_>>()
        .join(", ");
    compile(es, format!("class Views(Program):\n    @compute\n    def text(self, {parameter}: {annotation}) -> str:\n        return {expression}\n    def build(self):\n        items = LangItem.get(\"i1\").select({fields})\n        report = self.text(items)\n        return report\n"))
}

fn compile(es: &ExecuteSession, mut source: String) -> Result<PlasmCompBundle, String> {
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    for entity in [
        "LangItem",
        "LangTriageContext",
        "Library",
        "LangWorkSnapshot",
        "LangWorkSnapshotEmpty",
    ] {
        if source.contains(&format!("{entity}.")) {
            let entry = es.entry_id.as_str();
            source = source.replace(
                &format!("{entity}."),
                &format!("{}.", symbols.entity_sym_for(entry, entity)),
            );
        }
    }
    plasm_agent::plasm_compile::compile_python_program(es, &source)
        .map_err(|error| format!("{error}\nPython source:\n{source}"))
}

#[test]
fn matrix_views_python_teaches_nullary_get_without_fake_identity() {
    let es = super::views_execute_session(super::load_language_matrix_views_cgs());
    let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
        es.teaching_exposure.as_ref().unwrap(),
        &Default::default(),
    )
    .unwrap();
    for capability in ["lang_work_snapshot_get", "lang_work_snapshot_empty_get"] {
        let signature = wave
            .capabilities
            .iter()
            .find(|cap| cap.capability == capability)
            .unwrap()
            .signature
            .as_ref()
            .unwrap();
        assert!(signature.contains("def get(cls)"), "{signature}");
    }
    for entity in ["LangWorkSnapshot", "LangWorkSnapshotEmpty"] {
        compile(
            &es,
            format!(
                "class Snapshot(Program):\n    def build(self):\n        return {entity}.get()\n"
            ),
        )
        .unwrap();
        assert!(compile(&es, format!("class Snapshot(Program):\n    def build(self):\n        return {entity}.get(\"invented-identity\")\n")).is_err());
    }
}
