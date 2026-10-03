use super::*;
use crate::plasm_compile::compile_python_program;

#[test]
fn declared_compute_requires_a_lowered_callsite() {
    on_runtime(async {
        let (es, _, calls) = fixture(1);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let error = compile_python_program(
            &es,
            &format!("class Unused(Program):\n    @compute\n    def unused(self, row: Row) -> str:\n        return row.title\n    def build(self):\n        return {item}.get(\"i1\")\n"),
        )
        .await
        .expect_err("an unused declaration has no input contract witness");
        assert!(error.to_string().contains("requires a typed DAG callsite"));
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn discarded_statement_requires_resolved_write_evidence() {
    on_runtime(async {
        let (es, _, calls) = fixture(1);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let write = format!(
            "class Emit(Program):\n    def build(self):\n        {item}.{publish}(content=\"ok\")\n        return {item}.query()\n"
        );
        compile_python_program(&es, &write)
            .await
            .expect("a resolved write may be an expression statement");
        let pure = format!(
            "class Observe(Program):\n    def build(self):\n        {item}.query().select(\"title\")\n        return {item}.query()\n"
        );
        let error = compile_python_program(&es, &pure)
            .await
            .expect_err("discarding a pure rowset is not an effect");
        assert!(error
            .to_string()
            .contains("unused expression statements must be writes"));
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn python_compute_result_is_a_value_without_a_content_wrapper() {
    on_runtime(async {
        let (es, _, calls) = fixture(1);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let source = format!(
            "class Publish(Program):\n    @compute\n    def render(self, rows: list[Row]) -> str:\n        return 'ok'\n    def build(self):\n        text = self.render({item}.query().select(\"title\"))\n        return {item}.{publish}(content=text)\n"
        );
        compile_python_program(&es, &source)
            .await
            .expect("singleton compute values admit implicit scalar extraction");
        compile_python_program(
            &es,
            &source.replace("content=text)", "content=text.content)"),
        )
        .await
        .expect_err("str has no content attribute");
        assert!(
            calls.lock().unwrap().is_empty(),
            "admission must perform no IO"
        );
    });
}

#[test]
fn python_content_is_an_ordinary_record_field() {
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let source = r#"class Ordinary(Program):
    @compute
    def identity(self, row: Row) -> Row:
        return row
    def build(self):
        content = self.identity({"content": "ordinary", "value": 7})
        return {"content": content.content, "number": content.value, "whole": content}
"#;
        let bundle = compile_python_program(&es, source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"content":"ordinary", "number":7, "whole":{"content":"ordinary", "value":7}})
        );
        assert!(calls.lock().unwrap().is_empty());
        let mut artifact = bundle.artifact().clone();
        let mut wire = serde_json::to_value(&artifact.comp).unwrap();
        wire["steps"]["content"]["compute"]["op"]["output_type"] = serde_json::to_value(
            plasm_core::value_contract::ValueContract::scalar(plasm_core::FieldType::String),
        )
        .unwrap();
        artifact.comp = serde_json::from_value(wire).unwrap();
        let corrupt = crate::plasm_compile::PlasmCompBundle::new(artifact);
        if let Ok(corrupt) = corrupt {
            assert!(crate::plasm_plan_run::evaluate_plasm_comp_dry(&es, &corrupt).is_err());
        }
    });
}

#[test]
fn python_compact_teaching_example_compiles_and_reference_is_not_a_program() {
    use plasm_core::prompt_render::python::{
        prepare_python_teaching_wave, PythonTeachingState, LANGUAGE,
    };
    on_runtime(async {
        let mut cgs = plasm_core::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        cgs.entities
            .get_mut("LangItem")
            .unwrap()
            .fields
            .get_mut("score")
            .unwrap()
            .required = true;
        let cgs = Arc::new(cgs);
        let mut es = crate::test_support::session_fixtures::ExecuteSessionFixture::new()
            .entry_id("fixture")
            .entities(vec!["LangItem".into()])
            .build(cgs.clone());
        es.teaching_exposure = Some(TeachingExposureSession::new(&cgs, "fixture", &["LangItem"]));
        let wave = prepare_python_teaching_wave(
            es.teaching_exposure.as_ref().unwrap(),
            &PythonTeachingState::default(),
        )
        .unwrap();
        let root = LANGUAGE
            .lines()
            .find(|line| line.starts_with("class ") && line.ends_with("(Program):"))
            .expect("complete root example");
        let library = LANGUAGE
            .split_once(root)
            .unwrap()
            .1
            .split_once("\nAbsent:")
            .unwrap()
            .0;
        let library = format!("{root}{library}");
        ruff_python_parser::parse_module(&library).unwrap();
        compile_python_program(&es, &library).await.unwrap();
        assert!(LANGUAGE.contains("Cards: exact signatures"));
        assert!(
            compile_python_program(&es, &wave.declarations)
                .await
                .is_err(),
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
            let bundle = compile_python_program(&es, &source(&es)).await.unwrap();
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
        let bundle = compile_python_program(&es, &code).await.unwrap();
        evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let actual: Vec<_> = run.return_steps[0]
            .result
            .entities()
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
        .await
        .unwrap();
        let error = execute(&es, &host, &bundle).await.unwrap_err();
        assert!(
            error.diagnostic().contains("parent budget exceeded"),
            "{error}"
        );
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
            ("dynamic control",valid.replace("        items = e1.query()","        if True:\n            items = e1.query()")),
        ] {
            assert!(compile_python_program(&es,&src).await.is_err(),"accepted {case}");
            assert!(calls.lock().unwrap().is_empty());
        }
        let expected = compile_python_program(&es, &valid).await.unwrap();
        let unreachable =
            compile_python_program(&es, &format!("{valid}        items = e1.query()\n"))
                .await
                .unwrap();
        assert!(plasm_core::plasm_monad::comp_semantic_eq(
            &expected.artifact().comp,
            &unreachable.artifact().comp
        ));
        compile_python_program(&es, &format!("import os\n{valid}"))
            .await
            .unwrap();
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn python_lowering_source_spans_and_class_name_are_nonsemantic() {
    on_runtime(async {
        let (es, _, _) = fixture(0);
        let code = source(&es);
        let original = compile_python_program(&es, &code).await.unwrap();
        let renamed = compile_python_program(&es, &code.replace("ExportItems", "RenamedExport"))
            .await
            .unwrap();
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
                .await
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
        let bundle = compile_python_program(&es, &code).await.unwrap();
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
        let bundle = compile_python_program(&es, &source).await.unwrap();
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

#[test]
fn python_map_transformed_rows_feed_compute_and_write() {
    on_runtime(async {
        for count in [0, 3] {
            let (es, host, calls) = fixture(count);
            let code = composition_source(&es);
            let bundle = compile_python_program(&es, &code).await.unwrap();
            assert!(calls.lock().unwrap().is_empty());
            let run = execute(&es, &host, &bundle).await.unwrap();
            let expected = if count == 0 {
                ""
            } else {
                "Title 0:;Title 1:L0;Title 2:L0|L1"
            };
            assert_eq!(
                plasm_runtime::entity_to_agent_row_json(
                    &run.return_steps[0].result.entities()[0],
                    None
                )["value"],
                json!(expected)
            );
            assert_eq!(
                calls.lock().unwrap().last().unwrap(),
                &format!("/publish:{expected}")
            );
            assert_eq!(
                run.return_steps[0].result.coverage(),
                plasm_runtime::ResultCoverage::Complete
            );
        }
    });
}

fn composition_source(es: &ExecuteSession) -> String {
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let item = symbols.entity_sym_for("fixture", "Item");
    let touch = symbols.method_sym_for("fixture", "Item", "publish");
    let code = source(es)
        .replace("    def build(self):", "    @compute\n    def document(self, rows: list[Row]) -> str:\n        return \";\".join(row.title + \":\" + row.labels for row in rows)\n\n    def build(self):")
        .replace("        return items.map(", "        ids = items.select(\"id\").union(items.select(\"id\")).distinct(\"id\")\n        selected = items.where(lambda row: row.id in ids).where(lambda row: row.state == \"open\").distinct(\"id\").order_by(\"id\")\n        mapped = selected.map(");
    format!("{code}        text = self.document(mapped)\n        {item}.{touch}(content=text)\n        return text\n")
}

#[test]
fn python_map_failures_prevent_downstream_writes() {
    on_runtime(async {
        for bounded in [true, false] {
            let scope = crate::operation::ExecutionScope::new();
            let (es, host, calls) = fixture_with_cancel(3, (!bounded).then_some(scope.clone()));
            let code = composition_source(&es).replace(
                "max_parents=256",
                if bounded {
                    "max_parents=2"
                } else {
                    "max_parents=256"
                },
            );
            let bundle = compile_python_program(&es, &code).await.unwrap();
            let error = if bounded {
                execute(&es, &host, &bundle).await.unwrap_err()
            } else {
                let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
                run_plasm_comp(
                    &es,
                    &host,
                    &es.prompt_hash,
                    "cancel-map-write",
                    &bundle,
                    true,
                    None,
                    Some(&scope),
                    Some(dry),
                    None,
                )
                .await
                .unwrap_err()
            };
            assert!(
                error.diagnostic().contains(if bounded {
                    "budget exceeded"
                } else {
                    "cancelled"
                }),
                "{error}"
            );
            assert!(!calls
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.starts_with("/publish")));
        }
    });
}

#[test]
fn python_map_rejects_lost_fields_and_synthetic_receivers_before_io() {
    on_runtime(async {
        let (es, _, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let touch = symbols.method_sym_for("fixture", "Item", "touch");
        let code = composition_source(&es);
        for invalid in [
            code.replace("selected.map(", "selected.select(\"title\").map("),
            code.replace("selected.map(", "selected.select(\"id\").map("),
            code.replace(
                "        text =",
                &format!("        mapped.take(1).{touch}()\n        text ="),
            ),
            code.replace("row.labels", "row.absent"),
        ] {
            assert!(
                compile_python_program(&es, &invalid).await.is_err(),
                "accepted invalid capture or synthetic authority"
            );
        }
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn python_map_can_return_alongside_its_derived_document() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let code = composition_source(&es)
            .replace("        return text\n", "        return mapped, text\n");
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        // Two requested roots plus the unreturned publish operation receipt.
        assert_eq!(run.return_steps.len(), 3);
        assert_eq!(run.return_steps[0].result.count(), 3);
        assert_eq!(run.return_steps[1].result.count(), 1);
        assert_eq!(
            run.graph_summary["scope_instances"]
                .as_array()
                .unwrap()
                .len(),
            3 // Filter, mapped values, and final projection each retain their scope.
        );
    });
}

#[test]
fn python_conditional_csv_runs_in_monty_and_feeds_write() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let expression = r#"";".join(('"' + row.labels.replace('"', '""') + '"' if '|' in row.labels or ',' in row.labels or '"' in row.labels else row.labels) for row in rows)"#;
        let code = composition_source(&es).replace(
            r#"";".join(row.title + ":" + row.labels for row in rows)"#,
            expression,
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            plasm_runtime::entity_to_agent_row_json(
                &run.return_steps[0].result.entities()[0],
                None
            )["value"],
            json!(";L0;\"L0|L1\"")
        );
        assert_eq!(
            calls.lock().unwrap().last().unwrap(),
            "/publish:;L0;\"L0|L1\""
        );
        // Both branches are checked statically, but Monty must execute only the
        // selected branch (the other branch would raise IndexError).
        let lazy = composition_source(&es).replace(
            r#"";".join(row.title + ":" + row.labels for row in rows)"#,
            r#""safe" if True else "".split(",")[99]"#,
        );
        let bundle = compile_python_program(&es, &lazy).await.unwrap();
        execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(calls.lock().unwrap().last().unwrap(), "/publish:safe");
    });
}

#[test]
fn python_conditional_csv_preserves_special_characters() {
    on_runtime(async {
        for (input, expected) in [
            ("plain", "plain"),
            ("", ""),
            ("雪", "雪"),
            ("a,b", "\"a,b\""),
            ("a\"b", "\"a\"\"b\""),
            ("a\nb", "\"a\nb\""),
            ("a\rb", "\"a\rb\""),
        ] {
            let (es, host, calls) = fixture(1);
            let value = serde_json::to_string(input).unwrap();
            let expression = format!(
                r#"'"' + {value}.replace('"', '""') + '"' if ',' in {value} or '"' in {value} or '\n' in {value} or '\r' in {value} else {value}"#
            );
            let code = composition_source(&es).replace(
                r#"";".join(row.title + ":" + row.labels for row in rows)"#,
                &expression,
            );
            let bundle = compile_python_program(&es, &code).await.unwrap();
            execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(
                calls.lock().unwrap().last().unwrap(),
                &format!("/publish:{expected}")
            );
        }
    });
}

#[test]
fn upstream_admission_rejects_later_compute_before_any_write_and_on_replay() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let valid = composition_source(&es);
        let bad = valid
            .replace(
                "    def build(self):",
                &format!(
                    "    def build(self):\n        {item}.{publish}(content=\"must not run\")"
                ),
            )
            .replace("row.title + \":\" + row.labels", "row.absent");
        let error = compile_python_program(&es, &bad).await.unwrap_err();
        assert!(error.contains("absent"), "{error}");
        assert!(calls.lock().unwrap().is_empty());
        let bundle = compile_python_program(&es, &valid).await.unwrap();
        for change_profile in [false, true] {
            let mut artifact = bundle.artifact().clone();
            let plasm_core::PlasmStepPayload::Map(map) =
                artifact.comp.steps.get_mut("text").unwrap()
            else {
                panic!("compute")
            };
            let plasm_core::plasm_monad::ComputeOp::Python {
                source,
                language_profile,
                ..
            } = &mut map.compute.op
            else {
                panic!("python")
            };
            if change_profile {
                *language_profile = "unreviewed-profile".into();
            } else {
                *source = source.replace("row.title", "row.absent");
            }
            let tampered = crate::plasm_comp_bundle::PlasmCompBundle::new(artifact).unwrap();
            assert!(execute(&es, &host, &tampered).await.is_err());
            assert!(calls.lock().unwrap().is_empty());
        }
    });
}

fn assembled_source(es: &ExecuteSession) -> String {
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let item = symbols.entity_sym_for("fixture", "Item");
    let publish = symbols.method_sym_for("fixture", "Item", "publish");
    format!(
        r#"class Assemble(Program):
    @compute
    def destination(self, row: Row) -> str:
        return row.title.lower().replace(" ", "/")

    @compute
    def make_prefix(self, rows: list[Row]) -> str:
        return "prefix"

    @compute
    def document(self, rows: list[Row]) -> str:
        return ";".join(row.original + "=" + row.destination + ":" + row.prefix for row in rows)

    def build(self):
        items = {item}.query()
        prefix = self.make_prefix(items.select("id"))
        mapped = items.select("id", "title").map(lambda row: {{"original": row.title, "destination": self.destination(row)}}, max_parents=256)
        assembled = mapped.select("original", "destination", prefix=lambda row: prefix)
        text = self.document(assembled)
        {item}.{publish}(content=text)
        return text
"#
    )
}

#[test]
fn python_typed_input_assembly_preserves_row_pairing_and_dependencies() {
    on_runtime(async {
        for count in [0, 1, 3] {
            let (es, host, calls) = fixture(count);
            let bundle = compile_python_program(&es, &assembled_source(&es))
                .await
                .unwrap();
            assert!(calls.lock().unwrap().is_empty());
            let run = execute(&es, &host, &bundle).await.unwrap();
            let expected = (0..count)
                .map(|i| format!("Title {i}=title/{i}:prefix"))
                .collect::<Vec<_>>()
                .join(";");
            assert_eq!(
                plasm_runtime::entity_to_agent_row_json(
                    &run.return_steps[0].result.entities()[0],
                    None
                )["value"],
                json!(expected)
            );
            assert_eq!(
                calls.lock().unwrap().last().unwrap(),
                &format!("/publish:{expected}")
            );
        }
    });
}

#[test]
fn python_typed_input_assembly_rejects_missing_fields_and_plural_captures() {
    on_runtime(async {
        let (es, _, calls) = fixture(3);
        let source = assembled_source(&es);
        for invalid in [
            source.replace("row.title.lower()", "row.state.lower()"),
            source.replace(
                "prefix=lambda row: prefix",
                "prefix=lambda row: items.title",
            ),
            source.replace(
                "prefix=lambda row: prefix",
                "prefix=lambda row: prefix.absent",
            ),
        ] {
            assert!(compile_python_program(&es, &invalid).await.is_err());
        }
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn python_typed_input_assembly_preserves_recursive_captured_values() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let code = format!(
            r#"class RecursiveAssembly(Program):
    @compute
    def document(self, rows: list[Row]) -> str:
        return ";".join(row.title + ":" + row.metadata.title + ":" + row.metadata.choices[0] + ":" + str(row.metadata.optional) for row in rows)
    def build(self):
        items = {item}.query()
        settings = items.map(lambda row: {{"details": {{"title": row.title, "choices": [row.state], "optional": None}}}}, max_parents=256).take(1)
        assembled = items.select("title", metadata=lambda row: settings.details)
        return self.document(assembled)
"#
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            plasm_runtime::entity_to_agent_row_json(
                &run.return_steps[0].result.entities()[0],
                None
            )["value"],
            json!("Title 0:Title 0:open:None;Title 1:Title 0:open:None;Title 2:Title 0:open:None")
        );
    });
}

#[test]
fn python_typed_input_assembly_empty_capture_and_bound_prevent_write() {
    on_runtime(async {
        for count in [0, 3] {
            let (es, host, calls) = fixture(count);
            let source = assembled_source(&es);
            let source = if count == 0 {
                source.replace(
                    "prefix=lambda row: prefix",
                    "prefix=lambda row: items.take(1).title",
                )
            } else {
                source.replace("max_parents=256", "max_parents=2")
            };
            // Captures are deliberately named DAG bindings, never hidden Get/calls.
            let source = source
                .replace(
                    "        assembled =",
                    "        chosen = items.take(1)\n        chosen_title = chosen.title\n        assembled =",
                )
                .replace("items.take(1).title", "chosen_title");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let error = execute(&es, &host, &bundle).await.unwrap_err();
            assert!(
                error.diagnostic().contains(if count == 0 {
                    "singleton"
                } else {
                    "budget exceeded"
                }),
                "{error}"
            );
            assert!(!calls
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.starts_with("/publish")));
        }
    });
}

#[test]
fn python_typed_input_assembly_replay_cannot_restore_projected_fields() {
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let bundle = compile_python_program(&es, &assembled_source(&es))
            .await
            .unwrap();
        let mut artifact = bundle.artifact().clone();
        let PlasmStepPayload::MapBody(body) = artifact.comp.steps.get_mut("mapped").unwrap() else {
            panic!("map body")
        };
        let PlasmStepPayload::Map(compute) = body.body.steps.values_mut().find(|p| matches!(p, PlasmStepPayload::Map(m) if matches!(m.compute.op, ComputeOp::Python { .. }))).unwrap() else {
            panic!("compute")
        };
        let ComputeOp::Python {
            source,
            input_schema: Some(schema),
            ..
        } = &mut compute.compute.op
        else {
            panic!("python")
        };
        *source = source.replace("row.title", "row.state");
        let state_type = plasm_core::value_contract::ValueContract::from_domain(
            &es.cgs,
            "fixture",
            es.cgs.get_entity("Item").unwrap().fields["state"]
                .kind
                .registry_key(),
        )
        .unwrap();
        schema.fields = vec![SyntheticFieldSchema {
            name: OutputName::new("state").unwrap(),
            value_kind: state_type.summary(),
            value_type: Some(state_type),
            source: None,
        }];
        let tampered = crate::plasm_comp_bundle::PlasmCompBundle::new(artifact).unwrap();
        let error = execute(&es, &host, &tampered).await.unwrap_err();
        assert!(
            error.diagnostic().contains("projection") || error.diagnostic().contains("field"),
            "{error}"
        );
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn python_union_preserves_catalog_rows_in_correlated_reads() {
    on_runtime(async {
        for (union, project) in [(false, false), (true, false), (true, true)] {
            let (es, host, _) = fixture(3);
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let item = symbols.entity_sym_for("fixture", "Item");
            let tag = symbols.entity_sym_for("fixture", "Tag");
            let projection = if project { ".select('id')" } else { "" };
            let input = if union { "left.union(right)" } else { "left" };
            let source = format!(
                "class Read(Program):\n    def build(self):\n        left = {item}.query(){projection}\n        right = {item}.query(){projection}\n        rows = {input}\n        return rows.flat_map(lambda r: {tag}.query(item_id=r.id))\n"
            );
            let bundle = compile_python_program(&es, &source)
                .await
                .unwrap_or_else(|e| panic!("union={union} project={project}: {e}"));
            let run = execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(
                rows(&run).len(),
                3,
                "union must deduplicate parents and preserve correlated identity"
            );
        }
    });
}

#[test]
fn python_union_preserves_common_entity_receiver_authority() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
        let publish = symbols.method_sym_for("fixture", "Item", "mark");
        for projection in ["", ".select('id')"] {
            let prefix = format!("class Read(Program):\n    def build(self):\n        rows = {item}.query(){projection}.union({item}.query(){projection})\n");
            let source = format!("{prefix}        return rows.flat_map(lambda r: r.{relation})\n");
            let bundle = compile_python_program(&es, &source)
                .await
                .expect("same-owner union retains existing authority");
            let run = execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(rows(&run).len(), 3);
            calls.lock().unwrap().clear();
            let source = format!("{prefix}        return rows.flat_map(lambda r: r.{publish}())\n");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            execute(&es, &host, &bundle).await.unwrap();
            let effects: Vec<_> = calls
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.starts_with("/marks/"))
                .cloned()
                .collect();
            assert_eq!(effects, ["/marks/i0", "/marks/i1", "/marks/i2"]);
        }
        let synthetic = format!("class Synthetic(Program):\n    def build(self):\n        real = {item}.query().select('id')\n        copied = real.map(lambda r: {{'id': r.id}}, max_parents=3)\n        return real.union(copied).flat_map(lambda r: r.{relation})\n");
        assert!(
            compile_python_program(&es, &synthetic).await.is_err(),
            "matching fields cannot manufacture receiver authority"
        );
    });
}

