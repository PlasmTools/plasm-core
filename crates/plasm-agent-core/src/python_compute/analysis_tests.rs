#[test]
fn structured_analysis_preserves_python_types_without_execution() {
    use monty_types::analysis::{AnalysisLimits, AnalysisOutcome, AnalysisRequest, Node, Span};
    let stubs = "class Input:\n    description: str | None\n    values: list[int]\n";
    for (expression, expected) in [
        ("'needle' in (row.description or '').lower()", "bool"),
        ("(row.description or '').lower()", "str"),
        ("[n * 2 for n in row.values][0]", "int"),
        ("any(n > 0 for n in row.values)", "bool"),
    ] {
        // Module execution would raise. Analysis must neither call the function
        // nor execute the surrounding module to infer its return expression.
        let source = format!("def f(row: Input):\n    return {expression}\nraise RuntimeError('planning executed source')\n");
        let start = source.find(expression).unwrap() as u32;
        let result = monty_analysis::analyze(&AnalysisRequest {
            source,
            stubs: Some(stubs.into()),
            targets: vec![Span {
                start,
                end: start + expression.len() as u32,
            }],
            limits: AnalysisLimits::default(),
        })
        .unwrap();
        let AnalysisOutcome::Inferred(graph) = result.outcome else {
            panic!("{expression}: {:?}", result.outcome);
        };
        assert!(
            matches!(&graph.nodes[graph.roots[0].0 as usize], Node::Instance { identity, .. } if identity.path.last().is_some_and(|name| name == expected)),
            "{expression}: {graph:?}"
        );
    }
    // Authored errors remain structured diagnostics, not transport failures.
    let expression = "row.absent";
    let source = format!("def f(row: Input):\n    return {expression}\n");
    let start = source.find(expression).unwrap() as u32;
    let result = monty_analysis::analyze(&AnalysisRequest {
        source,
        stubs: Some(stubs.into()),
        targets: vec![Span {
            start,
            end: start + expression.len() as u32,
        }],
        limits: AnalysisLimits::default(),
    })
    .unwrap();
    assert!(matches!(result.outcome, AnalysisOutcome::Rejected(ref errors) if !errors.is_empty()));
}

#[test]
fn structured_analysis_retains_nominals_presence_and_null_independently() {
    use monty_types::analysis::{AnalysisLimits, AnalysisOutcome, AnalysisRequest, Node, Span};
    let stubs = "from typing import NewType, TypedDict, NotRequired\nToken = NewType('Token', str)\nclass Record(TypedDict):\n    key: Token\n    note: NotRequired[str | None]\nvalue: Record\n";
    let result = monty_analysis::analyze(&AnalysisRequest {
        source: "value".into(),
        stubs: Some(stubs.into()),
        targets: vec![Span { start: 0, end: 5 }],
        limits: AnalysisLimits::default(),
    })
    .unwrap();
    let AnalysisOutcome::Inferred(graph) = result.outcome else {
        panic!("{:?}", result.outcome);
    };
    let Node::Record { fields, .. } = &graph.nodes[graph.roots[0].0 as usize] else {
        panic!("{graph:?}");
    };
    let key = fields.iter().find(|f| f.name == "key").unwrap();
    assert!(key.required);
    assert!(
        matches!(&graph.nodes[key.ty.0 as usize], Node::NewType { identity, .. } if identity.source == "/analysis_stubs.pyi" && identity.path == ["Token"])
    );
    let note = fields.iter().find(|f| f.name == "note").unwrap();
    assert!(!note.required);
    let Node::Union(variants) = &graph.nodes[note.ty.0 as usize] else {
        panic!("nullable field lost its union: {graph:?}");
    };
    assert!(variants
        .iter()
        .any(|id| graph.nodes[id.0 as usize] == Node::None));
}

#[test]
fn static_definition_admission_neither_executes_nor_needs_a_worker() {
    super::admission::check_definition(
        "def f(value: str) -> str:\n    return value.lower()\nraise RuntimeError('analysis executed Python')\n",
        "",
    ).unwrap();
    assert!(
        super::admission::check_definition("def f() -> int:\n    return 'wrong'\n", "").is_err()
    );
}

#[test]
fn static_checker_failures_are_not_source_corrections() {
    let stubs = " ".repeat(monty_analysis::AnalysisLimits::default().max_source_bytes as usize);
    assert!(matches!(
        super::admission::check_definition("def f() -> int:\n    return 1\n", &stubs),
        Err(crate::compilation_error::CompilationError::Host(_))
    ));
    assert!(matches!(
        super::admission::check_definition("def f() -> int:\n    return 'wrong'\n", ""),
        Err(crate::compilation_error::CompilationError::Program(_))
    ));
}

#[test]
fn static_language_admission_checks_without_invocation() {
    let stubs = "class Input:\n    title: str\n    labels: list[str]\n    optional: str | None\n";
    for (source, accepted) in [
        ("def render(row: Input) -> str:\n    result = []\n    for label in row.labels:\n        if ',' in label:\n            result.append(label.replace(',', '|'))\n        else:\n            result.append(label)\n    return ','.join(result)\n", true),
        ("def render(row: Input) -> str:\n    return row.optional.upper() if row.optional is not None else ''\n", true),
        ("def render(row: Input) -> str:\n    raise ValueError('must never execute during admission')\n", true),
        ("def render(row: Input) -> str:\n    return row.absent\n", false),
        ("def render(row: Input) -> str:\n    return row.optional.upper()\n", false),
        ("def render(row: Input) -> str:\n    return 12\n", false),
        ("def render(row: Input) -> str:\n    return hidden(row.title)\n", false),
    ] {
        let result = super::admission::check_definition(source, stubs);
        assert_eq!(result.is_ok(), accepted, "{source}: {result:?}");
    }
}

#[test]
fn row_entity_annotation_matches_value_contract_recursively() {
    use super::*;
    use plasm_core::symbol_tuning::SymbolRender;
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_value_contract"),
    )
    .unwrap();
    let symbols =
        plasm_core::TeachingExposureSession::new(&cgs, "types", &["Sample"]).to_symbol_map();
    let token = symbols.entity_sym_for("types", "Sample");
    for (input, output, body) in [
        ("Value[E]", "Value[E]", "return row"),
        ("list[Value[E]]", "list[Value[E]]", "return row"),
        (
            "Value[E]",
            "Value[E] | None",
            "return row if row.flag else None",
        ),
        ("Value[E]", "list[list[Value[E]]]", "return [[row]]"),
    ] {
        let source = format!("@compute\ndef keep(row: {input}) -> {output}:\n    {body}\n")
            .replace("E", &token);
        let canonical = PreparedCompute::prepare(&source, &cgs, "types", symbols.as_ref()).unwrap();
        let alternate = PreparedCompute::prepare(
            &source.replace("Value[", "Row["),
            &cgs,
            "types",
            symbols.as_ref(),
        )
        .unwrap();
        assert_eq!(canonical.output, alternate.output);
        assert_eq!(canonical.definition, alternate.definition);
        assert_eq!(canonical.stubs, alternate.stubs);
        canonical.admit().unwrap();
        alternate.admit().unwrap();
    }
    let invalid = format!("@compute\ndef keep(row: Row[missing]) -> str:\n    return ''\n");
    assert!(PreparedCompute::prepare(&invalid, &cgs, "types", symbols.as_ref()).is_err());
}
