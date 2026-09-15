//! Γ binding contracts derived from DAG nodes.

use super::prelude::*;
use super::types::{BindingContractSource, CompileState, DagNode, DagNodeSource};
use crate::plasm_dag_surface_guards::{
    content_reference_error, path_is_render_content_stitch, ContentReferenceSite,
};
use crate::program_binding::ContinuationCapability;

pub(in crate::plasm_dag) fn binding_contract(
    state: &CompileState<'_>,
    label: &str,
) -> Option<ProgramBindingContract> {
    let node = state.get(label)?;
    Some(binding_contract_for_node(state, label, node))
}

/// Reject `label.content` when `label` is a scalar cell that is not a row-to-text render binding.
pub(in crate::plasm_dag) fn reject_illegal_content_stitch(
    state: &CompileState<'_>,
    node: &str,
    path: &[impl AsRef<str>],
) -> Result<(), String> {
    if !path_is_render_content_stitch(path) {
        return Ok(());
    }
    let Some(contract) = binding_contract(state, node) else {
        return Ok(());
    };
    if !matches!(
        contract.continuation,
        ContinuationCapability::RenderContentScalar
    ) && contract.is_scalar_cell()
    {
        return Err(content_reference_error(
            node,
            ContentReferenceSite::Continuation,
            contract.continuation,
        ));
    }
    Ok(())
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
    let value_kind = binding_value_kind(source);
    match source {
        DagNodeSource::Surface {
            parsed,
            kind,
            qualified_entity,
            result_shape,
            ..
        } => {
            // MutationResult writers decode entity rows → StaticSingleton + RelationDot (PLP-1).
            let mutation_result =
                matches!(result_shape, crate::plasm_plan::ResultShape::MutationResult);
            let read_get = matches!(kind, PlanNodeKind::Get) || matches!(parsed.expr, Expr::Get(_));
            let read_list = matches!(kind, PlanNodeKind::Query | PlanNodeKind::Search);
            let row_surface = read_get
                || read_list
                || mutation_result
                || matches!(parsed.expr, Expr::Query(_) | Expr::Chain(_));
            let row_cardinality = if read_get || mutation_result {
                RowCardinalityProof::StaticSingleton
            } else if read_list {
                RowCardinalityProof::StaticPlural
            } else {
                RowCardinalityProof::RuntimeChecked
            };
            let continuation = if row_surface {
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
                value_kind,
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
                value_kind,
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
            inherit_row_preserving_contract(
                label,
                value_kind,
                &parent,
                parent.row_cardinality,
                anchor,
            )
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
            let row_cardinality = if *count <= 1 {
                RowCardinalityProof::BoundedSingleton {
                    kind: BoundedSingletonKind::LimitOne,
                    from_plural_source: from_plural,
                }
            } else {
                RowCardinalityProof::StaticPlural
            };
            inherit_row_preserving_contract(
                label,
                value_kind,
                &parent,
                row_cardinality,
                ContinuationAnchor::BindingLabel,
            )
        }
        DagNodeSource::Compute {
            source,
            op:
                ComputeOp::Filter { .. }
                | ComputeOp::Sort { .. }
                | ComputeOp::DedupeBy { .. }
                | ComputeOp::With { .. },
            schema,
            ..
        } => {
            let parent = binding_contract(state, source)
                .unwrap_or_else(|| synthetic_row_contract(source, schema));
            inherit_row_preserving_contract(
                label,
                value_kind,
                &parent,
                parent.row_cardinality,
                ContinuationAnchor::BindingLabel,
            )
        }
        DagNodeSource::Compute {
            source,
            op: ComputeOp::Render { .. },
            ..
        } => {
            let parent_card = binding_contract(state, source)
                .map(|p| p.row_cardinality)
                .unwrap_or(RowCardinalityProof::RuntimeChecked);
            let result_shape = if parent_card.permits_scalar_field_extract() {
                crate::plasm_plan::ResultShape::Single
            } else {
                crate::plasm_plan::ResultShape::List
            };
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: QualifiedEntityKey {
                    entry_id: String::new(),
                    entity: String::new(),
                },
                result_shape,
                row_cardinality: parent_card,
                value_kind,
                continuation: ContinuationCapability::RenderContentScalar,
                anchor: ContinuationAnchor::None,
            }
        }
        DagNodeSource::Compute { schema, .. } => synthetic_terminal_contract(label, schema),
        DagNodeSource::Data(value) => {
            let shape = data_literal_shape(value);
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: QualifiedEntityKey {
                    entry_id: String::new(),
                    entity: String::new(),
                },
                result_shape: shape.result_shape,
                row_cardinality: shape.row_cardinality,
                value_kind,
                continuation: ContinuationCapability::Terminal,
                anchor: ContinuationAnchor::None,
            }
        }
        DagNodeSource::Derive { .. }
        | DagNodeSource::ForEach { .. }
        | DagNodeSource::IterateUntil { .. } => ProgramBindingContract {
            label: label.to_string(),
            row_entity: QualifiedEntityKey {
                entry_id: String::new(),
                entity: String::new(),
            },
            result_shape: crate::plasm_plan::ResultShape::Single,
            row_cardinality: RowCardinalityProof::RuntimeChecked,
            value_kind,
            continuation: ContinuationCapability::Terminal,
            anchor: ContinuationAnchor::None,
        },
        DagNodeSource::ScalarExtract { .. } => ProgramBindingContract {
            label: label.to_string(),
            row_entity: QualifiedEntityKey {
                entry_id: String::new(),
                entity: String::new(),
            },
            result_shape: crate::plasm_plan::ResultShape::Single,
            row_cardinality: RowCardinalityProof::StaticSingleton,
            value_kind,
            continuation: ContinuationCapability::Terminal,
            anchor: ContinuationAnchor::None,
        },
    }
}