#[test]
fn python_filtered_rows_keep_receiver_authority_for_effect_callbacks() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let mark = symbols.method_sym_for("fixture", "Item", "mark");
        let source = format!(
            "class Mark(Program):\n    def titles(self, rows):\n        return {{r.title for r in rows if r.title is not None}}\n    def build(self):\n        rows = {item}.query()\n        titles = self.titles(rows)\n        selected = rows.where(lambda r: r.title is not None and r.title in titles)\n        return selected.flat_map(lambda r: r.{mark}())\n"
        );
        let bundle = compile_python_program(&es, &source).await.unwrap();
        execute(&es, &host, &bundle).await.unwrap();
        let effects = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with("/marks/"))
            .count();
        assert_eq!(effects, 3);
    });
}

#[test]
fn unannotated_materialized_helper_preserves_independent_input_collections() {
    on_runtime(async {
        let (es, host, _) = fixture(1);
        let source = "class Values(Program):\n    def combine(self, left, right):\n        out = []\n        for value in left:\n            out.append(value)\n        for value in right:\n            out.append(value)\n        return out\n    def build(self):\n        return self.combine([1, 2], [3])\n";
        let bundle = compile_python_program(&es, source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"value": [1, 2, 3]})
        );
    });
}

#[test]
fn python_union_catalog_rows_preserve_effect_arguments() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let source = format!("class Write(Program):\n    def build(self):\n        rows = {item}.query().union({item}.query())\n        return rows.flat_map(lambda r: {item}.{publish}(content=r.id))\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        execute(&es, &host, &bundle).await.unwrap();
        let mut effects = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.starts_with("/publish:"))
            .cloned()
            .collect::<Vec<_>>();
        effects.sort();
        assert_eq!(effects, ["/publish:i0", "/publish:i1", "/publish:i2"]);
    });
}

