use super::*;
use crate::plasm_compile::compile_python_program;

#[test]
fn conditional_effects_preserve_source_scope_and_fallthrough() {
    on_runtime(async {
        for inside_branch in [true, false] {
            let (es, host, calls) = fixture(3);
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let item = symbols.entity_sym_for("fixture", "Item");
            let publish = symbols.method_sym_for("fixture", "Item", "publish");
            let indent = if inside_branch {
                "            "
            } else {
                "        "
            };
            let code = format!(
                "class P(Program):\n    def build(self):\n        return {item}.query().flat_map(lambda row: self.process(row))\n    def process(self, row):\n        if row.id == 'i1':\n            {item}.{publish}(content='selected:' + row.id)\n{indent}{item}.{publish}(content='follow:' + row.id)\n"
            );
            let bundle = compile_python_program(&es, &code).await.unwrap();
            assert!(calls.lock().unwrap().is_empty());
            evaluate_plasm_comp_dry(&es, &bundle).unwrap();
            execute(&es, &host, &bundle).await.unwrap();
            let expected = if inside_branch {
                vec!["/items", "/publish:selected:i1", "/publish:follow:i1"]
            } else {
                vec![
                    "/items",
                    "/publish:follow:i0",
                    "/publish:selected:i1",
                    "/publish:follow:i1",
                    "/publish:follow:i2",
                ]
            };
            assert_eq!(*calls.lock().unwrap(), expected, "{code}");
        }
    });
}

