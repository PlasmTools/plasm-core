//! Row execution uses the shared typed value-expression semantics.
#[cfg(test)]
use super::{row, value as json};
pub(super) use plasm_core::value_expression::evaluate_with as evaluate;
#[cfg(test)]
use plasm_core::value_expression::{arithmetic, compare};
#[cfg(test)]
use plasm_core::Value;
#[cfg(test)]
use plasm_core::{ArithOp, FieldPath, PlanPredicateOp, WithExpr, WithLiteral};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_branch_alone_demands_fields_and_preserves_heterogeneous_values() {
        use plasm_core::{ComputeOp, OutputName, WithColumn};
        let field = |name| WithExpr::Field(FieldPath::from_dotted(name).unwrap());
        let rows = vec![
            row!({"flag":true,"a":9007199254740993_i64}),
            row!({"flag":false,"b":"[1]"}),
        ];
        let expression = WithExpr::When {
            lhs: Box::new(field("flag")),
            op: PlanPredicateOp::Eq,
            rhs: Box::new(WithExpr::Literal(WithLiteral::Bool(true))),
            then: Box::new(field("a")),
            else_: Box::new(field("b")),
        };
        let ops = [ComputeOp::With {
            columns: vec![WithColumn {
                name: OutputName::new("out").unwrap(),
                expr: expression,
            }],
        }];
        let super::super::ComputeEvalOutcome::Rows(plasm_core::CollectedFrame {
            rows: actual, ..
        }) = super::super::evaluate_fixture(&ops, &rows).unwrap()
        else {
            panic!("rows")
        };
        assert_eq!(
            actual,
            vec![
                row!({"flag":true,"a":9007199254740993_i64,"out":9007199254740993_i64}),
                row!({"flag":false,"b":"[1]","out":"[1]"})
            ]
        );
        assert!(matches!(super::super::eval_compute_ops(
            &ops,
            &[row!({"flag":true,"b":"[1]"})],
            &super::super::fixture_contract(&rows)
        )
        .unwrap_err(), plasm_core::RowComputeError::MissingField { field } if field == "a"));
    }

    #[test]
    fn checked_arithmetic_has_no_silent_null_overflow_or_currency_conversion() {
        use plasm_core::value_expression::ArithmeticError;
        use ArithOp::*;
        for (op, left, right, expected) in [
            (
                Add,
                json!(9007199254740993_i64),
                json!(1),
                json!(9007199254740994_i64),
            ),
            (Sub, json!(2), json!(3), json!(-1)),
            (Mul, json!(2), json!(1.25), json!(2.5)),
            (Div, json!(7), json!(2), json!(3.5)),
            (Add, Value::Null, json!(7), Value::Null),
        ] {
            assert_eq!(arithmetic(op, left, right).unwrap(), expected);
        }
        assert_eq!(
            arithmetic(Add, json!(i64::MAX), json!(1)).unwrap_err(),
            ArithmeticError::IntegerOutOfRange
        );
        assert_eq!(
            arithmetic(Mul, json!(f64::MAX), json!(2.0)).unwrap_err(),
            ArithmeticError::NonFiniteResult
        );
        assert_eq!(
            arithmetic(Div, json!(1), json!(0)).unwrap_err(),
            ArithmeticError::DivisionByZero
        );
        assert!(arithmetic(Sub, json!(i64::MIN), json!(1)).is_err());
        assert!(arithmetic(Mul, json!(i64::MAX), json!(2)).is_err());
        assert!(arithmetic(Div, json!(1), json!(-0.0)).is_err());
        let money = |amount, currency| json!({"__plasm_money":amount,"currency":currency});
        assert_eq!(
            arithmetic(
                Add,
                money("1.0000000000000000001", "USD"),
                money("2", "USD")
            )
            .unwrap(),
            money("3.0000000000000000001", "USD")
        );
        assert_eq!(
            arithmetic(Add, money("1", "USD"), money("1", "EUR")).unwrap_err(),
            ArithmeticError::MoneyCurrencyMismatch
        );
        assert_eq!(
            arithmetic(Add, money("1", "USD"), json!({"__plasm_money":"1"})).unwrap_err(),
            ArithmeticError::MoneyCurrencyMismatch
        );
        assert!(arithmetic(Mul, money("1", "USD"), money("1", "USD")).is_err());
        assert!(arithmetic(Div, money("1", "USD"), json!(0)).is_err());
    }

    #[test]
    fn conditional_comparison_does_not_round_large_integers() {
        use plasm_core::value_expression::ComparisonError;
        assert!(compare(
            PlanPredicateOp::Gt,
            &json!(9007199254740993_i64),
            &json!(9007199254740992_i64)
        )
        .unwrap());
        assert!(!compare(
            PlanPredicateOp::Eq,
            &json!(9007199254740993_i64),
            &json!(9007199254740992_i64)
        )
        .unwrap());
        assert_eq!(
            compare(
                PlanPredicateOp::Gt,
                &Value::Float(f64::INFINITY),
                &Value::Float(0.0),
            )
            .unwrap_err(),
            ComparisonError::NonFiniteNumber
        );
    }
}
