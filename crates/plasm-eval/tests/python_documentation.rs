//! Published programs must compile through the production frontend, not a shape-only parser.
use plasm_eval::ProgramSession;

#[test]
fn published_language_definition_compiles_against_the_semantic_matrix() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cgs =
        plasm_core::load_schema_dir(&root.join("fixtures/schemas/plasm_language_matrix")).unwrap();
    let session = ProgramSession::new(&cgs, Some("LangItem")).unwrap();
    let path = root.join("doc-site/docs/reference/plasm-language-definition.md");
    let doc = std::fs::read_to_string(path).unwrap();
    assert!(
        !doc.contains("```plasm"),
        "retired grammar must not be taught"
    );
    let mut count = 0;
    for section in doc.split("```python\n").skip(1) {
        let source = section.split_once("```").expect("closed Python fence").0;
        session
            .compile(source)
            .unwrap_or_else(|error| panic!("{}\n{source}", error.agent_markdown()));
        count += 1;
    }
    assert!(
        count > 0,
        "the definition must teach an executable Python Program"
    );
}
