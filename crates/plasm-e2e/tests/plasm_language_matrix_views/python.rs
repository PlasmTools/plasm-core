//! Python-only view conformance programs.
use plasm_agent::execute_session::ExecuteSession;
use plasm_agent::plasm_compile::PlasmCompBundle;
use plasm_core::symbol_tuning::SymbolRender;
pub(super) async fn compile_views_program(
    es: &ExecuteSession,
    body: &str,
) -> Result<PlasmCompBundle, String> {
    let source = format!(
        "class Views(Program):\n    def build(self):\n{}\n",
        body.lines()
            .map(|line| format!("        {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    compile(es, source).await
}
pub(super) async fn render(
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
    compile(es, format!("class Views(Program):\n    @compute\n    def text(self, {parameter}: {annotation}) -> str:\n        return {expression}\n    def build(self):\n        items = LangItem.get(\"i1\").select({fields})\n        report = self.text(items)\n        return report\n")).await
}

async fn compile(es: &ExecuteSession, mut source: String) -> Result<PlasmCompBundle, String> {
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
        .await
        .map_err(|error| format!("{error}\nPython source:\n{source}"))
}

#[tokio::test]
async fn matrix_views_python_teaches_nullary_get_without_fake_identity() {
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
        .await
        .unwrap();
        assert!(compile(&es, format!("class Snapshot(Program):\n    def build(self):\n        return {entity}.get(\"invented-identity\")\n")).await.is_err());
    }
}
