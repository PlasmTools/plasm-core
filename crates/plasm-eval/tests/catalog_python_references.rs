//! Catalog integration evidence: every retained reference uses the production Python frontend.
use plasm_eval::ProgramSession;

#[test]
fn published_catalog_references_compile_as_python() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut failures = Vec::new();
    let mut count = 0;
    for catalog in ["github", "linear", "proof"] {
        let dir = root.join("apis").join(catalog);
        let cgs = plasm_core::load_schema_dir(&dir).unwrap();
        let session = ProgramSession::new(&cgs, None).unwrap();
        let cases: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(dir.join("eval/cases.yaml")).unwrap())
                .unwrap();
        for case in cases.as_sequence().unwrap() {
            let Some(source) = case["reference_expr"].as_str() else {
                continue;
            };
            count += 1;
            if let Err(error) = session.compile(source) {
                failures.push(format!(
                    "{}: {}",
                    case["id"].as_str().unwrap(),
                    error.agent_markdown()
                ));
            }
        }
    }
    assert_eq!(count, 31, "retain all reference cases");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
