//! Serializable effect **`Plan`** contract (JSON DAG).
//!
//! Plasm programs and other hosts compile to this shape; the runtime validates the DAG, runs dry
//! reviews, and executes when requested.

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
    /// Named Plan return or synthetic result field.
    OutputName
}

plan_string_atom! {
    /// Alias under which a materialized dependency is available during derived evaluation.
    InputAlias
}

plan_string_atom! {
    /// Declared CGS relation name used by a Plasm program relation-traversal node.
    RelationName
}

/// Dotted field path after validation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    pub fn new(segments: Vec<String>) -> Result<Self, String> {
        if segments.is_empty() || segments.iter().any(|s| s.trim().is_empty()) {
            return Err("FieldPath must contain non-empty segments".to_string());
        }
        Ok(Self(segments))
    }

    pub fn from_dotted(path: &str) -> Result<Self, String> {
        Self::new(path.split('.').map(str::to_string).collect())
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn dotted(&self) -> String {
        self.0.join(".")
    }
}

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
            entry_id: q.catalog_entry_id,
            entity: q.entity,
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
pub enum CardinalityAnalysis {
    StaticSingleton,
    PluralOrUnknown,
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

/// A structured predicate preserved alongside the rendered Plasm expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPredicate {
    /// Field or dotted path segments (`updated_at`, `owner.login`, ...).
    pub field_path: Vec<String>,
    pub op: PlanPredicateOp,
    pub value: PlanValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPredicateOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    In,
    Exists,
}

/// Predicate/template values in the Plan DAG. `helper` preserves intent such as `daysAgo(30)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanValue {
    Literal {
        value: serde_json::Value,
    },
    Helper {
        name: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
    },
    BindingSymbol {
        binding: String,
        #[serde(default)]
        path: Vec<String>,
    },
    NodeSymbol {
        node: String,
        alias: String,
        #[serde(default)]
        path: Vec<String>,
    },
    Symbol {
        path: String,
    },
    Template {
        template: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        input_bindings: Vec<PlanInputBinding>,
    },
    EntityRefKey {
        api: String,
        entity: String,
        key: Box<PlanValue>,
    },
    Array {
        #[serde(default)]
        items: Vec<PlanValue>,
    },
    Object {
        #[serde(default)]
        fields: BTreeMap<String, PlanValue>,
    },
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

/// Proof-bearing Plan artifact consumed by dry-run and execution.
#[derive(Debug, Clone)]
pub struct ValidatedPlanArtifact {
    artifact: Plan<ValidatedPlanState>,
    topo: Vec<PlanNodeId>,
    node_indices: HashMap<PlanNodeId, usize>,
    approval_gates: Vec<PlanNodeId>,
}

pub type ValidatedPlan = ValidatedPlanArtifact;

