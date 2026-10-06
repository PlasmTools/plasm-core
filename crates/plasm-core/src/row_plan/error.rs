//! Typed row-compute errors — no stringly engine failures.

use crate::identity::EntityName;
use crate::money::{CrossCurrencyError, MoneyError};
use crate::plasm_monad::ArithOp;
use thiserror::Error;

use crate::value_contract::ValueContract;

#[derive(Debug, Clone, Error)]
pub enum RowComputeError {
    #[error(transparent)]
    Contract(#[from] crate::row_plan::contracts::RowContractError),
    #[error(transparent)]
    ArithmeticContract(#[from] crate::value_arithmetic::ArithmeticContractError),
    #[error(transparent)]
    Correspondence(#[from] RowCorrespondenceError),
    #[error("field `{field}` is unobserved (not null)")]
    MissingField { field: String },
    #[error("row count exceeds the supported integer range")]
    RowCountOverflow,
    #[error("money sum requires a money contract")]
    MoneySumRequiresMoney,
    #[error("contains requires two string values")]
    ContainsRequiresStrings,
    #[error("membership requires a string or array on the right")]
    MembershipRequiresCollection,
    #[error("string membership requires a string on the left")]
    StringMembershipRequiresString,
    #[error("membership values cannot be compared in their declared value domain")]
    MembershipComparison,
    #[error(transparent)]
    Arithmetic(#[from] crate::value_expression::ArithmeticError),
    #[error(transparent)]
    Comparison(#[from] crate::value_expression::ComparisonError),
    #[error("numeric literal in a row expression is invalid")]
    InvalidExpressionNumber,
    #[error("length requires a string, array or record")]
    InvalidExpressionLengthOperand,
    #[error(transparent)]
    Type(Box<RowTypeError>),
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
    PredicateCompile(#[from] PredicateCompileError),
    #[error(transparent)]
    Equality(#[from] crate::value_equality::ValueEqualityError),
    #[error(transparent)]
    ValueHash(#[from] crate::value_hash::ValueHashError),
    #[error(transparent)]
    Ordering(#[from] crate::value_order::OrderingError),
    #[error(transparent)]
    Atom(#[from] crate::plasm_monad::PlanAtomError),
    #[error(transparent)]
    StepId(#[from] crate::plasm_monad::StepIdError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error(transparent)]
    Fusion(#[from] FusionError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PredicateCompileError {
    #[error(transparent)]
    Contract(#[from] crate::row_plan::contracts::RowContractError),
    #[error(transparent)]
    DataValue(#[from] crate::plasm_monad::payload::PlasmDataValueError),
    #[error(transparent)]
    Coercion(#[from] crate::wire_coercion::CoercionError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowTypeError {
    #[error("arithmetic `{op:?}` is not defined for {lhs:?} and {rhs:?}")]
    ArithDomain {
        op: ArithOp,
        lhs: ValueContract,
        rhs: ValueContract,
    },
    #[error("when() branches have mismatched types {then:?} vs {else_:?}")]
    WhenBranchMismatch {
        then: ValueContract,
        else_: ValueContract,
    },
    #[error("temporal arithmetic requires a temporal value, got {got:?}")]
    TemporalArithNotTemporal { got: ValueContract },
    #[error("project spec cannot be used as a .with column")]
    ProjectIntoWith,
    #[error(".with must preserve entity identity")]
    WithBreaksEntityShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameSchemaError {
    #[error("frame requires a record contract")]
    RequiresRecord,
    #[error("unknown column `{0}`")]
    UnknownColumn(String),
    #[error("empty pipeline is illegal")]
    EmptyPipeline,
    #[error("group_by requires at least one key")]
    EmptyGroupKeys,
    #[error("with requires at least one column")]
    EmptyWith,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowCorrespondenceError {
    #[error("row correspondence length does not match output rows")]
    LengthMismatch,
    #[error("row correspondence index is outside the input batch")]
    InputIndexOutOfBounds,
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
    #[error("union is a two-source collect, not a unary pipeline node")]
    UnionInPipeline,
    #[error("Python reduction requires the host compute boundary")]
    PythonInPipeline,
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
    pub fn temporal_arith_not_temporal(got: ValueContract) -> Self {
        RowTypeError::TemporalArithNotTemporal { got }.into()
    }
}

impl From<RowTypeError> for RowComputeError {
    fn from(error: RowTypeError) -> Self {
        Self::Type(Box::new(error))
    }
}