/// PLP-1 table: which DAG sources denote a proven scalar cell vs an entity row.
fn binding_value_kind(source: &DagNodeSource) -> BindingValueKind {
    match source {
        DagNodeSource::ScalarExtract { .. } => BindingValueKind::ScalarCell,
        DagNodeSource::Data(value) => data_literal_shape(value).value_kind,
        _ => BindingValueKind::EntityRow,
    }
}

struct DataLiteralShape {
    result_shape: crate::plasm_plan::ResultShape,
    row_cardinality: RowCardinalityProof,
    value_kind: BindingValueKind,
}

/// Single classifier for `DagNodeSource::Data` — cardinality and value-kind stay aligned.
fn data_literal_shape(value: &PlanValue) -> DataLiteralShape {
    match value {
        PlanValue::Literal { value: lit } => match lit {
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => DataLiteralShape {
                result_shape: crate::plasm_plan::ResultShape::Single,
                row_cardinality: RowCardinalityProof::StaticSingleton,
                value_kind: BindingValueKind::ScalarCell,
            },
            serde_json::Value::Array(items) if items.len() <= 1 => DataLiteralShape {
                result_shape: crate::plasm_plan::ResultShape::Single,
                row_cardinality: RowCardinalityProof::StaticSingleton,
                value_kind: BindingValueKind::EntityRow,
            },
            serde_json::Value::Array(_) => DataLiteralShape {
                result_shape: crate::plasm_plan::ResultShape::List,
                row_cardinality: RowCardinalityProof::StaticPlural,
                value_kind: BindingValueKind::EntityRow,
            },
            // Objects are non-array literals → prior `as_array().is_none_or(…)` treated them
            // as singleton rows, not scalar cells.
            serde_json::Value::Object(_) => DataLiteralShape {
                result_shape: crate::plasm_plan::ResultShape::Single,
                row_cardinality: RowCardinalityProof::StaticSingleton,
                value_kind: BindingValueKind::EntityRow,
            },
        },
        _ => DataLiteralShape {
            result_shape: crate::plasm_plan::ResultShape::List,
            row_cardinality: RowCardinalityProof::StaticPlural,
            value_kind: BindingValueKind::EntityRow,
        },
    }
}

fn inherit_row_preserving_contract(
    label: &str,
    value_kind: BindingValueKind,
    parent: &ProgramBindingContract,
    row_cardinality: RowCardinalityProof,
    anchor: ContinuationAnchor,
) -> ProgramBindingContract {
    ProgramBindingContract {
        label: label.to_string(),
        row_entity: parent.row_entity.clone(),
        result_shape: parent.result_shape,
        row_cardinality,
        value_kind,
        continuation: parent.continuation,
        anchor,
    }
}

