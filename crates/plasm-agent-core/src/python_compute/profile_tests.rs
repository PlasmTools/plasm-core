//! Representatives of syntax families, not a second Python grammar.
use super::*;

#[tokio::test]
async fn profile_syntax_families_check_and_execute() {
    let pool = crate::python_pool::PythonPool::default();
    for (family, body, expected) in [
        ("assignment_unpack_augmented", "a, *rest = [1, 2, 3]\n    a += sum(rest)\n    return str(a)", "6"),
        ("while_break_continue_else", "n = 0\n    while n < 8:\n        n += 1\n        if n < 2:\n            continue\n        if n == 3:\n            break\n    else:\n        n = 99\n    return str(n)", "3"),
        ("for_if_elif_else", "out = []\n    for n in range(3):\n        if n == 0:\n            out.append('a')\n        elif n == 1:\n            out.append('b')\n        else:\n            out.append('c')\n    return ''.join(out)", "abc"),
        ("exceptions_assert_pass", "out = 'a'\n    try:\n        assert False, 'expected'\n    except AssertionError:\n        out = 'b'\n    else:\n        pass\n    finally:\n        out += 'c'\n    return out", "bc"),
        ("nested_function_closure_lambda", "prefix = 'x'\n    def inner(n: int) -> str:\n        return prefix + str(n)\n    f = lambda n: inner(n)\n    return f(2)", "x2"),
        ("comprehensions_generators", "values = [n * 2 for n in range(4) if n > 0]\n    names = {str(n): n for n in values}\n    unique = {n for n in values}\n    return '|'.join(str(names[str(n)]) for n in sorted(unique))", "2|4|6"),
        ("operators_comparisons_conditionals", "n = -7 + 2 * 3\n    return str((n < 0 and 'ab' in 'abc') or False) if n != 0 else 'zero'", "True"),
        ("slice_subscript_walrus", "items = {'xs': [1, 2, 3]}\n    if (n := len(items['xs'])) > 1:\n        return str(items['xs'][1:])\n    return str(n)", "[2, 3]"),
        ("strings_formatting", "name = 'ab'\n    return f'{name!s:>3}' + '-{}'.format(3) + ('-%d' % 4)", " ab-3-4"),
        ("operand_selection_and_chained_calls", "def matches(description: str | None) -> bool:\n        return 'grocery' in (description or '').lower()\n    return str([matches(s) for s in [None, '', 'GROCERY bill', 'other']])", "[False, False, True, False]"),
        ("operand_selection_laziness", "def explode() -> str:\n        raise ValueError('must not execute')\n    return str([None or 'fallback', '' or 'fallback', 'kept' or explode(), 0 and explode(), [] or [1]])", "['fallback', 'fallback', 'kept', 0, [1]]"),
        ("local_mutation", "items = [1, 2]\n    items.clear()\n    return str(items)", "[]"),
        ("annotated_assignment", "n: int = 3\n    return str(n)", "3"),
        ("classes_context_managers", "class Scope:\n        def __enter__(self) -> str:\n            return 'inside'\n        def __exit__(self, kind, value, trace) -> bool:\n            return False\n    with Scope() as value:\n        return value", "inside"),
        ("nested_decorator", "def identity(f):\n        return f\n    @identity\n    def inner() -> str:\n        return 'decorated'\n    return inner()", "decorated"),
        ("nested_async_definitions", "async def inner() -> str:\n        return 'unused'\n    async def outer() -> str:\n        return await inner()\n    return 'defined'", "defined"),
        ("starred_calls", "def add(a: int, b: int) -> str:\n        return str(a + b)\n    return add(*[1, 2])", "3"),
        ("raise_from", "try:\n        raise ValueError('x') from None\n    except ValueError as error:\n        return str(error)", "x"),
    ] {
        let definition = format!("def render() -> str:\n    {body}\n");
        super::super::admission::check_definition(&definition, "").unwrap_or_else(|e| panic!("{family} admission: {e}"));
        let result = pool.compute(format!("{definition}\nrender()"), vec![]).await
            .unwrap_or_else(|e| panic!("{family} execution: {e}"));
        assert_eq!(result, expected, "{family}");
    }
    pool.close().await;
}

