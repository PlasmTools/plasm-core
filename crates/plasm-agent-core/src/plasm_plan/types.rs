//! Plan IR types and type-local invariants.

use plasm_core::Expr;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::marker::PhantomData;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PlanAtomError {
    #[error("{kind} must be non-empty")]
    Empty { kind: &'static str },
    #[error("{kind} contains JavaScript object string coercion ([object Object])")]
    ObjectCoercion { kind: &'static str },
}

impl From<PlanAtomError> for plasm_runtime::ExecutionFailure {
    fn from(error: PlanAtomError) -> Self {
        Self::new(
            plasm_runtime::FailureCause::Program,
            match &error {
                PlanAtomError::Empty { .. } => "plan_identifier_empty",
                PlanAtomError::ObjectCoercion { .. } => "plan_identifier_object_coercion",
            },
            error.to_string(),
        )
    }
}

impl From<plasm_core::plasm_monad::PlanAtomError> for PlanAtomError {
    fn from(error: plasm_core::plasm_monad::PlanAtomError) -> Self {
        match error {
            plasm_core::plasm_monad::PlanAtomError::Empty { kind } => Self::Empty { kind },
            plasm_core::plasm_monad::PlanAtomError::ObjectCoercion { kind } => {
                Self::ObjectCoercion { kind }
            }
            plasm_core::plasm_monad::PlanAtomError::EmptyPath => Self::Empty { kind: "FieldPath" },
            plasm_core::plasm_monad::PlanAtomError::EmptyPathSegment => Self::Empty {
                kind: "FieldPath segment",
            },
        }
    }
}

macro_rules! plan_string_atom {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, PlanAtomError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(PlanAtomError::Empty { kind: stringify!($name) });
                }
                if value.contains("[object Object]") {
                    return Err(PlanAtomError::ObjectCoercion { kind: stringify!($name) });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

plan_string_atom! {
    /// Validated local Plan node id.
    PlanNodeId
}

plan_string_atom! {
    /// Reference to a materialized Plan node.
    NodeRef
}

plan_string_atom! {
    /// Symbolic callback/item binding name.
    BindingName
}

plan_string_atom! {
    /// Alias under which a materialized dependency is available during derived evaluation.
    InputAlias
}

plan_string_atom! {
    /// Declared CGS relation name used by a Plasm program relation-traversal node.
    RelationName
}

/// Canonical compute + predicate wire types (shared with [`plasm_core::PlasmComp`] steps).
pub use plasm_core::{
    AggregateFunction, AggregateSpec, ComputeOp, ComputeTemplate, FieldPath, OutputName,
    PlanPredicate, PlanPredicateOp, PlasmDataValue as PlanValue, SyntheticFieldSchema,
    SyntheticResultSchema, SyntheticValueKind,
};

/// Typed source reference used by validated compute and derive nodes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceRef(pub NodeRef);

/// Effect classification (mirrors CGS capability + action output semantics; host authority).
pub use plasm_core::plasm_monad::EffectClass;

/// Expected host result shape for dry-run / planning.
pub use plasm_core::plasm_monad::ResultShape;

/// Qualified catalog entity key for dispatch (matches federation doctrine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedEntityKey {
    pub entry_id: String,
    pub entity: String,
}

impl From<plasm_core::QualifiedEntityKey> for QualifiedEntityKey {
    fn from(q: plasm_core::QualifiedEntityKey) -> Self {
        Self {
            entry_id: q.entry_id.into(),
            entity: q.entity.into(),
        }
    }
}

/// Reference to a prior node for symbolic `uses_result` edges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanResultUse {
    /// Node `id` (sandbox-local string).
    pub node: String,
    /// Local binding name.
    pub r#as: String,
    /// Catalog-qualified row domain of the **source** node (not the consumer).
    /// Required after [`enrich_uses_result_provenance`]; optional on raw DAG JSON until validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_entity: Option<QualifiedEntityKey>,
}

/// Cardinality contract for a data input consumed by a derived node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputCardinality {
    /// A typed receipt derived from the operation ledger, never an entity row.
    Acknowledgement,
    /// Preserve the entire complete rowset as a typed array, including empty.
    Collection,
    /// Host may broadcast only when the dependency is statically provable as singleton.
    Auto,
    /// The author explicitly requested singleton broadcast; runtime still verifies one row.
    Singleton,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputCardinalityProof {
    Acknowledgement,
    Collection,
    StaticSingleton,
    RuntimeCheckedSingleton,
}

fn default_input_cardinality() -> InputCardinality {
    InputCardinality::Auto
}

/// Explicit dataflow input for derived Plan nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDataInput {
    pub node: String,
    pub alias: String,
    #[serde(default = "default_input_cardinality")]
    pub cardinality: InputCardinality,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidatedPlanDataInput {
    pub(crate) node: PlanNodeId,
    pub(crate) alias: InputAlias,
    pub(crate) proof: InputCardinalityProof,
}

/// Executable Plasm IR for a program-plan node. `display_expr` is inert provenance only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanExprIr {
    pub expr: plasm_core::Expr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
}

/// Admitted executable expression. Review and execution share this structure; source text is erased.
#[derive(Debug, Clone, Serialize)]
pub struct ValidatedPlanExprIr {
    pub(crate) expr: Expr,
    pub(crate) projection: Option<Vec<String>>,
}

/// Structural expression with deferred operands. Binding cannot reconstruct expression structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanExprTemplate {
    pub expr: plasm_core::Expr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_bindings: Vec<PlanInputBinding>,
}

/// Deferred operands retain typed structure; binding cannot add, drop, or reparse operations.
#[derive(Debug, Clone, Serialize)]
pub struct ValidatedPlanExprTemplate {
    pub(crate) expr: plasm_core::Expr,
    pub(crate) projection: Option<Vec<String>>,
    #[allow(dead_code)]
    pub(crate) input_bindings: Vec<PlanInputBinding>,
}

/// Root `Plan` kind. We accept omission on the wire, but the canonical form is always a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    Program,
}

fn default_plan_kind() -> PlanKind {
    PlanKind::Program
}

fn default_plan_version() -> u32 {
    1
}

pub trait PlanState {
    type Node;
    type Return;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawPlanState {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatedPlanState {}

impl PlanState for RawPlanState {
    type Node = PlanNode;
    type Return = PlanReturn;
}

impl PlanState for ValidatedPlanState {
    type Node = ValidatedPlanNode;
    type Return = ValidatedPlanReturn;
}

/// Serialized program plan IR (`Plan`): validated by the runner and persisted for evaluation/trace lineage (not a separate external protocol).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "State::Node: Serialize, State::Return: Serialize",
    deserialize = "State::Node: Deserialize<'de>, State::Return: Deserialize<'de>"
))]
pub struct Plan<State: PlanState = RawPlanState> {
    #[serde(default = "default_plan_version")]
    pub version: u32,
    #[serde(default = "default_plan_kind")]
    pub kind: PlanKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub nodes: Vec<State::Node>,
    #[serde(rename = "return")]
    pub return_value: State::Return,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    state: PhantomData<State>,
}