#[test]
fn selected_receiver_and_ordered_effects_survive_projection_and_limit() {
    on_runtime(async {
        let (es, host, calls) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let mark = symbols.method_sym_for("fixture", "Item", "mark");
        let publish = symbols.method_sym_for("fixture", "Item", "publish");
        let code = format!(
            "class P(Program):\n    def build(self):\n        selected = {item}.query().where(lambda row: row.id == 'i2').select('id').take(1)\n        target = {item}.get(selected)\n        return selected.flat_map(lambda row: [target.{mark}(), {item}.{publish}(content=row.id), {item}.{publish}(content=row.id)])\n"
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
        evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let evidence: Vec<_> = run
            .return_steps
            .iter()
            .flat_map(|step| step.result.operations.entries())
            .flat_map(|ack| &ack.resolved_effects)
            .collect();
        assert_eq!(evidence.len(), 3);
        assert_eq!(
            evidence
                .iter()
                .filter(|e| e.target_kind == plasm_runtime::EffectTargetKind::Receiver)
                .count(),
            1
        );
        assert_eq!(
            evidence
                .iter()
                .filter(|e| e.target_kind == plasm_runtime::EffectTargetKind::Capability)
                .count(),
            2
        );
        assert_eq!(
            evidence
                .iter()
                .map(|e| e.invocation)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        for effect in &evidence {
            let origin = effect.origin.as_ref().unwrap();
            // Position zero belongs to the derived singleton, whose transported key is i2.
            assert_eq!(origin.selection.as_ref().unwrap().index, 0);
            assert_eq!(origin.occurrence_path, vec![0]);
        }
        let actual = calls.lock().unwrap();
        let effects = actual
            .iter()
            .filter(|call| call.starts_with("/marks/") || call.starts_with("/publish:"))
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(effects, ["/marks/i2", "/publish:i2", "/publish:i2"]);
    });
}

#[test]
fn relation_record_projection_conserves_parent_occurrences() {
    on_runtime(async {
        for count in [0, 1, 3] {
            let (es, host, _) = fixture(count);
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let item = symbols.entity_sym_for("fixture", "Item");
            let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
            let code = format!(
                "class P(Program):\n    def build(self):\n        parents = {item}.query()\n        return parents.map(lambda parent: {{'title': parent.title, 'children': parent.{relation}}}, max_parents=8)\n"
            );
            let bundle = compile_python_program(&es, &code).await.unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            let actual: Vec<_> = run.return_steps[0]
                .result
                .entities()
                .iter()
                .map(|row| plasm_runtime::entity_to_agent_row_json(row, None).unwrap())
                .collect();
            assert_eq!(actual.len(), count);
            for (index, row) in actual.iter().enumerate() {
                assert_eq!(row["title"], format!("Title {index}"));
                assert_eq!(row["children"].as_array().unwrap().len(), index);
            }
        }
    });
}

fn nested_source(es: &ExecuteSession, inner: &str, bound: u32) -> String {
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let item = symbols.entity_sym_for("fixture", "Item");
    let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
    format!(
        "class Nested(Program):\n    def build(self):\n        parents = {item}.query()\n        result = parents.map(lambda parent: {{'title': parent.title, 'children': parent.{relation}.map(lambda child: {inner}, max_parents={bound})}}, max_parents=8)\n        return result\n"
    )
}

#[test]
fn emitted_limits_preserve_value_contracts_and_ports_reject_mismatches() {
    use crate::map_body::{MapBodyValidationError, ScopeContractError};
    use crate::plasm_plan::{
        PlanNodeId, ValidatedCaptureNode, ValidatedComputeNode, ValidatedPlanNode,
    };
    use plasm_core::value_contract::ValueContract;
    on_runtime(async {
        let (es, _, calls) = fixture(0);
        for value in [
            json!("text"),
            json!(7),
            json!(true),
            json!(null),
            json!(["text", false, 3]),
        ] {
            let contract = ValueContract::literal(
                &serde_json::from_value::<plasm_core::Value>(value).unwrap(),
            )
            .unwrap();
            let source_id = PlanNodeId::new("value").unwrap();
            let limited_id = PlanNodeId::new("limited").unwrap();
            let nodes = vec![
                ValidatedPlanNode::Capture(ValidatedCaptureNode {
                    id: source_id.clone(),
                    entity: crate::plasm_plan::QualifiedEntityKey {
                        entry_id: "fixture".into(),
                        entity: "__value".into(),
                    },
                    contract: CaptureContract::Value {
                        value: contract.clone(),
                    },
                    singleton: true,
                }),
                ValidatedPlanNode::Compute(ValidatedComputeNode {
                    source_node: source_id.clone(),
                    id: limited_id,
                    effect_class: plasm_core::EffectClass::ArtifactRead,
                    result_shape: plasm_core::ResultShape::Single,
                    compute: ComputeTemplate {
                        source: "value".into(),
                        op: ComputeOp::Limit { count: 1 },
                        schema: SyntheticResultSchema::for_value(contract.clone()).unwrap(),
                        page_size: None,
                        collection_alias: None,
                    },
                    depends_on: vec![source_id],
                    uses_result: vec![],
                }),
            ];
            assert_eq!(
                crate::map_body_schema::row_contract(&es, &nodes, "limited").unwrap(),
                contract
            );
            let mut parent = ParentCapture {
                source: id("limited"),
                local: id("parent"),
                entity: owner("__value"),
                contract: CaptureContract::Value {
                    value: contract.clone(),
                },
            };
            crate::map_body::validate_capture_port(&es, &nodes, &parent).unwrap();
            let mut enclosing = ScopedCapture {
                source: id("limited"),
                local: id("enclosing"),
                entity: owner("__value"),
                contract: parent.contract.clone(),
                singleton: true,
            };
            crate::map_body::validate_capture_port(&es, &nodes, &enclosing).unwrap();
            let wrong = CaptureContract::Value {
                value: ValueContract::scalar(plasm_core::FieldType::Uuid),
            };
            parent.contract = wrong.clone();
            enclosing.contract = wrong;
            for error in [
                crate::map_body::validate_capture_port(&es, &nodes, &parent).unwrap_err(),
                crate::map_body::validate_capture_port(&es, &nodes, &enclosing).unwrap_err(),
            ] {
                assert!(matches!(
                    error,
                    MapBodyValidationError::Contract(
                        ScopeContractError::ScalarCaptureContractChanged
                    )
                ));
            }
        }
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn nested_helper_captures_preserve_recursive_value_cells() {
    on_runtime(async {
        for (literal, value) in [
            ("'text'", json!("text")),
            ("7", json!(7)),
            ("True", json!(true)),
            ("None", json!(null)),
            ("['text', False, 3]", json!(["text", false, 3])),
        ] {
            let (es, host, _) = fixture(3);
            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
            let item = symbols.entity_sym_for("fixture", "Item");
            let tags = symbols.ident_sym_relation_for("fixture", "Item", "tags");
            let source = format!(
                "class Captured(Program):\n    def build(self):\n        def selected(value):\n            return {item}.query().where(lambda row: row.title == 'Title 2').flat_map(lambda parent: parent.{tags}.map(lambda child: {{'value': value, 'child': child.label}}, max_parents=8), max_parents=8)\n        return selected({literal})\n"
            );
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let result = execute(&es, &host, &bundle).await.unwrap();
            let rows = result.return_steps[0]
                .result
                .entities()
                .iter()
                .map(|row| serde_json::to_value(&row.fields).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                rows,
                vec![
                    json!({"value":value,"child":"L0"}),
                    json!({"value":value,"child":"L1"})
                ]
            );
        }
    });
}

#[test]
fn nested_flat_map_captures_helper_scalar_and_parent_record() {
    on_runtime(async {
        let (es, host, _) = fixture(3);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let item = symbols.entity_sym_for("fixture", "Item");
        let relation = symbols.ident_sym_relation_for("fixture", "Item", "tags");
        let code = format!(
            "class Nested(Program):\n    def build(self):\n        def selected(title):\n            return {item}.query().where(lambda row: row.title == title).flat_map(lambda parent: parent.{relation}.map(lambda child: {{'parent': parent.title, 'child': child.label}}, max_parents=8), max_parents=8)\n        return selected('Title 2')\n"
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle)
            .await
            .unwrap_or_else(|error| panic!("{}", error.diagnostic()));
        let actual: Vec<_> = run.return_steps[0]
            .result
            .entities()
            .iter()
            .map(|row| serde_json::to_value(&row.fields).unwrap())
            .collect();
        assert_eq!(
            actual,
            vec![
                json!({"parent":"Title 2","child":"L0"}),
                json!({"parent":"Title 2","child":"L1"})
            ]
        );
    });
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
                .map(|e| plasm_runtime::entity_to_agent_row_json(e, None).unwrap())
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
        let outcomes = run.return_steps[0]
            .result
            .operations
            .entries()
            .iter()
            .flat_map(|ack| &ack.outcomes)
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .map(|outcome| outcome.occurrence_path.clone())
                .collect::<Vec<_>>(),
            vec![vec![1, 0], vec![2, 0], vec![2, 1]]
        );
        assert!(outcomes.iter().all(|outcome| outcome.scope_path.len() >= 3));
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
        )
        .unwrap();
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
        let code = nested_source(
            &es,
            &format!(
                "{{'grandchildren': {item}.query().map(lambda grandchild: {{'root': parent.title, 'child': child.label, 'grandchild': grandchild.title}}, max_parents=8)}}"
            ),
            8,
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        let row = plasm_runtime::entity_to_agent_row_json(
            &run.return_steps[0].result.entities()[1],
            None,
        )
        .unwrap();
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
        let code = format!(
            "class Nested(Program):\n    def build(self):\n        return {item}.query().map(lambda parent: {{'children': {item}.query().take(1).map(lambda child: {{'tags': child.{relation}.map(lambda tag: {{'label': tag.label}}, max_parents=8)}}, max_parents=8)}}, max_parents=8)"
        );
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
                    if let CaptureContract::Rows {
                        schema: Some(schema),
                        ..
                    } = &mut inner.captures[0].contract
                    {
                        schema.fields[0].name = OutputName::new("absent").unwrap();
                    } else {
                        panic!("fixture capture must be a row");
                    }
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
        let code = format!(
            "class Synthetic(Program):\n    def build(self):\n        values = {item}.query().map(lambda parent: {{'id': parent.id, 'name': parent.title}}, max_parents=8)\n        return values.map(lambda value: {{'label': value.name}}, max_parents=8)"
        );
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
            .map(|e| plasm_runtime::entity_to_agent_row_json(e, None).unwrap()["label"].clone())
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
        let code = format!(
            "class Labels(Program):\n    @compute\n    def labels(self, rows: list[Value[{item}]]) -> str:\n        return '|'.join(row.title for row in rows)\n    def build(self):\n        return {item}.query().map(lambda parent: {{'labels': self.labels({item}.query())}}, max_parents=8)"
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            plasm_runtime::entity_to_agent_row_json(
                &run.return_steps[0].result.entities()[0],
                None
            )
            .unwrap()["labels"],
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
        let code = format!(
            "class Nested(Program):\n    def build(self):\n        return {item}.query().flat_map(lambda parent: parent.{relation}.flat_map(lambda child: {item}.{publish}(content=f'{parent_expr}:{{child.label.lower()}}')))\n",
            parent_expr = "{parent.title.lower().replace(' ', '_')}"
        );
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
        let code = format!(
            "class Formatting(Program):\n    def build(self):\n        return {item}.query().take(1).flat_map(lambda row: {item}.{publish}(content=f'{{row.title!r:>12}}'), max_parents=1)\n"
        );
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
