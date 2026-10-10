//! Validation and typed conversion for Plan artifacts.

pub(crate) mod compute;
mod relation;
pub(crate) mod value;

use super::*;
use compute::*;
use relation::*;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use thiserror::Error;
use value::*;

pub use compute::{
    ComputeTemplateValidationError, OperandValidationError, PlanValueInputError,
    RenderTemplateValidationError,
};
pub use relation::PlanRelationValidationError;

#[derive(Debug, Error)]
pub enum PlanNodeStructureError {
    #[error("plan node {index} has an empty id")]
    EmptyNodeId { index: usize },
    #[error("plan node {index} id is invalid: {source}")]
    InvalidNodeId {
        index: usize,
        #[source]
        source: PlanAtomError,
    },
    #[error("plan node {index} repeats a uses_result alias `{alias}`")]
    DuplicateInputAlias { index: usize, alias: String },
    #[error("map_body node is missing its scoped payload")]
    MissingMapBodyPayload,
    #[error("map_body node is missing its parent body")]
    MissingMapBody,
    #[error(
        "map_body node must carry only its scoped payload and transitive effect/list contract"
    )]
    InvalidMapBodyPayload,
    #[error("map_body node must depend on its parent source")]
    MapBodyMissingParentDependency,
    #[error("map_body payload is only valid for a map_body node")]
    MapBodyPayloadOnOtherNode,
    #[error("until_scope and step_scope are only valid for iterate_until nodes")]
    IterationScopeOnOtherNode,
    #[error("executable node {index} ({kind}) requires ir or ir_template")]
    MissingExecutableIr { index: usize, kind: PlanNodeKind },
    #[error("plan node {index} cannot carry both ir and ir_template")]
    MultipleExecutableIr { index: usize },
    #[error("executable node {index} ({kind}) requires a qualified entity unless it is a page")]
    MissingQualifiedEntity { index: usize, kind: PlanNodeKind },
    #[error("search node {index} must have read effect")]
    SearchMustBeRead { index: usize },
    #[error("search node {index} must return a list")]
    SearchMustReturnList { index: usize },
    #[error("derive node {index} cannot carry executable expression fields")]
    DeriveCarriesExpression { index: usize },
    #[error("data node {index} is missing its value")]
    MissingDataValue { index: usize },
    #[error("data node {index} cannot carry executable expression fields")]
    DataCarriesExpression { index: usize },
    #[error("compute node {index} carries fields owned by other node kinds")]
    ComputeCarriesForeignPayload { index: usize },
    #[error("compute payload is only valid on compute nodes")]
    ComputePayloadOnOtherNode,
    #[error("relation node {index} must have read effect")]
    RelationMustBeRead { index: usize },
    #[error("relation node {index} must return one or many rows")]
    InvalidRelationResultShape { index: usize },
    #[error("relation node {index} carries fields owned by other node kinds")]
    RelationCarriesForeignPayload { index: usize },
    #[error("relation payload is only valid on relation nodes")]
    RelationPayloadOnOtherNode,
    #[error("derive node {index} references unknown source `{source_id}`")]
    UnknownDeriveSource { index: usize, source_id: String },
    #[error("derive node {index} requires an item binding for map/cell derivation")]
    MissingDeriveItemBinding { index: usize },
    #[error(
        "derive node {index} input `{source_id}` is not statically singleton and has no explicit singleton proof"
    )]
    UnprovenDeriveBroadcast { index: usize, source_id: String },
}