impl Plan<RawPlanState> {
    /// Construct an untrusted plan from structural lowering. This grants no execution authority.
    pub(crate) fn from_nodes(
        name: Option<String>,
        nodes: Vec<PlanNode>,
        return_value: PlanReturn,
        metadata: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            version: 1,
            kind: PlanKind::Program,
            name,
            nodes,
            return_value,
            metadata,
            state: PhantomData,
        }
    }
}

pub type RawPlanArtifact = Plan<RawPlanState>;

/// Agent-visible return shape: a single node or a parallel set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanReturn {
    Node { node: String },
    Parallel { nodes: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidatedPlanReturn {
    Parallel { parallel: Vec<PlanNodeId> },
    Node(PlanNodeId),
}

impl ValidatedPlanReturn {
    pub fn refs(&self) -> Vec<&PlanNodeId> {
        match self {
            ValidatedPlanReturn::Node(id) => vec![id],
            ValidatedPlanReturn::Parallel { parallel } => parallel.iter().collect(),
        }
    }
}

impl Plan<ValidatedPlanState> {
    pub(super) fn from_validated_parts(
        version: u32,
        kind: PlanKind,
        name: Option<String>,
        nodes: Vec<ValidatedPlanNode>,
        return_value: ValidatedPlanReturn,
        metadata: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            version,
            kind,
            name,
            nodes,
            return_value,
            metadata,
            state: PhantomData,
        }
    }

    pub(crate) fn new_program(
        version: u32,
        name: Option<String>,
        nodes: Vec<ValidatedPlanNode>,
        return_value: ValidatedPlanReturn,
        metadata: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        Self::from_validated_parts(
            version,
            PlanKind::Program,
            name,
            nodes,
            return_value,
            metadata,
        )
    }
}

/// Proof-bearing Plan artifact consumed by dry-run and execution.
#[derive(Debug, Clone, Serialize)]
pub struct ValidatedPlanArtifact {
    artifact: Plan<ValidatedPlanState>,
    topo: Vec<PlanNodeId>,
    node_indices: HashMap<PlanNodeId, usize>,
    approval_gates: Vec<PlanNodeId>,
}

impl ValidatedPlanArtifact {
    pub(super) fn from_parts(
        artifact: Plan<ValidatedPlanState>,
        topo: Vec<PlanNodeId>,
        node_indices: HashMap<PlanNodeId, usize>,
        approval_gates: Vec<PlanNodeId>,
    ) -> Self {
        Self {
            artifact,
            topo,
            node_indices,
            approval_gates,
        }
    }

    /// Conservative allocation capacity of the accounted validated-plan structure.
    pub(crate) fn structural_retention_capacity_bytes(&self) -> usize {
        self.structural_retention_capacity_bytes_at_depth(0)
    }

