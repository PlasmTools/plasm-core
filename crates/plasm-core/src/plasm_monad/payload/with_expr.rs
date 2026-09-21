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

impl WithExpr {
    /// Exhaustive structural traversal shared by name resolution and row dependency analysis.
    pub fn try_map_fields<E>(
        &self,
        visit: &mut impl FnMut(&FieldPath) -> Result<FieldPath, E>,
    ) -> Result<Self, E> {
        Ok(match self {
            Self::Field(path) => Self::Field(visit(path)?),
            Self::Len { field } => Self::Len {
                field: visit(field)?,
            },
            Self::Literal(value) => Self::Literal(value.clone()),
            Self::Now => Self::Now,
            Self::Arith { op, lhs, rhs } => Self::Arith {
                op: *op,
                lhs: Box::new(lhs.try_map_fields(visit)?),
                rhs: Box::new(rhs.try_map_fields(visit)?),
            },
            Self::When {
                lhs,
                op,
                rhs,
                then,
                else_,
            } => Self::When {
                lhs: Box::new(lhs.try_map_fields(visit)?),
                op: *op,
                rhs: Box::new(rhs.try_map_fields(visit)?),
                then: Box::new(then.try_map_fields(visit)?),
                else_: Box::new(else_.try_map_fields(visit)?),
            },
        })
    }
}
