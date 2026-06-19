//! Bidirectional convert between validated plan nodes and typed [`PlasmStepPayload`] wire steps.

use crate::plasm_comp_lift::ExecutablePlasmComp;
use crate::plasm_plan::{
    BindingName, EffectClass as PlanEffectClass, EffectTemplate, InputAlias, InputCardinalityProof,
    Plan, PlanNodeId, PlanNodeKind, PlanResultUse, PlanValue, QualifiedEntityKey,
    ResultShape as PlanResultShape, ValidatedComputeNode, ValidatedDataNode, ValidatedDeriveNode,
    ValidatedForEachNode, ValidatedPlan, ValidatedPlanArtifact, ValidatedPlanDataInput,
    ValidatedPlanExprIr, ValidatedPlanExprTemplate, ValidatedPlanNode,
    ValidatedPlanRelationTraversal, ValidatedPlanReturn, ValidatedRelationTraversalNode,
    ValidatedSurfaceNode,
};
use plasm_core::{
    BindingName as CoreBindingName, DeriveKind, DerivePayload, DeriveTemplate, EffectClass,
    EffectTemplate as CoreEffectTemplate, Expr, FlatMapEffectPayload, FlatMapRelationPayload,
    InputCardinality as CoreInputCardinality, InvokePayload, MapPayload, PlanDataInput, PlanExprIr,
    PlanExprTemplate, PlanInputBinding, PlanPredicate, PlanQualifiedEntityKey,
    PlanRelationTraversal, PlasmBindGraph, PlasmComp, PlasmDataValue, PlasmReturn,
    PlasmStepPayload, PurePayload, ResultShape, StepId, SurfaceKind,
};
use std::collections::HashMap;

pub(crate) fn validated_node_to_step_payload(
    node: &ValidatedPlanNode,
) -> Result<PlasmStepPayload, String> {
    match node {
        ValidatedPlanNode::Surface(n) => Ok(PlasmStepPayload::Invoke(surface_to_invoke(n)?)),
        ValidatedPlanNode::Data(n) => Ok(PlasmStepPayload::Pure(data_to_pure(n)?)),
        ValidatedPlanNode::Compute(n) => Ok(PlasmStepPayload::Map(compute_to_map(n)?)),
        ValidatedPlanNode::Derive(n) => Ok(PlasmStepPayload::Derive(derive_to_payload(n)?)),
        ValidatedPlanNode::RelationTraversal(n) => {
            Ok(PlasmStepPayload::FlatMapRelation(relation_to_payload(n)?))
        }
        ValidatedPlanNode::ForEach(n) => {
            Ok(PlasmStepPayload::FlatMapEffect(for_each_to_payload(n)?))
        }
    }
}

