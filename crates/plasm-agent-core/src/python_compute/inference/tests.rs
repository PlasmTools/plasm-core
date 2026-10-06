use super::*;
use plasm_core::{value_contract::DomainRef, ValueDomainKey};

fn fields(items: &[(&str, Type)]) -> Type {
    Type::record(
        items
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect(),
        Default::default(),
    )
}

fn dictionary(value: Type) -> Type {
    Type {
        shape: ValueShape::Dictionary {
            key: Box::new(Type::scalar(FieldType::String)),
            value: Box::new(value),
        },
        domain: None,
        nullable: false,
    }
}

#[test]
fn annotated_nested_helper_closes_collection_return() {
    let row = fields(&[
        ("song_id", Type::scalar(FieldType::Integer)),
        ("title", Type::scalar(FieldType::String)),
    ]);
    let body = "\n    def fmt(items: list) -> list[str]:\n        out = []\n        for item in items:\n            out.append(str(item.song_id) + ' | ' + item.title)\n        return out\n    return {'current': fmt(rows)}";
    assert_eq!(
        infer_body(body, &[("rows", &array(row))], "").unwrap(),
        dictionary(array(Type::scalar(FieldType::String)))
    );
}

#[test]
fn unannotated_nested_helper_infers_materialized_return() {
    let row = fields(&[("number", Type::scalar(FieldType::Integer))]);
    let body = "\n    def doubled(items):\n        return [item.number * 2 for item in items]\n    return {'values': doubled(rows)}";
    assert_eq!(
        infer_body(body, &[("rows", &array(row))], "").unwrap(),
        dictionary(array(Type::scalar(FieldType::Integer)))
    );
    let song = fields(&[
        ("song_id", Type::scalar(FieldType::Integer)),
        ("title", Type::scalar(FieldType::String)),
    ]);
    let body = "\n    def fmt(items):\n        out = []\n        for item in items:\n            out.append(str(item.song_id) + ' | ' + item.title)\n        return out\n    return {'current': fmt(rows)}";
    assert_eq!(
        infer_body(body, &[("rows", &array(song))], "").unwrap(),
        dictionary(array(Type::scalar(FieldType::String)))
    );
}

#[test]
fn local_helper_inference_covers_loops_chains_closures_and_multiple_calls() {
    let row = fields(&[("number", Type::scalar(FieldType::Integer))]);
    let rows = array(row);
    let integers = array(Type::scalar(FieldType::Integer));
    for body in [
        "\n    def numbers(items):\n        out = []\n        for item in items:\n            out.append(item.number)\n        return out\n    return numbers(rows)",
        "\n    def first(items):\n        return second(items)\n    def second(items):\n        return [item.number for item in items]\n    return first(rows)",
        "\n    def numbers():\n        return [item.number for item in rows]\n    return numbers()",
        "\n    def numbers(items):\n        return [item.number for item in items]\n    return numbers(rows) + numbers(rows)",
    ] {
        assert_eq!(infer_body(body, &[("rows", &rows)], "").unwrap(), integers, "{body}");
    }
}

#[test]
fn local_helper_inference_does_not_admit_incompatible_calls() {
    let body = "\n    def increment(value):\n        return value + 1\n    return increment(2) + increment('wrong')";
    assert!(infer_body(body, &[], "").is_err());
}

#[test]
fn local_helpers_preserve_set_and_optional_return_contracts() {
    let row = fields(&[("number", Type::scalar(FieldType::Integer))]);
    let rows = array(row);
    let set_body = "\n    def unique(items):\n        values = set()\n        for item in items:\n            values.add(item.number)\n        return values\n    return unique(rows)";
    assert_eq!(
        infer_body(set_body, &[("rows", &rows)], "").unwrap(),
        Type {
            shape: ValueShape::Set {
                element: Box::new(Type::scalar(FieldType::Integer))
            },
            domain: None,
            nullable: false
        }
    );
    let optional_body = "\n    def first(items):\n        if items:\n            return items[0].number\n        return None\n    return first(rows)";
    let mut optional = Type::scalar(FieldType::Integer);
    optional.nullable = true;
    assert_eq!(
        infer_body(optional_body, &[("rows", &rows)], "").unwrap(),
        optional
    );
}

