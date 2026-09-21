//! Review is a projection of executable structure, including after wire admission.
use super::*;
use proptest::prelude::*;

#[test]
fn application_suffix_rejects_with_selection_preserving_repair() {
    let session = test_session();
    let error = compile_plasm_dag_to_plan(
        &PromptPipelineConfig::default(), None, &session, "suffix",
        "items = LangItem\nselected = items => LangItem(_.id) | where score > 2\nselected",
    ).expect_err("a suffix must never disappear from execution");
    assert!(error.contains("final stage"), "{error}");
    assert!(error.contains("result = applied | where score > 2"), "{error}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn review_ignores_non_executable_provenance_after_serialization(
        threshold in 0i32..1000,
        false_description in "[a-zA-Z0-9 |_.=]{1,80}",
    ) {
        let session = test_session();
        let program = format!("items = LangItem\nreads = items => LangItem(_.id)\nselected = reads | where score > {threshold} | order by title | take 2\nselected");
        let original = compile_plasm_dag_to_plan(
            &PromptPipelineConfig::default(), None, &session, "review-law", &program,
        ).expect("compile valid separated application and selection");
        let reads = original["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "reads").unwrap();
        prop_assert_eq!(&reads["effect_class"], &json!("read"));
        prop_assert_eq!(&reads["result_shape"], &json!("list"));
        prop_assert_eq!(&reads["effect_template"]["result_shape"], &json!("single"));
        let mut poisoned = original.clone();
        for node in poisoned["nodes"].as_array_mut().unwrap() {
            if node.get("expr").is_some() { node["expr"] = json!(format!("source provenance: {false_description}")); }
            for pointer in ["/ir", "/ir_template", "/effect_template/ir_template", "/relation/ir"] {
                if let Some(ir) = node.pointer_mut(pointer) {
                    ir["display_expr"] = json!(format!("source provenance: {false_description}"));
                }
            }
            if let Some(effect) = node.get_mut("effect_template") {
                effect["expr_template"] = json!(format!("source provenance: {false_description}"));
            }
        }
        let admitted = |wire: &serde_json::Value| {
            let bytes = serde_json::to_vec(wire).unwrap();
            let decoded = serde_json::from_slice(&bytes).unwrap();
            let plan = crate::plasm_plan::parse_plan_value(&decoded).unwrap();
            crate::plasm_plan::validate_plan_artifact(&plan).unwrap()
        };
        let expected = admitted(&original);
        let actual = admitted(&poisoned);
        let operations = |plan: &crate::plasm_plan::ValidatedPlanArtifact| {
            plan.nodes().iter().map(crate::plasm_plan_run::render_node_operation).collect::<Vec<_>>()
        };
        prop_assert_eq!(operations(&actual), operations(&expected));
        let dry_original = evaluate_plasm_plan_dry(&session, &original).expect("dry original");
        let dry_poisoned = evaluate_plasm_plan_dry(&session, &poisoned).expect("dry poisoned");
        prop_assert_eq!(render_plasm_plan_dry_text(&dry_poisoned, None), render_plasm_plan_dry_text(&dry_original, None));
        prop_assert_eq!(
            crate::plasm_plan_run::render_plasm_plan_dry_text_for_session(&dry_poisoned, None, Some(&session)),
            crate::plasm_plan_run::render_plasm_plan_dry_text_for_session(&dry_original, None, Some(&session)),
        );
    }
}
