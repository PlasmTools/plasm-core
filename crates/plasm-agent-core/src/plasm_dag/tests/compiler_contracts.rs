use super::*;

#[test]
fn compiler_contract_relation_arguments_preserve_receiver() {
    let session = test_session();
    for program in [
        "LangItem(\"i1\").tags_by_score{item_id=\"i1\"}",
        "LangItem(\"i1\").tags_by_score{}",
        "items = LangItem\ntags = items => LangItem(_.id).tags_by_score{item_id=\"i1\"}\ntags",
        "items = LangItem\ntags = items => _.tags_by_score{item_id=\"i1\"}\ntags",
    ] {
        let error = crate::compile_plasm_program(&PromptPipelineConfig::default(), None, &session, "relation-contract", program).expect_err("relation braces are not a catalog source");
        assert!(error.to_string().contains("does not accept query braces"), "{program}: {error}");
    }
}

#[test]
fn compiler_contract_computed_fields_reject_invalid_or_unknown_operands() {
    let session = test_session();
    for expression in ["(title | split_part(\"/\") | last)", "missing_field + 1", "len(missing_field)", "when(score > 0, title, missing_field)"] {
        let program = format!("items = LangItem\nout = items | select id, computed = {expression}\nout");
        let result = crate::compile_plasm_program(&PromptPipelineConfig::default(), None, &session, "computed-contract", &program);
        assert!(result.is_err(), "invalid computed operand was accepted: {expression}");
    }
}
