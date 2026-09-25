use super::*;
use crate::plasm_compile::compile_python_program;

#[test]
fn python_teaching_library_and_domain_card_are_parseable_declarations() {
    use plasm_core::prompt_render::python::{
        prepare_python_teaching_wave, PythonTeachingState, LANGUAGE,
    };
    on_runtime(async {
        let (es, _, _) = fixture(1);
        let wave = prepare_python_teaching_wave(
            es.teaching_exposure.as_ref().unwrap(),
            &PythonTeachingState::default(),
        )
        .unwrap();
        let library = LANGUAGE
            .split_once("```pyi\n")
            .unwrap()
            .1
            .split_once("```")
            .unwrap()
            .0;
        ruff_python_parser::parse_module(library).unwrap();
        ruff_python_parser::parse_module(&wave.declarations).unwrap();
        assert!(
            compile_python_program(&es, &wave.declarations).is_err(),
            "declarations must not be confused with an executable root program"
        );
    });
}

fn source(es: &ExecuteSession) -> String {
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let item = symbols.entity_sym_for("fixture", "Item");
    let tag = symbols.entity_sym_for("fixture", "Tag");
    let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/schemas/python_dag_slice/program.py"
    ))
    .replace("e1", &item)
    .replace("e2", &tag)
    .replace("r1", &relation)
}
#[test]
fn python_lowering_parent_source_executes_with_real_occurrences() {
    on_runtime(async {
        for parents in [0, 1, 3] {
            let (es, host, calls) = fixture(parents);
            let bundle = compile_python_program(&es, &source(&es)).unwrap();
            assert!(
                calls.lock().unwrap().is_empty(),
                "compilation must not perform reads"
            );
            let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            assert!(dry.review.has_full_collection_compute);
            let run = execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(rows(&run),(0..parents).map(|i|json!({"title":format!("Title {i}"),"labels":(0..i).map(|j|format!("L{j}")).collect::<Vec<_>>().join("|")})).collect::<Vec<_>>());
            assert_eq!(calls.lock().unwrap().len(), 1 + parents);
            assert_eq!(
                run.graph_summary["scope_instances"][0]["completed"],
                parents
            );
        }
    });
}

#[test]
fn python_lowering_preserves_nested_record_and_array_outputs() {
    on_runtime(async {
        let (es, host, _) = fixture(1);
        let code = source(&es).replace("\"title\": item.title", "\"nested\": {\"title\": item.title, \"ids\": [item.id], \"empty\": [], \"flag\": True, \"amount\": 1.25}");
        assert_ne!(code, source(&es));
        let bundle = compile_python_program(&es, &code).unwrap();
        evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let actual: Vec<_> = run.return_steps[0]
            .result
            .entities
            .iter()
            .map(|e| {
                let row = plasm_runtime::entity_to_agent_row_json(e, None);
                json!({"nested": row["nested"], "labels": row["labels"]})
            })
            .collect();
        assert_eq!(
            actual,
            vec![
                json!({"nested": {"title": "Title 0", "ids": ["i0"], "empty": [], "flag": true, "amount": 1.25}, "labels": ""})
            ]
        );
        let body = bundle
            .artifact()
            .comp
            .steps
            .values()
            .find_map(|step| {
                if let PlasmStepPayload::MapBody(body) = step {
                    Some(body)
                } else {
                    None
                }
            })
            .unwrap();
        let schema = crate::map_body_schema::output_schema(&es, body).unwrap();
        let nested = schema
            .fields
            .iter()
            .find(|f| f.name.as_str() == "nested")
            .unwrap()
            .value_type
            .as_ref()
            .unwrap();
        let plasm_core::value_contract::ValueShape::Record { fields } = &nested.shape else {
            panic!("lost nested shape");
        };
        let plasm_core::value_contract::ValueShape::Array { element } = &fields["ids"].shape else {
            panic!("lost array element");
        };
        assert_eq!(
            element.domain.as_ref().unwrap().value_ref.as_str(),
            "item_id"
        );
    });
}
#[test]
fn python_lowering_parent_bound_is_execution_failure() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let bundle = compile_python_program(
            &es,
            &source(&es).replace("max_parents=256", "max_parents=2"),
        )
        .unwrap();
        let error = execute(&es, &host, &bundle).await.unwrap_err();
        assert!(error.contains("parent budget exceeded"), "{error}");
        assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
    });
}
#[test]
fn python_lowering_rejects_invalid_roots_and_captures_without_io() {
    on_runtime(async {
        let (es, _, calls) = fixture(3);
        let valid = source(&es);
        for (case,src) in [
            ("no subclass", "def build(self):\n    return 1".into()),
            ("inheritance",valid.replace("(Program)","(Program, Other)")),
            ("state",valid.replace("        items =", "        self.cache =")),
            ("no return",valid.replace("        return items.map(","        result = items.map(")),
            ("missing method",valid.replace("self.joined_labels(item.","self.absent(item.")),
            ("unknown field",valid.replace("item.title","item.absent")),
            ("capture escape",valid.replace("self.joined_labels(item.","self.joined_labels(other.")),
            ("zero bound",valid.replace("max_parents=256","max_parents=0")),
            ("huge bound",valid.replace("max_parents=256","max_parents=257")),
            ("unused bad compute",valid.replace("    def build(self):","    @compute\n    def bad(self, rows):\n        return self.secret\n\n    def build(self):")),
            ("after return",format!("{valid}        items = e1.query()\n")),
            ("import",format!("import os\n{valid}")),
            ("dynamic control",valid.replace("        items = e1.query()","        if True:\n            items = e1.query()")),
        ] {
            assert!(compile_python_program(&es,&src).is_err(),"accepted {case}");
            assert!(calls.lock().unwrap().is_empty());
        }
    });
}