#[test]
fn local_helper_defaults_and_keyword_calls_share_one_input_contract() {
    let row = fields(&[("number", Type::scalar(FieldType::Integer))]);
    let rows = array(row);
    let body = "\n    def shifted(items, amount=1):\n        return [item.number + amount for item in items]\n    return shifted(rows) + shifted(items=rows, amount=2)";
    assert_eq!(
        infer_body(body, &[("rows", &rows)], "").unwrap(),
        array(Type::scalar(FieldType::Integer))
    );
}

#[test]
fn upstream_expression_contracts_preserve_python_semantics() {
    let mut optional = Type::scalar(FieldType::String);
    optional.nullable = true;
    let input = fields(&[
        ("description", optional),
        ("number", Type::scalar(FieldType::Integer)),
        ("values", array(Type::scalar(FieldType::Integer))),
    ]);
    for (expression, expected) in [
        (
            "'grocery' in (row.description or '').lower()",
            Type::scalar(FieldType::Boolean),
        ),
        ("row.number // 2", Type::scalar(FieldType::Integer)),
        (
            "[n * 2 for n in row.values][0]",
            Type::scalar(FieldType::Integer),
        ),
        (
            "sum(n for n in row.values if n > 0)",
            Type::scalar(FieldType::Integer),
        ),
        (
            "row.number or 'missing'",
            Type::join(
                Type::scalar(FieldType::Integer),
                Type::scalar(FieldType::String),
            ),
        ),
    ] {
        assert_eq!(
            infer(expression, "row", &input, "").unwrap(),
            expected,
            "{expression}"
        );
    }
}

#[test]
fn inferred_tuples_materialize_as_closed_ordered_arrays() {
    let input = fields(&[
        ("number", Type::scalar(FieldType::Integer)),
        ("title", Type::scalar(FieldType::String)),
    ]);
    assert_eq!(
        infer("(row.number, row.number + 1)", "row", &input, "").unwrap(),
        array(Type::scalar(FieldType::Integer))
    );
    assert_eq!(
        infer("(row.number, row.title)", "row", &input, "").unwrap(),
        array(Type::join(
            Type::scalar(FieldType::Integer),
            Type::scalar(FieldType::String),
        ))
    );
}

#[test]
fn nominal_selection_and_record_presence_survive_inference() {
    let mut id = Type::scalar(FieldType::String);
    id.domain = Some(DomainRef {
        entry_id: "catalog-a".into(),
        catalog_hash: "pin-a".into(),
        value_ref: ValueDomainKey::new("id").unwrap(),
    });
    let mut other = id.clone();
    other.domain.as_mut().unwrap().entry_id = "catalog-b".into();
    let observed = Type::record(
        BTreeMap::from([("id".into(), id.clone())]),
        ["id".into()].into(),
    );
    let input = fields(&[
        ("id", id.clone()),
        ("other", other.clone()),
        ("record", observed.clone()),
        ("flag", Type::scalar(FieldType::Boolean)),
    ]);
    assert_eq!(infer("row.id", "row", &input, "").unwrap(), id);
    assert_eq!(infer("row['id']", "row", &input, "").unwrap(), id);
    assert_eq!(infer("row['record']['id']", "row", &input, "").unwrap(), id);
    let dynamic = fields(&[
        (
            "value",
            fields(&[("id", id.clone()), ("other", other.clone())]),
        ),
        ("key", Type::scalar(FieldType::String)),
    ]);
    assert_eq!(
        infer("row.value[row.key]", "row", &dynamic, "").unwrap(),
        Type::join(id.clone(), other.clone())
    );
    assert_eq!(infer("row.record", "row", &input, "").unwrap(), observed);
    assert_eq!(
        infer("row.id if row.flag else row.other", "row", &input, "").unwrap(),
        Type::join(id.clone(), other)
    );
    assert_eq!(
        infer("{'id': row.id, 'nested': [row.record]}", "row", &input, "").unwrap(),
        dictionary(Type::join(id, array(observed)))
    );
    assert_eq!(
        infer("row.id.lower()", "row", &input, "").unwrap(),
        Type::scalar(FieldType::String)
    );
}

