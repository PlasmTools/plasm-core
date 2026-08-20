//! `.with` expression AST stored on hashed [`super::ComputeOp::With`].

use super::atoms::{FieldPath, OutputName};
use super::value::PlanPredicateOp;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithColumn {
    pub name: OutputName,
    pub expr: WithExpr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithExpr {
    Field(FieldPath),
    Literal(WithLiteral),
    Arith {
        op: ArithOp,
        lhs: Box<WithExpr>,
        rhs: Box<WithExpr>,
    },
    /// Catalog-plane clock token (`now` → UTC). A catalog field named `now` loses.
    Now,
    Len {
        field: FieldPath,
    },
    When {
        lhs: Box<WithExpr>,
        op: PlanPredicateOp,
        rhs: Box<WithExpr>,
        then: Box<WithExpr>,
        else_: Box<WithExpr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithLiteral {
    Null,
    Bool(bool),
    Integer(i64),
    Number(String),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WithExprError {
    #[error("empty .with body")]
    EmptyBody,
    #[error("invalid .with column `{0}`")]
    BadColumn(String),
    #[error("invalid .with expression: {0}")]
    Parse(String),
}
