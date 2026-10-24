use super::*;
use plasm_core::symbol_tuning::SymbolRender;

#[tokio::test]
async fn compact_review_keeps_all_folded_return_roots() {
    let es = session();
    let bundle = crate::plasm_compile::compile_python_program(&es, "class Review(Program):\n    def build(self):\n        return (\"alpha\", \"beta\", \"gamma\", \"delta\")\n").await.unwrap();
    let dry = super::super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let text = super::super::super::render_plasm_plan_dry_text_for_session(&dry, None, Some(&es));
    for value in ["alpha", "beta", "gamma", "delta"] {
        assert!(text.contains(value), "{text}");
    }
}

#[tokio::test]
async fn compact_review_keeps_authored_tuple_bindings_and_verification() {
    let es = session();
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let counter = symbols.entity_sym_for("matrix", "Counter");
    let advance = symbols.method_sym_for("matrix", "Counter", "advance");
    for literal in ["None", "\"\""] {
        let program = format!("class Review(Program):\n    @compute\n    def verify(self, first: str, second: str, note: str | None) -> tuple[str, str]:\n        if first != second:\n            raise ValueError(\"mismatch\")\n        return (first, second)\n    def build(self):\n        observed = {counter}.get(\"one\")\n        left, right = self.verify(observed.id, observed.id, {literal})\n        changed = {counter}.{advance}(id=left)\n        fresh = {counter}.get(right)\n        return {{\"changed\": changed, \"fresh\": fresh}}\n");
        let bundle = crate::plasm_compile::compile_python_program(&es, &program)
            .await
            .unwrap();
        let before = serde_json::to_value(&bundle.artifact().comp).unwrap();
        let mut without_names = bundle.artifact().comp.clone();
        without_names.metadata.remove("python_binding_names");
        assert_eq!(
            crate::plasm_comp_wire::plasm_comp_commit_canonical(&bundle.artifact().comp),
            crate::plasm_comp_wire::plasm_comp_commit_canonical(&without_names),
            "display aliases must not change reviewed execution identity"
        );
        let dry = super::super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let view = super::super::super::plan_dry_compact_view(&dry, Some(&es));
        let text = crate::plan_dry_display::render_plan_dry_compact_text(&view, None);
        assert_eq!(
            before,
            serde_json::to_value(&bundle.artifact().comp).unwrap(),
            "review cannot change execution"
        );
        assert!(text.contains("left"), "{text}");
        assert!(text.contains("right"), "{text}");
        assert!(text.contains("compute value"), "{text}");
        assert!(
            text.contains(if literal == "None" { "null" } else { "\"\"" }),
            "literal compute arguments must survive folding: {text}"
        );
        assert!(text.contains("id=left"), "{text}");
        assert!(text.contains("fresh: get Counter(right)"), "{text}");
        assert!(text.contains("Advances the counter by one."), "{text}");
        assert!(
            view.steps.len() < view.node_count,
            "structural packets should fold: {text}"
        );
        assert!(
            !text
                .lines()
                .any(|l| l.starts_with("value_") && l.ends_with("{0}")),
            "{text}"
        );
        let changed = view.steps.iter().find(|s| s.id == "changed").unwrap();
        assert!(changed.uses.iter().any(|s| s == "left"), "{text}");
        let mut unfolded = view.clone();
        unfolded.steps = crate::plan_dry_display::unfolded_review_steps(
            dry.validated_plan(),
            &dry.topological_order,
            Some(&es),
        );
        let expanded = crate::plan_dry_display::render_plan_dry_compact_text(&unfolded, None);
        println!(
            "REVIEW_MEASUREMENT {}",
            serde_json::json!({"case": literal, "before": expanded, "after": text})
        );
    }
}

#[tokio::test]
async fn compact_review_scales_without_folding_executable_verification() {
    let es = session();
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let counter = symbols.entity_sym_for("matrix", "Counter");
    let advance = symbols.method_sym_for("matrix", "Counter", "advance");
    for count in [1, 4, 8] {
        let mut program = format!("class Review(Program):\n    @compute\n    def verify(self, identity: str, expected: str) -> str:\n        if identity != expected:\n            raise ValueError(\"mismatch\")\n        return identity\n    def build(self):\n        observed = {counter}.get(\"one\")\n");
        let mut previous = "observed.id".to_owned();
        for i in 0..count {
            program.push_str(&format!(
                "        checked_{i} = self.verify({previous}, \"one\")\n"
            ));
            previous = format!("checked_{i}");
        }
        program.push_str(&format!("        changed = {counter}.{advance}(id={previous})\n        fresh = {counter}.get(\"one\")\n        return {{\"changed\": changed, \"fresh\": fresh}}\n"));
        let bundle = crate::plasm_compile::compile_python_program(&es, &program)
            .await
            .unwrap();
        let dry = super::super::super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let view = super::super::super::plan_dry_compact_view(&dry, Some(&es));
        let text = crate::plan_dry_display::render_plan_dry_compact_text(&view, None);
        assert_eq!(text.matches("compute value").count(), count);
        for i in 0..count {
            assert!(text.contains(&format!("checked_{i}:")), "{text}");
        }
        assert!(text.contains(&format!("id={previous}")), "{text}");
        assert!(text.contains("Advances the counter by one."), "{text}");
        let mut unfolded = view.clone();
        unfolded.steps = crate::plan_dry_display::unfolded_review_steps(
            dry.validated_plan(),
            &dry.topological_order,
            Some(&es),
        );
        let expanded = crate::plan_dry_display::render_plan_dry_compact_text(&unfolded, None);
        assert!(text.len() < expanded.len(), "{text}");
        println!(
            "REVIEW_MEASUREMENT {}",
            serde_json::json!({"case": format!("checks_{count}"), "before": expanded, "after": text})
        );
    }
}