#[test]
fn python_union_distinct_entity_projections_remain_typed_values() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let tag = symbols.entity_sym_for("fixture", "Tag");
        let source = format!("class Values(Program):\n    def build(self):\n        left = {item}.query().select('id')\n        right = {tag}.query(item_id='i2').select('id')\n        return left.union(right).map(lambda r: {{'value': r.id}}, max_parents=8)\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let result = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(rows(&result).len(), 5);
    });
}

#[test]
fn python_per_row_values_compose_without_a_result_field() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let source = format!("class Values(Program):\n    @compute\n    def title(self, row: Row) -> str:\n        return row.title\n    @compute\n    def upper(self, text: str) -> str:\n        return text.upper()\n    def build(self):\n        titles = self.title({item}.query().select('title'))\n        upper = self.upper(titles)\n        return {{'titles': titles, 'upper': upper}}\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"titles":["Title 0","Title 1","Title 2"], "upper":["TITLE 0","TITLE 1","TITLE 2"]})
        );
    });
}

#[test]
fn python_compute_infers_materialized_inputs_at_each_call() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let source = format!(
            "class Values(Program):\n    @compute\n    def titles(self, rows):\n        return [row.title for row in rows]\n    @compute\n    def title(self, row):\n        return row.title\n    @compute\n    def uppercase(self, title):\n        return title.upper()\n    @compute\n    def summary(self, rows, row):\n        return {{'count': len(rows), 'first': row.title}}\n    def build(self):\n        return {{'titles': self.titles({item}.query()), 'title': self.title({item}.get('i0')), 'upper': self.uppercase({item}.get('i0').title), 'summary': self.summary(row={item}.get('i0'), rows={item}.query())}}\n"
        );
        let bundle = compile_python_program(&es, &source).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({
                "titles": ["Title 0", "Title 1", "Title 2"],
                "title": "Title 0",
                "upper": "TITLE 0",
                "summary": {"count": 3, "first": "Title 0"}
            })
        );
        let calls_before_rejection = calls.lock().unwrap().len();
        assert!(
            compile_python_program(
                &es,
                &source.replace("def titles(self, rows):", "def titles(self, rows: Row):")
            )
            .await
            .is_err(),
            "an explicit scalar assertion cannot change collection cardinality"
        );
        let effectful = format!("class Bad(Program):\n    @compute\n    def write(self, row):\n        return row.{publish}(content='x')\n    def build(self):\n        return self.write({item}.get('i0'))\n");
        assert!(compile_python_program(&es, &effectful).await.is_err());
        assert_eq!(calls.lock().unwrap().len(), calls_before_rejection);
    });
}