#[test]
fn unsupported_result_shapes_do_not_become_json() {
    let input = fields(&[]);
    for expression in ["lambda: 1", "{1: 2}"] {
        assert!(
            infer(expression, "row", &input, "").is_err(),
            "{expression}"
        );
    }
    assert_eq!(
        infer(
            "[{'n': n, 's': str(n)} for n in range(3)]",
            "row",
            &input,
            ""
        )
        .unwrap(),
        array(dictionary(Type::join(
            Type::scalar(FieldType::Integer),
            Type::scalar(FieldType::String)
        )))
    );
}

#[test]
fn exact_money_functions_have_static_contracts() {
    let money = Type::scalar(FieldType::Money);
    let input = fields(&[("price", money.clone())]);
    for expression in [
        "money_mul(row.price, '1.25')",
        "money_add(row.price, row.price)",
        "money_div(factor=2, value=row.price)",
    ] {
        assert_eq!(
            infer(expression, "row", &input, "").unwrap(),
            money,
            "{expression}"
        );
    }
    for expression in [
        "money_compare(row.price, 1)",
        "money_compare(row.price, '1.25')",
        "money_compare(right=row.price, left=row.price)",
    ] {
        assert_eq!(
            infer(expression, "row", &input, "").unwrap(),
            Type::scalar(FieldType::Integer)
        );
    }
    for expression in [
        "money_mul(row.price, 1.25)",
        "money_add(row.price, 1)",
        "money_mul(row.price, None)",
        "money_compare(row.price, 1.25)",
        "money_compare(row.price, None)",
    ] {
        assert!(
            infer(expression, "row", &input, "").is_err(),
            "{expression}"
        );
    }
}

#[test]
fn branch_contracts_are_general_and_do_not_leak_facts() {
    let mut optional = Type::scalar(FieldType::Integer);
    optional.nullable = true;
    let input = fields(&[("value", optional.clone()), ("other", optional.clone())]);
    let source = "@compute\ndef check(row: Row):\n    return row.value is not None\n";
    let facts = branch_contracts(
        source,
        &input,
        &[vec!["value".into()], vec!["other".into()]],
    )
    .unwrap()
    .unwrap();
    assert_eq!(facts[0].0, Type::scalar(FieldType::Integer));
    assert_eq!(facts[0].1.shape, ValueShape::Null);
    assert_eq!(facts[1], (optional.clone(), optional));
    let input = fields(&[(
        "value",
        Type::join(
            Type::scalar(FieldType::Integer),
            Type::scalar(FieldType::String),
        ),
    )]);
    let source = "@compute\ndef check(row: Row):\n    return isinstance(row.value, str)\n";
    let facts = branch_contracts(source, &input, &[vec!["value".into()]])
        .unwrap()
        .unwrap();
    assert_eq!(
        facts[0],
        (
            Type::scalar(FieldType::String),
            Type::scalar(FieldType::Integer)
        )
    );
}

#[test]
fn branch_literal_exclusions_project_to_sound_materialized_contracts() {
    let text = Type::scalar(FieldType::String);
    let input = fields(&[("value", text.clone())]);
    for predicate in ["row.value == 'skip'", "row.value != 'skip'", "row.value"] {
        let source = format!("def predicate(row: Row):\n    return {predicate}\n");
        let contracts = branch_contracts(&source, &input, &[vec!["value".into()]])
            .unwrap()
            .unwrap();
        assert_eq!(contracts, vec![(text.clone(), text.clone())]);
    }
}

