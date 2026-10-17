use super::support::*;
use super::*;
use crate::plasm_compile::compile_python_program;

fn source_cause(
    error: &crate::compilation_error::CompilationError,
) -> &crate::program_rejection::PythonSourceError {
    use crate::{
        compilation_error::CompilationError, program_diagnostic::ProgramStageError,
        program_rejection::PythonLoweringError,
    };
    let CompilationError::Program(stage) = error else {
        panic!("expected program rejection")
    };
    let ProgramStageError::PythonLowering { error } = stage.as_ref() else {
        panic!("expected lowering rejection")
    };
    let PythonLoweringError::Source { error, .. } = error else {
        panic!("expected source rejection")
    };
    error
}

#[test]
fn scalar_field_rejection_preserves_expression_cardinality_and_location() {
    on_runtime(async {
        let (es, _, calls) = fixture(2);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        for expression in ["rows.id", "str(rows.id)"] {
            let source = format!("class P(Program):\n    def build(self):\n        rows = {item}.query()\n        return {expression}\n");
            let error = compile_python_program(&es, &source).await.unwrap_err();
            assert!(
                matches!(source_cause(&error),
                    crate::program_rejection::PythonSourceError::FieldInputNeedsSingleton { expression, cardinality, .. }
                    if expression == "rows.id" && *cardinality == plasm_core::plasm_monad::RelationSourceCardinality::Many
                ),
                "{error:?}"
            );
            let crate::compilation_error::CompilationError::Program(stage) = &error else {
                panic!("program error")
            };
            let crate::program_diagnostic::ProgramStageError::PythonLowering { error: lowering } =
                stage.as_ref()
            else {
                panic!("lowering error")
            };
            assert_eq!(lowering.span_offset(), source.find("rows.id"));
            assert!(error.to_string().contains("rows.id"));
            let failure: plasm_runtime::ExecutionFailure = error.into();
            assert!(failure.diagnostic().contains("rows.id"));
            assert!(failure.diagnostic().contains("many rows"));
            assert!(failure.program_rejection.is_some());
        }
        let source = format!("class P(Program):\n    def build(self):\n        rows = {item}.get('i0')\n        return rows.id\n");
        compile_python_program(&es, &source).await.unwrap();
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn plain_nested_dag_values_admit_only_declared_fields() {
    on_runtime(async {
        let (es, host, _) = fixture(2);
        let symbol = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        for output in [
            format!("[{symbol}.get('i0'), {symbol}.get('i1')]"),
            format!("{{'items': [{symbol}.get('i0'), {symbol}.get('i1')], 'payload': {{'_ref': 'authored', '_custom': 'data'}}}}"),
        ] {
            let source = format!("class P(Program):\n    def build(self):\n        return {output}\n");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            let json = serde_json::to_value(run.return_steps.iter().flat_map(|s|s.result.entities()).map(|e|e.fields.clone()).collect::<Vec<_>>()).unwrap();
            let text = json.to_string();
            assert!(!text.contains("_completeness"), "{text}");
            assert!(!text.contains("_last_updated"), "{text}");
            if let Some(markdown) = &run.run_markdown {
                assert!(!markdown.contains("_completeness"), "{markdown}");
                assert!(!markdown.contains("_last_updated"), "{markdown}");
            }
            if output.contains("payload") { assert!(text.contains("authored")); }
        }
    });
}

#[test]
fn private_compute_names_and_unused_declarations_preserve_results() {
    on_runtime(async {
        for method in ["value", "_value"] {
            for unused in [
                "",
                "    @compute\n    def unused(self, row):\n        return row.unknown_field\n",
            ] {
                let (es, host, calls) = fixture(0);
                let source = format!("class P(Program):\n    def build(self):\n        return self.{method}()\n    @compute\n    def {method}(self):\n        return 1\n{unused}");
                let bundle = compile_python_program(&es, &source).await.unwrap();
                let run = execute(&es, &host, &bundle).await.unwrap();
                assert_eq!(
                    plasm_runtime::entity_to_agent_row_json(
                        &run.return_steps[0].result.entities()[0],
                        None
                    )
                    .unwrap()["value"],
                    json!(1)
                );
                assert!(calls.lock().unwrap().is_empty());
            }
        }
    });
}

#[test]
fn build_mutation_reports_the_dag_law_before_value_inference() {
    on_runtime(async {
        let (es, _, calls) = fixture(0);
        let source = "class P(Program):\n    def build(self):\n        values = []\n        values.append(1)\n        return values\n";
        let error = compile_python_program(&es, source).await.unwrap_err();
        assert!(matches!(
            source_cause(&error),
            crate::program_rejection::PythonSourceError::UnusedNonWriteExpression
        ));
        let message = error.to_string();
        assert!(message.contains("immutable"), "{message}");
        assert!(!message.contains("Never"), "{message}");
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn python_type_correction_preserves_cause_and_rendering() {
    use crate::program_rejection::PythonLoweringError;
    let cause = plasm_core::TypeError::RequiredParameterOmitted {
        parameter: "category".into(),
        expression: String::new(),
    };
    let error = PythonLoweringError::from(cause.clone());
    assert!(matches!(&error, PythonLoweringError::Type(actual) if **actual == cause));
    assert_eq!(error.to_string(), cause.python_correction());
    assert_eq!(error.into_message(), cause.python_correction());
}

#[test]
fn missing_get_names_the_receiver_and_preserves_the_typed_cause() {
    on_runtime(async {
        let (es, _, calls) = fixture(0);
        let tag = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Tag");
        let source =
            format!("class P(Program):\n    def build(self):\n        return {tag}.get('t1')\n");
        let error = compile_python_program(&es, &source).await.unwrap_err();
        assert!(
            matches!(source_cause(&error), crate::program_rejection::PythonSourceError::GetMethodUnavailable { entity, source } if entity == "Tag" && matches!(source.as_ref(), plasm_core::TypeError::CapabilityNotFound { .. }))
        );
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn missing_query_keyword_keeps_method_context_and_a_lawful_repair() {
    use crate::{
        compilation_error::CompilationError, program_diagnostic::ProgramStageError,
        program_rejection::PythonLoweringError,
    };
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let tag = symbols.entity_sym_for("fixture", "Tag");
        let method = symbols.method_sym_for("fixture", "Tag", "tag_query");
        let source =
            format!("class P(Program):\n    def build(self):\n        return {tag}.{method}()\n");
        let error = compile_python_program(&es, &source).await.unwrap_err();
        let CompilationError::Program(stage) = &error else {
            panic!("program rejection")
        };
        let ProgramStageError::PythonLowering { error } = stage.as_ref() else {
            panic!("lowering rejection")
        };
        assert!(error.span_offset().is_some());
        let PythonLoweringError::Located { error: inner, .. } = error else {
            panic!("located rejection")
        };
        assert!(
            matches!(inner.as_ref(), PythonLoweringError::Type(cause) if matches!(cause.as_ref(), plasm_core::TypeError::RequiredParameterOmitted { parameter, expression } if parameter == "item_id" && expression == &format!("{tag}.{method}"))),
            "{inner:?}"
        );
        assert!(error.to_string().contains("item_id=value"));
        assert!(calls.lock().unwrap().is_empty());
        let repaired = source.replace(&format!(".{method}()"), &format!(".{method}(item_id='i0')"));
        let bundle = compile_python_program(&es, &repaired).await.unwrap();
        execute(&es, &host, &bundle).await.unwrap();
    });
}

#[test]
fn entity_binding_annotation_correction_has_an_executable_repair() {
    on_runtime(async {
        let (es, host, _) = fixture(1);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let source = format!("class P(Program):\n    def build(self):\n        return self.title({item}.get('i0'))\n    @compute\n    def title(self, row: {item}):\n        return row.title\n");
        let error = compile_python_program(&es, &source).await.unwrap_err();
        assert!(matches!(
            source_cause(&error),
            crate::program_rejection::PythonSourceError::ComputeEntityBindingAnnotation
        ));
        assert!(error.to_string().contains("catalog binding"), "{error}");
        for annotation in [String::new(), ": Row".into(), format!(": Value[{item}]")] {
            let repaired = source.replace(&format!(": {item}"), &annotation);
            let bundle = compile_python_program(&es, &repaired).await.unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(
                plasm_runtime::entity_to_agent_row_json(
                    &run.return_steps[0].result.entities()[0],
                    None
                )
                .unwrap()["value"],
                json!("Title 0")
            );
        }
    });
}

#[test]
fn unused_compute_still_checks_declarations_and_invoked_compute_checks_types() {
    on_runtime(async {
        let (es, _, calls) = fixture(0);
        for (method, decorator) in [("__value", "@compute"), ("_value", "@compute()")] {
            let source = format!("class P(Program):\n    def build(self):\n        return 1\n    {decorator}\n    def {method}(self):\n        return 1\n");
            let error = compile_python_program(&es, &source).await.unwrap_err();
            let cause = source_cause(&error);
            if method.starts_with("__") {
                assert!(
                    matches!(cause, crate::program_rejection::PythonSourceError::ReservedComputeName { name } if name == method)
                );
            } else {
                assert!(matches!(
                    cause,
                    crate::program_rejection::PythonSourceError::ComputeDecoratorShape
                ));
            }
        }
        let source = "class P(Program):\n    def build(self):\n        return self._value(1)\n    @compute\n    def _value(self, x) -> str:\n        return x + 1\n";
        assert!(compile_python_program(&es, source).await.is_err());
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn reserved_build_binding_is_named_even_after_return() {
    use crate::{
        compilation_error::CompilationError,
        program_diagnostic::ProgramStageError,
        program_rejection::{PythonLoweringError, PythonProgramError},
    };
    on_runtime(async {
        let (es, _, calls) = fixture(0);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        for statements in [
            format!("{item} = 1\n        return {item}"),
            format!("return 1\n        {item} = 1"),
        ] {
            let source = format!("class P(Program):\n    def build(self):\n        {statements}\n");
            let error = compile_python_program(&es, &source).await.unwrap_err();
            let CompilationError::Program(stage) = error else {
                panic!("program rejection")
            };
            assert!(
                matches!(stage.as_ref(), ProgramStageError::PythonLowering { error: PythonLoweringError::Program(PythonProgramError::BuildLocalShadowsReservedBinding { name }) } if name == &item)
            );
        }
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn projected_row_scalar_annotation_repairs_with_explicit_cell() {
    on_runtime(async {
        let (es, host, calls) = fixture(1);
        let item = es
            .teaching_exposure
            .as_ref()
            .unwrap()
            .to_symbol_map()
            .entity_sym_for("fixture", "Item");
        let source = format!("class P(Program):\n    def build(self):\n        return self.render({item}.get('i0').select('title'), '!')\n    @compute\n    def render(self, title: str, suffix: str):\n        return title + suffix\n");
        let error = compile_python_program(&es, &source).await.unwrap_err();
        assert!(error.to_string().contains("singleton.field"), "{error}");
        assert!(calls.lock().unwrap().is_empty());
        let repaired = source.replace(".select('title')", ".title");
        let bundle = compile_python_program(&es, &repaired).await.unwrap();
        let run = execute(&es, &host, &bundle).await.unwrap();
        assert_eq!(
            plasm_runtime::entity_to_agent_row_json(
                &run.return_steps[0].result.entities()[0],
                None
            )
            .unwrap()["value"],
            json!("Title 0!")
        );
    });
}

#[test]
fn local_callback_invocation_preserves_collection_cardinality_and_annotations() {
    on_runtime(async {
        let (es, host, calls) = fixture(0);
        for (value, annotation, expected) in [
            ("[]", "", 0),
            ("[]", ": list[int]", 0),
            ("[1]", ": list[int]", 1),
            ("[1, 2]", ": list[int]", 2),
            ("[[1], [2, 3]]", "", 2),
        ] {
            let source = format!("class P(Program):\n    def build(self):\n        def count(values{annotation}) -> int:\n            return len(values)\n        return count({value})\n");
            let bundle = compile_python_program(&es, &source).await.unwrap();
            let run = execute(&es, &host, &bundle).await.unwrap();
            assert_eq!(
                plasm_runtime::entity_to_agent_row_json(
                    &run.return_steps[0].result.entities()[0],
                    None
                )
                .unwrap()["value"],
                json!(expected)
            );
        }
        for signature in ["values: str", "__values: list[int]"] {
            let source = format!("class P(Program):\n    def build(self):\n        def count({signature}) -> int:\n            return 1\n        return count([1, 2])\n");
            assert!(compile_python_program(&es, &source).await.is_err());
        }
        let source = "class P(Program):\n    def build(self):\n        def count(values: list[int]) -> str:\n            return len(values)\n        return count([1, 2])\n";
        assert!(compile_python_program(&es, source).await.is_err());
        assert!(calls.lock().unwrap().is_empty());
    });
}