#[test]
fn python_lowering_source_spans_and_class_name_are_nonsemantic() {
    on_runtime(async {
        let (es, _, _) = fixture(0);
        let code = source(&es);
        let original = compile_python_program(&es, &code).unwrap();
        let renamed =
            compile_python_program(&es, &code.replace("ExportItems", "RenamedExport")).unwrap();
        assert!(plasm_core::plasm_monad::comp_semantic_eq(
            &original.artifact().comp,
            &renamed.artifact().comp
        ));
        let spans = &original.artifact().comp.metadata["python_source_spans"];
        assert!(spans["items"]["start"].as_u64().is_some());
        let body = original
            .artifact()
            .comp
            .steps
            .values()
            .find_map(|s| {
                if let PlasmStepPayload::MapBody(b) = s {
                    Some(b)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(
            body.body.metadata["python_source_spans"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
        let changed =
            compile_python_program(&es, &code.replace("max_parents=256", "max_parents=255"))
                .unwrap();
        assert!(!plasm_core::plasm_monad::comp_semantic_eq(
            &original.artifact().comp,
            &changed.artifact().comp
        ));
    });
}
#[test]
fn python_lowering_repeated_helper_calls_keep_distinct_nodes() {
    on_runtime(async {
        let (es, host, _) = fixture(1);
        let code = source(&es);
        let line = code.lines().find(|l| l.contains("\"labels\":")).unwrap();
        let code = code.replace(
            line,
            &format!("{line}\n{}", line.replace("\"labels\"", "\"again\"")),
        );
        let bundle = compile_python_program(&es, &code).unwrap();
        let body = bundle
            .artifact()
            .comp
            .steps
            .values()
            .find_map(|s| {
                if let PlasmStepPayload::MapBody(b) = s {
                    Some(b)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(
            body.body
                .steps
                .values()
                .filter(|s| matches!(s, PlasmStepPayload::Map(_)))
                .count(),
            2
        );
        execute(&es, &host, &bundle).await.unwrap();
    });
}

#[test]
fn python_lowering_map_waits_for_unreturned_root_write() {
    on_runtime(async {
        let (es, host, calls) = fixture(2);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let touch = symbols.method_sym_for("fixture", "Item", "touch");
        let source = source(&es).replace(
            "        return items.map(",
            &format!("        {item}.{touch}()\n        return items.map("),
        );
        let bundle = compile_python_program(&es, &source).unwrap();
        let comp = &bundle.artifact().comp;
        let (map_id, _) = comp
            .steps
            .iter()
            .find(|(_, s)| matches!(s, PlasmStepPayload::MapBody(_)))
            .unwrap();
        let effect_id = comp
            .bind
            .topo
            .iter()
            .find(|id| id.as_str() != "items" && id.as_str() != map_id)
            .unwrap();
        assert!(comp.bind.deps[&id(map_id)].contains(effect_id));
        execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["/items", "/touch", "/items/i0/tags", "/items/i1/tags"]
        );
    });
}