#[test]
fn python_row_entity_annotation_runs_without_granting_authority() {
    on_runtime(async {
        let (es, host, _) = fixture(1);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
        let source = format!("class P(Program):\n    @compute\n    def identity(self, rows: list[Row[{item}]]) -> list[Row[{item}]]:\n        return rows\n    def build(self):\n        values = self.identity({item}.query())\n        return values\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(rows(&run).len(), 1);
        assert!(
            compile_python_program(
                &es,
                &source.replace(
                    "return values\n",
                    &format!("return values.flat_map(lambda r: r.{relation})\n")
                )
            )
            .await
            .is_err(),
            "typed compute records must not acquire entity authority"
        );
    });
}

#[test]
fn python_compute_infers_structural_return_at_public_admission() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let source = format!("class Summary(Program):\n    @compute\n    def describe(self, rows: list[Row]):\n        return [{{'key': row.id, 'title': row.title}} for row in rows]\n\n    def build(self):\n        return self.describe({item}.query())\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"value":[{"key":"i0","title":"Title 0"},{"key":"i1","title":"Title 1"},{"key":"i2","title":"Title 2"}]})
        );
        let bare_dict = source.replace("rows: list[Row]):", "rows: list[Row]) -> list[dict]:");
        let inferred = compile_python_program(&es, &bare_dict).await.unwrap();
        assert!(execute(&es, &host, &inferred).await.is_ok());
        let nested = format!("class Bad(Program):\n    def build(self):\n        @compute\n        def inner(row: Row) -> str:\n            return row.title\n        return {item}.query().map(inner, max_parents=3)\n");
        assert!(compile_python_program(&es, &nested)
            .await
            .unwrap_err()
            .contains("Program class method"));
        let invalid = source.replace(
            "        return [",
            "        intermediate = 1\n        return [",
        );
        let multi_statement = compile_python_program(&es, &invalid).await.unwrap();
        assert!(execute(&es, &host, &multi_statement).await.is_ok());
    });
}

