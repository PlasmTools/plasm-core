use super::*;
use crate::plasm_compile::compile_python_program;

fn nested_source(es: &ExecuteSession, inner: &str, bound: u32) -> String {
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let item = symbols.entity_sym_for("fixture", "Item");
    let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
    format!("class Nested(Program):\n    def build(self):\n        parents = {item}.query()\n        result = parents.map(lambda parent: {{'title': parent.title, 'children': parent.{relation}.map(lambda child: {inner}, max_parents={bound})}}, max_parents=8)\n        return result\n")
}

#[test]
fn nested_maps_preserve_parent_child_pairing_and_empty_children() {
    on_runtime(async {
        for count in [0, 1, 3] {
            let (es, host, calls) = fixture(count);
            let code = nested_source(&es, "{'parent': parent.title, 'child': child.label}", 8);
            let bundle = compile_python_program(&es, &code).await.unwrap();
            assert!(calls.lock().unwrap().is_empty());
            let run = execute(&es, &host, &bundle).await.unwrap();
            let actual: Vec<_> = run.return_steps[0]
                .result
                .entities()
                .iter()
                .map(|e| plasm_runtime::entity_to_agent_row_json(e, None))
                .collect();
            let expected: Vec<_> = (0..count).map(|i| json!({"title":format!("Title {i}"), "children": (0..i).map(|j| json!({"parent":format!("Title {i}"), "child":format!("L{j}")})).collect::<Vec<_>>()})).collect();
            for (row, expected) in actual.iter().zip(&expected) {
                assert_eq!(row["title"], expected["title"]);
                assert_eq!(row["children"], expected["children"]);
            }
            assert_eq!(actual.len(), expected.len());
            assert_eq!(calls.lock().unwrap().len(), count + 1);
        }
    });
}

#[test]
fn nested_maps_execute_effects_in_parent_child_order_and_retain_receipts() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let method = symbols.method_sym_for("fixture", "Item", "publish");
        let code = nested_source(
            &es,
            &format!("{{'child': child.label, 'written': {item}.{method}(content=parent.title)}}"),
            8,
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        assert!(crate::plan_flow::validated_plan_has_remote_mutation(
            dry.validated_plan()
        ));
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "/items",
                "/items/i0/tags",
                "/items/i1/tags",
                "/publish:Title 1",
                "/items/i2/tags",
                "/publish:Title 2",
                "/publish:Title 2"
            ]
        );
        let completed: u64 = run.return_steps[0]
            .result
            .operations
            .entries()
            .iter()
            .map(|a| a.completed as u64)
            .sum();
        assert_eq!(completed, 3);
    });
}

#[test]
fn nested_maps_check_each_child_bound_before_its_effects() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let method = symbols.method_sym_for("fixture", "Item", "publish");
        let code = nested_source(
            &es,
            &format!("{{'written': {item}.{method}(content=parent.title)}}"),
            1,
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let error = execute(&es, &host, &bundle).await.unwrap_err();
        assert!(error.diagnostic().contains("budget exceeded"), "{error}");
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "/items",
                "/items/i0/tags",
                "/items/i1/tags",
                "/publish:Title 1",
                "/items/i2/tags"
            ]
        );
    });
}

#[test]
fn nested_maps_compute_from_typed_parent_and_child_inputs() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let code = nested_source(&es, "{'text': self.pair({'parent': parent.title, 'child': child.label})}", 8)
            .replace("class Nested(Program):", "class Nested(Program):\n    @compute\n    def pair(self, row: Row) -> str:\n        return row.parent + ':' + row.child");
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let row = plasm_runtime::entity_to_agent_row_json(
            &run.return_steps[0].result.entities()[2],
            None,
        );
        assert_eq!(
            row["children"],
            json!([{"text":"Title 2:L0"},{"text":"Title 2:L1"}])
        );
    });
}

#[test]
fn nested_maps_three_levels_keep_ancestor_ports_distinct() {
    on_runtime(async {
        let (es, host, _) = fixture(2);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let code = nested_source(&es, &format!("{{'grandchildren': {item}.query().map(lambda grandchild: {{'root': parent.title, 'child': child.label, 'grandchild': grandchild.title}}, max_parents=8)}}"), 8);
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let row = plasm_runtime::entity_to_agent_row_json(
            &run.return_steps[0].result.entities()[1],
            None,
        );
        assert_eq!(
            row["children"],
            json!([{"grandchildren":[{"root":"Title 1","child":"L0","grandchild":"Title 0"},{"root":"Title 1","child":"L0","grandchild":"Title 1"}]}])
        );
    });
}