#[derive(Debug, Clone)]
pub enum ValidatedPlanNode {
    Surface(ValidatedSurfaceNode),
    Data(ValidatedDataNode),
    Derive(ValidatedDeriveNode),
    Compute(ValidatedComputeNode),
    ForEach(ValidatedForEachNode),
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

#[derive(Debug, Clone)]
pub struct ValidatedRelationTraversalNode {
    pub(crate) id: PlanNodeId,
    pub(crate) effect_class: EffectClass,
    pub(crate) result_shape: ResultShape,
    pub(crate) relation: ValidatedPlanRelationTraversal,
    pub(crate) depends_on: Vec<PlanNodeId>,
    pub(crate) uses_result: Vec<PlanResultUse>,
}

#[derive(Debug, Clone)]
pub struct ValidatedPlanRelationTraversal {
    pub(crate) source: PlanNodeId,
    pub(crate) relation: RelationName,
    pub(crate) target: QualifiedEntityKey,
    pub(crate) cardinality: RelationCardinality,
    pub(crate) source_cardinality: RelationSourceCardinality,
    pub(crate) ir: ValidatedPlanExprIr,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputeTemplate {
    pub source: String,
    pub op: ComputeOp,
    pub schema: SyntheticResultSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComputeOp {
    Project {
        fields: BTreeMap<OutputName, FieldPath>,
    },
    Filter {
        predicates: Vec<PlanPredicate>,
    },
    GroupBy {
        #[serde(alias = "key", deserialize_with = "deserialize_group_by_keys")]
        keys: Vec<FieldPath>,
        aggregates: Vec<AggregateSpec>,
    },
    Aggregate {
        aggregates: Vec<AggregateSpec>,
    },
    Sort {
        key: FieldPath,
        #[serde(default)]
        descending: bool,
    },
    Limit {
        count: usize,
    },
    Render {
        columns: Vec<OutputName>,
        template: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateSpec {
    pub name: OutputName,
    pub function: AggregateFunction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<FieldPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateFunction {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticResultSchema {
    #[serde(default)]
    pub entity: Option<String>,
    pub fields: Vec<SyntheticFieldSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticFieldSchema {
    pub name: OutputName,
    pub value_kind: SyntheticValueKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FieldPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticValueKind {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
    Unknown,
}

pub const PLAN_RENDER_MAX_TEMPLATE_CHARS: usize = 64 * 1024;
pub const PLAN_RENDER_MAX_ROWS: usize = 10_000;
pub const PLAN_RENDER_MAX_OUTPUT_CHARS: usize = 1024 * 1024;

fn has_cycle(adj: &[Vec<usize>]) -> bool {
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

fn return_refs(ret: &PlanReturn) -> Vec<&str> {
    match ret {
        PlanReturn::Node { node } => vec![node.as_str()],
        PlanReturn::Parallel { nodes } => nodes.iter().map(String::as_str).collect(),
    }
}

/// Deserialize a program-shaped [`Plan`] from a JSON value (same IR shape as evaluation archives).
pub fn parse_plan_value(plan: &serde_json::Value) -> Result<Plan, String> {
    serde_json::from_value(plan.clone()).map_err(|e| format!("invalid serialized plan: {e}"))
}

/// Deserialize and validate a serialized plan JSON value (HTTP resolved-plan, MCP, CLI).
pub fn parse_and_validate_plan_json(plan: &serde_json::Value) -> Result<ValidatedPlan, String> {
    let plan_typed = parse_plan_value(plan)?;
    validate_plan_artifact(&plan_typed)
}

/// Parse and validate one program-shaped Plan.
pub fn validate_plan(plan: &Plan) -> Result<(), String> {
    validate_plan_artifact(plan).map(|_| ())
}

/// Parse and validate one program-shaped Plan, returning typed execution metadata.
pub fn validate_plan_artifact(plan: &Plan) -> Result<ValidatedPlan, String> {
    if plan.version != 1 {
        return Err(format!("unsupported Plan version: {}", plan.version));
    }
    if plan.nodes.is_empty() {
        return Err(
            "plan.nodes must be non-empty: a Plasm program must include at least one executable DAG node (taught `query` / `get` / search / relation forms per DOMAIN); a literal-only final roots line is not executable alone"
                .to_string(),
        );
    }
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (i, n) in plan.nodes.iter().enumerate() {
        if n.id.trim().is_empty() {
            return Err(format!("plan.nodes[{i}].id is empty"));
        }
        if by_id.insert(n.id.clone(), i).is_some() {
            return Err(format!("duplicate plan node id: {}", n.id));
        }
        PlanNodeId::new(n.id.clone()).map_err(|e| format!("plan.nodes[{i}].id: {e}"))?;
    }

    for (i, n) in plan.nodes.iter().enumerate() {
        if n.kind.has_surface_expr() {
            if n.ir.is_none() && n.ir_template.is_none() {
                return Err(format!(
                    "plan.nodes[{i}].ir or ir_template is required for executable node {:?}",
                    n.kind
                ));
            }
            if n.ir.is_some() && n.ir_template.is_some() {
                return Err(format!(
                    "plan.nodes[{i}] must not carry both ir and ir_template"
                ));
            }
            if let Some(expr) = &n.expr {
                validate_no_js_object_coercion(expr, i, "expr")?;
            }
            if let Some(ir) = &n.ir {
                validate_plan_expr_ir(ir, i, "ir")?;
            }
            if let Some(template) = &n.ir_template {
                validate_plan_expr_template(template, i, "ir_template")?;
            }
            if n.qualified_entity.is_none() && n.result_shape != ResultShape::Page {
                return Err(format!(
                    "plan.nodes[{i}].qualified_entity is required for executable node {:?}",
                    n.kind
                ));
            }
            if n.kind == PlanNodeKind::Search {
                if n.effect_class != EffectClass::Read {
                    return Err(format!("plan.nodes[{i}].search effect_class must be read"));
                }
                if n.result_shape != ResultShape::List {
                    return Err(format!("plan.nodes[{i}].search result_shape must be list"));
                }
            }
        }
        if n.kind == PlanNodeKind::Derive
            && (n.expr.is_some() || n.ir.is_some() || n.ir_template.is_some())
        {
            return Err(format!(
                "plan.nodes[{i}].derive must not carry expr, ir, or ir_template"
            ));
        }
        if n.kind == PlanNodeKind::Data {
            if n.data.is_none() {
                return Err(format!("plan.nodes[{i}].data is required for data nodes"));
            }
            if n.expr.is_some() || n.ir.is_some() || n.ir_template.is_some() {
                return Err(format!(
                    "plan.nodes[{i}].data must not carry expr, ir, or ir_template"
                ));
            }
        }
        if n.kind == PlanNodeKind::Compute {
            let compute = n
                .compute
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{i}].compute is required for compute nodes"))?;
            if n.expr.is_some()
                || n.ir.is_some()
                || n.ir_template.is_some()
                || n.data.is_some()
                || n.effect_template.is_some()
                || n.relation.is_some()
            {
                return Err(format!(
                    "plan.nodes[{i}].compute must not carry expr, ir, ir_template, data, effect_template, or relation"
                ));
            }
            validate_compute_template(compute, i, &by_id)?;
        } else if n.compute.is_some() {
            return Err(format!(
                "plan.nodes[{i}].compute is only valid for compute nodes"
            ));
        }
        if n.kind == PlanNodeKind::Relation {
            let relation = n.relation.as_ref().ok_or_else(|| {
                format!("plan.nodes[{i}].relation is required for relation nodes")
            })?;
            if n.effect_class != EffectClass::Read {
                return Err(format!(
                    "plan.nodes[{i}].relation effect_class must be read"
                ));
            }
            if !matches!(n.result_shape, ResultShape::List | ResultShape::Single) {
                return Err(format!(
                    "plan.nodes[{i}].relation result_shape must be list or single"
                ));
            }
            if n.expr.is_some()
                || n.ir.is_some()
                || n.ir_template.is_some()
                || n.data.is_some()
                || n.effect_template.is_some()
                || n.compute.is_some()
            {
                return Err(format!(
                    "plan.nodes[{i}].relation must not carry expr, ir, ir_template, data, effect_template, or compute"
                ));
            }
            validate_relation_traversal(plan, relation, i, &by_id)?;
        } else if n.relation.is_some() {
            return Err(format!(
                "plan.nodes[{i}].relation is only valid for relation nodes"
            ));
        }
        if let Some(data) = &n.data {
            validate_plan_value_expr(data, i, "data")?;
        }
        if let Some(derive_template) = &n.derive_template {
            validate_plan_value_expr(&derive_template.value, i, "derive_template.value")?;
            if derive_template.kind == DeriveKind::Map {
                let source = derive_template.source.as_deref().unwrap_or_default();
                if source.trim().is_empty() || !by_id.contains_key(source) {
                    return Err(format!(
                        "plan.nodes[{i}].derive_template.source references unknown id {source:?}"
                    ));
                }
                let binding = derive_template.item_binding.as_deref().unwrap_or_default();
                if binding.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{i}].derive_template.item_binding is required for map"
                    ));
                }
            }
            for input in &derive_template.inputs {
                validate_plan_data_input(input, i, &by_id)?;
                if input.cardinality == InputCardinality::Auto
                    && analyze_static_cardinality(plan, &by_id, input.node.as_str())
                        != CardinalityAnalysis::StaticSingleton
                {
                    return Err(format!(
                        "plan.nodes[{i}].derive_template.inputs node {:?} is not statically singleton; wrap it with Plan.singleton(...) to request runtime-checked broadcast",
                        input.node
                    ));
                }
            }
            validate_derive_value_inputs(derive_template, i)?;
        }
        for (j, p) in n.predicates.iter().enumerate() {
            validate_predicate(p, i, j)?;
        }
        if n.kind == PlanNodeKind::ForEach {
            let source = n
                .source
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{i}].source is required for for_each"))?;
            if !by_id.contains_key(source) {
                return Err(format!(
                    "plan.nodes[{i}].source references unknown id {source:?}"
                ));
            }
            let binding = n.item_binding.as_deref().unwrap_or_default();
            if binding.trim().is_empty() {
                return Err(format!(
                    "plan.nodes[{i}].item_binding is required for for_each"
                ));
            }
            let template = n
                .effect_template
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{i}].effect_template is required"))?;
            validate_effect_template(template, i)?;
            let mut input_aliases: Vec<(&str, &str)> = Vec::new();
            for u in &n.uses_result {
                if u.r#as.as_str() != binding {
                    input_aliases.push((u.r#as.as_str(), u.node.as_str()));
                }
            }
            let ctx = plasm_core::TemplateRefContext {
                row_binding: Some(binding),
                input_aliases: &input_aliases,
            };
            validate_effect_template_interpolation(template, i, &ctx)?;
            for b in &template.input_bindings {
                if !b.from.starts_with(&format!("{binding}."))
                    && b.from.as_str() != binding
                    && !b.from.contains('.')
                {
                    return Err(format!(
                        "plan.nodes[{i}].effect_template.input_bindings source {:?} does not reference item binding {:?}",
                        b.from, binding
                    ));
                }
            }
        }
    }

    let mut adj: Vec<Vec<usize>> = vec![vec![]; plan.nodes.len()];
    for (i, n) in plan.nodes.iter().enumerate() {
        for d in &n.depends_on {
            let t = *by_id
                .get(d)
                .ok_or_else(|| format!("plan.nodes[{i}].depends_on references unknown id {d:?}"))?;
            adj[i].push(t);
        }
        for u in &n.uses_result {
            let t = *by_id.get(&u.node).ok_or_else(|| {
                format!(
                    "plan.nodes[{i}].uses_result.node {:?} is not a known id",
                    u.node
                )
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
        if let Some(source) = &n.source {
            let t = *by_id.get(source).ok_or_else(|| {
                format!("plan.nodes[{i}].source references unknown id {source:?}")
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
        if let Some(derive_template) = &n.derive_template {
            if let Some(source) = &derive_template.source {
                let t = *by_id.get(source).ok_or_else(|| {
                    format!(
                        "plan.nodes[{i}].derive_template.source references unknown id {source:?}"
                    )
                })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
            for input in &derive_template.inputs {
                let t = *by_id.get(&input.node).ok_or_else(|| {
                    format!(
                        "plan.nodes[{i}].derive_template.inputs references unknown id {:?}",
                        input.node
                    )
                })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
        }
        if let Some(compute) = &n.compute {
            let t = *by_id.get(&compute.source).ok_or_else(|| {
                format!(
                    "plan.nodes[{i}].compute.source references unknown id {:?}",
                    compute.source
                )
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
        if let Some(relation) = &n.relation {
            let t = *by_id.get(&relation.source).ok_or_else(|| {
                format!(
                    "plan.nodes[{i}].relation.source references unknown id {:?}",
                    relation.source
                )
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
    }
    if has_cycle(&adj) {
        return Err("plan: depends_on has a cycle".to_string());
    }
    let return_value = validate_return_refs(&plan.return_value, &by_id)?;
    let topo = topological_order(plan, &adj)?;
    let mut node_indices = HashMap::new();
    for (id, idx) in &by_id {
        node_indices.insert(PlanNodeId::new(id.clone())?, *idx);
    }
    let nodes = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| validated_node_from_raw(plan, node, i, &by_id))
        .collect::<Result<Vec<_>, _>>()?;
    let approval_gates = plan
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                PlanNodeKind::Create
                    | PlanNodeKind::Update
                    | PlanNodeKind::Delete
                    | PlanNodeKind::Action
                    | PlanNodeKind::ForEach
            ) || matches!(n.effect_class, EffectClass::Write | EffectClass::SideEffect)
        })
        .map(|n| PlanNodeId::new(n.id.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ValidatedPlan {
        artifact: Plan {
            version: plan.version,
            kind: plan.kind,
            name: plan.name.clone(),
            nodes,
            return_value,
            metadata: plan.metadata.clone(),
            state: PhantomData,
        },
        topo,
        node_indices,
        approval_gates,
    })
}

fn validated_node_from_raw(
    plan: &Plan,
    node: &PlanNode,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanNode, String> {
    let id = PlanNodeId::new(node.id.clone())?;
    let depends_on = typed_node_ids(&node.depends_on)?;
    let uses_result = node.uses_result.clone();
    match node.kind {
        kind @ (PlanNodeKind::Query
        | PlanNodeKind::Search
        | PlanNodeKind::Get
        | PlanNodeKind::Create
        | PlanNodeKind::Update
        | PlanNodeKind::Delete
        | PlanNodeKind::Action) => {
            let ir = node
                .ir
                .as_ref()
                .map(|ir| validated_plan_expr_ir(ir, node_index, "ir"))
                .transpose()?;
            let ir_template = node
                .ir_template
                .as_ref()
                .map(|template| validated_plan_expr_template(template, node_index, "ir_template"))
                .transpose()?;
            Ok(ValidatedPlanNode::Surface(ValidatedSurfaceNode {
                id,
                kind,
                qualified_entity: node.qualified_entity.clone(),
                ir,
                ir_template,
                display_expr: node.expr.clone(),
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                projection: node.projection.clone(),
                predicates: node.predicates.clone(),
                depends_on,
                uses_result,
                approval: node.approval.clone(),
                page_size: node.page_size,
            }))
        }
        PlanNodeKind::Data => Ok(ValidatedPlanNode::Data(ValidatedDataNode {
            id,
            effect_class: node.effect_class,
            result_shape: node.result_shape,
            data: node
                .data
                .clone()
                .ok_or_else(|| format!("plan.nodes[{node_index}].data is required"))?,
            depends_on,
            uses_result,
        })),
        PlanNodeKind::Derive => {
            let template = node
                .derive_template
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].derive_template is required"))?;
            let source = template
                .source
                .as_ref()
                .ok_or_else(|| {
                    format!("plan.nodes[{node_index}].derive_template.source is required")
                })
                .and_then(|s| PlanNodeId::new(s.clone()))?;
            let item_binding = template
                .item_binding
                .as_ref()
                .ok_or_else(|| {
                    format!("plan.nodes[{node_index}].derive_template.item_binding is required")
                })
                .and_then(|s| BindingName::new(s.clone()))?;
            let inputs = template
                .inputs
                .iter()
                .map(|input| validated_data_input(plan, input, by_id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ValidatedPlanNode::Derive(ValidatedDeriveNode {
                id,
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                source,
                item_binding,
                inputs,
                value: template.value.clone(),
                depends_on,
                uses_result,
            }))
        }
        PlanNodeKind::Compute => Ok(ValidatedPlanNode::Compute(ValidatedComputeNode {
            id,
            effect_class: node.effect_class,
            result_shape: node.result_shape,
            compute: node
                .compute
                .clone()
                .ok_or_else(|| format!("plan.nodes[{node_index}].compute is required"))?,
            depends_on,
            uses_result,
        })),
        PlanNodeKind::Relation => {
            let relation = node
                .relation
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].relation is required"))?;
            Ok(ValidatedPlanNode::RelationTraversal(
                ValidatedRelationTraversalNode {
                    id,
                    effect_class: node.effect_class,
                    result_shape: node.result_shape,
                    relation: ValidatedPlanRelationTraversal {
                        source: PlanNodeId::new(relation.source.clone())?,
                        relation: RelationName::new(relation.relation.clone())?,
                        target: relation.target.clone(),
                        cardinality: relation.cardinality,
                        source_cardinality: relation.source_cardinality,
                        ir: validated_plan_expr_ir(&relation.ir, node_index, "relation.ir")?,
                        binding_proofs: relation.binding_proofs.clone(),
                    },
                    depends_on,
                    uses_result,
                },
            ))
        }
        PlanNodeKind::ForEach => Ok(ValidatedPlanNode::ForEach(ValidatedForEachNode {
            id,
            effect_class: node.effect_class,
            result_shape: node.result_shape,
            source: node
                .source
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].source is required"))
                .and_then(|s| PlanNodeId::new(s.clone()))?,
            item_binding: node
                .item_binding
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].item_binding is required"))
                .and_then(|s| BindingName::new(s.clone()))?,
            effect_template: validated_effect_template(
                node.effect_template.as_ref().ok_or_else(|| {
                    format!("plan.nodes[{node_index}].effect_template is required")
                })?,
                node_index,
            )?,
            projection: node.projection.clone(),
            predicates: node.predicates.clone(),
            depends_on,
            uses_result,
            approval: node.approval.clone(),
        })),
    }
}

fn typed_node_ids(raw: &[String]) -> Result<Vec<PlanNodeId>, String> {
    raw.iter().cloned().map(PlanNodeId::new).collect()
}

fn validated_data_input(
    plan: &Plan,
    input: &PlanDataInput,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanDataInput, String> {
    let proof = match input.cardinality {
        InputCardinality::Singleton => InputCardinalityProof::RuntimeCheckedSingleton,
        InputCardinality::Auto => {
            match analyze_static_cardinality(plan, by_id, input.node.as_str()) {
                CardinalityAnalysis::StaticSingleton => InputCardinalityProof::StaticSingleton,
                CardinalityAnalysis::PluralOrUnknown => {
                    return Err(format!(
                        "input {:?} is not statically singleton and lacks explicit singleton proof",
                        input.node
                    ));
                }
            }
        }
    };
    Ok(ValidatedPlanDataInput {
        node: PlanNodeId::new(input.node.clone())?,
        alias: InputAlias::new(input.alias.clone())?,
        proof,
    })
}

fn validated_plan_expr_ir(
    ir: &PlanExprIr,
    node_index: usize,
    path: &str,
) -> Result<ValidatedPlanExprIr, String> {
    validate_plan_expr_ir(ir, node_index, path)?;
    let expr = serde_json::from_value::<Expr>(ir.expr.clone())
        .map_err(|e| format!("plan.nodes[{node_index}].{path}.expr is invalid Plasm IR: {e}"))?;
    Ok(ValidatedPlanExprIr {
        expr,
        projection: ir.projection.clone(),
        display_expr: ir.display_expr.clone(),
    })
}

fn validated_plan_expr_template(
    template: &PlanExprTemplate,
    node_index: usize,
    path: &str,
) -> Result<ValidatedPlanExprTemplate, String> {
    validate_plan_expr_template(template, node_index, path)?;
    Ok(ValidatedPlanExprTemplate {
        expr: template.expr.clone(),
        projection: template.projection.clone(),
        display_expr: template.display_expr.clone(),
        input_bindings: template.input_bindings.clone(),
    })
}

fn validated_effect_template(
    template: &EffectTemplate,
    node_index: usize,
) -> Result<EffectTemplate, String> {
    validate_effect_template(template, node_index)?;
    Ok(template.clone())
}

fn validate_effect_template_interpolation(
    template: &EffectTemplate,
    node_index: usize,
    ctx: &plasm_core::TemplateRefContext<'_>,
) -> Result<(), String> {
    plasm_core::validate_interpolation_syntax(&template.expr_template, |detail| {
        format!("plan.nodes[{node_index}].effect_template.expr_template {detail}")
    })?;
    ctx.validate_string_roots(&template.expr_template, |root| {
        format!(
            "plan.nodes[{node_index}].effect_template.expr_template references undeclared alias {root:?}"
        )
    })?;
    validate_json_interpolation_refs(
        &template.ir_template.expr,
        node_index,
        "effect_template.ir_template.expr",
        ctx,
    )
}

fn validate_json_interpolation_refs(
    value: &serde_json::Value,
    node_index: usize,
    path: &str,
    ctx: &plasm_core::TemplateRefContext<'_>,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(s) => {
            plasm_core::validate_interpolation_syntax(s, |detail| {
                format!("plan.nodes[{node_index}].{path} {detail}")
            })?;
            ctx.validate_string_roots(s, |root| {
                format!("plan.nodes[{node_index}].{path} references undeclared alias {root:?}")
            })
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                validate_json_interpolation_refs(item, node_index, &format!("{path}[{i}]"), ctx)?;
            }
            Ok(())
        }
        serde_json::Value::Object(fields) => {
            for (k, field) in fields {
                validate_json_interpolation_refs(field, node_index, &format!("{path}.{k}"), ctx)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_effect_template(t: &EffectTemplate, node_index: usize) -> Result<(), String> {
    if !t.kind.is_template_allowed() {
        return Err(format!(
            "plan.nodes[{node_index}].effect_template.kind {:?} is not executable",
            t.kind
        ));
    }
    if t.expr_template.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].effect_template.expr_template is empty"
        ));
    }
    validate_no_js_object_coercion(
        &t.expr_template,
        node_index,
        "effect_template.expr_template",
    )?;
    validate_plan_expr_template(&t.ir_template, node_index, "effect_template.ir_template")?;
    for b in &t.input_bindings {
        if b.from.trim().is_empty() || b.to.trim().is_empty() {
            return Err(format!(
                "plan.nodes[{node_index}].effect_template.input_bindings must be non-empty"
            ));
        }
    }
    Ok(())
}

fn validate_plan_expr_ir(ir: &PlanExprIr, node_index: usize, path: &str) -> Result<(), String> {
    if let Some(display) = &ir.display_expr {
        validate_no_js_object_coercion(display, node_index, path)?;
    }
    serde_json::from_value::<Expr>(ir.expr.clone())
        .map_err(|e| format!("plan.nodes[{node_index}].{path}.expr is invalid Plasm IR: {e}"))?;
    Ok(())
}

fn validate_plan_expr_template(
    template: &PlanExprTemplate,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    if let Some(display) = &template.display_expr {
        validate_no_js_object_coercion(display, node_index, path)?;
    }
    let concrete = instantiate_template_holes_for_validation(&template.expr);
    serde_json::from_value::<Expr>(concrete).map_err(|e| {
        format!("plan.nodes[{node_index}].{path}.expr is invalid templated Plasm IR: {e}")
    })?;
    for b in &template.input_bindings {
        if b.from.trim().is_empty() {
            return Err(format!(
                "plan.nodes[{node_index}].{path}.input_bindings must have non-empty from"
            ));
        }
    }
    Ok(())
}

fn instantiate_template_holes_for_validation(value: &serde_json::Value) -> serde_json::Value {
    if is_ir_hole(value) {
        return serde_json::Value::String("__plasm_hole__".to_string());
    }
    match value {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(instantiate_template_holes_for_validation)
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), instantiate_template_holes_for_validation(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) fn is_ir_hole(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .and_then(|obj| obj.get("__plasm_hole"))
        .is_some()
}

fn validate_plan_data_input(
    input: &PlanDataInput,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), String> {
    if input.node.trim().is_empty() || !by_id.contains_key(&input.node) {
        return Err(format!(
            "plan.nodes[{node_index}].derive_template.inputs references unknown id {:?}",
            input.node
        ));
    }
    if input.alias.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].derive_template.inputs alias must be non-empty"
        ));
    }
    Ok(())
}

fn validate_derive_value_inputs(
    template: &DeriveTemplate,
    node_index: usize,
) -> Result<(), String> {
    let mut inputs_by_alias = HashMap::new();
    for input in &template.inputs {
        if inputs_by_alias
            .insert(input.alias.as_str(), input.node.as_str())
            .is_some()
        {
            return Err(format!(
                "plan.nodes[{node_index}].derive_template.inputs duplicate alias {:?}",
                input.alias
            ));
        }
    }
    validate_plan_value_input_refs(
        &template.value,
        node_index,
        &inputs_by_alias,
        template.item_binding.as_deref(),
    )
}

fn validate_plan_value_input_refs(
    value: &PlanValue,
    node_index: usize,
    inputs_by_alias: &HashMap<&str, &str>,
    item_binding: Option<&str>,
) -> Result<(), String> {
    match value {
        PlanValue::NodeSymbol { node, alias, .. } => match inputs_by_alias.get(alias.as_str()) {
            Some(input_node) if *input_node == node.as_str() => Ok(()),
            Some(input_node) => Err(format!(
                "plan.nodes[{node_index}].derive_template.value node symbol alias {:?} points at {:?}, not {:?}",
                alias, input_node, node
            )),
            None => Err(format!(
                "plan.nodes[{node_index}].derive_template.value node symbol alias {:?} is not declared in inputs",
                alias
            )),
        },
        PlanValue::Template {
            template,
            input_bindings,
        } => {
            for binding in input_bindings {
                let Some((alias, _)) = binding.from.split_once('.') else {
                    continue;
                };
                validate_template_alias(alias, node_index, inputs_by_alias, item_binding)?;
            }
            for raw_path in plasm_core::interpolation_paths(template) {
                let (alias, _) = raw_path
                    .split_once('.')
                    .map_or((raw_path.as_str(), ""), |(alias, rest)| (alias, rest));
                if alias.is_empty() {
                    continue;
                }
                validate_template_alias(alias, node_index, inputs_by_alias, item_binding)?;
            }
            Ok(())
        }
        PlanValue::Array { items } => {
            for item in items {
                validate_plan_value_input_refs(item, node_index, inputs_by_alias, item_binding)?;
            }
            Ok(())
        }
        PlanValue::EntityRefKey { key, .. } => {
            validate_plan_value_input_refs(key, node_index, inputs_by_alias, item_binding)
        }
        PlanValue::Object { fields } => {
            for field in fields.values() {
                validate_plan_value_input_refs(field, node_index, inputs_by_alias, item_binding)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_template_alias(
    alias: &str,
    node_index: usize,
    inputs_by_alias: &HashMap<&str, &str>,
    item_binding: Option<&str>,
) -> Result<(), String> {
    if item_binding == Some(alias) || inputs_by_alias.contains_key(alias) {
        return Ok(());
    }
    Err(format!(
        "plan.nodes[{node_index}].derive_template.value template references undeclared alias {alias:?}"
    ))
}

fn analyze_static_cardinality(
    plan: &Plan,
    by_id: &HashMap<String, usize>,
    node_id: &str,
) -> CardinalityAnalysis {
    fn inner(
        plan: &Plan,
        by_id: &HashMap<String, usize>,
        node_id: &str,
        memo: &mut HashMap<String, CardinalityAnalysis>,
    ) -> CardinalityAnalysis {
        if let Some(v) = memo.get(node_id) {
            return *v;
        }
        let Some(index) = by_id.get(node_id).copied() else {
            return CardinalityAnalysis::PluralOrUnknown;
        };
        let node = &plan.nodes[index];
        let singleton = match node.kind {
            PlanNodeKind::Get => CardinalityAnalysis::StaticSingleton,
            PlanNodeKind::Data => match &node.data {
                Some(PlanValue::Array { items }) if items.len() == 1 => {
                    CardinalityAnalysis::StaticSingleton
                }
                Some(PlanValue::Literal { value })
                    if value.as_array().is_none_or(|items| items.len() == 1) =>
                {
                    CardinalityAnalysis::StaticSingleton
                }
                Some(
                    PlanValue::Array { .. }
                    | PlanValue::Literal { .. }
                    | PlanValue::EntityRefKey { .. },
                )
                | None => CardinalityAnalysis::PluralOrUnknown,
                Some(_) => CardinalityAnalysis::StaticSingleton,
            },
            PlanNodeKind::Derive => node
                .derive_template
                .as_ref()
                .and_then(|t| t.source.as_deref())
                .map(|source| inner(plan, by_id, source, memo))
                .unwrap_or(CardinalityAnalysis::PluralOrUnknown),
            PlanNodeKind::Compute => node
                .compute
                .as_ref()
                .map(|compute| match &compute.op {
                    ComputeOp::Aggregate { .. } | ComputeOp::Render { .. } => {
                        CardinalityAnalysis::StaticSingleton
                    }
                    ComputeOp::Project { .. }
                    | ComputeOp::Filter { .. }
                    | ComputeOp::Sort { .. } => inner(plan, by_id, &compute.source, memo),
                    ComputeOp::Limit { count } if *count <= 1 => {
                        CardinalityAnalysis::StaticSingleton
                    }
                    ComputeOp::Limit { .. } | ComputeOp::GroupBy { .. } => {
                        CardinalityAnalysis::PluralOrUnknown
                    }
                })
                .unwrap_or(CardinalityAnalysis::PluralOrUnknown),
            PlanNodeKind::Relation => node
                .relation
                .as_ref()
                .map(
                    |relation| match (relation.cardinality, relation.source_cardinality) {
                        (RelationCardinality::One, RelationSourceCardinality::Single) => {
                            inner(plan, by_id, &relation.source, memo)
                        }
                        (
                            RelationCardinality::One,
                            RelationSourceCardinality::RuntimeCheckedSingleton,
                        ) => CardinalityAnalysis::PluralOrUnknown,
                        (RelationCardinality::Many, _)
                        | (RelationCardinality::One, RelationSourceCardinality::Many) => {
                            CardinalityAnalysis::PluralOrUnknown
                        }
                    },
                )
                .unwrap_or(CardinalityAnalysis::PluralOrUnknown),
            _ => CardinalityAnalysis::PluralOrUnknown,
        };
        memo.insert(node_id.to_string(), singleton);
        singleton
    }
    inner(plan, by_id, node_id, &mut HashMap::new())
}

fn validate_compute_template(
    t: &ComputeTemplate,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), String> {
    if t.source.trim().is_empty() || !by_id.contains_key(&t.source) {
        return Err(format!(
            "plan.nodes[{node_index}].compute.source references unknown id {:?}",
            t.source
        ));
    }
    if t.schema.fields.is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].compute.schema.fields must be non-empty"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for field in &t.schema.fields {
        if !seen.insert(field.name.as_str().to_string()) {
            return Err(format!(
                "plan.nodes[{node_index}].compute.schema.fields contains duplicate {:?}",
                field.name.as_str()
            ));
        }
    }
    if t.page_size == Some(0) {
        return Err(format!(
            "plan.nodes[{node_index}].compute.page_size must be greater than zero"
        ));
    }
    match &t.op {
        ComputeOp::Project { fields } if fields.is_empty() => {
            return Err(format!(
                "plan.nodes[{node_index}].compute.project.fields must be non-empty"
            ));
        }
        ComputeOp::Filter { predicates } => {
            for (j, p) in predicates.iter().enumerate() {
                validate_predicate(p, node_index, j)?;
            }
        }
        ComputeOp::GroupBy { aggregates, .. } | ComputeOp::Aggregate { aggregates } => {
            if aggregates.is_empty() {
                return Err(format!(
                    "plan.nodes[{node_index}].compute aggregates must be non-empty"
                ));
            }
            for agg in aggregates {
                if agg.function != AggregateFunction::Count && agg.field.is_none() {
                    return Err(format!(
                        "plan.nodes[{node_index}].compute aggregate {:?} requires a field",
                        agg.name.as_str()
                    ));
                }
            }
        }
        ComputeOp::Limit { count } if *count == 0 => {
            return Err(format!(
                "plan.nodes[{node_index}].compute.limit.count must be greater than zero"
            ));
        }
        ComputeOp::Render { columns, template } => {
            validate_render_compute_template(t, columns, template, node_index)?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_render_compute_template(
    t: &ComputeTemplate,
    columns: &[OutputName],
    template: &str,
    node_index: usize,
) -> Result<(), String> {
    if columns.is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.columns must be non-empty"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for column in columns {
        OutputName::new(column.as_str().to_string())
            .map_err(|e| format!("plan.nodes[{node_index}].compute.render.columns: {e}"))?;
        if !seen.insert(column.as_str().to_string()) {
            return Err(format!(
                "plan.nodes[{node_index}].compute.render.columns has duplicate {:?}",
                column.as_str()
            ));
        }
    }
    if template.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.template must be non-empty"
        ));
    }
    if template.chars().count() > PLAN_RENDER_MAX_TEMPLATE_CHARS {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.template exceeds {PLAN_RENDER_MAX_TEMPLATE_CHARS} characters"
        ));
    }
    let mut env = minijinja::Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    env.add_template("plan_render", template)
        .map_err(|e| format!("plan.nodes[{node_index}].compute.render.template: {e}"))?;
    if t.schema.entity.as_deref() != Some("PlanRender")
        || t.schema.fields.len() != 1
        || t.schema.fields[0].name.as_str() != "content"
        || t.schema.fields[0].value_kind != SyntheticValueKind::String
    {
        return Err(format!(
            "plan.nodes[{node_index}].compute.render.schema must be entity PlanRender with a single string field named 'content'"
        ));
    }
    Ok(())
}

fn validate_relation_traversal(
    plan: &Plan,
    relation: &PlanRelationTraversal,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), String> {
    if relation.source.trim().is_empty() || !by_id.contains_key(&relation.source) {
        return Err(format!(
            "plan.nodes[{node_index}].relation.source references unknown id {:?}",
            relation.source
        ));
    }
    RelationName::new(relation.relation.clone())
        .map_err(|e| format!("plan.nodes[{node_index}].relation.relation: {e}"))?;
    if relation.target.entry_id.trim().is_empty() || relation.target.entity.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].relation.target must include non-empty entry_id and entity"
        ));
    }
    validate_plan_expr_ir(&relation.ir, node_index, "relation.ir")?;
    if relation.cardinality == RelationCardinality::One
        && relation.source_cardinality == RelationSourceCardinality::Many
    {
        return Err(format!(
            "plan.nodes[{node_index}].relation one-cardinality traversal requires a singleton source; wrap the source with Plan.singleton(...)"
        ));
    }
    if relation.cardinality == RelationCardinality::One
        && relation.source_cardinality == RelationSourceCardinality::Single
        && analyze_static_cardinality(plan, by_id, relation.source.as_str())
            != CardinalityAnalysis::StaticSingleton
    {
        return Err(format!(
            "plan.nodes[{node_index}].relation source {:?} is not statically singleton; use Plan.singleton(...) for runtime-checked traversal",
            relation.source
        ));
    }
    Ok(())
}

fn validate_return_refs(
    ret: &PlanReturn,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanReturn, String> {
    match ret {
        PlanReturn::Node { node } if node.trim().is_empty() => {
            return Err("plan.return node id must be non-empty".to_string());
        }
        PlanReturn::Parallel { nodes } if nodes.is_empty() => {
            return Err("plan.return.nodes must contain at least one node".to_string());
        }
        _ => {}
    }
    for id in return_refs(ret) {
        if !by_id.contains_key(id) {
            return Err(format!("plan.return references unknown id {id:?}"));
        }
    }
    match ret {
        PlanReturn::Node { node } => Ok(ValidatedPlanReturn::Node(PlanNodeId::new(node.clone())?)),
        PlanReturn::Parallel { nodes } => Ok(ValidatedPlanReturn::Parallel {
            parallel: nodes
                .iter()
                .cloned()
                .map(PlanNodeId::new)
                .collect::<Result<Vec<_>, _>>()?,
        }),
    }
}

fn topological_order(plan: &Plan, adj: &[Vec<usize>]) -> Result<Vec<PlanNodeId>, String> {
    fn visit(
        i: usize,
        plan: &Plan,
        adj: &[Vec<usize>],
        mark: &mut [u8],
        out: &mut Vec<PlanNodeId>,
    ) -> Result<(), String> {
        if mark[i] == 2 {
            return Ok(());
        }
        if mark[i] == 1 {
            return Err("plan: depends_on has a cycle".to_string());
        }
        mark[i] = 1;
        for &d in &adj[i] {
            visit(d, plan, adj, mark, out)?;
        }
        mark[i] = 2;
        out.push(PlanNodeId::new(plan.nodes[i].id.clone())?);
        Ok(())
    }
    let mut mark = vec![0u8; plan.nodes.len()];
    let mut out = Vec::with_capacity(plan.nodes.len());
    for i in 0..plan.nodes.len() {
        visit(i, plan, adj, &mut mark, &mut out)?;
    }
    out.dedup();
    Ok(out)
}

fn validate_predicate(
    p: &PlanPredicate,
    node_index: usize,
    pred_index: usize,
) -> Result<(), String> {
    if p.field_path.is_empty() || p.field_path.iter().any(|s| s.trim().is_empty()) {
        return Err(format!(
            "plan.nodes[{node_index}].predicates[{pred_index}].field_path must be non-empty"
        ));
    }
    if let PlanValue::Helper { name, .. } = &p.value {
        return Err(format!(
            "plan.nodes[{node_index}].predicates[{pred_index}].value helper {name:?} is not executable in program-plan predicates; pass a literal/entity_ref_key value or precompute it"
        ));
    }
    validate_plan_value_expr(
        &p.value,
        node_index,
        &format!("predicates[{pred_index}].value"),
    )?;
    Ok(())
}

fn validate_plan_value_expr(
    value: &PlanValue,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    match value {
        PlanValue::Literal { value } => {
            validate_json_value_no_js_object_coercion(value, node_index, path)
        }
        PlanValue::Helper { display, args, .. } => {
            if let Some(display) = display {
                validate_no_js_object_coercion(display, node_index, path)?;
            }
            for (i, arg) in args.iter().enumerate() {
                validate_json_value_no_js_object_coercion(
                    arg,
                    node_index,
                    &format!("{path}.args[{i}]"),
                )?;
            }
            Ok(())
        }
        PlanValue::Symbol { path: symbol_path } => {
            validate_no_js_object_coercion(symbol_path, node_index, path)
        }
        PlanValue::BindingSymbol {
            binding,
            path: segments,
        } => {
            validate_no_js_object_coercion(binding, node_index, path)?;
            for (i, segment) in segments.iter().enumerate() {
                validate_no_js_object_coercion(segment, node_index, &format!("{path}.path[{i}]"))?;
            }
            Ok(())
        }
        PlanValue::NodeSymbol {
            node,
            alias,
            path: segments,
        } => {
            validate_no_js_object_coercion(node, node_index, path)?;
            validate_no_js_object_coercion(alias, node_index, path)?;
            for (i, segment) in segments.iter().enumerate() {
                validate_no_js_object_coercion(segment, node_index, &format!("{path}.path[{i}]"))?;
            }
            Ok(())
        }
        PlanValue::Template {
            template,
            input_bindings,
        } => {
            validate_template_text(template, node_index, path)?;
            for b in input_bindings {
                if b.from.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{node_index}].{path} has an empty binding"
                    ));
                }
            }
            Ok(())
        }
        PlanValue::EntityRefKey { api, entity, key } => {
            validate_no_js_object_coercion(api, node_index, path)?;
            validate_no_js_object_coercion(entity, node_index, path)?;
            if api.trim().is_empty() || entity.trim().is_empty() {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} entity_ref_key api and entity must be non-empty"
                ));
            }
            validate_entity_ref_key_value(key, node_index, &format!("{path}.key"))
        }
        PlanValue::Array { items } => {
            for (i, item) in items.iter().enumerate() {
                validate_plan_value_expr(item, node_index, &format!("{path}.items[{i}]"))?;
            }
            Ok(())
        }
        PlanValue::Object { fields } => {
            if looks_like_unnormalized_entity_ref_wrapper(fields) {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} contains an unnormalized entity_ref wrapper; lower {{api, entity, key}} to its key payload before validation"
                ));
            }
            for (k, field) in fields {
                if k.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{node_index}].{path} contains an empty object key"
                    ));
                }
                validate_plan_value_expr(field, node_index, &format!("{path}.{k}"))?;
            }
            Ok(())
        }
    }
}