#[test]
fn python_compute_closes_unannotated_local_helper_at_public_admission() {
    on_runtime(async {
        let (es, host, _) = fixture(2);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let source = format!("class Summary(Program):\n    @compute\n    def describe(self, rows: list[Row]):\n        def project(items):\n            result = []\n            for row in items:\n                result.append({{'key': row.id, 'title': row.title}})\n            return result\n        return project(rows)\n    def build(self):\n        return self.describe({item}.query())\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"value":[{"key":"i0","title":"Title 0"},{"key":"i1","title":"Title 1"}]})
        );
        let wrong = source.replace(
            "def describe(self, rows: list[Row]):",
            "def describe(self, rows: list[Row]) -> list[int]:",
        );
        assert!(compile_python_program(&es, &wrong).await.is_err());
    });
}

#[test]
fn python_compute_uses_upstream_call_binding() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let source = r#"class Bound(Program):
    @compute
    def render(self, first: str, /, second: str, *, third: str = "C") -> str:
        return first + second + third
    def build(self):
        return self.render("A", third="!", second="B")
"#;
        for (program, expected) in [
            (source.to_string(), "AB!"),
            (
                source.replace("third=\"!\", second=\"B\"", "second=\"B\""),
                "ABC",
            ),
            (
                source
                    .replace("def build(self):", "def build(self, /, *, prefix=\"A\"):")
                    .replace("self.render(\"A\",", "self.render(prefix,"),
                "AB!",
            ),
        ] {
            let bundle = compile_python_program(&es, &program).await.unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(
                plasm_runtime::entity_to_agent_row_json(
                    &run.return_steps[0].result.entities()[0],
                    None
                )["value"],
                json!(expected)
            );
        }
        for program in [
            source.replace("\"A\", third", "first=\"A\", third"),
            source.replace("third=\"!\", second=\"B\"", "\"B\", second=\"C\""),
            source.replace("third=\"!\", second=\"B\"", "third=\"!\""),
            source.replace("third: str = \"C\"", "third: str = local"),
            source.replace("third: str = \"C\"", "third: str = (1 / 0)"),
            source.replace(
                "def build(self):",
                "def build(self, wrong: int = \"text\"):",
            ),
        ] {
            assert!(
                compile_python_program(&es, &program).await.is_err(),
                "{program}"
            );
        }
    });
}

