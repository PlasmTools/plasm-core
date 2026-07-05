//! Γ binding contracts derived from DAG nodes.

use super::prelude::*;
use super::types::{CompileState, DagNode, DagNodeSource, BindingContractSource};

pub(in crate::plasm_dag) fn binding_contract(state: &CompileState<'_>, label: &str) -> Option<ProgramBindingContract> {
    let node = state.get(label)?;
    Some(binding_contract_for_node(state, label, node))
}

pub(in crate::plasm_dag) fn binding_contract_for_node(
    state: &CompileState<'_>,
    label: &str,
    node: &DagNode,
) -> ProgramBindingContract {
    let mut contract = node
        .source
        .program_binding_contract(state, label, &node.expr);
    if node.singleton {
        contract.row_cardinality = match contract.row_cardinality {
            RowCardinalityProof::StaticPlural | RowCardinalityProof::RuntimeChecked => {
                RowCardinalityProof::BoundedSingleton {
                    kind: BoundedSingletonKind::ExplicitSingletonPostfix,
                    from_plural_source: true,
                }
            }
            other => other,
        };
    }
    contract
}

impl BindingContractSource for DagNodeSource {
    fn program_binding_contract(
        &self,
        state: &CompileState<'_>,
        label: &str,
        node_expr: &str,
    ) -> ProgramBindingContract {
        program_binding_contract_for_source(state, label, node_expr, self)
    }
}

