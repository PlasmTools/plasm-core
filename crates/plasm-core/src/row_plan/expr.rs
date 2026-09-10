//! Projection spec — distinct from `.with` columns.

use crate::plasm_monad::payload::{FieldPath, OutputName};
use serde::{Deserialize, Serialize};

pub use crate::plasm_monad::{ArithOp, WithColumn, WithExpr, WithExprError, WithLiteral};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSpec {
    pub fields: std::collections::BTreeMap<OutputName, FieldPath>,
}
