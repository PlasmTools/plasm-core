use super::*;

fn parse(source: &str) -> Result<plasm_core::Value, String> {
    let parsed = ruff_python_parser::parse_module(source).unwrap();
    let [Stmt::Expr(expression)] = parsed.suite().as_slice() else {
        panic!("expected expression");
    };
    literal(&expression.value)
}

#[test]
fn signed_numeric_literals_preserve_exact_domains() {
    for (source, value) in [
        ("-9223372036854775808", i64::MIN),
        ("+9223372036854775807", i64::MAX),
        ("-0x80", -128),
        ("+0b11", 3),
    ] {
        assert_eq!(parse(source).unwrap(), plasm_core::Value::Integer(value));
    }
    for (source, value) in [("-1.25", -1.25_f64), ("+1.25", 1.25), ("-0.0", -0.0)] {
        let plasm_core::Value::Float(actual) = parse(source).unwrap() else {
            panic!("lost float domain for {source}");
        };
        assert_eq!(actual.to_bits(), value.to_bits());
    }
    for source in [
        "-9223372036854775809",
        "+9223372036854775808",
        "-1e999",
        "+1e999",
        "-True",
        "+False",
        "-1j",
        "-value",
    ] {
        assert!(parse(source).is_err(), "accepted {source}");
    }
}
