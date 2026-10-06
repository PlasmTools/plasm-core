//! Formal monadic execution contract for Plasm programs.
//!
//! [`PlasmComp`] is the canonical plan artifact: typed steps sequenced by [`PlasmBindGraph`]
//! (bind / phased materialization). Surface bindings lower to `bind`; postfix transforms to
//! `map`; relation fanout and `for_each` to `flat_map`.

pub mod bind_graph;
pub mod comp;
pub mod correlated;
pub mod equiv;
#[cfg(test)]
mod laws;
pub mod operators;
pub mod payload;
pub mod step;

pub use bind_graph::{BindGraphError, PlasmBindGraph, PlasmHoleUse};
pub use comp::{
    PlasmComp, PlasmCompArtifact, PlasmCompValidationError, PlasmReturn, StepId, StepIdError,
    PLASM_COMP_WIRE_VERSION,
};
pub use correlated::{
    CorrelatedBody, CorrelatedBodyError, IterationStepEffectError, ParentCapture, ScopedCapture,
    ScopedOutput,
};
pub use equiv::{comp_equivalent, comp_semantic_eq, CompEquivDiff, CompEquivResult, RewritePolicy};
pub use operators::{
    empty_comp, invoke_step_payload, map_step_payload, plasm_bind_step, plasm_map_step,
    plasm_parallel_return, plasm_pure_step, EmptyParallelReturn,
};
pub use payload::{
    AggregateFunction, AggregateSpec, ArithOp, BindingName, ComputeOp, ComputeTemplate, DeriveKind,
    DerivePayload, DeriveTemplate, EffectTemplate, FieldPath, FlatMapApplyPayload,
    FlatMapRelationPayload, InputCardinality, InvokePayload, MapPayload, OutputName, PlanAtomError,
    PlanDataInput, PlanExprIr, PlanExprTemplate, PlanInputBinding, PlanPredicate, PlanPredicateOp,
    PlanQualifiedEntityKey, PlanRelationTraversal, PlanResultUse, PlasmDataValue,
    PlasmDataValueError, PlasmDataValueEvaluationError, PlasmStepPayload, PurePayload,
    RelationCardinality, RelationName, RelationSourceCardinality, SyntheticFieldSchema,
    SyntheticResultSchema, SyntheticResultSchemaError, SyntheticValueKind, UnfoldUntilPayload,
    WithColumn, WithExpr, WithExprError, WithLiteral,
};
pub use step::{
    EffectBarrier, EffectClass, EffectEvidence, PlasmStep, PlasmStepKind, ResultShape, SurfaceKind,
};
