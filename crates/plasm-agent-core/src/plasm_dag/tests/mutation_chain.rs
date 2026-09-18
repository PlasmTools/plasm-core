//! Bind-ordered mutations use the identity returned by an earlier write.
use super::*;

#[test]
fn create_then_update_chained_in_one_program_compiles() {
    let session = language_matrix_tags_session();
    let source = r#"created = LangItem.create(title="initial", score=1, owner="bot")
updated = LangItem(created.id).update(title="approved")
created, updated"#;
    let plan = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(), None, &session, "mutation-chain", source,
    ).expect("chained writes compile");
    let dry = evaluate_plasm_plan_dry(&session, &plan).expect("dry-run");
    assert!(matches!(dry.flow.verdict, crate::plan_flow::FlowVerdict::Clean));
    let updated = plan["nodes"].as_array().unwrap().iter().find(|node| node["id"] == "updated").unwrap();
    assert!(updated["uses_result"].as_array().unwrap().iter().any(|input| input["node"] == "created"));
}