    fn structural_retention_capacity_bytes_at_depth(&self, depth: usize) -> usize {
        if depth >= 64 {
            return usize::MAX;
        }
        fn vec_bytes<T>(value: &Vec<T>) -> usize {
            value.capacity().saturating_mul(std::mem::size_of::<T>())
        }
        fn id_bytes(ids: &Vec<PlanNodeId>) -> usize {
            ids.iter().fold(vec_bytes(ids), |sum, id| {
                sum.saturating_add(id.0.capacity())
            })
        }
        fn strings(values: &Vec<String>) -> usize {
            values.iter().fold(vec_bytes(values), |sum, value| {
                sum.saturating_add(value.capacity())
            })
        }
        fn data(value: &PlanValue, depth: usize) -> usize {
            if depth >= 64 {
                return usize::MAX;
            }
            let boxed = |value: &PlanValue| {
                std::mem::size_of::<PlanValue>().saturating_add(data(value, depth + 1))
            };
            match value {
                PlanValue::Quantified {
                    collection,
                    binding,
                    predicate,
                    ..
                } => binding
                    .capacity()
                    .saturating_add(boxed(collection))
                    .saturating_add(boxed(predicate)),
                PlanValue::Expression { expression } => {
                    let extra = match expression {
                        plasm_core::value_expression::ValueOperation::Field { name, .. } => {
                            name.capacity()
                        }
                        plasm_core::value_expression::ValueOperation::Refine {
                            contract, ..
                        } => contract.retention_capacity_bytes(),
                        _ => 0,
                    };
                    expression
                        .operands()
                        .fold(extra, |sum, value| sum.saturating_add(boxed(value)))
                }
                PlanValue::Literal { value } => {
                    plasm_runtime::materialization::resolved_value_retention_capacity(value.value())
                }
                PlanValue::BindingSymbol { binding, path } => {
                    binding.capacity().saturating_add(strings(path))
                }
                PlanValue::NodeSymbol { node, alias, path } => node
                    .capacity()
                    .saturating_add(alias.capacity())
                    .saturating_add(strings(path)),
                PlanValue::Symbol { path } => path.capacity(),
                PlanValue::Template {
                    template,
                    input_bindings,
                } => input_bindings.iter().fold(
                    template
                        .metadata_retention_capacity_bytes()
                        .saturating_add(vec_bytes(input_bindings)),
                    |sum, value| {
                        sum.saturating_add(value.from.capacity())
                            .saturating_add(value.to.capacity())
                    },
                ),
                PlanValue::EntityRefKey { api, entity, key } => api
                    .capacity()
                    .saturating_add(entity.capacity())
                    .saturating_add(boxed(key)),
                PlanValue::Array { items } => items.iter().fold(vec_bytes(items), |sum, value| {
                    sum.saturating_add(data(value, depth + 1))
                }),
                PlanValue::Object { fields } => fields.iter().fold(0usize, |sum, (name, value)| {
                    sum.saturating_add(1024)
                        .saturating_add(
                            16usize.saturating_mul(std::mem::size_of::<(String, PlanValue)>()),
                        )
                        .saturating_add(name.capacity())
                        .saturating_add(data(value, depth + 1))
                }),
            }
        }
        fn predicates(values: &Vec<PlanPredicate>) -> usize {
            values.iter().fold(vec_bytes(values), |sum, predicate| {
                sum.saturating_add(predicate.field_path.retention_capacity_bytes())
                    .saturating_add(data(&predicate.value, 0))
            })
        }
        fn entity(value: &QualifiedEntityKey) -> usize {
            value
                .entry_id
                .capacity()
                .saturating_add(value.entity.capacity())
        }
        fn bindings(values: &Vec<PlanInputBinding>) -> usize {
            values.iter().fold(vec_bytes(values), |sum, value| {
                sum.saturating_add(value.from.capacity())
                    .saturating_add(value.to.capacity())
            })
        }
        fn projection(value: &Option<Vec<String>>) -> usize {
            value.as_ref().map_or(0, strings)
        }
        fn template(value: &ValidatedPlanExprTemplate) -> usize {
            projection(&value.projection)
                .saturating_add(bindings(&value.input_bindings))
                .saturating_add(
                    plasm_runtime::execution::expression_retention_capacity_bytes(&value.expr),
                )
        }
        fn expr_ir(value: &ValidatedPlanExprIr) -> usize {
            projection(&value.projection).saturating_add(
                plasm_runtime::execution::expression_retention_capacity_bytes(&value.expr),
            )
        }
        fn effect(value: &ValidatedEffectTemplate) -> usize {
            entity(&value.qualified_entity)
                .saturating_add(template(&value.ir_template))
                .saturating_add(strings(&value.projection))
                .saturating_add(bindings(&value.input_bindings))
        }
        fn schema(value: &plasm_core::plasm_monad::SyntheticResultSchema) -> usize {
            let fields = value
                .fields
                .iter()
                .fold(vec_bytes(&value.fields), |sum, field| {
                    sum.saturating_add(field.name.retention_capacity_bytes())
                        .saturating_add(
                            field
                                .source
                                .as_ref()
                                .map_or(0, |path| path.retention_capacity_bytes()),
                        )
                        .saturating_add(
                            field
                                .value_type
                                .as_ref()
                                .map_or(0, |contract| contract.retention_capacity_bytes()),
                        )
                });
            value.optional_fields.iter().fold(
                fields.saturating_add(value.entity.as_ref().map_or(0, String::capacity)),
                |sum, field| {
                    sum.saturating_add(1024)
                        .saturating_add(16 * std::mem::size_of::<String>())
                        .saturating_add(field.capacity())
                },
            )
        }
        fn contract(value: &plasm_core::plasm_monad::CaptureContract) -> usize {
            match value {
                plasm_core::plasm_monad::CaptureContract::Value { value } => {
                    value.retention_capacity_bytes()
                }
                plasm_core::plasm_monad::CaptureContract::Rows { schema: value, .. } => {
                    value.as_ref().map_or(0, schema)
                }
            }
        }
        fn origin(value: &plasm_core::plasm_monad::RecordedOutputOrigin) -> usize {
            let mut bytes = value
                .run_id
                .capacity()
                .saturating_add(value.entry_id.capacity())
                .saturating_add(value.entity.capacity());
            for label in &value.labels {
                bytes = bytes
                    .saturating_add(1024)
                    .saturating_add(16 * std::mem::size_of::<plasm_core::DataClassName>())
                    .saturating_add(label.retention_capacity_bytes());
            }
            for source in &value.provenance {
                bytes = bytes
                    .saturating_add(1024)
                    .saturating_add(16 * std::mem::size_of::<String>())
                    .saturating_add(source.capacity());
            }
            bytes
        }
        fn monad_entity(value: &plasm_core::plasm_monad::PlanQualifiedEntityKey) -> usize {
            value
                .entry_id
                .capacity()
                .saturating_add(value.entity.capacity())
        }
        fn monad_bindings(values: &Vec<plasm_core::plasm_monad::PlanInputBinding>) -> usize {
            values.iter().fold(vec_bytes(values), |sum, binding| {
                sum.saturating_add(binding.from.capacity())
                    .saturating_add(binding.to.capacity())
            })
        }
        fn monad_ir(value: &plasm_core::plasm_monad::PlanExprIr) -> usize {
            plasm_runtime::execution::expression_retention_capacity_bytes(&value.expr)
                .saturating_add(projection(&value.projection))
                .saturating_add(value.display_expr.as_ref().map_or(0, String::capacity))
        }
        fn monad_template(value: &plasm_core::plasm_monad::PlanExprTemplate) -> usize {
            plasm_runtime::execution::expression_retention_capacity_bytes(&value.expr)
                .saturating_add(projection(&value.projection))
                .saturating_add(value.display_expr.as_ref().map_or(0, String::capacity))
                .saturating_add(monad_bindings(&value.input_bindings))
        }
        fn monad_effect(value: &plasm_core::plasm_monad::EffectTemplate) -> usize {
            monad_entity(&value.qualified_entity)
                .saturating_add(value.expr_template.capacity())
                .saturating_add(monad_template(&value.ir_template))
                .saturating_add(strings(&value.projection))
                .saturating_add(monad_bindings(&value.input_bindings))
        }
        fn view_proof(value: &plasm_core::ValidatedViewEmbedProof) -> usize {
            value
                .view
                .capacity()
                .saturating_add(value.producer_node.capacity())
                .saturating_add(value.relation.capacity())
        }
        fn relation(value: &plasm_core::plasm_monad::PlanRelationTraversal) -> usize {
            value.binding_proofs.iter().fold(
                value
                    .source
                    .capacity()
                    .saturating_add(value.relation.capacity())
                    .saturating_add(monad_entity(&value.target))
                    .saturating_add(value.expr.capacity())
                    .saturating_add(monad_ir(&value.ir))
                    .saturating_add(
                        value
                            .materialize
                            .as_ref()
                            .map_or(0, |materialize| materialize.retention_capacity_bytes()),
                    )
                    .saturating_add(value.view_embed_proof.as_ref().map_or(0, view_proof))
                    .saturating_add(vec_bytes(&value.binding_proofs)),
                |sum, proof| {
                    sum.saturating_add(proof.cap_param.capacity())
                        .saturating_add(proof.parent_field.capacity())
                },
            )
        }
        fn compute(value: &plasm_core::plasm_monad::ComputeTemplate) -> usize {
            use plasm_core::plasm_monad::{ComputeOp, WithExpr, WithLiteral};
            fn predicate(value: &plasm_core::BooleanExpr<PlanPredicate>, depth: usize) -> usize {
                if depth >= 64 {
                    return usize::MAX;
                }
                match value {
                    plasm_core::BooleanExpr::Atom(value) => value
                        .field_path
                        .retention_capacity_bytes()
                        .saturating_add(data(&value.value, 0)),
                    plasm_core::BooleanExpr::And(values) | plasm_core::BooleanExpr::Or(values) => {
                        values.iter().fold(vec_bytes(values), |sum, value| {
                            sum.saturating_add(predicate(value, depth + 1))
                        })
                    }
                    plasm_core::BooleanExpr::Not(value) => {
                        std::mem::size_of::<plasm_core::BooleanExpr<PlanPredicate>>()
                            .saturating_add(predicate(value, depth + 1))
                    }
                }
            }
            fn with_expr(value: &WithExpr, depth: usize) -> usize {
                if depth >= 64 {
                    return usize::MAX;
                }
                let boxed = |value: &WithExpr| {
                    std::mem::size_of::<WithExpr>().saturating_add(with_expr(value, depth + 1))
                };
                match value {
                    WithExpr::Field(path) | WithExpr::Len { field: path } => {
                        path.retention_capacity_bytes()
                    }
                    WithExpr::Literal(WithLiteral::Number(value) | WithLiteral::String(value)) => {
                        value.capacity()
                    }
                    WithExpr::Literal(_) | WithExpr::Now => 0,
                    WithExpr::Arith { lhs, rhs, .. } => boxed(lhs).saturating_add(boxed(rhs)),
                    WithExpr::When {
                        lhs,
                        rhs,
                        then,
                        else_,
                        ..
                    } => boxed(lhs)
                        .saturating_add(boxed(rhs))
                        .saturating_add(boxed(then))
                        .saturating_add(boxed(else_)),
                }
            }
            fn paths(values: &Vec<plasm_core::plasm_monad::FieldPath>) -> usize {
                values.iter().fold(vec_bytes(values), |sum, value| {
                    sum.saturating_add(value.retention_capacity_bytes())
                })
            }
            fn aggregates(values: &Vec<plasm_core::plasm_monad::AggregateSpec>) -> usize {
                values.iter().fold(vec_bytes(values), |sum, value| {
                    sum.saturating_add(value.name.retention_capacity_bytes())
                        .saturating_add(
                            value
                                .field
                                .as_ref()
                                .map_or(0, |field| field.retention_capacity_bytes()),
                        )
                })
            }
            fn names(values: &Vec<plasm_core::plasm_monad::OutputName>) -> usize {
                values.iter().fold(vec_bytes(values), |sum, value| {
                    sum.saturating_add(value.retention_capacity_bytes())
                })
            }
            let op = match &value.op {
                ComputeOp::NarrowEntity { target, .. } => monad_entity(target),
                ComputeOp::Python {
                    source,
                    entry_id,
                    entity,
                    catalog_hash,
                    language_profile,
                    input_schema,
                    output_type,
                    selection,
                    ..
                } => {
                    let selection = selection.as_ref().map_or(0, |selection| {
                        selection.ports.iter().fold(
                            std::mem::size_of::<plasm_core::plasm_monad::PythonRowSelection>()
                                .saturating_add(monad_entity(&selection.owner))
                                .saturating_add(vec_bytes(&selection.ports)),
                            |sum, port| {
                                sum.saturating_add(port.parameter.capacity())
                                    .saturating_add(port.source.capacity())
                                    .saturating_add(
                                        port.packet_field.as_ref().map_or(0, String::capacity),
                                    )
                            },
                        )
                    });
                    source
                        .capacity()
                        .saturating_add(entry_id.capacity())
                        .saturating_add(entity.as_ref().map_or(0, String::capacity))
                        .saturating_add(catalog_hash.capacity())
                        .saturating_add(language_profile.capacity())
                        .saturating_add(input_schema.as_ref().map_or(0, schema))
                        .saturating_add(output_type.retention_capacity_bytes())
                        .saturating_add(selection)
                }
                ComputeOp::Project { fields } => fields.iter().fold(0usize, |sum, (name, path)| {
                    sum.saturating_add(1024)
                        .saturating_add(
                            16 * std::mem::size_of::<(
                                plasm_core::plasm_monad::OutputName,
                                plasm_core::plasm_monad::FieldPath,
                            )>(),
                        )
                        .saturating_add(name.retention_capacity_bytes())
                        .saturating_add(path.retention_capacity_bytes())
                }),
                ComputeOp::Filter { predicates } => predicate(predicates, 0),
                ComputeOp::GroupBy {
                    keys,
                    aggregates: values,
                } => paths(keys).saturating_add(aggregates(values)),
                ComputeOp::Aggregate { aggregates: values } => aggregates(values),
                ComputeOp::Sort { key, .. } => key.retention_capacity_bytes(),
                ComputeOp::Limit { .. } => 0,
                ComputeOp::DedupeBy { keys } => paths(keys),
                ComputeOp::With { columns } => {
                    columns.iter().fold(vec_bytes(columns), |sum, column| {
                        sum.saturating_add(column.name.retention_capacity_bytes())
                            .saturating_add(with_expr(&column.expr, 0))
                    })
                }
                ComputeOp::Union { other } | ComputeOp::MergeBranches { other } => {
                    other.retention_capacity_bytes()
                }
                ComputeOp::Render {
                    columns,
                    template,
                    column_aliases,
                    render_bindings,
                } => column_aliases.iter().fold(
                    names(columns)
                        .saturating_add(template.capacity())
                        .saturating_add(names(render_bindings)),
                    |sum, (key, name)| {
                        sum.saturating_add(1024)
                            .saturating_add(
                                16 * std::mem::size_of::<(
                                    String,
                                    plasm_core::plasm_monad::OutputName,
                                )>(),
                            )
                            .saturating_add(key.capacity())
                            .saturating_add(name.retention_capacity_bytes())
                    },
                ),
            };
            value
                .source
                .capacity()
                .saturating_add(schema(&value.schema))
                .saturating_add(
                    value
                        .collection_alias
                        .as_ref()
                        .map_or(0, |alias| alias.retention_capacity_bytes()),
                )
                .saturating_add(op)
        }
        fn comp_structure(value: &plasm_core::plasm_monad::PlasmComp, depth: usize) -> usize {
            use plasm_core::plasm_monad::{PlasmReturn, PlasmStepPayload, StepId};

            // Account conservatively for B-tree nodes, which do not expose capacity.
            fn tree_entry<T>() -> usize {
                1024usize.saturating_add(16usize.saturating_mul(std::mem::size_of::<T>()))
            }
            let mut bytes = value.name.as_ref().map_or(0, String::capacity);
            bytes = value.steps.iter().fold(bytes, |sum, (id, payload)| {
                sum.saturating_add(tree_entry::<(String, PlasmStepPayload)>())
                    .saturating_add(id.capacity())
                    .saturating_add(match payload {
                        PlasmStepPayload::Pure(pure) => data(&pure.data, 0)
                            .saturating_add(pure.recorded_output.as_ref().map_or(0, origin)),
                        PlasmStepPayload::Invoke(invoke) => invoke
                            .qualified_entity
                            .as_ref()
                            .map_or(0, monad_entity)
                            .saturating_add(invoke.ir.as_ref().map_or(0, monad_ir))
                            .saturating_add(invoke.ir_template.as_ref().map_or(0, monad_template))
                            .saturating_add(strings(&invoke.projection))
                            .saturating_add(predicates(&invoke.predicates))
                            .saturating_add(invoke.approval.as_ref().map_or(0, String::capacity))
                            .saturating_add(
                                invoke.display_expr.as_ref().map_or(0, String::capacity),
                            ),
                        PlasmStepPayload::Derive(derive) => derive.derive.inputs.iter().fold(
                            derive
                                .derive
                                .source
                                .as_ref()
                                .map_or(0, String::capacity)
                                .saturating_add(
                                    derive
                                        .derive
                                        .item_binding
                                        .as_ref()
                                        .map_or(0, |v| v.retention_capacity_bytes()),
                                )
                                .saturating_add(vec_bytes(&derive.derive.inputs))
                                .saturating_add(data(&derive.derive.value, 0)),
                            |sum, input| {
                                sum.saturating_add(input.node.capacity())
                                    .saturating_add(input.alias.capacity())
                            },
                        ),
                        PlasmStepPayload::FlatMapApply(apply) => apply
                            .source
                            .capacity()
                            .saturating_add(apply.item_binding.retention_capacity_bytes())
                            .saturating_add(monad_effect(&apply.effect_template))
                            .saturating_add(strings(&apply.projection))
                            .saturating_add(predicates(&apply.predicates))
                            .saturating_add(apply.approval.as_ref().map_or(0, String::capacity)),
                        PlasmStepPayload::UnfoldUntil(iter) => iter
                            .source
                            .capacity()
                            .saturating_add(iter.item_binding.retention_capacity_bytes())
                            .saturating_add(monad_effect(&iter.effect_template))
                            .saturating_add(predicates(&iter.until_predicates))
                            .saturating_add(
                                iter.until_scope
                                    .as_deref()
                                    .map_or(0, |body| scope(body, depth + 1)),
                            )
                            .saturating_add(
                                iter.step_scope
                                    .as_deref()
                                    .map_or(0, |body| scope(body, depth + 1)),
                            )
                            .saturating_add(iter.seed_ir.as_ref().map_or(0, monad_ir))
                            .saturating_add(iter.approval.as_ref().map_or(0, String::capacity)),
                        PlasmStepPayload::MapBody(body) => scope(body, depth + 1),
                        PlasmStepPayload::Map(map) => compute(&map.compute),
                        PlasmStepPayload::FlatMapRelation(value) => relation(&value.relation),
                    })
            });
            bytes = value.metadata.iter().fold(bytes, |sum, (key, value)| {
                sum.saturating_add(tree_entry::<(String, serde_json::Value)>())
                    .saturating_add(key.capacity())
                    .saturating_add(plasm_runtime::materialization::retained_json_capacity(
                        value,
                    ))
            });
            let bind = &value.bind;
            bytes = bind
                .topo
                .iter()
                .fold(bytes.saturating_add(vec_bytes(&bind.topo)), |sum, id| {
                    sum.saturating_add(id.0.capacity())
                });
            bytes = bind.deps.iter().fold(bytes, |sum, (id, deps)| {
                deps.iter().fold(
                        sum.saturating_add(tree_entry::<(
                            StepId,
                            std::collections::BTreeSet<StepId>,
                        )>())
                        .saturating_add(id.0.capacity()),
                        |sum, dependency| {
                            sum.saturating_add(tree_entry::<StepId>())
                                .saturating_add(dependency.0.capacity())
                        },
                    )
            });
            bytes = bind.primary.iter().fold(bytes, |sum, (id, primary)| {
                sum.saturating_add(tree_entry::<(StepId, StepId)>())
                    .saturating_add(id.0.capacity())
                    .saturating_add(primary.0.capacity())
            });
            bytes = bind.holes.iter().fold(bytes, |sum, (id, holes)| {
                holes.iter().fold(
                    sum.saturating_add(tree_entry::<(
                        StepId,
                        Vec<plasm_core::plasm_monad::PlasmHoleUse>,
                    )>())
                    .saturating_add(id.0.capacity())
                    .saturating_add(vec_bytes(holes)),
                    |sum, hole| {
                        sum.saturating_add(hole.step.0.capacity())
                            .saturating_add(hole.alias.capacity())
                    },
                )
            });
            match &value.return_ {
                PlasmReturn::Step { step } => bytes.saturating_add(step.0.capacity()),
                PlasmReturn::Parallel { steps } => steps
                    .iter()
                    .fold(bytes.saturating_add(vec_bytes(steps)), |sum, id| {
                        sum.saturating_add(id.0.capacity())
                    }),
            }
        }
        fn scope(value: &plasm_core::plasm_monad::CorrelatedBody, depth: usize) -> usize {
            if depth >= 64 {
                return usize::MAX;
            }
            let parent = &value.parent;
            let mut bytes = std::mem::size_of::<plasm_core::plasm_monad::CorrelatedBody>()
                .saturating_add(parent.source.0.capacity())
                .saturating_add(parent.local.0.capacity())
                .saturating_add(parent.entity.entry_id.capacity())
                .saturating_add(parent.entity.entity.capacity())
                .saturating_add(contract(&parent.contract))
                .saturating_add(comp_structure(&value.body, depth));
            if let plasm_core::plasm_monad::ScopedOutput::Rows {
                entity,
                schema: output,
                ..
            } = &value.output
            {
                bytes = bytes
                    .saturating_add(entity.entry_id.capacity())
                    .saturating_add(entity.entity.capacity())
                    .saturating_add(schema(output));
            }
            value.captures.iter().fold(
                bytes.saturating_add(vec_bytes(&value.captures)),
                |sum, capture| {
                    sum.saturating_add(capture.source.0.capacity())
                        .saturating_add(capture.local.0.capacity())
                        .saturating_add(capture.entity.entry_id.capacity())
                        .saturating_add(capture.entity.entity.capacity())
                        .saturating_add(contract(&capture.contract))
                },
            )
        }
        fn common(dependencies: &Vec<PlanNodeId>, uses: &Vec<PlanResultUse>) -> usize {
            uses.iter().fold(
                id_bytes(dependencies).saturating_add(vec_bytes(uses)),
                |sum, value| {
                    let entity = value.qualified_entity.as_ref().map_or(0, |entity| {
                        entity
                            .entry_id
                            .capacity()
                            .saturating_add(entity.entity.capacity())
                    });
                    sum.saturating_add(value.node.capacity())
                        .saturating_add(value.r#as.capacity())
                        .saturating_add(entity)
                },
            )
        }
        let mut bytes = vec_bytes(&self.artifact.nodes)
            .saturating_add(id_bytes(&self.topo))
            .saturating_add(id_bytes(&self.approval_gates))
            .saturating_add(self.artifact.name.as_ref().map_or(0, String::capacity))
            .saturating_add(
                self.node_indices
                    .capacity()
                    .saturating_mul(2)
                    .saturating_mul(std::mem::size_of::<(PlanNodeId, usize)>().saturating_add(1)),
            );
        for (key, value) in &self.artifact.metadata {
            bytes = bytes
                .saturating_add(1024)
                .saturating_add(16 * std::mem::size_of::<(String, serde_json::Value)>())
                .saturating_add(key.capacity())
                .saturating_add(plasm_runtime::materialization::retained_json_capacity(
                    value,
                ));
        }
        bytes = bytes.saturating_add(match &self.artifact.return_value {
            ValidatedPlanReturn::Node(id) => id.0.capacity(),
            ValidatedPlanReturn::Parallel { parallel } => id_bytes(parallel),
        });
        for id in self.node_indices.keys() {
            bytes = bytes.saturating_add(id.0.capacity());
        }
        for node in &self.artifact.nodes {
            bytes = bytes.saturating_add(node.id().0.capacity());
            let node_common = match node {
                ValidatedPlanNode::Surface(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::Data(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::Derive(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::Compute(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::ForEach(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::IterateUntil(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::RelationTraversal(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::MapBody(n) => common(&n.depends_on, &n.uses_result),
                ValidatedPlanNode::Capture(_) => 0,
            };
            bytes = bytes.saturating_add(node_common);
            let node_owned = match node {
                ValidatedPlanNode::Surface(n) => n
                    .qualified_entity
                    .as_ref()
                    .map_or(0, entity)
                    .saturating_add(n.ir.as_ref().map_or(0, expr_ir))
                    .saturating_add(n.ir_template.as_ref().map_or(0, template))
                    .saturating_add(strings(&n.projection))
                    .saturating_add(predicates(&n.predicates))
                    .saturating_add(n.approval.as_ref().map_or(0, String::capacity)),
                ValidatedPlanNode::Data(n) => {
                    data(&n.data, 0).saturating_add(n.recorded_output.as_ref().map_or(0, origin))
                }
                ValidatedPlanNode::Derive(n) => n
                    .inputs
                    .iter()
                    .fold(
                        n.source
                            .0
                            .capacity()
                            .saturating_add(n.item_binding.0.capacity())
                            .saturating_add(vec_bytes(&n.inputs)),
                        |sum, input| {
                            sum.saturating_add(input.node.0.capacity())
                                .saturating_add(input.alias.0.capacity())
                        },
                    )
                    .saturating_add(data(&n.value, 0)),
                ValidatedPlanNode::Compute(n) => n
                    .source_node
                    .0
                    .capacity()
                    .saturating_add(compute(&n.compute)),
                ValidatedPlanNode::ForEach(n) => n
                    .source
                    .0
                    .capacity()
                    .saturating_add(n.item_binding.0.capacity())
                    .saturating_add(effect(&n.effect_template))
                    .saturating_add(strings(&n.projection))
                    .saturating_add(predicates(&n.predicates))
                    .saturating_add(n.approval.as_ref().map_or(0, String::capacity)),
                ValidatedPlanNode::IterateUntil(n) => n
                    .source
                    .0
                    .capacity()
                    .saturating_add(n.item_binding.0.capacity())
                    .saturating_add(effect(&n.effect_template))
                    .saturating_add(predicates(&n.until_predicates))
                    .saturating_add(n.approval.as_ref().map_or(0, String::capacity))
                    .saturating_add(n.seed_ir.as_ref().map_or(0, expr_ir))
                    .saturating_add(n.until_scope.as_deref().map_or(0, |body| scope(body, 0)))
                    .saturating_add(n.step_scope.as_deref().map_or(0, |body| scope(body, 0))),
                ValidatedPlanNode::RelationTraversal(n) => n.relation.binding_proofs.iter().fold(
                    n.relation
                        .source
                        .0
                        .capacity()
                        .saturating_add(n.relation.relation.0.capacity())
                        .saturating_add(entity(&n.relation.target))
                        .saturating_add(expr_ir(&n.relation.ir))
                        .saturating_add(n.relation.materialize.retention_capacity_bytes())
                        .saturating_add(n.relation.view_embed_proof.as_ref().map_or(0, view_proof))
                        .saturating_add(vec_bytes(&n.relation.binding_proofs)),
                    |sum, proof| {
                        sum.saturating_add(proof.cap_param.capacity())
                            .saturating_add(proof.parent_field.capacity())
                    },
                ),
                ValidatedPlanNode::MapBody(n) => scope(&n.body, 0),
                ValidatedPlanNode::Capture(n) => {
                    entity(&n.entity).saturating_add(contract(&n.contract))
                }
            };
            bytes = bytes.saturating_add(node_owned);
            for nested in node.nested_plans() {
                bytes = bytes
                    .saturating_add(std::mem::size_of::<ValidatedPlanArtifact>())
                    .saturating_add(nested.structural_retention_capacity_bytes_at_depth(depth + 1));
            }
        }
        bytes
    }
}

pub type ValidatedPlan = ValidatedPlanArtifact;

#[derive(Debug, Clone, Serialize)]
pub enum ValidatedPlanNode {
    Surface(ValidatedSurfaceNode),
    Data(ValidatedDataNode),
    Derive(ValidatedDeriveNode),
    Compute(ValidatedComputeNode),
    ForEach(ValidatedForEachNode),
    IterateUntil(ValidatedIterateUntilNode),
    RelationTraversal(ValidatedRelationTraversalNode),
    MapBody(ValidatedMapBodyNode),
    /// Internal scope input; never admitted as a public wire operation.
    Capture(ValidatedCaptureNode),
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedMapBodyNode {
    pub(crate) id: PlanNodeId,
    pub(crate) body: Box<plasm_core::plasm_monad::CorrelatedBody>,
    pub(crate) plan: Box<ValidatedPlan>,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedCaptureNode {
    pub(crate) id: PlanNodeId,
    pub(crate) entity: QualifiedEntityKey,
    pub(crate) contract: plasm_core::plasm_monad::CaptureContract,
    pub(crate) singleton: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedSurfaceNode {
    pub(crate) id: PlanNodeId,
    pub(crate) kind: PlanNodeKind,
    pub(crate) qualified_entity: Option<QualifiedEntityKey>,
    pub(crate) ir: Option<ValidatedPlanExprIr>,
    pub(crate) ir_template: Option<ValidatedPlanExprTemplate>,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) projection: Vec<String>,
    pub(crate) predicates: Vec<PlanPredicate>,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
    pub(crate) approval: Option<String>,
    pub(crate) page_size: Option<usize>,
    pub(crate) pushed_read_budget: Option<crate::plan_read_bounds::PushedReadBudget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedDataNode {
    pub(crate) recorded_output: Option<plasm_core::plasm_monad::RecordedOutputOrigin>,
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) data: PlanValue,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedDeriveNode {
    pub(crate) kind: DeriveKind,
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) source: PlanNodeId,
    pub(crate) item_binding: BindingName,
    pub(crate) inputs: Vec<ValidatedPlanDataInput>,
    pub(crate) value: PlanValue,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedComputeNode {
    pub(crate) source_node: PlanNodeId,
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) compute: ComputeTemplate,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedForEachNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) source: PlanNodeId,
    pub(crate) item_binding: BindingName,
    pub(crate) effect_template: ValidatedEffectTemplate,
    pub(crate) projection: Vec<String>,
    pub(crate) predicates: Vec<PlanPredicate>,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
    pub(crate) approval: Option<String>,
}

/// PLP-8 state iterator: observe seed, step+reobserve until predicate or bound exhaustion.
#[derive(Debug, Clone, Serialize)]
pub struct ValidatedIterateUntilNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) source: PlanNodeId,
    pub(crate) item_binding: BindingName,
    pub(crate) effect_template: ValidatedEffectTemplate,
    pub(crate) until_predicates: Vec<PlanPredicate>,
    pub(crate) until_scope: Option<Box<plasm_core::plasm_monad::CorrelatedBody>>,
    pub(crate) step_scope: Option<Box<plasm_core::plasm_monad::CorrelatedBody>>,
    pub(crate) until_plan: Option<Box<ValidatedPlan>>,
    pub(crate) step_plan: Option<Box<ValidatedPlan>>,
    pub(crate) take: u32,
    /// Seed Get IR for re-observe after each step (PLP-8).
    pub(crate) seed_ir: Option<ValidatedPlanExprIr>,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
    pub(crate) approval: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedRelationTraversalNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) relation: ValidatedPlanRelationTraversal,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
    pub(crate) pushed_read_budget: Option<crate::plan_read_bounds::PushedReadBudget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedPlanRelationTraversal {
    pub(crate) source: PlanNodeId,
    pub(crate) relation: RelationName,
    pub(crate) target: QualifiedEntityKey,
    pub(crate) cardinality: RelationCardinality,
    pub(crate) source_cardinality: RelationSourceCardinality,
    pub(crate) ir: ValidatedPlanExprIr,
    /// Frozen from CGS at plan lower time; plan/runtime must not re-infer from cache shape.
    pub(crate) materialize: plasm_core::RelationMaterialization,
    /// Validated view producer lineage for `view_embed` hops (required at plan acceptance).
    pub(crate) view_embed_proof: Option<plasm_core::ValidatedViewEmbedProof>,
    /// Catalog-derived scoped-binding witnesses preserved on comp wire.
    pub(crate) binding_proofs: Vec<plasm_core::RelationBindingProof>,
}

impl ValidatedPlanNode {
    /// Every nested executable scope, independent of its surface consumer.
    pub(crate) fn nested_plans(&self) -> impl DoubleEndedIterator<Item = &ValidatedPlan> {
        let plans = match self {
            Self::MapBody(map) => [Some(map.plan.as_ref()), None],
            Self::IterateUntil(it) => [it.until_plan.as_deref(), it.step_plan.as_deref()],
            _ => [None, None],
        };
        plans.into_iter().flatten()
    }

    pub(crate) fn nested_plans_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = &mut ValidatedPlan> {
        let plans = match self {
            Self::MapBody(map) => [Some(map.plan.as_mut()), None],
            Self::IterateUntil(it) => [it.until_plan.as_deref_mut(), it.step_plan.as_deref_mut()],
            _ => [None, None],
        };
        plans.into_iter().flatten()
    }

    pub fn id(&self) -> &PlanNodeId {
        match self {
            Self::Surface(n) => &n.id,
            Self::Data(n) => &n.id,
            Self::Derive(n) => &n.id,
            Self::Compute(n) => &n.id,
            Self::ForEach(n) => &n.id,
            Self::IterateUntil(n) => &n.id,
            Self::RelationTraversal(n) => &n.id,
            Self::MapBody(n) => &n.id,
            Self::Capture(n) => &n.id,
        }
    }

    pub fn kind(&self) -> PlanNodeKind {
        match self {
            Self::Surface(n) => n.kind,
            Self::Data(_) => PlanNodeKind::Data,
            Self::Derive(_) => PlanNodeKind::Derive,
            Self::Compute(_) => PlanNodeKind::Compute,
            Self::ForEach(_) => PlanNodeKind::ForEach,
            Self::IterateUntil(_) => PlanNodeKind::IterateUntil,
            Self::RelationTraversal(_) => PlanNodeKind::Relation,
            Self::MapBody(_) => PlanNodeKind::MapBody,
            Self::Capture(_) => PlanNodeKind::Capture,
        }
    }

    pub fn effect_class(&self) -> EffectClass {
        match self {
            Self::Surface(n) => n.effect_class,
            Self::Data(n) => n.effect_class,
            Self::Derive(n) => n.effect_class,
            Self::Compute(n) => n.effect_class,
            Self::ForEach(n) => n.effect_class,
            Self::IterateUntil(n) => n.effect_class,
            Self::RelationTraversal(n) => n.effect_class,
            Self::MapBody(map) => map.body.effect_class(),
            Self::Capture(_) => EffectClass::ArtifactRead,
        }
    }

    pub fn result_shape(&self) -> ResultShape {
        match self {
            Self::Surface(n) => n.result_shape,
            Self::Data(n) => n.result_shape,
            Self::Derive(n) => n.result_shape,
            Self::Compute(n) => n.result_shape,
            Self::ForEach(n) => n.result_shape,
            Self::IterateUntil(n) => n.result_shape,
            Self::RelationTraversal(n) => n.result_shape,
            Self::MapBody(map) => map.body.result_shape(),
            Self::Capture(c) => {
                if c.singleton {
                    ResultShape::Single
                } else {
                    ResultShape::List
                }
            }
        }
    }

    pub fn depends_on(&self) -> &[PlanNodeId] {
        match self {
            Self::Surface(n) => &n.depends_on,
            Self::Data(n) => &n.depends_on,
            Self::Derive(n) => &n.depends_on,
            Self::Compute(n) => &n.depends_on,
            Self::ForEach(n) => &n.depends_on,
            Self::IterateUntil(n) => &n.depends_on,
            Self::RelationTraversal(n) => &n.depends_on,
            Self::MapBody(n) => &n.depends_on,
            Self::Capture(_) => &[],
        }
    }

    pub fn uses_result(&self) -> &[PlanResultUse] {
        match self {
            Self::Surface(n) => &n.uses_result,
            Self::Data(n) => &n.uses_result,
            Self::Derive(n) => &n.uses_result,
            Self::Compute(n) => &n.uses_result,
            Self::ForEach(n) => &n.uses_result,
            Self::IterateUntil(n) => &n.uses_result,
            Self::RelationTraversal(n) => &n.uses_result,
            Self::MapBody(n) => &n.uses_result,
            Self::Capture(_) => &[],
        }
    }

    pub fn as_surface(&self) -> Option<&ValidatedSurfaceNode> {
        match self {
            Self::Surface(n) => Some(n),
            _ => None,
        }
    }
}

impl plasm_core::plasm_monad::EffectEvidence for ValidatedPlanNode {
    fn effect_class(&self) -> EffectClass {
        ValidatedPlanNode::effect_class(self)
    }
}

impl ValidatedPlanArtifact {
    pub fn artifact(&self) -> &Plan<ValidatedPlanState> {
        &self.artifact
    }

    pub fn version(&self) -> u32 {
        self.artifact.version
    }

    pub fn name(&self) -> Option<&str> {
        self.artifact.name.as_deref()
    }

    pub fn nodes(&self) -> &[ValidatedPlanNode] {
        &self.artifact.nodes
    }

    pub fn return_value(&self) -> &ValidatedPlanReturn {
        &self.artifact.return_value
    }

    pub fn topological_order(&self) -> &[PlanNodeId] {
        &self.topo
    }

    pub fn node_index(&self, id: &PlanNodeId) -> Option<usize> {
        self.node_indices.get(id).copied()
    }

    pub fn approval_gates(&self) -> &[PlanNodeId] {
        &self.approval_gates
    }

    pub(crate) fn nodes_mut(&mut self) -> &mut [ValidatedPlanNode] {
        &mut self.artifact.nodes
    }

    pub(crate) fn from_validated_parts(
        artifact: Plan<ValidatedPlanState>,
        topo: Vec<PlanNodeId>,
        node_indices: HashMap<PlanNodeId, usize>,
        approval_gates: Vec<PlanNodeId>,
    ) -> Self {
        Self {
            artifact,
            topo,
            node_indices,
            approval_gates,
        }
    }
}

/// Kinds the typed Plan DAG understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNodeKind {
    Query,
    Search,
    Get,
    Create,
    Update,
    Delete,
    Action,
    Data,
    Derive,
    Compute,
    ForEach,
    IterateUntil,
    Relation,
    MapBody,
    Capture,
}

impl PlanNodeKind {
    pub fn has_surface_expr(self) -> bool {
        matches!(
            self,
            PlanNodeKind::Query
                | PlanNodeKind::Search
                | PlanNodeKind::Get
                | PlanNodeKind::Create
                | PlanNodeKind::Update
                | PlanNodeKind::Delete
                | PlanNodeKind::Action
        )
    }

    pub fn is_template_allowed(self) -> bool {
        matches!(
            self,
            PlanNodeKind::Query
                | PlanNodeKind::Search
                | PlanNodeKind::Get
                | PlanNodeKind::Create
                | PlanNodeKind::Update
                | PlanNodeKind::Delete
                | PlanNodeKind::Action
        )
    }
}

impl fmt::Display for PlanNodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Query => "query",
            Self::Search => "search",
            Self::Get => "get",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Action => "action",
            Self::Data => "data input",
            Self::Derive => "derived value",
            Self::Compute => "compute",
            Self::ForEach => "for_each scope",
            Self::IterateUntil => "iterate_until scope",
            Self::Relation => "relation traversal",
            Self::MapBody => "map body",
            Self::Capture => "capture",
        })
    }
}

/// One typed node in the Plan DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_output: Option<plasm_core::plasm_monad::RecordedOutputOrigin>,
    pub id: String,
    pub kind: PlanNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_entity: Option<QualifiedEntityKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ir: Option<PlanExprIr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ir_template: Option<PlanExprTemplate>,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<PlanPredicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_binding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_template: Option<EffectTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<PlanValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive_template: Option<DeriveTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute: Option<ComputeTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<PlanRelationTraversal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_body: Option<Box<plasm_core::plasm_monad::CorrelatedBody>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_scope: Option<Box<plasm_core::plasm_monad::CorrelatedBody>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_scope: Option<Box<plasm_core::plasm_monad::CorrelatedBody>>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub uses_result: Vec<PlanResultUse>,
    /// Paging cap for a surface query (Plasm program `e#…page_size(n)` / `.page_size(n)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    /// Hard bound for `iterate_until` (PLP-8); required when `kind == IterateUntil`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<u32>,
    /// Until predicate body for `iterate_until` (same shape as `| where`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

/// Relation traversal on a validated plan node.
///
/// Field layout must stay aligned with `plasm_core::plasm_monad::PlanRelationTraversal`
/// for wire/serde compatibility across host and monad payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRelationTraversal {
    pub source: String,
    pub relation: String,
    pub target: QualifiedEntityKey,
    pub cardinality: RelationCardinality,
    pub source_cardinality: RelationSourceCardinality,
    pub expr: String,
    pub ir: PlanExprIr,
    /// Catalog-derived `(cap_param ← parent_field)` witnesses for scoped materialization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binding_proofs: Vec<plasm_core::RelationBindingProof>,
    #[serde(default, skip_serializing_if = "missing_materialize")]
    pub materialize: Option<plasm_core::RelationMaterialization>,
    /// Required when `materialize` is `view_embed`: the plan node that executed the view root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_embed_proof: Option<plasm_core::ValidatedViewEmbedProof>,
}

pub(super) fn missing_materialize(m: &Option<plasm_core::RelationMaterialization>) -> bool {
    m.is_none()
}

pub use plasm_core::plasm_monad::RelationCardinality;

pub use plasm_core::plasm_monad::RelationSourceCardinality;

#[derive(Debug, Clone, Serialize)]
pub struct ValidatedEffectTemplate {
    pub(crate) kind: PlanNodeKind,
    pub(crate) qualified_entity: QualifiedEntityKey,
    pub(crate) ir_template: ValidatedPlanExprTemplate,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) projection: Vec<String>,
    pub(crate) input_bindings: Vec<PlanInputBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectTemplate {
    pub kind: PlanNodeKind,
    pub qualified_entity: QualifiedEntityKey,
    pub expr_template: String,
    pub ir_template: PlanExprTemplate,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_bindings: Vec<PlanInputBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanInputBinding {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeriveTemplate {
    pub kind: DeriveKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_binding: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<PlanDataInput>,
    pub value: PlanValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeriveKind {
    /// A single opaque cell, including object-valued primitive domains.
    Cell,
    Map,
    Data,
}

pub const PLAN_RENDER_MAX_TEMPLATE_CHARS: usize = 64 * 1024;
pub const PLAN_RENDER_MAX_ROWS: usize = 10_000;
pub const PLAN_RENDER_MAX_OUTPUT_CHARS: usize = 1024 * 1024;

/// Dependency-first DFS order, or `None` for a cycle. Frames live on the heap:
/// appending dependencies never increases native call depth.
pub(super) fn dependency_order(adj: &[Vec<usize>]) -> Option<Vec<usize>> {
    let mut marks = vec![0u8; adj.len()];
    let mut order = Vec::with_capacity(adj.len());
    let mut frames = Vec::new();
    for root in 0..adj.len() {
        if marks[root] != 0 {
            continue;
        }
        marks[root] = 1;
        frames.push((root, 0));
        while let Some((node, next)) = frames.last_mut() {
            if let Some(&child) = adj[*node].get(*next) {
                *next += 1;
                if child >= marks.len() {
                    continue;
                }
                match marks[child] {
                    1 => return None,
                    0 => {
                        marks[child] = 1;
                        frames.push((child, 0));
                    }
                    _ => {}
                }
            } else {
                marks[*node] = 2;
                order.push(*node);
                frames.pop();
            }
        }
    }
    Some(order)
}

pub(super) fn return_refs(ret: &PlanReturn) -> Vec<&str> {
    match ret {
        PlanReturn::Node { node } => vec![node.as_str()],
        PlanReturn::Parallel { nodes } => nodes.iter().map(String::as_str).collect(),
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