pub(in crate::plasm_dag) fn synthetic_row_contract(
    label: &str,
    schema: &SyntheticResultSchema,
) -> ProgramBindingContract {
    ProgramBindingContract {
        label: label.to_string(),
        row_entity: QualifiedEntityKey {
            entry_id: String::new(),
            entity: schema.entity.clone().unwrap_or_default(),
        },
        result_shape: crate::plasm_plan::ResultShape::List,
        row_cardinality: RowCardinalityProof::RuntimeChecked,
        value_kind: BindingValueKind::EntityRow,
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
        value_kind: BindingValueKind::EntityRow,
        continuation: ContinuationCapability::Terminal,
        anchor: ContinuationAnchor::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plasm_dag::types::DagNodeSource;
    use crate::plasm_plan::{ComputeOp, SyntheticResultSchema};

    #[test]
    fn binding_value_kind_table() {
        assert_eq!(
            binding_value_kind(&DagNodeSource::ScalarExtract {
                source: "peer".into(),
                wire: "title".into(),
            }),
            BindingValueKind::ScalarCell
        );
        assert_eq!(
            binding_value_kind(&DagNodeSource::Compute {
                source: "src".into(),
                op: ComputeOp::Render {
                    columns: Vec::new(),
                    template: String::new(),
                    column_aliases: Default::default(),
                    render_bindings: Vec::new(),
                },
                schema: SyntheticResultSchema {
                    entity: None,
                    fields: Vec::new(),
                },
                collection_alias: None,
            }),
            BindingValueKind::EntityRow
        );
        assert_eq!(
            binding_value_kind(&DagNodeSource::Data(PlanValue::Literal {
                value: serde_json::json!("hi"),
            })),
            BindingValueKind::ScalarCell
        );
        assert_eq!(
            binding_value_kind(&DagNodeSource::Data(PlanValue::Literal {
                value: serde_json::json!({"k": 1}),
            })),
            BindingValueKind::EntityRow
        );
        assert_eq!(
            binding_value_kind(&DagNodeSource::Derive {
                source: "src".into(),
                value: PlanValue::Literal {
                    value: serde_json::json!("x"),
                },
                inputs: Vec::new(),
            }),
            BindingValueKind::EntityRow
        );
    }

    #[test]
    fn data_literal_shape_aligns_cardinality_and_value_kind() {
        let s = data_literal_shape(&PlanValue::Literal {
            value: serde_json::json!("cell"),
        });
        assert_eq!(s.value_kind, BindingValueKind::ScalarCell);
        assert!(matches!(
            s.row_cardinality,
            RowCardinalityProof::StaticSingleton
        ));

        let arr1 = data_literal_shape(&PlanValue::Literal {
            value: serde_json::json!([1]),
        });
        assert_eq!(arr1.value_kind, BindingValueKind::EntityRow);
        assert!(matches!(
            arr1.row_cardinality,
            RowCardinalityProof::StaticSingleton
        ));

        let arr_many = data_literal_shape(&PlanValue::Literal {
            value: serde_json::json!([1, 2]),
        });
        assert_eq!(arr_many.value_kind, BindingValueKind::EntityRow);
        assert!(matches!(
            arr_many.row_cardinality,
            RowCardinalityProof::StaticPlural
        ));
    }

    #[test]
    fn mutation_result_surface_is_static_singleton_relation_dot() {
        use plasm_core::expr::{CreateExpr, InvokeExpr};
        use plasm_core::expr_parser::ParsedExpr;
        use plasm_core::{CatalogEntryStamp, InvokeInputPayload};

        let pipeline = PromptPipelineConfig::default();
        let state = CompileState::new(&pipeline, None);
        let qe = QualifiedEntityKey {
            entry_id: "test".into(),
            entity: "AuthSession".into(),
        };

        let create_parsed = ParsedExpr::from_expr(Expr::Create(CreateExpr {
            capability: "create".into(),
            entity: "AuthSession".into(),
            input: InvokeInputPayload::Raw(plasm_core::Value::Object(Default::default())),
            catalog_entry_id: CatalogEntryStamp::none(),
            dotted_receiver: None,
        }));
        let create_src = DagNodeSource::Surface {
            parsed: create_parsed,
            kind: PlanNodeKind::Create,
            qualified_entity: qe.clone(),
            effect_class: EffectClass::Write,
            result_shape: crate::plasm_plan::ResultShape::MutationResult,
            uses_result: Vec::new(),
        };
        let create_c =
            program_binding_contract_for_source(&state, "created", "e1.m1()", &create_src);
        assert!(matches!(
            create_c.row_cardinality,
            RowCardinalityProof::StaticSingleton
        ));
        assert!(matches!(
            create_c.continuation,
            ContinuationCapability::RelationDot { .. }
        ));

        let ack_parsed = ParsedExpr::from_expr(Expr::Invoke(InvokeExpr {
            capability: "logout".into(),
            target: Ref::new("AuthSession", ""),
            input: None,
            catalog_entry_id: CatalogEntryStamp::none(),
        }));
        let ack_src = DagNodeSource::Surface {
            parsed: ack_parsed,
            kind: PlanNodeKind::Action,
            qualified_entity: qe,
            effect_class: EffectClass::SideEffect,
            result_shape: crate::plasm_plan::ResultShape::SideEffectAck,
            uses_result: Vec::new(),
        };
        let ack_c = program_binding_contract_for_source(&state, "done", "e1.m2()", &ack_src);
        assert!(matches!(
            ack_c.row_cardinality,
            RowCardinalityProof::RuntimeChecked
        ));
        assert!(matches!(
            ack_c.continuation,
            ContinuationCapability::Terminal
        ));
    }
}
