//! Structural lowering from resolved DAG nodes to plan nodes.

use super::super::prelude::*;
use super::super::types::{DagNode, DagNodeSource, PlanNodeEmitter};
use super::template_uses::{relation_plan_uses_result, result_use};
use crate::plasm_plan::{
    ComputeTemplate, DeriveKind, DeriveTemplate, EffectTemplate, PlanExprTemplate,
    PlanInputBinding, PlanNode, PlanResultUse, ResultShape,
};

pub(in crate::plasm_dag) fn lower_plan_node(node: &DagNode) -> Result<PlanNode, String> {
    node.source.emit_plan_node(node)
}

impl PlanNodeEmitter for DagNodeSource {
    fn emit_plan_node(&self, node: &DagNode) -> Result<PlanNode, String> {
        // This is an untrusted structural plan node. Admission owns checked construction.
        let mut out = PlanNode {
            id: node.id.clone(),
            kind: PlanNodeKind::Data,
            qualified_entity: None,
            expr: None,
            ir: None,
            ir_template: None,
            effect_class: EffectClass::ArtifactRead,
            result_shape: ResultShape::Artifact,
            projection: vec![],
            predicates: vec![],
            source: None,
            item_binding: None,
            effect_template: None,
            approval: None,
            data: None,
            derive_template: None,
            compute: None,
            relation: None,
            depends_on: vec![],
            uses_result: vec![],
            page_size: None,
            take: None,
            until: None,
        };
        match self {
            Self::Surface {
                view_singleton: _,
                parsed,
                kind,
                qualified_entity,
                effect_class,
                result_shape,
                uses_result,
            } => {
                out.kind = *kind;
                out.qualified_entity =
                    (*result_shape != ResultShape::Page).then(|| qualified_entity.clone());
                out.expr = Some(node.expr.clone());
                out.effect_class = *effect_class;
                out.result_shape = *result_shape;
                out.projection = parsed.projection.clone().unwrap_or_default();
                out.uses_result = uses_result.clone();
                out.page_size = node.page_size;
                // Identity is concrete `ir` only when resolved at plan time (no result
                // bindings). Bound Get (`e#{id_field=tok}`) stays `ir_template` so dry/CML
                // never stuffs a Binding hole (`@tok`) or dry stub into HTTP bearer.
                // Iterate seed admits Get *kind* with `ir` or `ir_template` (PLP-8); do not
                // use `uses_result.is_empty()` as the iterate gate.
                if uses_result.is_empty() {
                    out.ir = Some(PlanExprIr {
                        expr: parsed.expr.clone(),
                        projection: parsed.projection.clone(),
                        display_expr: None,
                    });
                } else {
                    out.ir_template = Some(expression_template(parsed, uses_result));
                }
            }
            Self::RelationTraversal {
                source_label,
                parsed,
                plan_relation,
                qualified_entity,
                effect_class,
                result_shape,
                ..
            } => {
                out.kind = PlanNodeKind::Relation;
                out.qualified_entity = Some(qualified_entity.clone());
                out.effect_class = *effect_class;
                out.result_shape = *result_shape;
                out.projection = parsed.projection.clone().unwrap_or_default();
                out.relation = Some(plan_relation.clone());
                out.uses_result = relation_plan_uses_result(source_label, parsed);
                out.page_size = node.page_size;
            }
            Self::Data(value) => {
                out.data = Some(value.clone());
                if let PlanValue::Template { input_bindings, .. } = value {
                    out.uses_result = input_bindings
                        .iter()
                        .map(|b| result_use(&b.from, &b.to))
                        .collect();
                    out.result_shape = ResultShape::Single;
                }
            }
            Self::Compute {
                source,
                op,
                schema,
                collection_alias,
            } => {
                out.kind = PlanNodeKind::Compute;
                out.result_shape = if matches!(op, ComputeOp::Python { per_row: false, .. })
                    || (matches!(op, ComputeOp::Render { .. }) && node.singleton)
                {
                    ResultShape::Single
                } else {
                    ResultShape::List
                };
                out.compute = Some(ComputeTemplate {
                    source: source.clone(),
                    op: op.clone(),
                    schema: schema.clone(),
                    page_size: node.page_size,
                    collection_alias: collection_alias.clone(),
                });
                out.uses_result = match op {
                    ComputeOp::Render {
                        render_bindings, ..
                    } => render_plan_graph_edges(source, render_bindings).1,
                    ComputeOp::Filter { predicates } => filter_plan_graph_edges(source, predicates),
                    ComputeOp::Union { other } => {
                        let mut uses = vec![result_use(source, "source")];
                        if other.as_str() != source {
                            uses.push(result_use(other.as_str(), other.as_str()));
                        }
                        uses
                    }
                    _ => vec![result_use(source, "source")],
                };
            }
            Self::Derive {
                source,
                value,
                inputs,
            } => {
                out.kind = PlanNodeKind::Derive;
                out.uses_result = std::iter::once(result_use(source, "_"))
                    .chain(
                        inputs
                            .iter()
                            .map(|input| result_use(&input.node, &input.alias)),
                    )
                    .collect();
                out.derive_template = Some(DeriveTemplate {
                    kind: DeriveKind::Map,
                    source: Some(source.clone()),
                    item_binding: Some("_".into()),
                    inputs: inputs.clone(),
                    value: value.clone(),
                });
            }
            Self::ScalarExtract { source, wire } => {
                out.kind = PlanNodeKind::Derive;
                out.result_shape = ResultShape::Single;
                out.uses_result = vec![result_use(source, "_")];
                out.derive_template = Some(DeriveTemplate {
                    kind: DeriveKind::Map,
                    source: Some(source.clone()),
                    item_binding: Some("_".into()),
                    inputs: vec![],
                    value: PlanValue::BindingSymbol {
                        binding: "_".into(),
                        path: vec![wire.clone()],
                    },
                });
            }
            Self::ForEach {
                source,
                parsed_template,
                display_expr,
                effect_kind,
                effect_class,
                result_shape,
                qualified_entity,
                uses_result,
            } => {
                out.kind = PlanNodeKind::ForEach;
                out.effect_class = *effect_class;
                out.result_shape = ResultShape::List;
                out.source = Some(source.clone());
                out.item_binding = Some("_".into());
                out.uses_result = std::iter::once(result_use(source, "_"))
                    .chain(uses_result.iter().cloned())
                    .collect();
                out.effect_template = Some(effect_template(
                    *effect_kind,
                    *effect_class,
                    *result_shape,
                    qualified_entity,
                    display_expr,
                    parsed_template,
                ));
            }
            Self::IterateUntil {
                seed,
                parsed_step_template,
                step_display,
                effect_kind,
                effect_class,
                result_shape,
                qualified_entity,
                until_body,
                until_predicates,
                take,
                uses_result,
            } => {
                out.kind = PlanNodeKind::IterateUntil;
                out.effect_class = *effect_class;
                out.result_shape = ResultShape::Single;
                out.source = Some(seed.clone());
                out.item_binding = Some("_".into());
                out.take = Some(*take);
                out.until = Some(until_body.clone());
                out.predicates = until_predicates.clone();
                out.uses_result = std::iter::once(result_use(seed, "_"))
                    .chain(uses_result.iter().cloned())
                    .collect();
                out.effect_template = Some(effect_template(
                    *effect_kind,
                    *effect_class,
                    *result_shape,
                    qualified_entity,
                    step_display,
                    parsed_step_template,
                ));
            }
        }
        out.depends_on = out
            .uses_result
            .iter()
            .map(|input| input.node.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(out)
    }
}

fn effect_template(
    kind: PlanNodeKind,
    effect_class: EffectClass,
    result_shape: ResultShape,
    qualified_entity: &QualifiedEntityKey,
    display: &str,
    template: &PlanExprTemplate,
) -> EffectTemplate {
    EffectTemplate {
        kind,
        qualified_entity: qualified_entity.clone(),
        expr_template: display.to_owned(),
        ir_template: template.clone(),
        effect_class,
        result_shape,
        projection: vec![],
        input_bindings: vec![],
    }
}

fn filter_plan_graph_edges(
    source: &str,
    predicates: &plasm_core::BooleanExpr<PlanPredicate>,
) -> Vec<PlanResultUse> {
    let mut uses = vec![result_use(source, "source")];
    for pred in predicates {
        for binding in pred.value.dependencies() {
            if binding != source {
                uses.push(result_use(&binding, &binding));
            }
        }
    }
    uses
}

pub(in crate::plasm_dag) fn expression_template(
    parsed: &plasm_core::expr_parser::ParsedExpr,
    uses: &[PlanResultUse],
) -> PlanExprTemplate {
    PlanExprTemplate {
        expr: parsed.expr.clone(),
        projection: parsed.projection.clone(),
        display_expr: None,
        input_bindings: uses
            .iter()
            .map(|input| PlanInputBinding {
                from: input.r#as.clone(),
                to: input.r#as.clone(),
            })
            .collect(),
    }
}