fn surface_to_invoke(node: &ValidatedSurfaceNode) -> Result<InvokePayload, String> {
    Ok(InvokePayload {
        plan_kind: plan_kind_to_surface(node.kind)?,
        qualified_entity: node.qualified_entity.as_ref().map(qualified_entity_key),
        ir: node
            .ir
            .as_ref()
            .map(validated_expr_ir_to_plan)
            .transpose()?,
        ir_template: node
            .ir_template
            .as_ref()
            .map(validated_expr_template_to_plan),
        projection: node.projection.clone(),
        predicates: convert_predicates(&node.predicates)?,
        page_size: node.page_size,
        approval: node.approval.clone(),
        display_expr: node.display_expr.clone(),
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn data_to_pure(node: &ValidatedDataNode) -> Result<PurePayload, String> {
    Ok(PurePayload {
        data: plan_value_to_data(&node.data)?,
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn compute_to_map(node: &ValidatedComputeNode) -> Result<MapPayload, String> {
    Ok(MapPayload {
        compute: convert_via_json(&node.compute)?,
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn derive_to_payload(node: &ValidatedDeriveNode) -> Result<DerivePayload, String> {
    Ok(DerivePayload {
        derive: DeriveTemplate {
            kind: DeriveKind::Map,
            source: Some(node.source.as_str().to_string()),
            item_binding: Some(binding_name(&node.item_binding)?),
            inputs: node
                .inputs
                .iter()
                .map(validated_data_input_to_plan)
                .collect(),
            value: plan_value_to_data(&node.value)?,
        },
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn relation_to_payload(
    node: &ValidatedRelationTraversalNode,
) -> Result<FlatMapRelationPayload, String> {
    Ok(FlatMapRelationPayload {
        relation: relation_traversal_to_plan(&node.relation)?,
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn for_each_to_payload(node: &ValidatedForEachNode) -> Result<FlatMapEffectPayload, String> {
    Ok(FlatMapEffectPayload {
        source: node.source.as_str().to_string(),
        item_binding: binding_name(&node.item_binding)?,
        effect_template: effect_template_to_core(&node.effect_template)?,
        projection: node.projection.clone(),
        predicates: convert_predicates(&node.predicates)?,
        approval: node.approval.clone(),
        effect_class: effect_class(node.effect_class),
        result_shape: result_shape(node.result_shape),
    })
}

fn relation_traversal_to_plan(
    relation: &ValidatedPlanRelationTraversal,
) -> Result<PlanRelationTraversal, String> {
    Ok(PlanRelationTraversal {
        source: relation.source.as_str().to_string(),
        relation: relation.relation.as_str().to_string(),
        target: qualified_entity_key(&relation.target),
        cardinality: convert_via_json(&relation.cardinality)?,
        source_cardinality: convert_via_json(&relation.source_cardinality)?,
        expr: relation_expr(&relation.ir),
        ir: validated_expr_ir_to_plan(&relation.ir)?,
        binding_proofs: relation.binding_proofs.clone(),
        materialize: Some(relation.materialize.clone()),
    })
}

fn effect_template_to_core(template: &EffectTemplate) -> Result<CoreEffectTemplate, String> {
    Ok(CoreEffectTemplate {
        kind: plan_kind_to_surface(template.kind)?,
        qualified_entity: qualified_entity_key(&template.qualified_entity),
        expr_template: template.expr_template.clone(),
        ir_template: convert_via_json(&template.ir_template)?,
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

fn validated_expr_ir_to_plan(ir: &ValidatedPlanExprIr) -> Result<PlanExprIr, String> {
    Ok(PlanExprIr {
        expr: serde_json::to_value(&ir.expr).map_err(|e| e.to_string())?,
        projection: ir.projection.clone(),
        display_expr: ir.display_expr.clone(),
    })
}

fn validated_expr_template_to_plan(template: &ValidatedPlanExprTemplate) -> PlanExprTemplate {
    PlanExprTemplate {
        expr: template.expr.clone(),
        projection: template.projection.clone(),
        display_expr: template.display_expr.clone(),
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

fn binding_name(name: &crate::plasm_plan::BindingName) -> Result<CoreBindingName, String> {
    CoreBindingName::new(name.as_str())
}

fn plan_kind_to_surface(kind: PlanNodeKind) -> Result<SurfaceKind, String> {
    match kind {
        PlanNodeKind::Query => Ok(SurfaceKind::Query),
        PlanNodeKind::Search => Ok(SurfaceKind::Search),
        PlanNodeKind::Get => Ok(SurfaceKind::Get),
        PlanNodeKind::Create => Ok(SurfaceKind::Create),
        PlanNodeKind::Update => Ok(SurfaceKind::Update),
        PlanNodeKind::Delete => Ok(SurfaceKind::Delete),
        PlanNodeKind::Action => Ok(SurfaceKind::Action),
        other => Err(format!("expected surface plan kind, got {other:?}")),
    }
}

fn plan_value_to_data(value: &PlanValue) -> Result<PlasmDataValue, String> {
    convert_via_json(value)
}

fn convert_predicates(
    predicates: &[crate::plasm_plan::PlanPredicate],
) -> Result<Vec<PlanPredicate>, String> {
    predicates.iter().map(convert_via_json).collect()
}

fn relation_expr(ir: &ValidatedPlanExprIr) -> String {
    ir.display_expr
        .clone()
        .unwrap_or_else(|| crate::expr_display::expr_display(&ir.expr))
}

fn effect_class(value: PlanEffectClass) -> EffectClass {
    convert_via_json(&value).expect("EffectClass wire shape aligns")
}

fn result_shape(value: PlanResultShape) -> ResultShape {
    convert_via_json(&value).expect("ResultShape wire shape aligns")
}

fn convert_via_json<T, U>(value: &T) -> Result<U, String>
where
    T: serde::Serialize,
    U: for<'de> serde::Deserialize<'de>,
{
    serde_json::from_value(serde_json::to_value(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// Lift executable comp steps back into proof-bearing validated plan nodes.
pub(crate) fn step_payload_to_validated_node(
    step_id: &StepId,
    payload: &PlasmStepPayload,
    bind: &PlasmBindGraph,
) -> Result<ValidatedPlanNode, String> {
    let id = PlanNodeId::new(step_id.as_str().to_string())?;
    let depends_on = step_depends_on(step_id, bind);
    let uses_result = step_uses_result(step_id, bind);
    match payload {
        PlasmStepPayload::Invoke(p) => Ok(ValidatedPlanNode::Surface(ValidatedSurfaceNode {
            id,
            kind: surface_kind_to_plan(p.plan_kind)?,
            qualified_entity: p.qualified_entity.as_ref().map(plan_qualified_entity_key),
            ir: p.ir.as_ref().map(plan_expr_ir_to_validated).transpose()?,
            ir_template: p.ir_template.as_ref().map(plan_expr_template_to_validated),
            display_expr: p.display_expr.clone(),
            effect_class: plan_effect_class(p.effect_class),
            result_shape: plan_result_shape(p.result_shape),
            projection: p.projection.clone(),
            predicates: convert_predicates_back(&p.predicates)?,
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
            data: data_value_to_plan(&p.data)?,
            depends_on,
            uses_result,
        })),
        PlasmStepPayload::Map(p) => Ok(ValidatedPlanNode::Compute(ValidatedComputeNode {
            id,
            effect_class: plan_effect_class(p.effect_class),
            result_shape: plan_result_shape(p.result_shape),
            compute: convert_via_json(&p.compute)?,
            depends_on,
            uses_result,
        })),
        PlasmStepPayload::Derive(p) => {
            let derive = &p.derive;
            Ok(ValidatedPlanNode::Derive(ValidatedDeriveNode {
                id,
                effect_class: plan_effect_class(p.effect_class),
                result_shape: plan_result_shape(p.result_shape),
                source: PlanNodeId::new(
                    derive.source.as_deref().ok_or_else(|| {
                        format!("derive step {} missing source", step_id.as_str())
                    })?,
                )?,
                item_binding: BindingName::new(
                    derive
                        .item_binding
                        .as_ref()
                        .ok_or_else(|| {
                            format!("derive step {} missing item_binding", step_id.as_str())
                        })?
                        .as_str(),
                )?,
                inputs: derive
                    .inputs
                    .iter()
                    .map(plan_data_input_to_validated)
                    .collect::<Result<_, _>>()?,
                value: data_value_to_plan(&derive.value)?,
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
        PlasmStepPayload::FlatMapEffect(p) => {
            Ok(ValidatedPlanNode::ForEach(ValidatedForEachNode {
                id,
                effect_class: plan_effect_class(p.effect_class),
                result_shape: plan_result_shape(p.result_shape),
                source: PlanNodeId::new(p.source.clone())?,
                item_binding: BindingName::new(p.item_binding.as_str())?,
                effect_template: effect_template_to_plan(&p.effect_template)?,
                projection: p.projection.clone(),
                predicates: convert_predicates_back(&p.predicates)?,
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
) -> Result<ValidatedPlan, String> {
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
    Ok(ValidatedPlanArtifact::from_validated_parts(
        Plan::new_program(
            comp.version,
            comp.name.clone(),
            nodes,
            return_value,
            comp.metadata.clone(),
        ),
        topo,
        node_indices,
        approval_gates,
    ))
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
                })
                .collect()
        })
        .unwrap_or_default()
}

fn plasm_return_to_validated(ret: &PlasmReturn) -> Result<ValidatedPlanReturn, String> {
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

fn plan_expr_ir_to_validated(ir: &PlanExprIr) -> Result<ValidatedPlanExprIr, String> {
    let expr: Expr = serde_json::from_value(ir.expr.clone()).map_err(|e| e.to_string())?;
    Ok(ValidatedPlanExprIr {
        expr,
        projection: ir.projection.clone(),
        display_expr: ir.display_expr.clone(),
    })
}

fn plan_expr_template_to_validated(template: &PlanExprTemplate) -> ValidatedPlanExprTemplate {
    ValidatedPlanExprTemplate {
        expr: template.expr.clone(),
        projection: template.projection.clone(),
        display_expr: template.display_expr.clone(),
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

fn plan_data_input_to_validated(input: &PlanDataInput) -> Result<ValidatedPlanDataInput, String> {
    Ok(ValidatedPlanDataInput {
        node: PlanNodeId::new(input.node.clone())?,
        alias: InputAlias::new(input.alias.clone())?,
        proof: match input.cardinality {
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

fn surface_kind_to_plan(kind: SurfaceKind) -> Result<PlanNodeKind, String> {
    Ok(match kind {
        SurfaceKind::Query => PlanNodeKind::Query,
        SurfaceKind::Search => PlanNodeKind::Search,
        SurfaceKind::Get => PlanNodeKind::Get,
        SurfaceKind::Create => PlanNodeKind::Create,
        SurfaceKind::Update => PlanNodeKind::Update,
        SurfaceKind::Delete => PlanNodeKind::Delete,
        SurfaceKind::Action => PlanNodeKind::Action,
    })
}

fn relation_traversal_to_validated(
    relation: &PlanRelationTraversal,
) -> Result<ValidatedPlanRelationTraversal, String> {
    Ok(ValidatedPlanRelationTraversal {
        source: PlanNodeId::new(relation.source.clone())?,
        relation: crate::plasm_plan::RelationName::new(relation.relation.clone())?,
        target: plan_qualified_entity_key(&relation.target),
        cardinality: convert_via_json(&relation.cardinality)?,
        source_cardinality: convert_via_json(&relation.source_cardinality)?,
        ir: plan_expr_ir_to_validated(&relation.ir)?,
        materialize: relation
            .materialize
            .clone()
            .unwrap_or(plasm_core::RelationMaterialization::Unavailable),
        binding_proofs: relation.binding_proofs.clone(),
    })
}

fn effect_template_to_plan(template: &CoreEffectTemplate) -> Result<EffectTemplate, String> {
    Ok(EffectTemplate {
        kind: surface_kind_to_plan(template.kind)?,
        qualified_entity: plan_qualified_entity_key(&template.qualified_entity),
        expr_template: template.expr_template.clone(),
        ir_template: convert_via_json(&template.ir_template)?,
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
    })
}

fn data_value_to_plan(value: &PlasmDataValue) -> Result<PlanValue, String> {
    convert_via_json(value)
}

fn convert_predicates_back(
    predicates: &[PlanPredicate],
) -> Result<Vec<crate::plasm_plan::PlanPredicate>, String> {
    predicates.iter().map(convert_via_json).collect()
}

fn plan_effect_class(value: EffectClass) -> PlanEffectClass {
    convert_via_json(&value).expect("EffectClass wire shape aligns")
}

fn plan_result_shape(value: ResultShape) -> PlanResultShape {
    convert_via_json(&value).expect("ResultShape wire shape aligns")
}