#[test]
fn python_expanded_lists_are_upstream_expressions() {
    on_runtime(async {
        let (es, host, _) = fixture(0);
        let source = "class Expand(Program):\n    def build(self):\n        values = [1, 2]\n        return {'values': [0, *values, 3]}\n";
        let bundle = compile_python_program(&es, source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            plasm_runtime::entity_to_agent_row_json(
                &run.return_steps[0].result.entities()[0],
                None
            )["values"],
            json!([0, 1, 2, 3])
        );
    });
}

#[test]
fn python_reasonable_structural_input_and_set_capture() {
    on_runtime(async {
        let (es, host, _) = fixture(1);
        for (source, expected) in [
            ("class P(Program):\n    @compute\n    def make(self, value: int) -> dict[str, int]:\n        return {'n': value}\n    @compute\n    def read(self, data: dict[str, int]) -> int:\n        return data['n']\n    def build(self):\n        return self.read(self.make(2))\n", json!({"value":2})),
            ("class P(Program):\n    @compute\n    def show(self, rows: list[dict], suffix: str) -> str:\n        return rows[0].get('name', '') + suffix\n    def build(self):\n        return self.show([{'name': 'a', 'count': 2}], '!')\n", json!({"value":"a!"})),
            ("class P(Program):\n    @compute\n    def show(self, rows: list[dict]) -> str:\n        return rows[0].get('name', '') + str(rows[0]['count'])\n    def build(self):\n        return self.show({'name': 'a', 'count': 2})\n", json!({"value":"a2"})),
            ("class P(Program):\n    def build(self):\n        names = {'a', 'a', 'b'}\n        return {'count': len(names), 'member': 'a' in names}\n", json!({"count":2,"member":true})),
        ] {
            let bundle = compile_python_program(&es, source).await.unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(), expected);
        }
    });
}