#[test]
fn nested_maps_reject_invalid_later_bodies_before_any_io() {
    on_runtime(async {
        for body in [
            "{'bad': parent.missing}",
            "{'bad': child.missing}",
            "{'bad': parents.title}",
        ] {
            let (es, _, calls) = fixture(0);
            assert!(compile_python_program(&es, &nested_source(&es, body, 8))
                .await
                .is_err());
            assert!(calls.lock().unwrap().is_empty());
        }
        let (es, _, calls) = fixture(2);
        let shadow = nested_source(&es, "{'title': parent.title}", 8)
            .replace("lambda child:", "lambda parent:");
        let error = compile_python_program(&es, &shadow).await.unwrap_err();
        assert!(
            error.to_string().contains("not a row field on `Tag`"),
            "{error}"
        );
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn nested_maps_compute_feeds_effect_without_leaving_the_dag() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let method = symbols.method_sym_for("fixture", "Item", "publish");
        let code = nested_source(&es, &format!("{{'written': {item}.{method}(content=self.pair({{'parent': parent.title, 'child': child.label}}))}}"), 8)
            .replace("class Nested(Program):", "class Nested(Program):\n    @compute\n    def pair(self, row: Row) -> str:\n        return row.parent + ':' + row.child");
        let bundle = compile_python_program(&es, &code).await.unwrap();
        execute(&es, &host, &bundle).await.unwrap();
        let writes: Vec<_> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("/publish"))
            .cloned()
            .collect();
        assert_eq!(
            writes,
            [
                "/publish:Title 1:L0",
                "/publish:Title 2:L0",
                "/publish:Title 2:L1"
            ]
        );
    });
}

#[test]
fn nested_maps_stream_full_addresses_and_cancel_inner_reads() {
    on_runtime(async {
        use crate::occurrence_progress::OccurrencePhase;
        let gate = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
        let (es, host, calls) = fixture_with_controls(2, None, Some(gate.clone()));
        let es = Arc::new(es);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
        let code = format!("class Nested(Program):\n    def build(self):\n        return {item}.query().map(lambda parent: {{'children': {item}.query().take(1).map(lambda child: {{'tags': child.{relation}.map(lambda tag: {{'label': tag.label}}, max_parents=8)}}, max_parents=8)}}, max_parents=8)");
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let handle = es.mint_operation_handle("l_AAAAAAAAQACAAAAAAAAAAQ");
        let cancel = plasm_runtime::CancelSignal::new();
        es.try_begin_async_operation(handle.clone(), cancel.clone(), Default::default())
            .unwrap();
        let scope = crate::operation::ExecutionScope::for_async_operation(
            es.clone(),
            handle.clone(),
            cancel,
        );
        let run = crate::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "nested-cancel",
            &bundle,
            true,
            None,
            Some(&scope),
            None,
            None,
        );
        let inspect = async {
            gate.0.notified().await;
            let snapshot = crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle).unwrap();
            assert!(snapshot
                .occurrences
                .iter()
                .any(|e| e.address.scope_path.len() == 2
                    && e.occurrence_path == [0, 0]
                    && e.phase == OccurrencePhase::Running));
            es.cancel_operation(&handle, None);
            gate.1.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(run, inspect)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().diagnostic().contains("cancelled"));
        let snapshot = crate::op_ui_telemetry::OpUiTelemetry::from_live(&es, &handle).unwrap();
        assert!(snapshot
            .occurrences
            .iter()
            .any(|e| e.address.scope_path.len() == 2
                && e.occurrence_path == [0, 0]
                && e.phase == OccurrencePhase::Cancelled));
        assert!(!calls.lock().unwrap().iter().any(|p| p == "/items/i1/tags"));
    });
}