#[test]
fn upstream_temporal_equality_has_a_boolean_contract() {
    let input = fields(&[]);
    for operand in [
        "datetime.fromisoformat('2024-01-01T01:00:00+01:00')",
        "datetime(2024, 1, 1, tzinfo=timezone.utc)",
    ] {
        assert_eq!(
            infer(operand, "row", &input, crate::python_datetime::PRELUDE).unwrap(),
            plasm_core::temporal_value::TemporalKind::Datetime.contract(),
            "{operand}"
        );
    }
    for expression in [
        "datetime.fromisoformat('2024-01-01T01:00:00+01:00') == datetime(2024, 1, 1, tzinfo=timezone.utc)",
        "date(2024, 1, 1) == date(2024, 1, 1)",
        "datetime(2024, 1, 1) != datetime(2024, 1, 2)",
        "timedelta(days=1) == timedelta(hours=24)",
    ] {
        assert_eq!(infer(expression, "row", &input, crate::python_datetime::PRELUDE).unwrap(),
            Type::scalar(FieldType::Boolean), "{expression}");
    }
}

#[test]
fn upstream_chained_guards_narrow_nullable_operands() {
    for mut value in [
        Type::scalar(FieldType::Integer),
        plasm_core::temporal_value::TemporalKind::Datetime.contract(),
    ] {
        let cutoff = value.clone();
        value.nullable = true;
        let input = fields(&[("value", value), ("cutoff", cutoff)]);
        assert_eq!(
            infer("None is not row.value < row.cutoff", "row", &input, "").unwrap(),
            Type::scalar(FieldType::Boolean)
        );
    }
}

#[test]
fn boolean_membership_branches_preserve_materialized_contracts() {
    for (nullable, nominal) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut flag = Type::scalar(FieldType::Boolean);
        flag.nullable = nullable;
        if nominal {
            flag.domain = Some(DomainRef {
                entry_id: "matrix".into(),
                catalog_hash: "pin".into(),
                value_ref: ValueDomainKey::new("flag").unwrap(),
            });
        }
        let input = fields(&[("value", flag.clone())]);
        for predicate in [
            "row.value is True",
            "row.value is not True",
            "row.value is False",
            "row.value is not False",
            "row.value",
        ] {
            let source = format!("def predicate(row: Row):\n    return {predicate}\n");
            let result = branch_contracts(&source, &input, &[vec!["value".into()]]);
            let branches = result
                .unwrap_or_else(|error| {
                    panic!("{predicate}, nullable={nullable}, nominal={nominal}: {error}")
                })
                .unwrap();
            for branch in [&branches[0].0, &branches[0].1] {
                assert_eq!(branch.domain, flag.domain, "{predicate}: domain erased");
                assert_eq!(branch.shape, flag.shape, "{predicate}: carrier changed");
            }
            if predicate == "row.value is True" || predicate == "row.value is False" {
                assert!(!branches[0].0.nullable);
                assert_eq!(branches[0].1.nullable, nullable);
            }
        }
    }
}

// Drive the exported graph boundary directly: upstream intersection ordering is
// not an authority ordering, including when metadata occurs under containers.
fn decode_intersection_contracts(values: &[Type]) -> Result<Type, InferenceError> {
    let mut declarations = declarations::Declarations::default();
    let mut nodes = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let name = format!("Sealed{index}");
        declarations.contracts.insert(name.clone(), value.clone());
        nodes.push(Node::Instance {
            identity: monty_analysis::Identity {
                source: "/analysis_stubs.pyi".into(),
                start: None,
                path: vec![name],
            },
            arguments: vec![],
        });
    }
    let root = TypeId(nodes.len() as u32);
    nodes.push(Node::Intersection {
        positive: (0..values.len()).map(|i| TypeId(i as u32)).collect(),
        negative: vec![],
    });
    let graph = Graph {
        roots: vec![root],
        nodes,
    };
    Decoder {
        graph: &graph,
        declarations: &declarations,
    }
    .decode(root, 0)
}

