//! Recursive literal constructors; contextual consumers retain their own shape rules.
use super::*;
pub(super) enum LiteralOperand<'a> {
    Text(&'a PyExpr),
    Number(&'a ruff_python_ast::ExprNumberLiteral),
    Signed(&'a ruff_python_ast::ExprUnaryOp),
    Boolean(bool),
    Null(()),
    Array(&'a ruff_python_ast::ExprList),
    Record(&'a ruff_python_ast::ExprDict),
}
impl<'a> LiteralOperand<'a> {
    pub(super) fn classify(e: &'a PyExpr) -> Option<Self> {
        Some(match e {
            PyExpr::StringLiteral(_) => Self::Text(e),
            PyExpr::NumberLiteral(n) => Self::Number(n),
            PyExpr::UnaryOp(n)
                if matches!(
                    n.op,
                    ruff_python_ast::UnaryOp::UAdd | ruff_python_ast::UnaryOp::USub
                ) && matches!(&*n.operand, PyExpr::NumberLiteral(_)) =>
            {
                Self::Signed(n)
            }
            PyExpr::BooleanLiteral(b) => Self::Boolean(b.value),
            PyExpr::NoneLiteral(_) => Self::Null(()),
            PyExpr::List(list) => Self::Array(list),
            PyExpr::Dict(dict) => Self::Record(dict),
            _ => return None,
        })
    }
    pub(super) fn scalar(self, e: &PyExpr) -> Result<plasm_core::Value, String> {
        match self {
            Self::Signed(unary) => {
                let PyExpr::NumberLiteral(number) = &*unary.operand else {
                    return Err(at(e, "signed value requires a numeric literal"));
                };
                let negative = unary.op == ruff_python_ast::UnaryOp::USub;
                match &number.value {
                    ruff_python_ast::Number::Int(value) => {
                        // Parse the sign with the magnitude: i64::MIN has no positive i64.
                        let spelling = format!("{}{value}", if negative { "-" } else { "" });
                        Ok(plasm_core::Value::Integer(
                            spelling
                                .parse()
                                .map_err(|_| at(e, "integer out of range"))?,
                        ))
                    }
                    ruff_python_ast::Number::Float(value) if value.is_finite() => {
                        Ok(plasm_core::Value::Float(if negative {
                            -*value
                        } else {
                            *value
                        }))
                    }
                    _ => Err(at(e, "expected a finite real number")),
                }
            }
            Self::Text(expr) => Ok(plasm_core::Value::String(string(expr)?)),
            Self::Number(n) => match &n.value {
                ruff_python_ast::Number::Float(value) if value.is_finite() => {
                    Ok(plasm_core::Value::Float(*value))
                }
                ruff_python_ast::Number::Int(_) => Ok(plasm_core::Value::Integer(integer(e)?)),
                _ => Err(at(e, "expected a finite real number")),
            },
            Self::Null(()) => Ok(plasm_core::Value::Null),
            Self::Boolean(b) => Ok(plasm_core::Value::Bool(b)),
            Self::Array(_) | Self::Record(_) => Err(at(
                e,
                "expected a string, finite number, boolean or null literal",
            )),
        }
    }
}
