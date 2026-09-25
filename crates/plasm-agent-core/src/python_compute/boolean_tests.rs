use super::*;

fn check(source: &str) -> Result<Kind, String> {
    let ast = ruff_python_parser::parse_module(source).unwrap();
    let [Stmt::Expr(expression)] = ast.suite().as_slice() else {
        panic!("expected expression");
    };
    infer(
        &expression.value,
        &mut BTreeMap::from([
            ("optional".into(), Kind::OptionalString),
            ("score".into(), Kind::Integer),
            ("rows".into(), Kind::Rows),
        ]),
        &BTreeMap::new(),
        &mut BTreeSet::new(),
    )
}

#[test]
fn scalar_boolean_alternatives_preserve_possible_results() {
    assert!(matches!(check("optional or ''").unwrap(), Kind::String));
    assert!(matches!(
        check("(optional or '').strip()").unwrap(),
        Kind::String
    ));
    assert!(matches!(
        check("score or '—'").unwrap(),
        Kind::ScalarUnion(_)
    ));
    assert!(matches!(
        check("f\"{score or '—'}\"").unwrap(),
        Kind::String
    ));
    assert!(matches!(check("None and score").unwrap(), Kind::Null));
    for source in [
        "(optional and '').strip()",
        "(score or '—').strip()",
        "rows or ''",
        "True or undeclared",
    ] {
        assert!(check(source).is_err(), "accepted {source}");
    }
}

#[test]
fn compute_literals_keep_integer_and_number_distinct() {
    for source in ["-7", "+7", "7"] {
        assert!(matches!(check(source).unwrap(), Kind::Integer));
    }
    for source in ["-1.25", "+1.25", "1.25"] {
        assert!(matches!(check(source).unwrap(), Kind::Number));
    }
    for source in ["-1e999", "+1e999", "1j", "-True"] {
        assert!(check(source).is_err(), "accepted {source}");
    }
}