fn validate_entity_ref_key_value(
    value: &PlanValue,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    match value {
        PlanValue::Literal { .. }
        | PlanValue::BindingSymbol { .. }
        | PlanValue::NodeSymbol { .. }
        | PlanValue::Template { .. } => validate_plan_value_expr(value, node_index, path),
        PlanValue::Object { fields } => {
            if fields.is_empty() {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} compound entity_ref_key object must not be empty"
                ));
            }
            if looks_like_unnormalized_entity_ref_wrapper(fields) {
                return Err(format!(
                    "plan.nodes[{node_index}].{path} contains an unnormalized entity_ref wrapper; entity_ref_key.key must contain only key fields"
                ));
            }
            for (field, child) in fields {
                if field.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{node_index}].{path} compound entity_ref_key object contains an empty key"
                    ));
                }
                match child {
                    PlanValue::Literal { .. }
                    | PlanValue::BindingSymbol { .. }
                    | PlanValue::NodeSymbol { .. }
                    | PlanValue::Template { .. } => {
                        validate_plan_value_expr(child, node_index, &format!("{path}.{field}"))?
                    }
                    other => {
                        return Err(format!(
                            "plan.nodes[{node_index}].{path}.{field} must be a literal, binding symbol, node symbol, or template for compound entity_ref_key (got {other:?})"
                        ));
                    }
                }
            }
            Ok(())
        }
        other => Err(format!(
            "plan.nodes[{node_index}].{path} must be a literal, binding symbol, node symbol, template, or non-empty compound object for entity_ref_key (got {other:?})"
        )),
    }
}