#[test]
fn nested_maps_replay_rejects_capture_schema_and_bound_tampering() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let bundle = compile_python_program(
            &es,
            &nested_source(&es, "{'parent': parent.title, 'child': child.label}", 8),
        )
        .await
        .unwrap();
        for change in ["field", "owner", "bound", "escape"] {
            let mut artifact = bundle.artifact().clone();
            let PlasmStepPayload::MapBody(outer) = artifact.comp.steps.get_mut("result").unwrap()
            else {
                panic!("outer map")
            };
            let inner = outer
                .body
                .steps
                .values_mut()
                .find_map(|p| match p {
                    PlasmStepPayload::MapBody(body) => Some(body),
                    _ => None,
                })
                .unwrap();
            match change {
                "field" => {
                    inner.captures[0].schema.fields[0].name = OutputName::new("absent").unwrap()
                }
                "owner" => inner.captures[0].entity.entity = "Tag".into(),
                "bound" => inner.max_parents = NonZeroU32::new(65_537).unwrap(),
                "escape" => inner.captures[0].source = id("unrelated"),
                _ => unreachable!(),
            }
            match PlasmCompBundle::new(artifact) {
                Err(_) => {}
                Ok(tampered) => {
                    assert!(
                        execute(&es, &host, &tampered).await.is_err(),
                        "admitted {change}"
                    );
                }
            }
            assert!(
                calls.lock().unwrap().is_empty(),
                "tampering reached IO: {change}"
            );
        }
    });
}

#[test]
fn nested_maps_accept_synthetic_rows_without_receiver_authority() {
    on_runtime(async {
        let (es, host, calls) = fixture(2);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let method = symbols.method_sym_for("fixture", "Item", "touch");
        let code = format!("class Synthetic(Program):\n    def build(self):\n        values = {item}.query().map(lambda parent: {{'id': parent.id, 'name': parent.title}}, max_parents=8)\n        return values.map(lambda value: {{'label': value.name}}, max_parents=8)");
        let invalid = code.replace(
            "{'label': value.name}",
            &format!("{{'done': value.{method}()}}"),
        );
        assert!(compile_python_program(&es, &invalid).await.is_err());
        assert!(calls.lock().unwrap().is_empty());
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let values: Vec<_> = run.return_steps[0]
            .result
            .entities()
            .iter()
            .map(|e| plasm_runtime::entity_to_agent_row_json(e, None)["label"].clone())
            .collect();
        assert_eq!(values, vec![json!("Title 0"), json!("Title 1")]);
    });
}

#[test]
fn scoped_compute_uses_value_fields_without_loading_navigation_edges() {
    on_runtime(async {
        let (es, host, _) = fixture(2);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let code = format!("class Labels(Program):\n    @compute\n    def labels(self, rows: list[Value[{item}]]) -> str:\n        return '|'.join(row.title for row in rows)\n    def build(self):\n        return {item}.query().map(lambda parent: {{'labels': self.labels({item}.query())}}, max_parents=8)");
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            plasm_runtime::entity_to_agent_row_json(
                &run.return_steps[0].result.entities()[0],
                None
            )["labels"],
            "Title 0|Title 1"
        );
    });
}

#[test]
fn ordinary_nested_flat_map_and_captured_fstring_execute() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let code = format!("class Nested(Program):\n    def build(self):\n        return {item}.query().flat_map(lambda parent: parent.{relation}.flat_map(lambda child: {item}.{publish}(content=f'{parent_expr}:{{child.label.lower()}}')))\n", parent_expr="{parent.title.lower().replace(' ', '_')}");
        let bundle = compile_python_program(&es, &code).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let run = execute(&es, &host, &bundle).await.unwrap();
        let writes: Vec<_> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("/publish:"))
            .cloned()
            .collect();
        assert_eq!(
            writes,
            [
                "/publish:title_1:l0",
                "/publish:title_2:l0",
                "/publish:title_2:l1"
            ]
        );
        assert_eq!(
            run.return_steps[0]
                .result
                .operations
                .entries()
                .iter()
                .map(|o| o.completed)
                .sum::<usize>(),
            3
        );
    });
}

#[test]
fn captured_formatting_executes_conversion_and_format_spec() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let code = format!("class Formatting(Program):\n    def build(self):\n        return {item}.query().take(1).flat_map(lambda row: {item}.{publish}(content=f'{{row.title!r:>12}}'), max_parents=1)\n");
        let bundle = compile_python_program(&es, &code).await.unwrap();
        assert!(
            calls.lock().unwrap().is_empty(),
            "planning must not execute captures"
        );
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            run.return_steps[0].result.coverage(),
            plasm_runtime::ResultCoverage::Complete
        );
        let writes: Vec<_> = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.starts_with("/publish:"))
            .cloned()
            .collect();
        assert_eq!(writes, ["/publish:   'Title 0'"]);
    });
}