#[test]
fn python_program_helpers_preserve_independent_ports_and_effects() {
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let source = format!("class P(Program):\n    def _publish(self, row: Row, *, text: str='ok'):\n        if row.title is not None:\n            return {item}.{publish}(content=text)\n        return None\n    def build(self):\n        rows = {item}.query()\n        return rows.flat_map(lambda row: self._publish(row, text='done'))\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["/items", "/publish:done"]);
        let independent = "class P(Program):\n    def _combine(self, left, right):\n        return {'left': left, 'right': right}\n    def build(self):\n        return self._combine([1, 2], [3])\n";
        let bundle = compile_python_program(&es, independent).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"left":[1,2],"right":[3]})
        );
        for invalid in [
            "class P(Program):\n    def _loop(self, value):\n        return self._loop(value)\n    def build(self):\n        return self._loop(1)\n",
            "class P(Program):\n    def _leak(self, value):\n        return secret\n    def build(self):\n        secret = 'caller local'\n        return self._leak(1)\n",
        ] { assert!(compile_python_program(&es, invalid).await.is_err()); }
    });
}

#[test]
fn multi_statement_helper_preserves_deferred_row_operations() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let source = format!(
            "class Filter(Program):\n    def selected(self, rows):\n        matches = rows.where(lambda row: row.title is not None)\n        return matches.take(1)\n    def build(self):\n        return self.selected({item}.query())\n"
        );
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let result = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(rows(&result).len(), 1);
        assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
        let typed = format!(
            "class Filter(Program):\n    def selected(self, rows: Rows[{item}]) -> Rows[{item}]:\n        return rows.take(1)\n    def build(self):\n        return self.selected({item}.query())\n"
        );
        let bundle = compile_python_program(&es, &typed).await.unwrap();
        let result = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(rows(&result).len(), 1);
    });
}