pub(in crate::plasm_dag) fn program_binding_contract_for_source(
    state: &CompileState<'_>,
    label: &str,
    node_expr: &str,
    source: &DagNodeSource,
) -> ProgramBindingContract {
    match source {
        DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            result_shape,
            ..
        } => {
            let row_cardinality =
                if matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_)) {
                    RowCardinalityProof::StaticSingleton
                } else if matches!(kind, PlanNodeKind::Query | PlanNodeKind::Search) {
                    RowCardinalityProof::StaticPlural
                } else {
                    RowCardinalityProof::RuntimeChecked
                };
            let continuation =
                if matches!(
                    kind,
                    PlanNodeKind::Get | PlanNodeKind::Query | PlanNodeKind::Search
                ) || matches!(parsed.expr, Expr::Get(_) | Expr::Query(_) | Expr::Chain(_))
                {
                    ContinuationCapability::RelationDot {
                        segments: SegmentPolicy::MultiSegment,
                        method_invoke: true,
                    }
                } else {
                    ContinuationCapability::Terminal
                };
            let anchor = if matches!(&continuation, ContinuationCapability::Terminal) {
                ContinuationAnchor::None
            } else {
                ContinuationAnchor::RootSurface(node_expr.to_string())
            };
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: qualified_entity.clone(),
                result_shape: *result_shape,
                row_cardinality,
                continuation,
                anchor,
            }
        }
        DagNodeSource::RelationTraversal {
            qualified_entity,
            result_shape,
            plan_relation,
            expanded_plasm,
            source_label,
            ..
        } => {
            let parent = binding_contract(state, source_label)
                .map(|c| c.row_cardinality)
                .unwrap_or(RowCardinalityProof::RuntimeChecked);
            let row_cardinality = match plan_relation.cardinality {
                RelationCardinality::One => parent.after_one_cardinality_relation(),
                RelationCardinality::Many => parent.after_many_cardinality_relation(),
            };
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: qualified_entity.clone(),
                result_shape: *result_shape,
                row_cardinality,
                continuation: ContinuationCapability::RelationDot {
                    segments: SegmentPolicy::SingleSegment,
                    method_invoke: true,
                },
                anchor: ContinuationAnchor::RelationExpand(expanded_plasm.clone()),
            }
        }
        DagNodeSource::Compute {
            source,
            op: ComputeOp::Project { .. },
            schema,
            ..
        } => {
            let parent = binding_contract(state, source)
                .unwrap_or_else(|| synthetic_row_contract(source, schema));
            let anchor = match state.get(source).map(|n| &n.source) {
                Some(DagNodeSource::Surface { parsed, .. })
                    if matches!(parsed.expr, Expr::Get(_)) =>
                {
                    ContinuationAnchor::RootSurface(state.get(source).expect("source").expr.clone())
                }
                _ => ContinuationAnchor::BindingLabel,
            };
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: parent.row_entity.clone(),
                result_shape: parent.result_shape,
                row_cardinality: parent.row_cardinality,
                continuation: parent.continuation,
                anchor,
            }
        }
        DagNodeSource::Compute {
            source,
            op: ComputeOp::Limit { count },
            schema,
            ..
        } => {
            let parent = binding_contract(state, source)
                .unwrap_or_else(|| synthetic_row_contract(source, schema));
            let from_plural = matches!(
                parent.row_cardinality,
                RowCardinalityProof::StaticPlural | RowCardinalityProof::RuntimeChecked
            ) || *count > 1;
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: parent.row_entity.clone(),
                result_shape: parent.result_shape,
                row_cardinality: if *count <= 1 {
                    RowCardinalityProof::BoundedSingleton {
                        kind: BoundedSingletonKind::LimitOne,
                        from_plural_source: from_plural,
                    }
                } else {
                    RowCardinalityProof::StaticPlural
                },
                continuation: parent.continuation,
                anchor: ContinuationAnchor::BindingLabel,
            }
        }
        DagNodeSource::Compute {
            op: ComputeOp::Render { .. },
            ..
        } => ProgramBindingContract {
            label: label.to_string(),
            row_entity: QualifiedEntityKey {
                entry_id: String::new(),
                entity: String::new(),
            },
            result_shape: crate::plasm_plan::ResultShape::Single,
            row_cardinality: RowCardinalityProof::StaticSingleton,
            continuation: ContinuationCapability::RenderContentScalar,
            anchor: ContinuationAnchor::None,
        },
        DagNodeSource::Compute { schema, .. } => synthetic_terminal_contract(label, schema),
        DagNodeSource::Data(value) => {
            let singleton = matches!(
                value,
                PlanValue::Literal { value }
                    if value.as_array().is_none_or(|items| items.len() <= 1)
            );
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: QualifiedEntityKey {
                    entry_id: String::new(),
                    entity: String::new(),
                },
                result_shape: if singleton {
                    crate::plasm_plan::ResultShape::Single
                } else {
                    crate::plasm_plan::ResultShape::List
                },
                row_cardinality: if singleton {
                    RowCardinalityProof::StaticSingleton
                } else {
                    RowCardinalityProof::StaticPlural
                },
                continuation: ContinuationCapability::Terminal,
                anchor: ContinuationAnchor::None,
            }
        }
        DagNodeSource::Derive { .. } | DagNodeSource::ForEach { .. } => ProgramBindingContract {
            label: label.to_string(),
            row_entity: QualifiedEntityKey {
                entry_id: String::new(),
                entity: String::new(),
            },
            result_shape: crate::plasm_plan::ResultShape::Single,
            row_cardinality: RowCardinalityProof::RuntimeChecked,
            continuation: ContinuationCapability::Terminal,
            anchor: ContinuationAnchor::None,
        },
    }
}

pub(in crate::plasm_dag) fn synthetic_row_contract(label: &str, schema: &SyntheticResultSchema) -> ProgramBindingContract {
    ProgramBindingContract {
        label: label.to_string(),
        row_entity: QualifiedEntityKey {
            entry_id: String::new(),
            entity: schema.entity.clone().unwrap_or_default(),
        },
        result_shape: crate::plasm_plan::ResultShape::List,
        row_cardinality: RowCardinalityProof::RuntimeChecked,
        continuation: ContinuationCapability::PostfixOnly,
        anchor: ContinuationAnchor::None,
    }
}

pub(in crate::plasm_dag) fn synthetic_terminal_contract(
    label: &str,
    schema: &SyntheticResultSchema,
) -> ProgramBindingContract {
    ProgramBindingContract {
        label: label.to_string(),
        row_entity: QualifiedEntityKey {
            entry_id: String::new(),
            entity: schema.entity.clone().unwrap_or_default(),
        },
        result_shape: crate::plasm_plan::ResultShape::Single,
        row_cardinality: RowCardinalityProof::RuntimeChecked,
        continuation: ContinuationCapability::Terminal,
        anchor: ContinuationAnchor::None,
    }
}
