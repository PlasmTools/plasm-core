//! Typed row-compute errors — no stringly engine failures.

use crate::identity::EntityName;
use crate::money::{CrossCurrencyError, MoneyError};
use crate::plasm_monad::ArithOp;
use thiserror::Error;

use super::schema::LogicalColumnType;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowComputeError {
    #[error(transparent)]
    Type(#[from] RowTypeError),
    #[error(transparent)]
    Money(#[from] MoneyError),
    #[error("cannot compare money in {left} to money in {right}")]
    CrossCurrency { left: String, right: String },
    #[error(transparent)]
    Schema(#[from] FrameSchemaError),
    #[error(transparent)]
    Collect(#[from] CollectError),
    #[error(transparent)]
    Expr(#[from] crate::plasm_monad::WithExprError),
    #[error(transparent)]
    Predicate(#[from] RowFilterError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Fusion(#[from] FusionError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowTypeError {
    #[error("arithmetic `{op:?}` is not defined for {lhs:?} and {rhs:?}")]
    ArithDomain {
        op: ArithOp,
        lhs: LogicalColumnType,
        rhs: LogicalColumnType,
    },
    #[error("when() branches have mismatched types {then:?} vs {else_:?}")]
    WhenBranchMismatch {
        then: LogicalColumnType,
        else_: LogicalColumnType,
    },
    #[error("temporal arithmetic requires a temporal value, got {got:?}")]
    TemporalArithNotTemporal { got: LogicalColumnType },
    #[error("money must not be stored as Utf8")]
    MoneyStoredAsUtf8,
    #[error("project spec cannot be used as a .with column")]
    ProjectIntoWith,
    #[error(".with must preserve entity identity")]
    WithBreaksEntityShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameSchemaError {
    #[error("unknown column `{0}`")]
    UnknownColumn(String),
    #[error("empty pipeline is illegal")]
    EmptyPipeline,
    #[error("limit count must be non-zero")]
    ZeroLimit,
    #[error("group_by requires at least one key")]
    EmptyGroupKeys,
    #[error("with requires at least one column")]
    EmptyWith,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CollectError {
    #[error("collect is only legal at a program-return, page, invoke-arg, or render barrier")]
    CollectNotAtBarrier,
    #[error("render row cap exceeded: got {got}, max {max}")]
    RenderRowCap { got: usize, max: usize },
    #[error("silent page exhaust is forbidden")]
    PageExhaustSilent,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowFilterError {
    #[error("row filter requires at least one predicate")]
    Empty,
    #[error("row filter cannot be rewritten as a catalog filter")]
    CrossPlanePushdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScanError {
    #[error("unbound frame")]
    UnboundFrame,
    #[error("fixture scan `{0}` is not loaded")]
    MissingFixture(u64),
    #[error("entity `{0}` is not in the graph snapshot")]
    MissingGraphEntity(EntityName),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FusionError {
    #[error("sort and limit must not be commuted")]
    CommuteSortLimit,
    #[error("optimizer must not rewrite row filters into catalog filters")]
    CrossPlanePushdown,
    #[error("join cannot be constructed from the surface")]
    JoinFromSurface,
    #[error("render is a collect barrier, not a pipeline node")]
    RenderInPipeline,
    #[error("derive remap cannot fold into a row-compute pipeline")]
    DeriveInPipeline,
}

impl From<CrossCurrencyError> for RowComputeError {
    fn from(e: CrossCurrencyError) -> Self {
        Self::CrossCurrency {
            left: e.left().to_string(),
            right: e.right().to_string(),
        }
    }
}

impl RowComputeError {
    #[must_use]
    pub fn temporal_arith_not_temporal(got: LogicalColumnType) -> Self {
        Self::Type(RowTypeError::TemporalArithNotTemporal { got })
    }
}