fn looks_like_unnormalized_entity_ref_wrapper(fields: &BTreeMap<String, PlanValue>) -> bool {
    if fields.len() != 3
        || !fields.contains_key("api")
        || !fields.contains_key("entity")
        || !fields.contains_key("key")
    {
        return false;
    }
    matches!(
        fields.get("api"),
        Some(PlanValue::Literal {
            value: serde_json::Value::String(_)
        })
    ) && matches!(
        fields.get("entity"),
        Some(PlanValue::Literal {
            value: serde_json::Value::String(_)
        })
    )
}

fn validate_template_text(template: &str, node_index: usize, path: &str) -> Result<(), String> {
    validate_no_js_object_coercion(template, node_index, path)?;
    plasm_core::validate_interpolation_syntax(template, |detail| {
        format!("plan.nodes[{node_index}].{path} {detail}")
    })
}

fn validate_json_value_no_js_object_coercion(
    value: &serde_json::Value,
    node_index: usize,
    path: &str,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(s) => validate_no_js_object_coercion(s, node_index, path),
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                validate_json_value_no_js_object_coercion(
                    item,
                    node_index,
                    &format!("{path}[{i}]"),
                )?;
            }
            Ok(())
        }
        serde_json::Value::Object(fields) => {
            for (k, field) in fields {
                validate_json_value_no_js_object_coercion(
                    field,
                    node_index,
                    &format!("{path}.{k}"),
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_no_js_object_coercion(text: &str, node_index: usize, path: &str) -> Result<(), String> {
    if text.contains("[object Object]") {
        return Err(format!(
            "plan.nodes[{node_index}].{path} contains JavaScript object string coercion ([object Object]); use a symbolic field/template value instead"
        ));
    }
    Ok(())
}

/// Deserialize and validate a program-shaped plan value (same serialized IR as traces/archives).
pub fn validate_plan_value(plan: &serde_json::Value) -> Result<(), String> {
    let plan = parse_plan_value(plan)?;
    validate_plan(&plan)
}

fn deserialize_group_by_keys<'de, D>(deserializer: D) -> Result<Vec<FieldPath>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::String(s) => FieldPath::from_dotted(s.as_str())
            .map(|k| vec![k])
            .map_err(D::Error::custom),
        serde_json::Value::Array(items) => {
            let mut keys = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        keys.push(FieldPath::from_dotted(s.as_str()).map_err(D::Error::custom)?);
                    }
                    serde_json::Value::Array(segs) => {
                        let parts: Vec<String> = segs
                            .iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect();
                        keys.push(FieldPath::new(parts).map_err(D::Error::custom)?);
                    }
                    other => {
                        return Err(D::Error::custom(format!(
                            "group_by key entry must be string or path array, got {other}"
                        )));
                    }
                }
            }
            if keys.is_empty() {
                return Err(D::Error::custom("group_by requires at least one key"));
            }
            Ok(keys)
        }
        other => Err(D::Error::custom(format!(
            "group_by keys must be a string or array, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_query_ok() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "read-products",
            "nodes": [{
                "id": "n1",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list",
                "projection": [],
                "predicates": [{
                    "field_path": ["state"],
                    "op": "eq",
                    "value": { "kind": "literal", "value": "open" }
                }],
                "depends_on": [],
                "uses_result": []
            }],
            "return": { "kind": "node", "node": "n1" }
        });
        validate_plan_value(&v).expect("ok");
    }

    #[test]
    fn legacy_expr_list_is_rejected() {
        let v = serde_json::json!({
            "nodes": [{ "expr": "x" }],
        });
        let err = parse_plan_value(&v).expect_err("legacy expression list rejected");
        assert!(err.contains("missing field"), "{err}");
    }

    #[test]
    fn executable_text_without_ir_is_rejected() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "n1",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "n1" }
        });
        let err = validate_plan_value(&v).expect_err("text-only executable rejected");
        assert!(err.contains("ir or ir_template is required"), "{err}");
    }

    #[test]
    fn legacy_untagged_returns_are_rejected() {
        let base_nodes = serde_json::json!([{
            "id": "n1",
            "kind": "data",
            "effect_class": "artifact_read",
            "result_shape": "artifact",
            "data": { "kind": "literal", "value": [{ "id": "i1" }] }
        }]);
        for return_value in [
            serde_json::json!("n1"),
            serde_json::json!({ "parallel": ["n1"] }),
            serde_json::json!({ "name": "n1" }),
        ] {
            let v = serde_json::json!({
                "version": 1,
                "kind": "program",
                "nodes": base_nodes.clone(),
                "return": return_value
            });
            let err = parse_plan_value(&v).expect_err("untagged return rejected");
            assert!(err.contains("tag") || err.contains("kind"), "{err}");
        }
    }

    #[test]
    fn reject_cycle() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "cycle",
            "nodes": [
                {
                    "id": "a",
                    "kind": "derive",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "depends_on": ["b"]
                },
                {
                    "id": "b",
                    "kind": "derive",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "depends_on": ["a"]
                }
            ],
            "return": { "kind": "node", "node": "a" }
        });
        assert!(validate_plan_value(&v).is_err());
    }

    #[test]
    fn reject_missing_qualified_entity() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "n1",
                "kind": "query",
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "n1" }
        });
        assert!(validate_plan_value(&v).is_err());
    }

    #[test]
    fn page_surface_node_may_omit_qualified_entity() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "pg",
                "kind": "query",
                "expr": "page(s0_pg1)",
                "ir": { "expr": { "op": "page", "handle": "s0_pg1" } },
                "effect_class": "read",
                "result_shape": "page"
            }],
            "return": { "kind": "node", "node": "pg" }
        });
        validate_plan_value(&v).expect("page node validates without CGS entity key");
    }

    #[test]
    fn for_each_write_template_does_not_trust_agent_authored_approval() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "find",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "github", "entity": "Issue" },
                    "expr": "Issue{state=open}",
                    "ir": { "expr": { "op": "query", "entity": "Issue", "predicate": { "type": "comparison", "field": "state", "op": "=", "value": "open" } } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "label",
                    "kind": "for_each",
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "source": "find",
                    "item_binding": "issue",
                    "depends_on": ["find"],
                    "uses_result": [{ "node": "find", "as": "issue" }],
                    "effect_template": {
                        "kind": "action",
                        "qualified_entity": { "entry_id": "github", "entity": "Issue" },
                        "expr_template": "Issue(${issue.id}).add-label(label=\"stale\")",
                        "ir_template": {
                            "expr": {
                                "op": "invoke",
                                "capability": "add_label",
                                "target": { "entity_type": "Issue", "key": { "__plasm_hole": { "kind": "binding", "binding": "issue", "path": ["id"] } } },
                                "input": { "label": "stale" }
                            },
                            "input_bindings": [{ "from": "issue.id", "to": "id" }]
                        },
                        "effect_class": "side_effect",
                        "result_shape": "side_effect_ack"
                    }
                }
            ],
            "return": { "kind": "parallel", "nodes": ["find", "label"] }
        });
        validate_plan_value(&v).expect("host infers approval gates during dry-run");
    }

    #[test]
    fn reject_js_object_coercion_in_surface_and_templates() {
        let bad_surface = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "n1",
                "kind": "get",
                "qualified_entity": { "entry_id": "acme", "entity": "Item" },
                "expr": "Item(\"[object Object]\")",
                "ir": { "expr": { "op": "get", "ref": { "entity_type": "Item", "key": "x" } } },
                "effect_class": "read",
                "result_shape": "single"
            }],
            "return": { "kind": "node", "node": "n1" }
        });
        let err = validate_plan_value(&bad_surface).expect_err("object coercion rejected");
        assert!(err.contains("[object Object]"), "{err}");

        let bad_template = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "rows",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "id": "i1" }] }
                },
                {
                    "id": "mapped",
                    "kind": "derive",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "depends_on": ["rows"],
                    "uses_result": [{ "node": "rows", "as": "item" }],
                    "derive_template": {
                        "kind": "map",
                        "source": "rows",
                        "item_binding": "item",
                        "inputs": [],
                        "value": { "kind": "template", "template": "Item(\"[object Object]\")" }
                    }
                }
            ],
            "return": { "kind": "node", "node": "mapped" }
        });
        let err = validate_plan_value(&bad_template).expect_err("bad template rejected");
        assert!(err.contains("[object Object]"), "{err}");
    }

    #[test]
    fn reject_malformed_template_substitutions() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "rows",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "id": "i1" }] }
                },
                {
                    "id": "mapped",
                    "kind": "derive",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "depends_on": ["rows"],
                    "uses_result": [{ "node": "rows", "as": "item" }],
                    "derive_template": {
                        "kind": "map",
                        "source": "rows",
                        "item_binding": "item",
                        "inputs": [],
                        "value": { "kind": "template", "template": "Item(${})" }
                    }
                }
            ],
            "return": { "kind": "node", "node": "mapped" }
        });
        let err = validate_plan_value(&v).expect_err("empty substitution rejected");
        assert!(
            err.contains("empty") && err.contains("substitution"),
            "{err}"
        );
    }

    #[test]
    fn reject_unnormalized_entity_ref_wrapper_predicate_values() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "commits",
                "kind": "query",
                "qualified_entity": { "entry_id": "github", "entity": "Commit" },
                "expr": "Commit{repository=\"ryan-s-roberts/plasm-core\"}",
                "ir": {
                    "expr": {
                        "op": "query",
                        "entity": "Commit",
                        "predicate": {
                            "type": "comparison",
                            "field": "repository",
                            "op": "=",
                            "value": "ryan-s-roberts/plasm-core"
                        }
                    }
                },
                "effect_class": "read",
                "result_shape": "list",
                "predicates": [{
                    "field_path": ["repository"],
                    "op": "eq",
                    "value": {
                        "kind": "object",
                        "fields": {
                            "api": { "kind": "literal", "value": "github" },
                            "entity": { "kind": "literal", "value": "Repository" },
                            "key": { "kind": "literal", "value": "ryan-s-roberts/plasm-core" }
                        }
                    }
                }]
            }],
            "return": { "kind": "node", "node": "commits" }
        });
        let err = validate_plan_value(&v).expect_err("unnormalized wrapper rejected");
        assert!(err.contains("unnormalized entity_ref wrapper"), "{err}");
    }

    #[test]
    fn explicit_entity_ref_key_predicate_values_validate() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "commits",
                "kind": "query",
                "qualified_entity": { "entry_id": "github", "entity": "Commit" },
                "expr": "Commit{repository=\"ryan-s-roberts/plasm-core\"}",
                "ir": {
                    "expr": {
                        "op": "query",
                        "entity": "Commit",
                        "predicate": {
                            "type": "comparison",
                            "field": "repository",
                            "op": "=",
                            "value": "ryan-s-roberts/plasm-core"
                        }
                    }
                },
                "effect_class": "read",
                "result_shape": "list",
                "predicates": [{
                    "field_path": ["repository"],
                    "op": "eq",
                    "value": {
                        "kind": "entity_ref_key",
                        "api": "github",
                        "entity": "Repository",
                        "key": { "kind": "literal", "value": "ryan-s-roberts/plasm-core" }
                    }
                }]
            }],
            "return": { "kind": "node", "node": "commits" }
        });
        validate_plan_value(&v).expect("explicit entity_ref_key is valid");
    }

    #[test]
    fn predicate_helper_values_are_rejected() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "n1",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product{updated_at>30d}",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list",
                "predicates": [{
                    "field_path": ["updated_at"],
                    "op": "gt",
                    "value": {
                        "kind": "helper",
                        "name": "daysAgo",
                        "args": [30],
                        "display": "30d"
                    }
                }]
            }],
            "return": { "kind": "node", "node": "n1" }
        });
        let err = validate_plan_value(&v).expect_err("helper predicate rejected");
        assert!(
            err.contains("helper \"daysAgo\" is not executable"),
            "{err}"
        );
    }

    #[test]
    fn compound_entity_ref_key_predicate_values_validate() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "n1",
                "kind": "query",
                "qualified_entity": { "entry_id": "github", "entity": "Commit" },
                "expr": "Commit.query(repository=Repository{owner=\"ryan-s-roberts\", repo=\"plasm-core\"})",
                "ir": {
                    "expr": {
                        "op": "query",
                        "entity": "Commit",
                        "predicate": {
                            "type": "comparison",
                            "field": "repository",
                            "op": "=",
                            "value": {
                                "owner": "ryan-s-roberts",
                                "repo": "plasm-core"
                            }
                        }
                    }
                },
                "effect_class": "read",
                "result_shape": "list",
                "predicates": [{
                    "field_path": ["repository"],
                    "op": "eq",
                    "value": {
                        "kind": "entity_ref_key",
                        "api": "github",
                        "entity": "Repository",
                        "key": {
                            "kind": "object",
                            "fields": {
                                "owner": { "kind": "literal", "value": "ryan-s-roberts" },
                                "repo": { "kind": "literal", "value": "plasm-core" }
                            }
                        }
                    }
                }]
            }],
            "return": { "kind": "node", "node": "n1" }
        });
        validate_plan_value(&v).expect("compound entity_ref_key is valid");
    }

    #[test]
    fn compound_get_ref_key_validates() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "repo",
                "kind": "get",
                "qualified_entity": { "entry_id": "github", "entity": "Repository" },
                "expr": "Repository{owner=\"ryan-s-roberts\", repo=\"plasm-core\"}",
                "ir": {
                    "expr": {
                        "op": "get",
                        "ref": {
                            "entity_type": "Repository",
                            "key": {
                                "owner": "ryan-s-roberts",
                                "repo": "plasm-core"
                            }
                        }
                    }
                },
                "effect_class": "read",
                "result_shape": "single"
            }],
            "return": { "kind": "node", "node": "repo" }
        });
        validate_plan_value(&v).expect("compound get ref key is valid");
    }

    #[test]
    fn compute_rejects_unknown_source_and_bad_aggregate() {
        let unknown_source = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "by_state",
                "kind": "compute",
                "effect_class": "artifact_read",
                "result_shape": "list",
                "compute": {
                    "source": "missing",
                    "op": { "kind": "group_by", "keys": ["state"], "aggregates": [{ "name": "count", "function": "count" }] },
                    "schema": { "fields": [{ "name": "key", "value_kind": "string" }, { "name": "count", "value_kind": "integer" }] }
                }
            }],
            "return": { "kind": "node", "node": "by_state" }
        });
        assert!(validate_plan_value(&unknown_source).is_err());

        let bad_aggregate = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "rows",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "points": 1 }] }
                },
                {
                    "id": "totals",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "rows",
                        "op": { "kind": "aggregate", "aggregates": [{ "name": "total", "function": "sum" }] },
                        "schema": { "fields": [{ "name": "total", "value_kind": "number" }] }
                    }
                }
            ],
            "return": { "kind": "node", "node": "totals" }
        });
        assert!(validate_plan_value(&bad_aggregate).is_err());
    }

    #[test]
    fn search_requires_read_list_shape() {
        let bad_shape = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "search",
                "kind": "search",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product~\"bolt\"",
                "ir": { "expr": { "op": "query", "entity": "Product", "predicate": { "type": "comparison", "field": "q", "op": "=", "value": "bolt" }, "capability_name": "product_search" } },
                "effect_class": "read",
                "result_shape": "single"
            }],
            "return": { "kind": "node", "node": "search" }
        });
        let err = validate_plan_value(&bad_shape).expect_err("bad search shape rejected");
        assert!(err.contains("search result_shape must be list"), "{err}");
    }

    #[test]
    fn relation_traversal_carries_validated_proof() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "product",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "category",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "single",
                    "relation": {
                        "source": "product",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Category" },
                        "cardinality": "one",
                        "source_cardinality": "single",
                        "expr": "Product(\"p1\").category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["product"],
                    "uses_result": [{ "node": "product", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "category" }
        });
        let plan = parse_plan_value(&v).expect("parse");
        let validated = validate_plan_artifact(&plan).expect("validate");
        assert_eq!(validated.topological_order()[0].as_str(), "product");
        assert_eq!(validated.topological_order()[1].as_str(), "category");
        assert!(matches!(
            &validated.nodes()[1],
            ValidatedPlanNode::RelationTraversal(node)
                if node.relation.target.entity == "Category"
        ));
    }

    #[test]
    fn relation_one_rejects_plural_source_without_singleton_proof() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "products",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "category",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "list",
                    "relation": {
                        "source": "products",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Category" },
                        "cardinality": "one",
                        "source_cardinality": "many",
                        "expr": "Product.category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "query", "entity": "Product" }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["products"]
                }
            ],
            "return": { "kind": "node", "node": "category" }
        });
        let err = validate_plan_value(&v).expect_err("plural one relation rejected");
        assert!(err.contains("requires a singleton source"), "{err}");
    }

    #[test]
    fn validated_plan_exposes_topology_and_typed_return_refs() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "rows",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "state": "open" }] }
                },
                {
                    "id": "limited",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "rows",
                        "op": { "kind": "limit", "count": 1 },
                        "schema": { "fields": [{ "name": "state", "value_kind": "string", "source": ["state"] }] },
                        "page_size": 1
                    }
                }
            ],
            "return": { "kind": "node", "node": "limited" }
        });
        let plan = parse_plan_value(&v).expect("parse");
        let validated = validate_plan_artifact(&plan).expect("validate");
        assert_eq!(validated.topological_order()[0].as_str(), "rows");
        assert_eq!(validated.topological_order()[1].as_str(), "limited");
        assert_eq!(validated.return_value().refs()[0].as_str(), "limited");
    }

    fn render_plan(columns: serde_json::Value, template: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "rows",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "name": "bolt" }] }
                },
                {
                    "id": "doc",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "rows",
                        "op": { "kind": "render", "columns": columns, "template": template },
                        "schema": {
                            "entity": "PlanRender",
                            "fields": [{ "name": "content", "value_kind": "string" }]
                        }
                    },
                    "depends_on": ["rows"],
                    "uses_result": [{ "node": "rows", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "doc" }
        })
    }

    #[test]
    fn validate_render_rejects_empty_columns() {
        let err = validate_plan_value(&render_plan(serde_json::json!([]), serde_json::json!("ok")))
            .expect_err("empty columns rejected");
        assert!(
            err.contains("compute.render.columns must be non-empty"),
            "{err}"
        );
    }

    #[test]
    fn validate_render_rejects_empty_template() {
        let err = validate_plan_value(&render_plan(
            serde_json::json!(["name"]),
            serde_json::json!(" "),
        ))
        .expect_err("empty template rejected");
        assert!(
            err.contains("compute.render.template must be non-empty"),
            "{err}"
        );
    }

    #[test]
    fn validate_render_rejects_duplicate_columns() {
        let err = validate_plan_value(&render_plan(
            serde_json::json!(["name", "name"]),
            serde_json::json!("{{ rows }}"),
        ))
        .expect_err("duplicate columns rejected");
        assert!(
            err.contains("compute.render.columns has duplicate"),
            "{err}"
        );
    }

    #[test]
    fn validate_render_rejects_empty_column_name() {
        let err = validate_plan_value(&render_plan(
            serde_json::json!([""]),
            serde_json::json!("{{ rows }}"),
        ))
        .expect_err("empty column name rejected");
        assert!(err.contains("OutputName must be non-empty"), "{err}");
    }

    #[test]
    fn validate_render_rejects_non_content_schema() {
        let mut plan = render_plan(serde_json::json!(["name"]), serde_json::json!("{{ rows }}"));
        plan["nodes"][1]["compute"]["schema"] = serde_json::json!({
            "entity": "PlanRender",
            "fields": [{ "name": "body", "value_kind": "string" }]
        });
        let err = validate_plan_value(&plan).expect_err("bad schema rejected");
        assert!(err.contains("single string field named 'content'"), "{err}");
    }

    #[test]
    fn validate_render_rejects_template_syntax_errors() {
        let err = validate_plan_value(&render_plan(
            serde_json::json!(["name"]),
            serde_json::json!("{{"),
        ))
        .expect_err("bad minijinja syntax rejected");
        assert!(err.contains("compute.render.template"), "{err}");
    }

    #[test]
    fn analyze_static_cardinality_render_is_static_singleton() {
        let plan = parse_plan_value(&render_plan(
            serde_json::json!(["name"]),
            serde_json::json!("{{ rows }}"),
        ))
        .expect("parse");
        let by_id = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| (node.id.clone(), idx))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            analyze_static_cardinality(&plan, &by_id, "doc"),
            CardinalityAnalysis::StaticSingleton
        );
    }

    #[test]
    fn for_each_effect_template_rejects_undeclared_interpolation_alias() {
        let v = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "bad-for-each",
            "nodes": [
                {
                    "id": "find",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "data": { "kind": "literal", "value": [{ "id": "p1" }] }
                },
                {
                    "id": "label",
                    "kind": "for_each",
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "source": "find",
                    "item_binding": "_",
                    "depends_on": ["find"],
                    "uses_result": [{ "node": "find", "as": "_" }],
                    "effect_template": {
                        "kind": "action",
                        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                        "expr_template": "Product.create(title=<<T\n${missing}\nT\n)",
                        "ir_template": {
                            "expr": {
                                "op": "create",
                                "capability": "product_create",
                                "entity": "Product",
                                "input": { "title": "<<T\n${missing}\nT\n" }
                            },
                            "input_bindings": []
                        },
                        "effect_class": "side_effect",
                        "result_shape": "side_effect_ack"
                    }
                }
            ],
            "return": { "kind": "node", "node": "label" }
        });
        let plan = parse_plan_value(&v).expect("parse");
        let err = validate_plan_artifact(&plan).expect_err("undeclared alias rejected");
        assert!(err.contains("undeclared alias"), "{err}");
    }
}