#[test]
fn profile_upstream_rejections_are_pre_execution() {
    for body in [
        "async def inner():\n        async with None:\n            pass\n    return ''",
        "async def inner():\n        async for x in []:\n            pass\n    return ''",
        "async def inner():\n        return [x async for x in []]\n    return ''",
        "class C:\n        @staticmethod\n        def f():\n            return 1\n    return ''",
        "return str(t'hello')",
        "yield 'x'",
        "match 1:\n        case 1:\n            return 'x'\n    return ''",
        "xs = [1]\n    del xs[0]\n    return ''",
        "type Alias = int\n    return ''",
        "return str(1j)",
        "try:\n        pass\n    except* ValueError:\n        pass\n    return ''",
        "class Child(str):\n        pass\n    return ''",
    ] {
        let source = format!("def render() -> str:\n    {body}\n");
        assert!(
            super::super::admission::check_definition(&source, "").is_err(),
            "accepted {body}"
        );
    }
}

#[test]
fn generated_boundary_members_remain_sealed() {
    for name in ["class", "x: str\n    injected", "__dict__", "a.b"] {
        assert!(validate_member(name).is_err(), "accepted {name}");
    }
}

#[tokio::test]
async fn compute_capabilities_are_enforced_by_host_interactions() {
    let pool = crate::python_pool::PythonPool::default();
    // Valid Python spellings do not grant host authority. No environment value
    // or file content is returned to the program.
    for source in [
        "import random\nstr(random.random())",
        "import time\ntime.sleep(1)",
        "import os\nos.getenv('PLASM_UNEXPOSED_TEST_VALUE')",
        "from pathlib import Path\nPath('/tmp/plasm-unexposed').read_text()",
    ] {
        assert!(pool.compute(source.into(), vec![]).await.is_err());
    }
    for source in [
        "def f():\n    open = 'local'\n    return open\nf()",
        "import math\nstr(math.sqrt(9))",
        "import random\nrandom.seed(123)\nstr(random.randint(1, 1))",
    ] {
        super::super::admission::check_definition(source, "").unwrap();
        assert!(pool.compute(source.into(), vec![]).await.is_ok());
    }
    pool.close().await;
}

#[tokio::test]
async fn generated_control_flow_crosses_recursive_boundary_types() {
    use crate::fixture_value as json;
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_value_contract"),
    )
    .unwrap();
    let record = Type {
        shape: ValueShape::Record {
            fields: BTreeMap::from([("n".into(), Type::scalar(FieldType::Integer))]),
        },
        domain: None,
        nullable: false,
    };
    let nested = Type {
        shape: ValueShape::Array {
            element: Box::new(record.clone()),
        },
        domain: None,
        nullable: false,
    };
    let optional = Type {
        nullable: true,
        ..Type::scalar(FieldType::String)
    };
    let union = Type {
        shape: ValueShape::Union {
            variants: vec![record, Type::scalar(FieldType::String)],
        },
        domain: None,
        nullable: false,
    };
    let cases = [
        (
            Type::scalar(FieldType::Integer),
            json!(9007199254740993_i64),
        ),
        (Type::scalar(FieldType::Boolean), json!(true)),
        (Type::scalar(FieldType::String), json!("Unicode Ω")),
        (optional.clone(), json!(null)),
        (optional, json!("present")),
        (nested.clone(), json!([])),
        (nested, json!([{"n": 1}, {"n": 2}])),
        (union.clone(), json!("alternate")),
        (union, json!({"n": 3})),
        (
            Type::from_domain(
                &cgs,
                "types",
                &plasm_core::ValueDomainKey::new("state").unwrap(),
            )
            .unwrap(),
            json!("open"),
        ),
    ];
    let pool = crate::python_pool::PythonPool::default();
    for (ty, value) in cases {
        ty.validate(&value, &cgs, "types", "value").unwrap();
        let types = BTreeMap::from([("value".into(), ty)]);
        let declarations = stubs(&types, &cgs).unwrap();
        for body in [
            "value = row.value\n    return 'none' if value is None else 'present'",
            "return '|'.join('none' if value is None else 'present' for value in [row.value])",
            "for value in [row.value]:\n        if value is None:\n            return 'none'\n        else:\n            return 'present'\n    return 'empty'",
        ] {
            let definition = format!("def render(row: PlasmInput) -> str:\n    {body}\n");
            super::super::admission::check_definition(&definition, &declarations).unwrap();
            let result = pool.compute_typed(format!("{definition}\nrender(__input[0])"),
                vec![BTreeMap::from([("value".into(), value.clone())])], &types).await.unwrap();
            assert_eq!(result, if value.is_null() { "none" } else { "present" });
        }
    }
    pool.close().await;
}
