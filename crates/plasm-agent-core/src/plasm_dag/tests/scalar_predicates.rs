use super::*;

#[test]
fn scalar_predicate_reference_retains_binding_dependency() {
    for (field, rhs) in [("owner", "one.owner"), ("owner", "\"{{ one.owner }}\""), ("contact_email", "one.contact_email"), ("score", "one.score")] {
        let session = test_session();
        let program = format!("one = LangItem(\"i1\")\nselected = LangItem | where {field} = {rhs}\nselected");
        let bundle = crate::compile_plasm_program(
            &PromptPipelineConfig::default(), None, &session, "scalar-predicate", &program,
        ).expect("compile scalar comparison");
        let wire = serde_json::to_value(&bundle.artifact().comp).unwrap();
        let selected = &wire["steps"]["selected"];
        assert!(wire["bind"]["deps"]["selected"].as_array().unwrap().contains(&json!("one")),
            "comparison {rhs} erased its dependency: {selected}");
        assert!(!selected.to_string().contains("__plasm_string_template"),
            "template must not be a literal marker object");
    }
}

#[test]
fn scalar_predicate_rejects_plural_and_preserves_quoted_literal() {
    let session = test_session();
    let config = PromptPipelineConfig::default();
    let err = crate::compile_plasm_program(&config, None, &session, "scalar", "many = LangItem\nLangItem | where owner = many.owner").unwrap_err();
    assert!(err.to_string().contains("plural"), "{err}");
    let literal = crate::compile_plasm_program(&config, None, &session, "scalar", "one = LangItem(\"i1\")\nselected = LangItem | where owner = \"one.owner\"\nselected");
    // Quoted binding-shaped text is always data (PLP-11).
    let wire = serde_json::to_value(&literal.unwrap().artifact().comp).unwrap();
    assert!(!wire["bind"]["deps"]["selected"].as_array().unwrap().contains(&json!("one")));
}

#[test]
fn until_scalar_operand_retains_its_dependency() {
    let session = test_session();
    let program = "expected = LangItem(\"i1\")\ncur = LangItem(\"i2\")\ndone = iterate cur step LangItem(_.id).ping() until title = expected.title take 3\ndone";
    let bundle = crate::compile_plasm_program(&PromptPipelineConfig::default(), None, &session, "scalar-until", program).expect("compile typed until");
    let wire = serde_json::to_value(&bundle.artifact().comp).unwrap();
    assert!(wire["bind"]["deps"]["done"].as_array().unwrap().contains(&json!("expected")));
}