#[derive(Debug, Error)]
pub enum PlanValidationError {
    #[error("serialized plan is invalid: {0}")]
    Deserialize(#[from] serde_json::Error),
    #[error("invalid Plan identifier: {0}")]
    Atom(#[source] PlanAtomError),
    #[error(transparent)]
    Correlated(#[from] plasm_core::plasm_monad::CorrelatedBodyError),
    #[error("invalid bind graph: {0}")]
    BindGraph(#[source] plasm_core::plasm_monad::BindGraphError),
    #[error("Plan lowering failed: {0}")]
    Lift(#[source] crate::plasm_step_convert::StepPayloadLiftError),
    #[error("unsupported Plan version {actual}; expected {expected}")]
    UnsupportedVersion { expected: u32, actual: u32 },
    #[error("Plan contains no executable nodes")]
    EmptyNodes,
    #[error("plan node `{node}` duplicates an existing id")]
    DuplicateNode { node: String },
    #[error("required Plan field `{field}` is missing")]
    MissingField { field: String },
    #[error("iterate_until seed `{node}` is not a replayable Get identity")]
    InvalidIterateSeed { node: String },
    #[error("Plan return must name at least one node")]
    EmptyReturn,
    #[error("Plan return references unknown node `{node}`")]
    UnknownReturnNode { node: String },
    #[error("iterate_until requires an explicit positive hard bound")]
    InvalidIterateBound,
    #[error("{kind} source `{source_id}` is not present in the Plan")]
    UnknownRowEffectSource { kind: String, source_id: String },
    #[error("{kind} requires a non-empty item binding")]
    MissingRowEffectBinding { kind: String },
    #[error(transparent)]
    Value(#[from] value::ValueValidationError),
    #[error(transparent)]
    DataInput(#[from] compute::PlanDataInputError),
    #[error(transparent)]
    Operand(#[from] compute::OperandValidationError),
    #[error(transparent)]
    ValueInput(#[from] compute::PlanValueInputError),
    #[error(transparent)]
    ComputeTemplate(#[from] compute::ComputeTemplateValidationError),
    #[error(transparent)]
    Relation(#[from] relation::PlanRelationValidationError),
    #[error(transparent)]
    Structure(#[from] PlanNodeStructureError),
    #[error(transparent)]
    IterationEffect(#[from] plasm_core::plasm_monad::correlated::IterationStepEffectError),
    #[error("iterate step differs from its sealed owner or expression")]
    IterationStepMismatch,
    #[error("{0}")]
    Compute(#[source] compute::PlanExpressionError),
    #[error("plan node {index} references unknown {reference} source `{node}`")]
    UnknownDependency {
        index: usize,
        reference: PlanDependencyKind,
        node: String,
    },
    #[error("plan dependency graph has a cycle")]
    DependencyCycle,
    #[error(
        "plan node {index} effect binding `{source_id}` does not reference item binding `{binding}`"
    )]
    InvalidEffectBindingSource {
        index: usize,
        source_id: String,
        binding: String,
    },
    #[error("plan node {index} iterate_until requires a stop predicate")]
    MissingIterationPredicate { index: usize },
    #[error("uses_result provenance references unknown node `{node}`")]
    UnknownProvenanceNode { node: String },
    #[error("uses_result provenance node `{node}` has no catalog domain")]
    MissingProvenanceDomain { node: String },
    #[error("uses_result provenance exceeded source walk depth from `{node}`")]
    ProvenanceDepthExceeded { node: String },
    #[error(
        "plan node `{consumer}` alias `{alias}` has contradictory provenance for source `{node}`"
    )]
    ContradictoryProvenance {
        consumer: String,
        alias: String,
        node: String,
        expected: Box<QualifiedEntityKey>,
        actual: Box<QualifiedEntityKey>,
    },
    #[error("capture ports require a scoped body")]
    UnscopedCapture,
    #[error("acknowledgement input `{node}` requires a side-effect acknowledgement source")]
    InvalidAcknowledgementSource { node: String },
    #[error("input `{node}` is not statically singleton and lacks explicit singleton proof")]
    UnprovenInputBroadcast { node: String },
    #[error(transparent)]
    SessionProvision(#[from] crate::plan_session_provisions::SessionProvisionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDependencyKind {
    ExplicitDependency,
    ResultUse,
    Source,
    DeriveSource,
    DeriveInput,
    ComputeSource,
    UnionSource,
    RelationSource,
}

impl PlanValidationError {
    pub fn diagnostic(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for PlanDependencyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ExplicitDependency => "explicit dependency",
            Self::ResultUse => "result use",
            Self::Source => "source",
            Self::DeriveSource => "derive source",
            Self::DeriveInput => "derive input",
            Self::ComputeSource => "compute source",
            Self::UnionSource => "union source",
            Self::RelationSource => "relation source",
        })
    }
}

impl From<PlanAtomError> for PlanValidationError {
    fn from(error: PlanAtomError) -> Self {
        Self::Atom(error)
    }
}

impl From<crate::plasm_step_convert::StepPayloadLiftError> for PlanValidationError {
    fn from(error: crate::plasm_step_convert::StepPayloadLiftError) -> Self {
        Self::Lift(error)
    }
}
impl From<plasm_core::plasm_monad::BindGraphError> for PlanValidationError {
    fn from(error: plasm_core::plasm_monad::BindGraphError) -> Self {
        Self::BindGraph(error)
    }
}

impl From<compute::PlanExpressionError> for PlanValidationError {
    fn from(error: compute::PlanExpressionError) -> Self {
        Self::Compute(error)
    }
}

/// Deserialize a program-shaped [`Plan`] from a JSON value (same IR shape as evaluation archives).
pub fn parse_plan_value(plan: &serde_json::Value) -> Result<Plan, PlanValidationError> {
    Ok(serde_json::from_value(plan.clone())?)
}

/// Deserialize and validate a serialized plan JSON value (HTTP resolved-plan, MCP, CLI).
pub fn parse_and_validate_plan_json(
    plan: &serde_json::Value,
) -> Result<ValidatedPlan, PlanValidationError> {
    let plan_typed = parse_plan_value(plan)?;
    validate_plan_artifact(&plan_typed)
}

/// Parse and validate one program-shaped Plan.
pub fn validate_plan(plan: &Plan) -> Result<(), PlanValidationError> {
    validate_plan_artifact(plan).map(|_| ())
}

/// Parse and validate one program-shaped Plan, returning typed execution metadata.
pub fn validate_plan_artifact(plan: &Plan) -> Result<ValidatedPlan, PlanValidationError> {
    if plan.version != 1 {
        return Err(PlanValidationError::UnsupportedVersion {
            expected: 1,
            actual: plan.version,
        });
    }
    if plan.nodes.is_empty() {
        return Err(PlanValidationError::EmptyNodes);
    }
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (i, n) in plan.nodes.iter().enumerate() {
        if n.id.trim().is_empty() {
            return Err(PlanNodeStructureError::EmptyNodeId { index: i }.into());
        }
        if by_id.insert(n.id.clone(), i).is_some() {
            return Err(PlanValidationError::DuplicateNode { node: n.id.clone() });
        }
        PlanNodeId::new(n.id.clone())
            .map_err(|source| PlanNodeStructureError::InvalidNodeId { index: i, source })?;
    }

    for (i, n) in plan.nodes.iter().enumerate() {
        let mut aliases = std::collections::HashSet::new();
        for input in &n.uses_result {
            InputAlias::new(input.r#as.clone())?;
            if !aliases.insert(input.r#as.as_str()) {
                return Err(PlanNodeStructureError::DuplicateInputAlias {
                    index: i,
                    alias: input.r#as.to_string(),
                }
                .into());
            }
        }
        if n.kind == PlanNodeKind::MapBody {
            let body = n
                .map_body
                .as_ref()
                .ok_or(PlanNodeStructureError::MissingMapBodyPayload)?;
            body.execution_layers()?;
            if n.effect_class
                != n.map_body
                    .as_ref()
                    .ok_or(PlanNodeStructureError::MissingMapBody)?
                    .effect_class()
                || n.result_shape != body.result_shape()
                || n.ir.is_some()
                || n.ir_template.is_some()
                || n.expr.is_some()
                || n.compute.is_some()
                || n.data.is_some()
                || n.derive_template.is_some()
                || n.effect_template.is_some()
                || n.relation.is_some()
            {
                return Err(PlanNodeStructureError::InvalidMapBodyPayload.into());
            }
            if !n
                .depends_on
                .iter()
                .any(|id| id == body.parent.source.as_str())
            {
                return Err(PlanNodeStructureError::MapBodyMissingParentDependency.into());
            }
        } else if n.map_body.is_some() {
            return Err(PlanNodeStructureError::MapBodyPayloadOnOtherNode.into());
        }
        if (n.until_scope.is_some() || n.step_scope.is_some())
            && n.kind != PlanNodeKind::IterateUntil
        {
            return Err(PlanNodeStructureError::IterationScopeOnOtherNode.into());
        }
        if n.kind.has_surface_expr() {
            if n.ir.is_none() && n.ir_template.is_none() {
                return Err(PlanNodeStructureError::MissingExecutableIr {
                    index: i,
                    kind: n.kind,
                }
                .into());
            }
            if n.ir.is_some() && n.ir_template.is_some() {
                return Err(PlanNodeStructureError::MultipleExecutableIr { index: i }.into());
            }
            let input_aliases: Vec<_> = n
                .uses_result
                .iter()
                .map(|input| (input.r#as.as_str(), input.node.as_str()))
                .collect();
            let ctx = plasm_core::TemplateRefContext {
                row_binding: None,
                input_aliases: &input_aliases,
            };
            if let Some(ir) = &n.ir {
                validate_expression_operands(&ir.expr, i, &ctx)?;
            }
            if let Some(template) = &n.ir_template {
                validate_plan_expr_template(template, i, "ir_template")
                    .map_err(PlanValidationError::Compute)?;
                validate_expression_operands(&template.expr, i, &ctx)?;
            }
            if n.qualified_entity.is_none() && n.result_shape != ResultShape::Page {
                return Err(PlanNodeStructureError::MissingQualifiedEntity {
                    index: i,
                    kind: n.kind,
                }
                .into());
            }
            if n.kind == PlanNodeKind::Search {
                if n.effect_class != EffectClass::Read {
                    return Err(PlanNodeStructureError::SearchMustBeRead { index: i }.into());
                }
                if n.result_shape != ResultShape::List {
                    return Err(PlanNodeStructureError::SearchMustReturnList { index: i }.into());
                }
            }
        }
        if n.kind == PlanNodeKind::Derive
            && (n.expr.is_some() || n.ir.is_some() || n.ir_template.is_some())
        {
            return Err(PlanNodeStructureError::DeriveCarriesExpression { index: i }.into());
        }
        if n.kind == PlanNodeKind::Data {
            if n.data.is_none() {
                return Err(PlanNodeStructureError::MissingDataValue { index: i }.into());
            }
            if n.expr.is_some() || n.ir.is_some() || n.ir_template.is_some() {
                return Err(PlanNodeStructureError::DataCarriesExpression { index: i }.into());
            }
        }
        if n.kind == PlanNodeKind::Compute {
            let compute = n
                .compute
                .as_ref()
                .ok_or_else(|| PlanValidationError::MissingField {
                    field: format!("plan.nodes[{i}].compute"),
                })?;
            if n.expr.is_some()
                || n.ir.is_some()
                || n.ir_template.is_some()
                || n.data.is_some()
                || n.effect_template.is_some()
                || n.relation.is_some()
            {
                return Err(
                    PlanNodeStructureError::ComputeCarriesForeignPayload { index: i }.into(),
                );
            }
            if let ComputeOp::Filter { predicates } = &compute.op {
                for (predicate_index, predicate) in predicates.iter().enumerate() {
                    validate_predicate(predicate, i, predicate_index)?;
                }
            }
            validate_compute_template(compute, i, &by_id)?;
        } else if n.compute.is_some() {
            return Err(PlanNodeStructureError::ComputePayloadOnOtherNode.into());
        }
        if n.kind == PlanNodeKind::Relation {
            let relation =
                n.relation
                    .as_ref()
                    .ok_or_else(|| PlanValidationError::MissingField {
                        field: format!("plan.nodes[{i}].relation"),
                    })?;
            if n.effect_class != EffectClass::Read {
                return Err(PlanNodeStructureError::RelationMustBeRead { index: i }.into());
            }
            if !matches!(n.result_shape, ResultShape::List | ResultShape::Single) {
                return Err(PlanNodeStructureError::InvalidRelationResultShape { index: i }.into());
            }
            if n.expr.is_some()
                || n.ir.is_some()
                || n.ir_template.is_some()
                || n.data.is_some()
                || n.effect_template.is_some()
                || n.compute.is_some()
            {
                return Err(
                    PlanNodeStructureError::RelationCarriesForeignPayload { index: i }.into(),
                );
            }
            validate_relation_traversal(plan, relation, i, &by_id)?;
        } else if n.relation.is_some() {
            return Err(PlanNodeStructureError::RelationPayloadOnOtherNode.into());
        }
        if let Some(data) = &n.data {
            validate_plan_value_expr(data, i, "data")?;
        }
        if let Some(derive_template) = &n.derive_template {
            validate_plan_value_expr(&derive_template.value, i, "derive_template.value")?;
            if matches!(derive_template.kind, DeriveKind::Map | DeriveKind::Cell) {
                let source = derive_template.source.as_deref().unwrap_or_default();
                if source.trim().is_empty() || !by_id.contains_key(source) {
                    return Err(PlanNodeStructureError::UnknownDeriveSource {
                        index: i,
                        source_id: source.to_owned(),
                    }
                    .into());
                }
                let binding = derive_template.item_binding.as_deref().unwrap_or_default();
                if binding.trim().is_empty() {
                    return Err(
                        PlanNodeStructureError::MissingDeriveItemBinding { index: i }.into(),
                    );
                }
            }
            for input in &derive_template.inputs {
                validate_plan_data_input(input, i, &by_id)?;
                if input.cardinality == InputCardinality::Auto
                    && !cardinality::analyze_static_cardinality(plan, &by_id, input.node.as_str())
                        .permits_auto_broadcast()
                {
                    return Err(PlanNodeStructureError::UnprovenDeriveBroadcast {
                        index: i,
                        source_id: input.node.to_string(),
                    }
                    .into());
                }
            }
            validate_derive_value_inputs(derive_template, i)?;
        }
        for (j, p) in n.predicates.iter().enumerate() {
            validate_predicate(p, i, j)?;
        }
        if n.kind == PlanNodeKind::ForEach {
            let (binding, template) =
                validate_row_effect_source_and_template(n, i, &by_id, "for_each")?;
            for b in &template.input_bindings {
                if !b.from.starts_with(&format!("{binding}."))
                    && b.from.as_str() != binding
                    && !b.from.contains('.')
                {
                    return Err(PlanValidationError::InvalidEffectBindingSource {
                        index: i,
                        source_id: b.from.to_string(),
                        binding: binding.to_string(),
                    });
                }
            }
        }
        if n.kind == PlanNodeKind::IterateUntil {
            require_iterate_hard_bound(n, i)?;
            if n.until
                .as_ref()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
                && n.predicates.is_empty()
                && n.until_scope.is_none()
            {
                return Err(PlanValidationError::MissingIterationPredicate { index: i });
            }
            let _ = validate_row_effect_source_and_template(n, i, &by_id, "iterate_until")?;
        }
    }

    let mut adj: Vec<Vec<usize>> = vec![vec![]; plan.nodes.len()];
    for (i, n) in plan.nodes.iter().enumerate() {
        for d in &n.depends_on {
            let t = *by_id
                .get(d)
                .ok_or_else(|| PlanValidationError::UnknownDependency {
                    index: i,
                    reference: PlanDependencyKind::ExplicitDependency,
                    node: d.as_str().to_owned(),
                })?;
            adj[i].push(t);
        }
        for u in &n.uses_result {
            let t = *by_id
                .get(&u.node)
                .ok_or_else(|| PlanValidationError::UnknownDependency {
                    index: i,
                    reference: PlanDependencyKind::ResultUse,
                    node: u.node.as_str().to_owned(),
                })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
        if let Some(source) = &n.source {
            let t = *by_id
                .get(source)
                .ok_or_else(|| PlanValidationError::UnknownDependency {
                    index: i,
                    reference: PlanDependencyKind::Source,
                    node: source.as_str().to_owned(),
                })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
        if let Some(derive_template) = &n.derive_template {
            if let Some(source) = &derive_template.source {
                let t =
                    *by_id
                        .get(source)
                        .ok_or_else(|| PlanValidationError::UnknownDependency {
                            index: i,
                            reference: PlanDependencyKind::DeriveSource,
                            node: source.as_str().to_owned(),
                        })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
            for input in &derive_template.inputs {
                let t = *by_id.get(&input.node).ok_or_else(|| {
                    PlanValidationError::UnknownDependency {
                        index: i,
                        reference: PlanDependencyKind::DeriveInput,
                        node: input.node.as_str().to_owned(),
                    }
                })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
        }
        if let Some(compute) = &n.compute {
            let t = *by_id.get(&compute.source).ok_or_else(|| {
                PlanValidationError::UnknownDependency {
                    index: i,
                    reference: PlanDependencyKind::ComputeSource,
                    node: compute.source.as_str().to_owned(),
                }
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
            if let ComputeOp::Union { other } | ComputeOp::MergeBranches { other } = &compute.op {
                let t = *by_id.get(other.as_str()).ok_or_else(|| {
                    PlanValidationError::UnknownDependency {
                        index: i,
                        reference: PlanDependencyKind::UnionSource,
                        node: other.as_str().to_owned(),
                    }
                })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
        }
        if let Some(relation) = &n.relation {
            let t = *by_id.get(&relation.source).ok_or_else(|| {
                PlanValidationError::UnknownDependency {
                    index: i,
                    reference: PlanDependencyKind::RelationSource,
                    node: relation.source.as_str().to_owned(),
                }
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
    }
    if has_cycle(&adj) {
        return Err(PlanValidationError::DependencyCycle);
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
                    | PlanNodeKind::IterateUntil
            ) || matches!(n.effect_class, EffectClass::Write | EffectClass::SideEffect)
        })
        .map(|n| PlanNodeId::new(n.id.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let artifact = Plan::from_validated_parts(
        plan.version,
        plan.kind,
        plan.name.clone(),
        nodes,
        return_value,
        plan.metadata.clone(),
    );
    let mut validated = ValidatedPlan::from_parts(artifact, topo, node_indices, approval_gates);
    crate::plan_read_bounds::apply_read_budgets(&mut validated);
    Ok(validated)
}

/// Resolve the catalog-qualified row domain of a plan node, walking compute/derive sources.
/// Returns `None` for pure Data literals (no catalog domain). Fail-closed otherwise — no
/// primary-catalog fallback when a catalog-backed source lacks provenance.
pub fn resolve_plan_node_qualified_entity(
    plan: &Plan,
    node_id: &str,
) -> Result<Option<QualifiedEntityKey>, PlanValidationError> {
    let by_id: HashMap<&str, &PlanNode> = plan.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut cur = node_id.to_string();
    for _ in 0..512 {
        let n = by_id
            .get(cur.as_str())
            .ok_or_else(|| PlanValidationError::UnknownProvenanceNode { node: cur.clone() })?;
        if n.kind == PlanNodeKind::Data {
            return Ok(None);
        }
        if let Some(qe) = &n.qualified_entity {
            return Ok(Some(qe.clone()));
        }
        if let Some(rel) = &n.relation {
            return Ok(Some(rel.target.clone()));
        }
        if let Some(effect) = &n.effect_template {
            return Ok(Some(effect.qualified_entity.clone()));
        }
        if let Some(body) = &n.map_body {
            cur = body.parent.source.to_string();
            continue;
        }
        if let Some(compute) = &n.compute {
            cur = compute.source.clone();
            continue;
        }
        if let Some(derive) = &n.derive_template {
            if let Some(src) = &derive.source {
                cur = src.clone();
                continue;
            }
        }
        if let Some(src) = &n.source {
            cur = src.clone();
            continue;
        }
        return Err(PlanValidationError::MissingProvenanceDomain { node: cur.clone() });
    }
    Err(PlanValidationError::ProvenanceDepthExceeded {
        node: node_id.to_owned(),
    })
}

/// Stamp each `uses_result` edge with the source node's [`QualifiedEntityKey`] when the source is
/// catalog-backed. Data literal sources remain unqualified. Rejects contradictory provenance.
pub fn enrich_uses_result_provenance(
    uses: &[PlanResultUse],
    plan: &Plan,
    consumer_id: &str,
) -> Result<Vec<PlanResultUse>, PlanValidationError> {
    let mut out = Vec::with_capacity(uses.len());
    for u in uses {
        let resolved = resolve_plan_node_qualified_entity(plan, u.node.as_str())?;
        match (u.qualified_entity.as_ref(), resolved) {
            (Some(existing), Some(resolved)) if existing != &resolved => {
                return Err(PlanValidationError::ContradictoryProvenance {
                    consumer: consumer_id.to_owned(),
                    alias: u.r#as.to_string(),
                    node: u.node.to_string(),
                    expected: Box::new(resolved),
                    actual: Box::new(existing.clone()),
                });
            }
            (_, Some(resolved)) => {
                out.push(PlanResultUse {
                    node: u.node.clone(),
                    r#as: u.r#as.clone(),
                    qualified_entity: Some(resolved),
                });
            }
            (existing, None) => {
                out.push(PlanResultUse {
                    node: u.node.clone(),
                    r#as: u.r#as.clone(),
                    qualified_entity: existing.cloned(),
                });
            }
        }
    }
    Ok(out)
}

fn validated_node_from_raw(
    plan: &Plan,
    node: &PlanNode,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanNode, PlanValidationError> {
    let id = PlanNodeId::new(node.id.clone())?;
    let depends_on = typed_node_ids(&node.depends_on)?;
    let uses_result = enrich_uses_result_provenance(&node.uses_result, plan, node.id.as_str())?;
    match node.kind {
        PlanNodeKind::MapBody => {
            let body = node.map_body.as_ref().ok_or_else(|| {
                PlanValidationError::Structure(PlanNodeStructureError::MissingMapBodyPayload)
            })?;
            Ok(ValidatedPlanNode::MapBody(ValidatedMapBodyNode {
                id,
                body: body.clone(),
                plan: Box::new(crate::plasm_step_convert::lift_body(body)?),
                depends_on,
                uses_result,
            }))
        }
        PlanNodeKind::Capture => Err(PlanValidationError::UnscopedCapture),
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
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                projection: node.projection.clone(),
                predicates: node.predicates.clone(),
                depends_on,
                uses_result,
                approval: node.approval.clone(),
                page_size: node.page_size,
                pushed_read_budget: None,
            }))
        }
        PlanNodeKind::Data => Ok(ValidatedPlanNode::Data(ValidatedDataNode {
            id,
            effect_class: node.effect_class,
            result_shape: node.result_shape,
            data: node
                .data
                .clone()
                .ok_or_else(|| PlanValidationError::MissingField {
                    field: format!("plan.nodes[{node_index}].data"),
                })?,
            depends_on,
            uses_result,
        })),
        PlanNodeKind::Derive => {
            let template =
                node.derive_template
                    .as_ref()
                    .ok_or_else(|| PlanValidationError::MissingField {
                        field: format!("plan.nodes[{node_index}].derive_template"),
                    })?;
            let source =
                template
                    .source
                    .as_ref()
                    .ok_or_else(|| PlanValidationError::MissingField {
                        field: format!("plan.nodes[{node_index}].derive_template.source"),
                    })?;
            let source = PlanNodeId::new(source.clone())?;
            let item_binding = template.item_binding.as_ref().ok_or_else(|| {
                PlanValidationError::MissingField {
                    field: format!("plan.nodes[{node_index}].derive_template.item_binding"),
                }
            })?;
            let item_binding = BindingName::new(item_binding.clone())?;
            let inputs = template
                .inputs
                .iter()
                .map(|input| validated_data_input(plan, input, by_id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ValidatedPlanNode::Derive(ValidatedDeriveNode {
                kind: template.kind,
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
            source_node: PlanNodeId::new(
                &node
                    .compute
                    .as_ref()
                    .ok_or_else(|| PlanValidationError::MissingField {
                        field: format!("plan.nodes[{node_index}].compute"),
                    })?
                    .source,
            )?,
            id,
            effect_class: node.effect_class,
            result_shape: node.result_shape,
            compute: node
                .compute
                .clone()
                .ok_or_else(|| PlanValidationError::MissingField {
                    field: format!("plan.nodes[{node_index}].compute"),
                })?,
            depends_on,
            uses_result,
        })),
        PlanNodeKind::Relation => {
            let relation =
                node.relation
                    .as_ref()
                    .ok_or_else(|| PlanValidationError::MissingField {
                        field: format!("plan.nodes[{node_index}].relation"),
                    })?;
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
                        materialize: relation
                            .materialize
                            .clone()
                            .unwrap_or(plasm_core::RelationMaterialization::Unavailable),
                        view_embed_proof: relation.view_embed_proof.clone(),
                        binding_proofs: relation.binding_proofs.clone(),
                    },
                    depends_on,
                    uses_result,
                    pushed_read_budget: None,
                },
            ))
        }
        PlanNodeKind::ForEach => {
            let source = node
                .source
                .as_ref()
                .ok_or_else(|| PlanValidationError::MissingField {
                    field: format!("plan.nodes[{node_index}].source"),
                })?;
            let source = PlanNodeId::new(source.clone())?;
            let item_binding =
                node.item_binding
                    .as_ref()
                    .ok_or_else(|| PlanValidationError::MissingField {
                        field: format!("plan.nodes[{node_index}].item_binding"),
                    })?;
            let item_binding = BindingName::new(item_binding.clone())?;
            Ok(ValidatedPlanNode::ForEach(ValidatedForEachNode {
                id,
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                source,
                item_binding,
                effect_template: validated_effect_template(
                    node.effect_template
                        .as_ref()
                        .ok_or_else(|| {
                            format!("plan.nodes[{node_index}].effect_template is required")
                        })
                        .map_err(|field| PlanValidationError::MissingField { field })?,
                    node_index,
                )?,
                projection: node.projection.clone(),
                predicates: node.predicates.clone(),
                depends_on,
                uses_result,
                approval: node.approval.clone(),
            }))
        }
        PlanNodeKind::IterateUntil => {
            let source = node
                .source
                .as_ref()
                .ok_or_else(|| PlanValidationError::MissingField {
                    field: format!("plan.nodes[{node_index}].source"),
                })?;
            let source = PlanNodeId::new(source.clone())?;
            let item_binding =
                node.item_binding
                    .as_ref()
                    .ok_or_else(|| PlanValidationError::MissingField {
                        field: format!("plan.nodes[{node_index}].item_binding"),
                    })?;
            let item_binding = BindingName::new(item_binding.clone())?;
            let take = require_iterate_hard_bound(node, node_index)?;
            if node.predicates.is_empty() && node.until_scope.is_none() {
                return Err(PlanValidationError::MissingIterationPredicate { index: node_index });
            }
            Ok(ValidatedPlanNode::IterateUntil(ValidatedIterateUntilNode {
                id,
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                source: source.clone(),
                item_binding,
                effect_template: validated_effect_template(
                    node.effect_template
                        .as_ref()
                        .ok_or_else(|| {
                            format!("plan.nodes[{node_index}].effect_template is required")
                        })
                        .map_err(|field| PlanValidationError::MissingField { field })?,
                    node_index,
                )?,
                until_predicates: node.predicates.clone(),
                until_scope: node.until_scope.clone(),
                step_scope: node.step_scope.clone(),
                until_plan: node
                    .until_scope
                    .as_deref()
                    .map(crate::plasm_step_convert::lift_body)
                    .transpose()?
                    .map(Box::new),
                step_plan: node
                    .step_scope
                    .as_deref()
                    .map(crate::plasm_step_convert::lift_body)
                    .transpose()?
                    .map(Box::new),
                take,
                seed_ir: Some({
                    let seed_node = plan
                        .nodes
                        .iter()
                        .find(|n| n.id == source.as_str())
                        .ok_or_else(|| PlanValidationError::InvalidIterateSeed {
                            node: source.as_str().to_owned(),
                        })?;
                    if seed_node.kind != PlanNodeKind::Get {
                        return Err(PlanValidationError::InvalidIterateSeed {
                            node: source.as_str().to_owned(),
                        });
                    }
                    // Bound identity is `ir_template` at plan time; literal identity is `ir`.
                    // Iterate replays either form (template + binding), not `uses_result.is_empty()`.
                    let seed_replay = if let Some(ir) = seed_node.ir.as_ref() {
                        ir.clone()
                    } else if let Some(template) = seed_node.ir_template.as_ref() {
                        crate::plasm_plan::PlanExprIr {
                            expr: template.expr.clone(),
                            projection: template.projection.clone(),
                            display_expr: template.display_expr.clone(),
                        }
                    } else {
                        return Err(PlanValidationError::InvalidIterateSeed {
                            node: source.as_str().to_owned(),
                        });
                    };
                    validated_plan_expr_ir(&seed_replay, node_index, "iterate_until.seed_ir")?
                }),
                depends_on,
                uses_result,
                approval: node.approval.clone(),
            }))
        }
    }
}

fn typed_node_ids(raw: &[String]) -> Result<Vec<PlanNodeId>, PlanValidationError> {
    raw.iter()
        .cloned()
        .map(PlanNodeId::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(PlanValidationError::from)
}

fn validated_data_input(
    plan: &Plan,
    input: &PlanDataInput,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanDataInput, PlanValidationError> {
    let proof = match input.cardinality {
        InputCardinality::Acknowledgement => {
            if by_id
                .get(&input.node)
                .map(|index| plan.nodes[*index].result_shape)
                != Some(ResultShape::SideEffectAck)
            {
                return Err(PlanValidationError::InvalidAcknowledgementSource {
                    node: input.node.to_string(),
                });
            }
            InputCardinalityProof::Acknowledgement
        }
        InputCardinality::Collection => InputCardinalityProof::Collection,
        InputCardinality::Singleton => InputCardinalityProof::RuntimeCheckedSingleton,
        InputCardinality::Auto => {
            cardinality::analyze_static_cardinality(plan, by_id, input.node.as_str())
                .try_auto_broadcast_input_proof()
                .ok_or_else(|| PlanValidationError::UnprovenInputBroadcast {
                    node: input.node.to_string(),
                })?
        }
    };
    Ok(ValidatedPlanDataInput {
        node: PlanNodeId::new(input.node.clone())?,
        alias: InputAlias::new(input.alias.clone())?,
        proof,
    })
}

fn validate_return_refs(
    ret: &PlanReturn,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanReturn, PlanValidationError> {
    match ret {
        PlanReturn::Node { node } if node.trim().is_empty() => {
            return Err(PlanValidationError::EmptyReturn);
        }
        PlanReturn::Parallel { nodes } if nodes.is_empty() => {
            return Err(PlanValidationError::EmptyReturn);
        }
        _ => {}
    }
    for id in return_refs(ret) {
        if !by_id.contains_key(id) {
            return Err(PlanValidationError::UnknownReturnNode {
                node: id.to_owned(),
            });
        }
    }
    match ret {
        PlanReturn::Node { node } => Ok(ValidatedPlanReturn::Node(PlanNodeId::new(node.clone())?)),
        PlanReturn::Parallel { nodes } => Ok(ValidatedPlanReturn::Parallel {
            parallel: nodes
                .iter()
                .cloned()
                .map(PlanNodeId::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(PlanValidationError::from)?,
        }),
    }
}

fn require_iterate_hard_bound(
    node: &PlanNode,
    _node_index: usize,
) -> Result<u32, PlanValidationError> {
    let take = node.take.ok_or(PlanValidationError::InvalidIterateBound)?;
    if take == 0 {
        return Err(PlanValidationError::InvalidIterateBound);
    }
    Ok(take)
}

/// Shared source + item binding + effect-template interpolation checks for `for_each` / `iterate_until`.
fn validate_row_effect_source_and_template<'a>(
    n: &'a PlanNode,
    i: usize,
    by_id: &HashMap<String, usize>,
    kind: &str,
) -> Result<(&'a str, &'a EffectTemplate), PlanValidationError> {
    let source = n
        .source
        .as_ref()
        .ok_or_else(|| PlanValidationError::MissingField {
            field: format!("plan.nodes[{i}].source for {kind}"),
        })?;
    if !by_id.contains_key(source) {
        return Err(PlanValidationError::UnknownRowEffectSource {
            kind: kind.to_owned(),
            source_id: source.clone(),
        });
    }
    let binding = n.item_binding.as_deref().unwrap_or_default();
    if binding.trim().is_empty() {
        return Err(PlanValidationError::MissingRowEffectBinding {
            kind: kind.to_owned(),
        });
    }
    let template = n
        .effect_template
        .as_ref()
        .ok_or_else(|| PlanValidationError::MissingField {
            field: format!("plan.nodes[{i}].effect_template"),
        })?;
    validate_effect_template(template, i).map_err(PlanValidationError::Compute)?;
    if let Some(body) = &n.step_scope {
        let effect = plasm_core::plasm_monad::correlated::iteration_step_effect(body)?;
        if body.parent.source.as_str() != source
            || effect.ir_template.expr != template.ir_template.expr
            || effect.qualified_entity.entry_id != template.qualified_entity.entry_id
            || effect.qualified_entity.entity != template.qualified_entity.entity
        {
            return Err(PlanValidationError::IterationStepMismatch);
        }
        return Ok((binding, template));
    }
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
    validate_expression_operands(&template.ir_template.expr, i, &ctx)?;
    Ok((binding, template))
}

fn topological_order(
    plan: &Plan,
    adj: &[Vec<usize>],
) -> Result<Vec<PlanNodeId>, PlanValidationError> {
    fn visit(
        i: usize,
        plan: &Plan,
        adj: &[Vec<usize>],
        mark: &mut [u8],
        out: &mut Vec<PlanNodeId>,
    ) -> Result<(), PlanValidationError> {
        if mark[i] == 2 {
            return Ok(());
        }
        if mark[i] == 1 {
            return Err(PlanValidationError::DependencyCycle);
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

/// Deserialize and validate a program-shaped plan value (same serialized IR as traces/archives).
pub fn validate_plan_value(plan: &serde_json::Value) -> Result<(), PlanValidationError> {
    let plan = parse_plan_value(plan)?;
    validate_plan(&plan)
}

#[cfg(test)]
mod error_footprint_tests {
    use super::*;

    #[test]
    fn provenance_metadata_remains_owned_and_error_stays_small() {
        let bytes = std::mem::size_of::<PlanValidationError>();
        assert!(
            bytes < 128,
            "PlanValidationError is {bytes} bytes; expected <128"
        );
        let expected = QualifiedEntityKey {
            entry_id: "catalog-a".into(),
            entity: "item".into(),
        };
        let actual = QualifiedEntityKey {
            entry_id: "catalog-b".into(),
            entity: "item".into(),
        };
        let error = PlanValidationError::ContradictoryProvenance {
            consumer: "consumer".into(),
            alias: "input".into(),
            node: "source".into(),
            expected: Box::new(expected.clone()),
            actual: Box::new(actual.clone()),
        };
        assert_eq!(
            error.to_string(),
            "plan node `consumer` alias `input` has contradictory provenance for source `source`"
        );
        match error {
            PlanValidationError::ContradictoryProvenance {
                expected: stored_expected,
                actual: stored_actual,
                ..
            } => {
                assert_eq!(*stored_expected, expected);
                assert_eq!(*stored_actual, actual);
            }
            _ => panic!("expected contradictory provenance"),
        }
    }
}