#[test]
fn python_unelected_effect_has_no_completed_invocation() {
    on_runtime(async {
        let (es, host, calls) = fixture(4);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let source = format!("class P(Program):\n    def build(self):\n        rows = {item}.query()\n        def choose(row):\n            if row.title == 'never matches':\n                return {item}.{publish}(content='done')\n            return None\n        return rows.flat_map(choose)\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
        for step in &run.return_steps {
            for ack in step.result.operations.entries() {
                assert_eq!(ack.logical_invocations, 0);
                assert_eq!(ack.completed, 0);
                assert!(
                    ack.outcomes.is_empty(),
                    "parent completion leaked into action receipts: {ack:?}"
                );
            }
        }
    });
}

#[test]
fn python_flat_map_sequences_returned_effects_in_source_order() {
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let source = format!("class Effects(Program):\n    def build(self):\n        rows = {item}.query()\n        def sequence(row):\n            first = {item}.{publish}(content='first')\n            if row.title is not None:\n                second = {item}.{publish}(content='second')\n                return [first, second]\n            return [first]\n        return rows.flat_map(sequence)\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert!(run
            .return_steps
            .iter()
            .all(|step| step.result.entities().is_empty()));
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["/items", "/publish:first", "/publish:second"]
        );
        let completed: usize = run
            .return_steps
            .iter()
            .flat_map(|step| step.result.operations.entries())
            .map(|entry| entry.completed)
            .sum();
        assert_eq!(completed, 2);
        let mixed = source.replace("return [first, second]", "return [first, row.title]");
        assert!(compile_python_program(&es, &mixed).await.is_err());
    });
}

#[test]
fn python_typed_pure_helper_materializes_only_its_complete_set_result() {
    on_runtime(async {
        let (es, host, calls) = fixture(2);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let source = format!("class Values(Program):\n    def titles(self, rows: list[Row]) -> set[str]:\n        names = set()\n        for row in rows:\n            if row.title is not None:\n                names.add(row.title)\n        return names\n    def build(self):\n        names = self.titles({item}.query())\n        return {{'has_title': 'Title 0' in names, 'count': len(names)}}\n");
        let bundle = compile_python_program(&es, &source).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"has_title":true,"count":2})
        );
        assert_eq!(*calls.lock().unwrap(), vec!["/items"]);
        let constant = "class Values(Program):\n    def label(self) -> str:\n        return 'ready'\n    def build(self):\n        return self.label()\n";
        let bundle = compile_python_program(&es, constant).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap(),
            json!({"value":"ready"})
        );
    });
}

#[test]
fn python_zero_input_compute_uses_private_unit_port() {
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let source = "import datetime\nclass Values(Program):\n    @compute\n    def cutoff(self) -> datetime.datetime:\n        return datetime.datetime(2026, 10, 3) - datetime.timedelta(days=50)\n    def build(self):\n        return self.cutoff()\n";
        let bundle = compile_python_program(&es, source).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let run = execute(&es, &host, &bundle).await.unwrap();
        let value = serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap();
        assert_eq!(value["value"]["__plasm_temporal"], "datetime");
        assert_eq!(value["value"]["components"]["year"], 2026);
        assert_eq!(value["value"]["components"]["month"], 8);
        assert_eq!(value["value"]["components"]["day"], 14);
        let live_source =
            source.replace("datetime.datetime(2026, 10, 3)", "datetime.datetime.now()");
        let live_bundle = compile_python_program(&es, &live_source).await.unwrap();
        let live = execute(&es, &host, &live_bundle).await.unwrap();
        let live_value =
            serde_json::to_value(&live.return_steps[0].result.entities()[0].fields).unwrap();
        assert_eq!(live_value["value"]["__plasm_temporal"], "datetime");
        assert!(calls.lock().unwrap().is_empty());
    });
}
