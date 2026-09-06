//! Plan IR types and type-local invariants.

use plasm_core::Expr;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;

macro_rules! plan_string_atom {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(format!("{} must be non-empty", stringify!($name)));
                }
                if value.contains("[object Object]") {
                    return Err(format!(
                        "{} contains JavaScript object string coercion ([object Object])",
                        stringify!($name)
                    ));
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    Read,
    Write,
    SideEffect,
    ArtifactRead,
}

/// Expected host result shape for dry-run / planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultShape {
    List,
    Single,
    MutationResult,
    SideEffectAck,
    Page,
    Artifact,
}

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
    /// Host may broadcast only when the dependency is statically provable as singleton.
    Auto,
    /// The author explicitly requested singleton broadcast; runtime still verifies one row.
    Singleton,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputCardinalityProof {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlanDataInput {
    pub(crate) node: PlanNodeId,
    pub(crate) alias: InputAlias,
    pub(crate) proof: InputCardinalityProof,
}

/// Executable Plasm IR for a program-plan node. `display_expr` is inert provenance only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanExprIr {
    pub expr: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedPlanExprIr {
    pub(crate) expr: Expr,
    pub(crate) projection: Option<Vec<String>>,
    pub(crate) display_expr: Option<String>,
}

/// IR template with value holes. The `expr` JSON must become `plasm_core::Expr`
/// after holes are instantiated; strings are never reparsed as Plasm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanExprTemplate {
    pub expr: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_bindings: Vec<PlanInputBinding>,
}

#[derive(Debug, Clone)]
pub struct ValidatedPlanExprTemplate {
    pub(crate) expr: serde_json::Value,
    pub(crate) projection: Option<Vec<String>>,
    pub(crate) display_expr: Option<String>,
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
#[derive(Debug, Clone)]
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
}

pub type ValidatedPlan = ValidatedPlanArtifact;

#[derive(Debug, Clone)]
pub enum ValidatedPlanNode {
    Surface(ValidatedSurfaceNode),
    Data(ValidatedDataNode),
    Derive(ValidatedDeriveNode),
    Compute(ValidatedComputeNode),
    ForEach(ValidatedForEachNode),
    IterateUntil(ValidatedIterateUntilNode),
    RelationTraversal(ValidatedRelationTraversalNode),
}

#[derive(Debug, Clone)]
pub struct ValidatedSurfaceNode {
    pub(crate) id: PlanNodeId,
    pub(crate) kind: PlanNodeKind,
    pub(crate) qualified_entity: Option<QualifiedEntityKey>,
    pub(crate) ir: Option<ValidatedPlanExprIr>,
    pub(crate) ir_template: Option<ValidatedPlanExprTemplate>,
    pub(crate) display_expr: Option<String>,
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

#[derive(Debug, Clone)]
pub struct ValidatedDataNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) data: PlanValue,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
}

#[derive(Debug, Clone)]
pub struct ValidatedDeriveNode {
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

#[derive(Debug, Clone)]
pub struct ValidatedComputeNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) compute: ComputeTemplate,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
}

#[derive(Debug, Clone)]
pub struct ValidatedForEachNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) source: PlanNodeId,
    pub(crate) item_binding: BindingName,
    pub(crate) effect_template: EffectTemplate,
    pub(crate) projection: Vec<String>,
    pub(crate) predicates: Vec<PlanPredicate>,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
    pub(crate) approval: Option<String>,
}

/// PLP-8 state iterator: observe seed, step+reobserve until predicate or bound exhaustion.
#[derive(Debug, Clone)]
pub struct ValidatedIterateUntilNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) source: PlanNodeId,
    pub(crate) item_binding: BindingName,
    pub(crate) effect_template: EffectTemplate,
    pub(crate) until_predicates: Vec<PlanPredicate>,
    pub(crate) take: u32,
    /// Seed Get IR for re-observe after each step (PLP-8).
    pub(crate) seed_ir: Option<ValidatedPlanExprIr>,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
    pub(crate) approval: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedRelationTraversalNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) relation: ValidatedPlanRelationTraversal,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
    pub(crate) pushed_read_budget: Option<crate::plan_read_bounds::PushedReadBudget>,
}

#[derive(Debug, Clone)]
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
    pub fn id(&self) -> &PlanNodeId {
        match self {
            Self::Surface(n) => &n.id,
            Self::Data(n) => &n.id,
            Self::Derive(n) => &n.id,
            Self::Compute(n) => &n.id,
            Self::ForEach(n) => &n.id,
            Self::IterateUntil(n) => &n.id,
            Self::RelationTraversal(n) => &n.id,
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
        }
    }

    pub fn as_surface(&self) -> Option<&ValidatedSurfaceNode> {
        match self {
            Self::Surface(n) => Some(n),
            _ => None,
        }
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

/// One typed node in the Plan DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanNode {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationCardinality {
    One,
    Many,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationSourceCardinality {
    Single,
    Many,
    RuntimeCheckedSingleton,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    Map,
    Data,
}

pub const PLAN_RENDER_MAX_TEMPLATE_CHARS: usize = 64 * 1024;
pub const PLAN_RENDER_MAX_ROWS: usize = 10_000;
pub const PLAN_RENDER_MAX_OUTPUT_CHARS: usize = 1024 * 1024;

pub(super) fn has_cycle(adj: &[Vec<usize>]) -> bool {
    let n = adj.len();
    let mut vis = vec![0u8; n];
    fn dfs(u: usize, adj: &[Vec<usize>], vis: &mut [u8]) -> bool {
        vis[u] = 1;
        for &v in &adj[u] {
            if v >= vis.len() {
                continue;
            }
            if vis[v] == 1 {
                return true;
            }
            if vis[v] == 0 && dfs(v, adj, vis) {
                return true;
            }
        }
        vis[u] = 2;
        false
    }
    for i in 0..n {
        if vis[i] == 0 && dfs(i, adj, &mut vis) {
            return true;
        }
    }
    false
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