#[test]
fn intersection_metadata_is_recursive_and_order_independent() {
    let plain = Type::scalar(FieldType::String);
    let mut sealed = plain.clone();
    sealed.domain = Some(DomainRef {
        entry_id: "matrix".into(),
        catalog_hash: "pin".into(),
        value_ref: ValueDomainKey::new("identifier").unwrap(),
    });
    let temporal = plasm_core::temporal_value::TemporalKind::Datetime.contract();
    let mut wired = temporal.clone();
    let ValueShape::Temporal { wire, .. } = &mut wired.shape else {
        unreachable!()
    };
    *wire = Some(plasm_core::TemporalWireFormat::UnixMs);
    for (carrier, expected) in [
        (plain.clone(), sealed.clone()),
        (array(plain.clone()), array(sealed.clone())),
        (fields(&[("id", plain)]), fields(&[("id", sealed)])),
        (temporal, wired),
    ] {
        for operands in [
            vec![carrier.clone(), expected.clone()],
            vec![expected.clone(), carrier.clone()],
        ] {
            assert_eq!(decode_intersection_contracts(&operands).unwrap(), expected);
        }
        assert_eq!(
            decode_intersection_contracts(&[expected.clone(), expected.clone()]).unwrap(),
            expected
        );
    }
}

#[test]
fn nominal_literal_branch_constraints_preserve_nested_domains() {
    for (field_type, literal) in [
        (FieldType::Integer, "42"),
        (FieldType::String, "'selected'"),
        (FieldType::Uuid, "'123e4567-e89b-12d3-a456-426614174000'"),
        (FieldType::DigitId, "'000123'"),
        (FieldType::Select, "'open'"),
    ] {
        let mut value = Type::scalar(field_type);
        value.domain = Some(DomainRef {
            entry_id: "matrix".into(),
            catalog_hash: "pin".into(),
            value_ref: ValueDomainKey::new("value").unwrap(),
        });
        for nullable in [false, true] {
            value.nullable = nullable;
            let input = fields(&[("box", fields(&[("value", value.clone())]))]);
            for operator in ["==", "!="] {
                let source = format!(
                    "def predicate(row: Row):\n    return row.box.value {operator} {literal}\n"
                );
                let branches =
                    branch_contracts(&source, &input, &[vec!["box".into(), "value".into()]])
                        .unwrap_or_else(|e| panic!("{source}: {e}"))
                        .unwrap();
                for branch in [&branches[0].0, &branches[0].1] {
                    assert_eq!(branch.domain, value.domain, "{source}");
                    assert_eq!(branch.shape, value.shape, "{source}");
                }
                let selected = if operator == "==" {
                    &branches[0].0
                } else {
                    &branches[0].1
                };
                assert!(!selected.nullable, "{source}");
            }
        }
    }
}

#[test]
fn complete_body_inference_uses_upstream_locals_and_returns() {
    let input = fields(&[("number", Type::scalar(FieldType::Integer))]);
    let actual = infer_body("\n    n = row.number + 1\n    if n > 0:\n        return {'value': n}\n    return {'value': 0}\n", &[("row", &input)], "").unwrap();
    assert_eq!(actual, dictionary(Type::scalar(FieldType::Integer)));
}

#[test]
fn mutated_dictionary_inference_preserves_upstream_element_types() {
    let input = array(fields(&[("number", Type::scalar(FieldType::Integer))]));
    let result = infer_body("\n    result = []\n    for row in rows:\n        result.append({'value': row.number})\n    return result\n", &[("rows", &input)], "");
    assert_eq!(
        result.unwrap(),
        array(dictionary(Type::scalar(FieldType::Integer)))
    );
}

#[test]
fn whole_function_returns_include_upstream_implicit_none() {
    let input = Type::scalar(FieldType::Integer);
    let output = infer_body(
        "\n    if number > 0:\n        return number\n",
        &[("number", &input)],
        "",
    )
    .unwrap();
    assert!(output.nullable || matches!(output.shape, ValueShape::Union { .. }));
}

