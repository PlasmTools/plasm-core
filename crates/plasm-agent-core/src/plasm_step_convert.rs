//! Bidirectional convert between validated plan nodes and typed [`PlasmStepPayload`] wire steps.

use crate::plasm_comp_lift::ExecutablePlasmComp;
use crate::plasm_plan::{
    BindingName, EffectClass as PlanEffectClass, InputAlias, InputCardinalityProof, Plan,
    PlanNodeId, PlanNodeKind, PlanResultUse, PlanValue, QualifiedEntityKey,
    ResultShape as PlanResultShape, ValidatedComputeNode, ValidatedDataNode, ValidatedDeriveNode,
    ValidatedEffectTemplate, ValidatedForEachNode, ValidatedIterateUntilNode, ValidatedPlan,
    ValidatedPlanArtifact, ValidatedPlanDataInput, ValidatedPlanExprIr, ValidatedPlanExprTemplate,
    ValidatedPlanNode, ValidatedPlanRelationTraversal, ValidatedPlanReturn,
    ValidatedRelationTraversalNode, ValidatedSurfaceNode,
};
use plasm_core::plasm_monad::StepIdError;
use plasm_core::{
    BindingName as CoreBindingName, DeriveKind, DerivePayload, DeriveTemplate, EffectClass,
    EffectTemplate as CoreEffectTemplate, FlatMapApplyPayload, FlatMapRelationPayload,
    InputCardinality as CoreInputCardinality, InvokePayload, MapPayload, PlanDataInput, PlanExprIr,
    PlanExprTemplate, PlanInputBinding, PlanPredicate, PlanQualifiedEntityKey,
    PlanRelationTraversal, PlasmBindGraph, PlasmComp, PlasmDataValue, PlasmReturn,
    PlasmStepPayload, PurePayload, ResultShape, StepId, SurfaceKind, UnfoldUntilPayload,
};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StepPayloadLiftError {
    #[error(transparent)]
    Correlated(#[from] plasm_core::plasm_monad::CorrelatedBodyError),
    #[error("invalid plan atom: {0}")]
    PlanAtom(#[source] crate::plasm_plan::PlanAtomError),
    #[error(transparent)]
    StepId(#[from] StepIdError),
    #[error(transparent)]
    BindGraph(#[from] plasm_core::plasm_monad::BindGraphError),
    #[error(transparent)]
    IterationEffect(#[from] plasm_core::plasm_monad::IterationStepEffectError),
    #[error("derive step `{step}` is missing its source")]
    MissingDeriveSource { step: String },
    #[error("derive step `{step}` is missing its item binding")]
    MissingDeriveItemBinding { step: String },
    #[error("derive input `{input}` on `{step}` claims static singleton without proof")]
    InvalidSingletonProof { step: String, input: String },
    #[error("acknowledgement input `{input}` on `{step}` is not an acknowledgement source")]
    InvalidAcknowledgementProof { step: String, input: String },
    #[error("relation step `{step}` claims a single source without singleton proof")]
    InvalidRelationSingletonProof { step: String, source_node: String },
    #[error("scoped body has an invalid return shape")]
    InvalidScopedReturn,
    #[error("scoped body output schema is invalid")]
    InvalidScopedOutput,
    #[error("predicate scope is missing its parent schema")]
    MissingScopedParentSchema,
}

impl From<StepPayloadLiftError> for plasm_runtime::ExecutionFailure {
    fn from(error: StepPayloadLiftError) -> Self {
        let code = match &error {
            StepPayloadLiftError::Correlated(_) => "scope_body_invalid",
            StepPayloadLiftError::PlanAtom(_) => "plan_identifier_invalid",
            StepPayloadLiftError::StepId(_) => "step_id_invalid",
            StepPayloadLiftError::BindGraph(_) => "plan_bind_graph_invalid",
            StepPayloadLiftError::IterationEffect(_) => "scope_iteration_effect_invalid",
            StepPayloadLiftError::MissingDeriveSource { .. } => "derive_source_missing",
            StepPayloadLiftError::MissingDeriveItemBinding { .. } => "derive_item_binding_missing",
            StepPayloadLiftError::InvalidSingletonProof { .. } => "derive_singleton_proof_invalid",
            StepPayloadLiftError::InvalidAcknowledgementProof { .. } => {
                "derive_acknowledgement_proof_invalid"
            }
            StepPayloadLiftError::InvalidRelationSingletonProof { .. } => {
                "relation_singleton_proof_invalid"
            }
            StepPayloadLiftError::InvalidScopedReturn => "scope_return_invalid",
            StepPayloadLiftError::InvalidScopedOutput => "scope_output_invalid",
            StepPayloadLiftError::MissingScopedParentSchema => "scope_parent_schema_missing",
        };
        Self::new(
            plasm_runtime::FailureCause::Program,
            code,
            error.to_string(),
        )
    }
}

impl From<crate::plasm_plan::PlanAtomError> for StepPayloadLiftError {
    fn from(error: crate::plasm_plan::PlanAtomError) -> Self {
        Self::PlanAtom(error)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum StepPayloadConversionError {
    #[error("capture node `{node}` cannot be serialized as a root step")]
    CaptureCannotSerializeRoot { node: String },
    #[error("validated surface node has non-surface plan kind `{kind:?}`")]
    ExpectedSurfacePlanKind { kind: PlanNodeKind },
    #[error("validated plan contains invalid binding name `{name}`")]
    InvalidBindingName { name: String },
}

pub(crate) fn validated_node_to_step_payload(
    node: &ValidatedPlanNode,
) -> Result<PlasmStepPayload, StepPayloadConversionError> {
    match node {
        ValidatedPlanNode::MapBody(n) => Ok(PlasmStepPayload::MapBody(n.body.clone())),
        ValidatedPlanNode::Capture(_) => {
            Err(StepPayloadConversionError::CaptureCannotSerializeRoot {
                node: node.id().as_str().to_owned(),
            })
        }
        ValidatedPlanNode::Surface(n) => Ok(PlasmStepPayload::Invoke(surface_to_invoke(n)?)),
        ValidatedPlanNode::Data(n) => Ok(PlasmStepPayload::Pure(data_to_pure(n))),
        ValidatedPlanNode::Compute(n) => Ok(PlasmStepPayload::Map(compute_to_map(n))),
        ValidatedPlanNode::Derive(n) => Ok(PlasmStepPayload::Derive(derive_to_payload(n)?)),
        ValidatedPlanNode::RelationTraversal(n) => {
            Ok(PlasmStepPayload::FlatMapRelation(relation_to_payload(n)?))
        }
        ValidatedPlanNode::ForEach(n) => {
            Ok(PlasmStepPayload::FlatMapApply(for_each_to_payload(n)?))
        }
        ValidatedPlanNode::IterateUntil(n) => {
            Ok(PlasmStepPayload::UnfoldUntil(iterate_until_to_payload(n)?))
        }
    }
}

fn surface_to_invoke(
    node: &ValidatedSurfaceNode,
) -> Result<InvokePayload, StepPayloadConversionError> {
    Ok(InvokePayload {
        plan_kind: plan_kind_to_surface(node.kind)?,
        qualified_entity: node.qualified_entity.as_ref().map(qualified_entity_key),
        ir: node.ir.as_ref().map(validated_expr_ir_to_plan),
        ir_template: node
            .ir_template
            .as_ref()
            .map(validated_expr_template_to_plan),
        projection: node.projection.clone(),
        predicates: convert_predicates(&node.predicates),
        page_size: node.page_size,
        approval: node.approval.clone(),
        display_expr: None,
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn data_to_pure(node: &ValidatedDataNode) -> PurePayload {
    PurePayload {
        data: plan_value_to_data(&node.data),
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    }
}

fn compute_to_map(node: &ValidatedComputeNode) -> MapPayload {
    MapPayload {
        compute: node.compute.clone(),
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    }
}

fn derive_to_payload(
    node: &ValidatedDeriveNode,
) -> Result<DerivePayload, StepPayloadConversionError> {
    Ok(DerivePayload {
        derive: DeriveTemplate {
            kind: match node.kind {
                crate::plasm_plan::DeriveKind::Cell => DeriveKind::Cell,
                crate::plasm_plan::DeriveKind::Map => DeriveKind::Map,
                crate::plasm_plan::DeriveKind::Data => DeriveKind::Data,
            },
            source: Some(node.source.as_str().to_string()),
            item_binding: Some(binding_name(&node.item_binding)?),
            inputs: node
                .inputs
                .iter()
                .map(validated_data_input_to_plan)
                .collect(),
            value: plan_value_to_data(&node.value),
        },
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn relation_to_payload(
    node: &ValidatedRelationTraversalNode,
) -> Result<FlatMapRelationPayload, StepPayloadConversionError> {
    Ok(FlatMapRelationPayload {
        relation: relation_traversal_to_plan(&node.relation)?,
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn for_each_to_payload(
    node: &ValidatedForEachNode,
) -> Result<FlatMapApplyPayload, StepPayloadConversionError> {
    Ok(FlatMapApplyPayload {
        source: node.source.as_str().to_string(),
        item_binding: binding_name(&node.item_binding)?,
        effect_template: effect_template_to_core(&node.effect_template)?,
        projection: node.projection.clone(),
        predicates: convert_predicates(&node.predicates),
        approval: node.approval.clone(),
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn iterate_until_to_payload(
    node: &ValidatedIterateUntilNode,
) -> Result<UnfoldUntilPayload, StepPayloadConversionError> {
    Ok(UnfoldUntilPayload {
        source: node.source.as_str().to_string(),
        item_binding: binding_name(&node.item_binding)?,
        effect_template: effect_template_to_core(&node.effect_template)?,
        until_predicates: convert_predicates(&node.until_predicates),
        until_scope: node.until_scope.clone(),
        step_scope: node.step_scope.clone(),
        take: node.take,
        seed_ir: node.seed_ir.as_ref().map(validated_expr_ir_to_plan),
        approval: node.approval.clone(),
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn relation_traversal_to_plan(
    relation: &ValidatedPlanRelationTraversal,
) -> Result<PlanRelationTraversal, StepPayloadConversionError> {
    Ok(PlanRelationTraversal {
        source: relation.source.as_str().to_string(),
        relation: relation.relation.as_str().to_string(),
        target: qualified_entity_key(&relation.target),
        cardinality: relation.cardinality,
        source_cardinality: relation.source_cardinality,
        expr: relation_expr(&relation.ir),
        ir: validated_expr_ir_to_plan(&relation.ir),
        binding_proofs: relation.binding_proofs.clone(),
        materialize: Some(relation.materialize.clone()),
        view_embed_proof: relation.view_embed_proof.clone(),
    })
}

fn effect_template_to_core(
    template: &ValidatedEffectTemplate,
) -> Result<CoreEffectTemplate, StepPayloadConversionError> {
    Ok(CoreEffectTemplate {
        kind: plan_kind_to_surface(template.kind)?,
        qualified_entity: qualified_entity_key(&template.qualified_entity),
        expr_template: crate::plan_dry_display::render_executable_expr(
            &template.ir_template.expr,
            template.ir_template.projection.as_deref(),
            None,
        ),
        ir_template: validated_expr_template_to_plan(&template.ir_template),
        effect_class: effect_class(template.effect_class),
        result_shape: result_shape(template.result_shape),
        projection: template.projection.clone(),
        input_bindings: template
            .input_bindings
            .iter()
            .map(|b| PlanInputBinding {
                from: b.from.clone(),
                to: b.to.clone(),
            })
            .collect(),
    })
}

fn validated_expr_ir_to_plan(ir: &ValidatedPlanExprIr) -> PlanExprIr {
    PlanExprIr {
        expr: ir.expr.clone(),
        projection: ir.projection.clone(),
        display_expr: None,
    }
}

fn validated_expr_template_to_plan(template: &ValidatedPlanExprTemplate) -> PlanExprTemplate {
    PlanExprTemplate {
        expr: template.expr.clone(),
        projection: template.projection.clone(),
        display_expr: None,
        input_bindings: template
            .input_bindings
            .iter()
            .map(|b| PlanInputBinding {
                from: b.from.clone(),
                to: b.to.clone(),
            })
            .collect(),
    }
}

fn validated_data_input_to_plan(input: &ValidatedPlanDataInput) -> PlanDataInput {
    PlanDataInput {
        node: input.node.as_str().to_string(),
        alias: input.alias.as_str().to_string(),
        cardinality: match input.proof {
            InputCardinalityProof::Acknowledgement => CoreInputCardinality::Acknowledgement,
            InputCardinalityProof::Collection => CoreInputCardinality::Collection,
            InputCardinalityProof::StaticSingleton => CoreInputCardinality::Auto,
            InputCardinalityProof::RuntimeCheckedSingleton => CoreInputCardinality::Singleton,
        },
    }
}

fn qualified_entity_key(q: &crate::plasm_plan::QualifiedEntityKey) -> PlanQualifiedEntityKey {
    PlanQualifiedEntityKey {
        entry_id: q.entry_id.clone(),
        entity: q.entity.clone(),
    }
}

fn binding_name(
    name: &crate::plasm_plan::BindingName,
) -> Result<CoreBindingName, StepPayloadConversionError> {
    CoreBindingName::new(name.as_str()).map_err(|_| {
        StepPayloadConversionError::InvalidBindingName {
            name: name.as_str().to_owned(),
        }
    })
}

fn plan_kind_to_surface(kind: PlanNodeKind) -> Result<SurfaceKind, StepPayloadConversionError> {
    match kind {
        PlanNodeKind::Query => Ok(SurfaceKind::Query),
        PlanNodeKind::Search => Ok(SurfaceKind::Search),
        PlanNodeKind::Get => Ok(SurfaceKind::Get),
        PlanNodeKind::Create => Ok(SurfaceKind::Create),
        PlanNodeKind::Update => Ok(SurfaceKind::Update),
        PlanNodeKind::Delete => Ok(SurfaceKind::Delete),
        PlanNodeKind::Action => Ok(SurfaceKind::Action),
        other => Err(StepPayloadConversionError::ExpectedSurfacePlanKind { kind: other }),
    }
}

fn plan_value_to_data(value: &PlanValue) -> PlasmDataValue {
    value.clone()
}

fn convert_predicates(predicates: &[PlanPredicate]) -> Vec<PlanPredicate> {
    predicates.to_vec()
}

fn relation_expr(ir: &ValidatedPlanExprIr) -> String {
    crate::plan_dry_display::render_executable_expr(&ir.expr, ir.projection.as_deref(), None)
}

fn effect_class(value: PlanEffectClass) -> EffectClass {
    value
}

fn result_shape(value: PlanResultShape) -> ResultShape {
    value
}

/// Decode one untrusted wire step. The caller must validate graph-wide proofs before constructing
/// a [`ValidatedPlan`]; keeping this private prevents another module from treating the decoded
/// node as compiler-issued evidence.
fn step_payload_to_validated_node(
    step_id: &StepId,
    payload: &PlasmStepPayload,
    bind: &PlasmBindGraph,
) -> Result<ValidatedPlanNode, StepPayloadLiftError> {
    let id = PlanNodeId::new(step_id.as_str().to_string())?;
    let depends_on = step_depends_on(step_id, bind);
    let uses_result = step_uses_result(step_id, bind);
    match payload {
        PlasmStepPayload::MapBody(body) => {
            body.execution_layers()?;
            let plan = lift_body(body)?;
            Ok(ValidatedPlanNode::MapBody(
                crate::plasm_plan::ValidatedMapBodyNode {
                    id,
                    body: body.clone(),
                    plan: Box::new(plan),
                    depends_on,
                    uses_result,
                },
            ))
        }
        PlasmStepPayload::Invoke(p) => Ok(ValidatedPlanNode::Surface(ValidatedSurfaceNode {
            id,
            kind: surface_kind_to_plan(p.plan_kind),
            qualified_entity: p.qualified_entity.as_ref().map(plan_qualified_entity_key),
            ir: p.ir.as_ref().map(plan_expr_ir_to_validated),
            ir_template: p.ir_template.as_ref().map(plan_expr_template_to_validated),
            effect_class: plan_effect_class(p.effect_class),
            result_shape: plan_result_shape(p.result_shape),
            projection: p.projection.clone(),
            predicates: convert_predicates_back(&p.predicates),
            depends_on,
            uses_result,
            approval: p.approval.clone(),
            page_size: p.page_size,
            pushed_read_budget: None,
        })),
        PlasmStepPayload::Pure(p) => Ok(ValidatedPlanNode::Data(ValidatedDataNode {
            id,
            effect_class: plan_effect_class(p.effect_class),
            result_shape: plan_result_shape(p.result_shape),
            data: data_value_to_plan(&p.data),
            depends_on,
            uses_result,
        })),
        PlasmStepPayload::Map(p) => Ok(ValidatedPlanNode::Compute(ValidatedComputeNode {
            source_node: PlanNodeId::new(&p.compute.source)?,
            id,
            effect_class: plan_effect_class(p.effect_class),
            result_shape: plan_result_shape(p.result_shape),
            compute: p.compute.clone(),
            depends_on,
            uses_result,
        })),
        PlasmStepPayload::Derive(p) => {
            let derive = &p.derive;
            Ok(ValidatedPlanNode::Derive(ValidatedDeriveNode {
                kind: match derive.kind {
                    DeriveKind::Cell => crate::plasm_plan::DeriveKind::Cell,
                    DeriveKind::Map => crate::plasm_plan::DeriveKind::Map,
                    DeriveKind::Data => crate::plasm_plan::DeriveKind::Data,
                },
                id,
                effect_class: plan_effect_class(p.effect_class),
                result_shape: plan_result_shape(p.result_shape),
                source: PlanNodeId::new(derive.source.as_deref().ok_or_else(|| {
                    StepPayloadLiftError::MissingDeriveSource {
                        step: step_id.as_str().to_owned(),
                    }
                })?)?,
                item_binding: BindingName::new(
                    derive
                        .item_binding
                        .as_ref()
                        .ok_or_else(|| StepPayloadLiftError::MissingDeriveItemBinding {
                            step: step_id.as_str().to_owned(),
                        })?
                        .as_str(),
                )?,
                inputs: derive
                    .inputs
                    .iter()
                    .map(plan_data_input_to_validated)
                    .collect::<Result<_, _>>()?,
                value: data_value_to_plan(&derive.value),
                depends_on,
                uses_result,
            }))
        }
        PlasmStepPayload::FlatMapRelation(p) => Ok(ValidatedPlanNode::RelationTraversal(
            ValidatedRelationTraversalNode {
                id,
                effect_class: plan_effect_class(p.effect_class),
                result_shape: plan_result_shape(p.result_shape),
                relation: relation_traversal_to_validated(&p.relation)?,
                depends_on,
                uses_result,
                pushed_read_budget: None,
            },
        )),
        PlasmStepPayload::FlatMapApply(p) => Ok(ValidatedPlanNode::ForEach(ValidatedForEachNode {
            id,
            effect_class: plan_effect_class(p.effect_class),
            result_shape: plan_result_shape(p.result_shape),
            source: PlanNodeId::new(p.source.clone())?,
            item_binding: BindingName::new(p.item_binding.as_str())?,
            effect_template: effect_template_to_plan(&p.effect_template),
            projection: p.projection.clone(),
            predicates: convert_predicates_back(&p.predicates),
            depends_on,
            uses_result,
            approval: p.approval.clone(),
        })),
        PlasmStepPayload::UnfoldUntil(p) => {
            Ok(ValidatedPlanNode::IterateUntil(ValidatedIterateUntilNode {
                id,
                effect_class: plan_effect_class(p.effect_class),
                result_shape: plan_result_shape(p.result_shape),
                source: PlanNodeId::new(p.source.clone())?,
                item_binding: BindingName::new(p.item_binding.as_str())?,
                effect_template: effect_template_to_plan(&p.effect_template),
                until_predicates: convert_predicates_back(&p.until_predicates),
                until_scope: p.until_scope.clone(),
                step_scope: p.step_scope.clone(),
                until_plan: p
                    .until_scope
                    .as_deref()
                    .map(crate::plasm_step_convert::lift_body)
                    .transpose()?
                    .map(Box::new),
                step_plan: p
                    .step_scope
                    .as_deref()
                    .map(crate::plasm_step_convert::lift_body)
                    .transpose()?
                    .map(Box::new),
                take: p.take,
                seed_ir: p.seed_ir.as_ref().map(plan_expr_ir_to_validated),
                depends_on,
                uses_result,
                approval: p.approval.clone(),
            }))
        }
    }
}

/// Reconstruct a [`ValidatedPlan`] from lifted executable comp (for display/commit adapters).
pub(crate) fn build_validated_plan_from_executable(
    comp: &PlasmComp,
    executable: &ExecutablePlasmComp,
) -> Result<ValidatedPlan, StepPayloadLiftError> {
    let mut nodes = Vec::with_capacity(executable.steps_topo.len());
    let mut node_indices = HashMap::new();
    for (i, (step_id, payload)) in executable.steps_topo.iter().enumerate() {
        let node = step_payload_to_validated_node(step_id, payload, &executable.bind)?;
        node_indices.insert(node.id().clone(), i);
        nodes.push(node);
    }
    let return_value = plasm_return_to_validated(&executable.return_)?;
    let topo: Vec<PlanNodeId> = executable
        .bind
        .topo
        .iter()
        .map(|id| PlanNodeId::new(id.as_str().to_string()))
        .collect::<Result<_, _>>()?;
    let approval_gates = executable
        .approval_gates
        .iter()
        .map(|id| PlanNodeId::new(id.as_str().to_string()))
        .collect::<Result<_, _>>()?;
    let plan = Plan::new_program(
        comp.version,
        comp.name.clone(),
        nodes,
        return_value,
        comp.metadata.clone(),
    );
    validate_rehydrated_cardinality_proofs(&plan)?;
    Ok(ValidatedPlanArtifact::from_validated_parts(
        plan,
        topo,
        node_indices,
        approval_gates,
    ))
}

/// Rehydrated wire data is an untrusted DTO. In particular, the wire `auto` cardinality marker is
/// a request for compiler inference, not a serialized `StaticSingleton` witness. Recompute the
/// witness from the closed typed graph before constructing a trusted [`ValidatedPlan`].
fn validate_rehydrated_cardinality_proofs(
    plan: &Plan<crate::plasm_plan::ValidatedPlanState>,
) -> Result<(), StepPayloadLiftError> {
    for node in &plan.nodes {
        if let ValidatedPlanNode::Derive(derive) = node {
            for input in &derive.inputs {
                if input.proof == InputCardinalityProof::Acknowledgement
                    && plan
                        .nodes
                        .iter()
                        .find(|node| node.id() == &input.node)
                        .map(ValidatedPlanNode::result_shape)
                        != Some(PlanResultShape::SideEffectAck)
                {
                    return Err(StepPayloadLiftError::InvalidAcknowledgementProof {
                        step: derive.id.as_str().to_owned(),
                        input: input.alias.as_str().to_owned(),
                    });
                }
                if input.proof == InputCardinalityProof::StaticSingleton
                    && !crate::plasm_plan::validated_source_is_static_singleton(
                        plan,
                        input.node.as_str(),
                    )
                {
                    return Err(StepPayloadLiftError::InvalidSingletonProof {
                        step: derive.id.as_str().to_owned(),
                        input: input.alias.as_str().to_owned(),
                    });
                }
            }
        }
        if let ValidatedPlanNode::RelationTraversal(relation) = node {
            if relation.relation.cardinality == plasm_core::RelationCardinality::One
                && relation.relation.source_cardinality
                    == crate::plasm_plan::RelationSourceCardinality::Single
                && !crate::plasm_plan::validated_source_is_static_singleton(
                    plan,
                    relation.relation.source.as_str(),
                )
            {
                return Err(StepPayloadLiftError::InvalidRelationSingletonProof {
                    step: relation.id.as_str().to_owned(),
                    source_node: relation.relation.source.as_str().to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn step_depends_on(step_id: &StepId, bind: &PlasmBindGraph) -> Vec<PlanNodeId> {
    bind.deps
        .get(step_id)
        .map(|deps| {
            deps.iter()
                .filter_map(|d| PlanNodeId::new(d.as_str().to_string()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn step_uses_result(step_id: &StepId, bind: &PlasmBindGraph) -> Vec<PlanResultUse> {
    bind.holes
        .get(step_id)
        .map(|holes| {
            holes
                .iter()
                .map(|h| PlanResultUse {
                    node: h.step.as_str().to_string(),
                    r#as: h.alias.clone(),
                    qualified_entity: None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn plasm_return_to_validated(
    ret: &PlasmReturn,
) -> Result<ValidatedPlanReturn, StepPayloadLiftError> {
    match ret {
        PlasmReturn::Step { step } => Ok(ValidatedPlanReturn::Node(PlanNodeId::new(
            step.as_str().to_string(),
        )?)),
        PlasmReturn::Parallel { steps } => Ok(ValidatedPlanReturn::Parallel {
            parallel: steps
                .iter()
                .map(|s| PlanNodeId::new(s.as_str().to_string()))
                .collect::<Result<_, _>>()?,
        }),
    }
}

fn plan_expr_ir_to_validated(ir: &PlanExprIr) -> ValidatedPlanExprIr {
    let expr = ir.expr.clone();
    ValidatedPlanExprIr {
        expr,
        projection: ir.projection.clone(),
    }
}

fn plan_expr_template_to_validated(template: &PlanExprTemplate) -> ValidatedPlanExprTemplate {
    ValidatedPlanExprTemplate {
        expr: template.expr.clone(),
        projection: template.projection.clone(),
        input_bindings: template
            .input_bindings
            .iter()
            .map(|b| crate::plasm_plan::PlanInputBinding {
                from: b.from.clone(),
                to: b.to.clone(),
            })
            .collect(),
    }
}

fn plan_data_input_to_validated(
    input: &PlanDataInput,
) -> Result<ValidatedPlanDataInput, StepPayloadLiftError> {
    Ok(ValidatedPlanDataInput {
        node: PlanNodeId::new(input.node.clone())?,
        alias: InputAlias::new(input.alias.clone())?,
        proof: match input.cardinality {
            CoreInputCardinality::Acknowledgement => InputCardinalityProof::Acknowledgement,
            CoreInputCardinality::Collection => InputCardinalityProof::Collection,
            CoreInputCardinality::Auto => InputCardinalityProof::StaticSingleton,
            CoreInputCardinality::Singleton => InputCardinalityProof::RuntimeCheckedSingleton,
        },
    })
}

fn plan_qualified_entity_key(q: &PlanQualifiedEntityKey) -> QualifiedEntityKey {
    QualifiedEntityKey {
        entry_id: q.entry_id.clone(),
        entity: q.entity.clone(),
    }
}

pub(crate) fn surface_kind_to_plan(kind: SurfaceKind) -> PlanNodeKind {
    match kind {
        SurfaceKind::Query => PlanNodeKind::Query,
        SurfaceKind::Search => PlanNodeKind::Search,
        SurfaceKind::Get => PlanNodeKind::Get,
        SurfaceKind::Create => PlanNodeKind::Create,
        SurfaceKind::Update => PlanNodeKind::Update,
        SurfaceKind::Delete => PlanNodeKind::Delete,
        SurfaceKind::Action => PlanNodeKind::Action,
    }
}

fn relation_traversal_to_validated(
    relation: &PlanRelationTraversal,
) -> Result<ValidatedPlanRelationTraversal, StepPayloadLiftError> {
    Ok(ValidatedPlanRelationTraversal {
        source: PlanNodeId::new(relation.source.clone())?,
        relation: crate::plasm_plan::RelationName::new(relation.relation.clone())?,
        target: plan_qualified_entity_key(&relation.target),
        cardinality: relation.cardinality,
        source_cardinality: relation.source_cardinality,
        ir: plan_expr_ir_to_validated(&relation.ir),
        materialize: relation
            .materialize
            .clone()
            .unwrap_or(plasm_core::RelationMaterialization::Unavailable),
        view_embed_proof: relation.view_embed_proof.clone(),
        binding_proofs: relation.binding_proofs.clone(),
    })
}

fn effect_template_to_plan(template: &CoreEffectTemplate) -> ValidatedEffectTemplate {
    ValidatedEffectTemplate {
        kind: surface_kind_to_plan(template.kind),
        qualified_entity: plan_qualified_entity_key(&template.qualified_entity),
        ir_template: plan_expr_template_to_validated(&template.ir_template),
        effect_class: plan_effect_class(template.effect_class),
        result_shape: plan_result_shape(template.result_shape),
        projection: template.projection.clone(),
        input_bindings: template
            .input_bindings
            .iter()
            .map(|b| crate::plasm_plan::PlanInputBinding {
                from: b.from.clone(),
                to: b.to.clone(),
            })
            .collect(),
    }
}

fn data_value_to_plan(value: &PlasmDataValue) -> PlanValue {
    value.clone()
}

fn convert_predicates_back(predicates: &[PlanPredicate]) -> Vec<PlanPredicate> {
    predicates.to_vec()
}

fn plan_effect_class(value: EffectClass) -> PlanEffectClass {
    value
}

fn plan_result_shape(value: ResultShape) -> PlanResultShape {
    value
}

/// Lift a closed body with an explicit input port. No fake read/data step enters its wire plan.
pub(crate) fn lift_body(
    body: &plasm_core::plasm_monad::CorrelatedBody,
) -> Result<ValidatedPlan, StepPayloadLiftError> {
    let capture = PlanNodeId::new(body.parent.local.as_str())?;
    let mut nodes = vec![ValidatedPlanNode::Capture(
        crate::plasm_plan::ValidatedCaptureNode {
            id: capture.clone(),
            entity: plan_qualified_entity_key(&body.parent.entity),
            schema: body.parent_schema.clone(),
            value_contract: None,
            singleton: true,
            entity_authority: body.parent_entity_authority,
        },
    )];
    let mut topo = vec![capture];
    for capture in &body.captures {
        let id = PlanNodeId::new(capture.local.as_str())?;
        nodes.push(ValidatedPlanNode::Capture(
            crate::plasm_plan::ValidatedCaptureNode {
                id: id.clone(),
                entity: plan_qualified_entity_key(&capture.entity),
                schema: Some(capture.schema.clone()),
                value_contract: capture.value_contract.clone(),
                singleton: capture.singleton,
                entity_authority: capture.entity_authority,
            },
        ));
        topo.push(id);
    }
    for id in body.execution_layers()?.iter().flatten() {
        nodes.push(step_payload_to_validated_node(
            id,
            &body.body.steps[id.as_str()],
            &body.body.bind,
        )?);
        topo.push(PlanNodeId::new(id.as_str())?);
    }
    let indices = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id().clone(), i))
        .collect();
    let plan = Plan::new_program(
        body.body.version,
        body.body.name.clone(),
        nodes,
        plasm_return_to_validated(&body.body.return_)?,
        body.body.metadata.clone(),
    );
    validate_rehydrated_cardinality_proofs(&plan)?;
    Ok(ValidatedPlanArtifact::from_validated_parts(
        plan,
        topo,
        indices,
        vec![],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_plan::{
        ComputeOp, ComputeTemplate, EffectClass, InputAlias, InputCardinalityProof, OutputName,
        PlanNodeId, PlanValue, ResultShape, SyntheticFieldSchema, SyntheticResultSchema,
        SyntheticValueKind, ValidatedDataNode, ValidatedDeriveNode, ValidatedPlanDataInput,
        ValidatedPlanNode, ValidatedPlanReturn,
    };
    use plasm_core::PlasmBindGraph;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn non_surface_plan_kind_is_a_typed_conversion_failure() {
        assert_eq!(
            plan_kind_to_surface(PlanNodeKind::Capture),
            Err(StepPayloadConversionError::ExpectedSurfacePlanKind {
                kind: PlanNodeKind::Capture,
            })
        );
    }

    fn sample_render_node() -> ValidatedPlanNode {
        let mut column_aliases = BTreeMap::new();
        column_aliases.insert("p23".into(), OutputName::new("name").expect("name"));
        ValidatedPlanNode::Compute(ValidatedComputeNode {
            source_node: PlanNodeId::new("items").unwrap(),
            id: PlanNodeId::new("render0").expect("id"),
            effect_class: EffectClass::Read,
            result_shape: ResultShape::Single,
            compute: ComputeTemplate {
                source: "items".into(),
                op: ComputeOp::Render {
                    columns: vec![OutputName::new("name").expect("name")],
                    template: "{{ p23 }}".into(),
                    column_aliases,
                    render_bindings: vec![],
                },
                schema: SyntheticResultSchema {
                    optional_fields: Default::default(),
                    entity: Some("PlanRender".into()),
                    fields: vec![SyntheticFieldSchema {
                        value_type: None,
                        name: OutputName::new("content").expect("content"),
                        value_kind: SyntheticValueKind::String,
                        source: None,
                    }],
                },
                page_size: None,
                collection_alias: Some(OutputName::new("items").expect("items")),
            },
            depends_on: vec![PlanNodeId::new("items").expect("dep")],
            uses_result: vec![],
        })
    }

    #[test]
    fn render_column_aliases_survive_core_wire_serde() {
        let node = sample_render_node();
        let ValidatedPlanNode::Compute(c) = &node else {
            panic!("sample node");
        };
        let payload = validated_node_to_step_payload(&node).expect("to payload");
        let PlasmStepPayload::Map(map) = payload else {
            panic!("expected map payload");
        };
        match &map.compute.op {
            ComputeOp::Render { column_aliases, .. } => {
                assert_eq!(column_aliases.len(), 1);
                assert!(column_aliases.contains_key("p23"));
            }
            other => panic!("expected render op, got {other:?}"),
        }
        assert_eq!(
            map.compute.collection_alias.as_ref().map(|a| a.as_str()),
            Some("items")
        );
        assert_eq!(map.compute, c.compute);
    }

    #[test]
    fn render_column_aliases_survive_step_payload_round_trip() {
        let node = sample_render_node();
        let payload = validated_node_to_step_payload(&node).expect("to payload");
        let step_id = StepId::new("render0").expect("step id");
        let items_id = StepId::new("items").expect("items id");
        let mut deps = BTreeMap::new();
        deps.insert(step_id.clone(), BTreeSet::from([items_id.clone()]));
        let bind = PlasmBindGraph {
            topo: vec![items_id, step_id.clone()],
            deps,
            ..Default::default()
        };
        let back = step_payload_to_validated_node(&step_id, &payload, &bind).expect("from payload");
        let ValidatedPlanNode::Compute(c) = back else {
            panic!("round-trip node");
        };
        match c.compute.op {
            ComputeOp::Render { column_aliases, .. } => {
                assert_eq!(column_aliases.len(), 1);
                assert_eq!(column_aliases.get("p23").map(|c| c.as_str()), Some("name"));
            }
            other => panic!("expected render op, got {other:?}"),
        }
        assert_eq!(
            c.compute.collection_alias.as_ref().map(|a| a.as_str()),
            Some("items")
        );
    }

    #[test]
    fn rehydration_rejects_forged_static_singleton_proof() {
        let rows = PlanNodeId::new("rows").expect("rows");
        let mapped = PlanNodeId::new("mapped").expect("mapped");
        let plan = Plan::new_program(
            1,
            Some("forged-cardinality".into()),
            vec![
                ValidatedPlanNode::Data(ValidatedDataNode {
                    id: rows.clone(),
                    effect_class: EffectClass::ArtifactRead,
                    result_shape: ResultShape::Artifact,
                    data: PlanValue::Literal {
                        value: plasm_core::operand_binding::ResolvedValue::from_wire(
                            serde_json::json!([{"id": 1}, {"id": 2}]),
                        )
                        .expect("literal data"),
                    },
                    depends_on: vec![],
                    uses_result: vec![],
                }),
                ValidatedPlanNode::Derive(ValidatedDeriveNode {
                    kind: crate::plasm_plan::DeriveKind::Map,
                    id: mapped.clone(),
                    effect_class: EffectClass::ArtifactRead,
                    result_shape: ResultShape::Artifact,
                    source: rows.clone(),
                    item_binding: crate::plasm_plan::BindingName::new("item").expect("binding"),
                    inputs: vec![ValidatedPlanDataInput {
                        node: rows,
                        alias: InputAlias::new("items").expect("alias"),
                        proof: InputCardinalityProof::StaticSingleton,
                    }],
                    value: PlanValue::Literal {
                        value: plasm_core::operand_binding::ResolvedValue::from_wire(
                            serde_json::json!({"ok": true}),
                        )
                        .expect("literal data"),
                    },
                    depends_on: vec![],
                    uses_result: vec![],
                }),
            ],
            ValidatedPlanReturn::Node(mapped),
            BTreeMap::new(),
        );
        let error = validate_rehydrated_cardinality_proofs(&plan)
            .expect_err("untrusted wire proof must be recomputed");
        assert!(
            matches!(error, StepPayloadLiftError::InvalidSingletonProof { step, input }
            if step == "mapped" && input == "items")
        );

        let mut forged_ack = plan;
        let ValidatedPlanNode::Derive(derive) = &mut forged_ack.nodes[1] else {
            unreachable!();
        };
        derive.inputs[0].proof = InputCardinalityProof::Acknowledgement;
        let error = validate_rehydrated_cardinality_proofs(&forged_ack)
            .expect_err("a rowset cannot masquerade as an effect acknowledgement");
        assert!(
            matches!(error, StepPayloadLiftError::InvalidAcknowledgementProof { step, input }
            if step == "mapped" && input == "items")
        );
    }
}