#[test]
fn annotation_resolution_is_upstream_owned() {
    let aliases = BTreeMap::new();
    let integer = Type::scalar(FieldType::Integer);
    assert_eq!(super::annotation("'int'", &aliases).unwrap(), integer);
    assert_eq!(
        super::annotation("list['int']", &aliases).unwrap(),
        array(integer)
    );
    assert!(matches!(
        super::annotation("dict[int, str]", &aliases).unwrap_err(),
        InferenceError::Graph(InferenceGraphError::DictionaryKeyNotString)
    ));
    assert!(super::annotation("list[int, str]", &aliases).is_err());
    assert!(super::annotation("NotAType", &aliases).is_err());
}

#[test]
fn inferred_returns_follow_upstream_reachability() {
    for body in [
        "\n    return 1\n    return\n",
        "\n    if False:\n        return\n    return 1\n",
        "\n    if True:\n        return 1\n    return 'unreachable'\n",
    ] {
        assert_eq!(
            infer_body(body, &[], "").unwrap(),
            Type::scalar(FieldType::Integer)
        );
    }
}

#[test]
fn annotated_returns_are_checked_before_literal_erasure() {
    let aliases = BTreeMap::new();
    for (annotation, body, expected) in [
        ("Literal['yes']", "\n    return 'yes'\n", true),
        ("Literal['yes']", "\n    return 'no'\n", false),
        ("int", "\n    return 'wrong'\n", false),
        (
            "int",
            "\n    def helper():\n        return 'wrong'\n    return helper()\n",
            false,
        ),
        (
            "int",
            "\n    def helper():\n        return 1\n    return helper()\n",
            true,
        ),
        (
            "Literal['yes']",
            "\n    def helper():\n        return 'yes'\n    return helper()\n",
            true,
        ),
        ("dict[str, int]", "\n    return {'n': 1}\n", true),
        ("dict[str, int]", "\n    return {'n': 'wrong'}\n", false),
    ] {
        assert_eq!(
            check_annotated_body(
                body,
                &[],
                annotation,
                &aliases,
                "from typing import Literal",
                &plasm_core::loader::load_schema_dir(
                    &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../fixtures/schemas/python_value_contract")
                )
                .unwrap(),
                &BTreeMap::new(),
            )
            .is_ok(),
            expected,
            "{annotation}: {body}"
        );
    }
}

#[test]
fn authored_record_returns_preserve_container_and_literal_constraints() {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_value_contract"),
    )
    .unwrap();
    let row = Type::record(
        BTreeMap::from([("n".into(), Type::scalar(FieldType::Integer))]),
        Default::default(),
    );
    let rows = Type {
        shape: ValueShape::Array {
            element: Box::new(row.clone()),
        },
        domain: None,
        nullable: false,
    };
    let aliases = BTreeMap::from([("Row".into(), row)]);
    for (annotation, body, accepted) in [
        ("list[Row]", "\n    return rows\n", true),
        ("list[Row]", "\n    return [{'n': 1}]\n", true),
        (
            "dict[Literal['fixed'], Row]",
            "\n    return {'wrong': {'n': 1}}\n",
            false,
        ),
        ("Literal['yes'] | Row", "\n    return 'no'\n", false),
    ] {
        assert_eq!(
            check_annotated_body(
                body,
                &[("rows", &rows)],
                annotation,
                &aliases,
                "from typing import Literal",
                &cgs,
                &BTreeMap::new()
            )
            .is_ok(),
            accepted,
            "{annotation}: {body}"
        );
    }
}
#[test]
fn checker_rejection_retains_original_record() {
    let record = monty_analysis::AnalysisDiagnostic {
        code: "invalid-argument-type".into(),
        message: "argument rejected".into(),
        source: Some("analysis.py".into()),
        span: None,
    };
    let failure = InferenceError::Diagnostics {
        diagnostics: vec![record.clone()].into(),
    };
    let InferenceError::Diagnostics { diagnostics } = &failure else {
        unreachable!()
    };
    assert_eq!(diagnostics.entries[0].diagnostic, record);
    assert!(std::error::Error::source(&failure).is_some());
    assert_eq!(
        diagnostics.to_string(),
        "invalid-argument-type: argument rejected"
    );
}
