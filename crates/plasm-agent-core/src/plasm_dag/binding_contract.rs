//! Γ binding contracts derived from DAG nodes.

use super::prelude::*;
use super::types::{CompileState, DagNode, DagNodeSource};
use crate::program_binding::ContinuationCapability;

pub(in crate::plasm_dag) fn binding_contract(
    state: &CompileState<'_>,
    label: &str,
) -> Option<ProgramBindingContract> {
    let node = state.get(label)?;
    Some(binding_contract_for_node(state, label, node))
}

pub(in crate::plasm_dag) fn binding_contract_for_node(
    state: &CompileState<'_>,
    label: &str,
    node: &DagNode,
) -> ProgramBindingContract {
    let mut contract =
        program_binding_contract_for_source(state, label, &node.expr, &node.source, node.singleton);
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

pub(in crate::plasm_dag) fn program_binding_contract_for_source(
    state: &CompileState<'_>,
    label: &str,
    node_expr: &str,
    source: &DagNodeSource,
    explicit_singleton: bool,
) -> ProgramBindingContract {
    let value_kind = binding_value_kind(source);
    match source {
        DagNodeSource::MapBody { body, schema } => {
            if matches!(body.output, plasm_core::plasm_monad::ScopedOutput::Filter) {
                return binding_contract(state, body.parent.source.as_str())
                    .expect("predicate source contract");
            }
            let mut contract = synthetic_row_contract(label, schema);
            contract.result_shape = body.result_shape();
            contract.row_cardinality =
                crate::plasm_plan::map_body_cardinality_transfer(&body.output, || {
                    binding_contract(state, body.parent.source.as_str())
                        .map(|parent| parent.row_cardinality)
                        .unwrap_or(RowCardinalityProof::RuntimeChecked)
                });
            if let plasm_core::plasm_monad::ScopedOutput::Rows {
                entity,
                entity_authority,
                ..
            } = &body.output
            {
                contract.row_entity = QualifiedEntityKey {
                    entry_id: entity.entry_id.clone(),
                    entity: entity.entity.clone(),
                };
                if *entity_authority {
                    contract.continuation = ContinuationCapability::RelationDot {
                        segments: SegmentPolicy::MultiSegment,
                        method_invoke: true,
                    };
                    contract.anchor = ContinuationAnchor::BindingLabel;
                }
            }
            contract
        }
        DagNodeSource::Surface {
            view_singleton,
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
            let row_cardinality = if read_get || mutation_result || *view_singleton {
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
            op: op @ ComputeOp::Project { .. },
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
            let mut contract = inherit_row_preserving_contract(
                label,
                value_kind,
                &parent,
                crate::plasm_plan::compute_cardinality_transfer(op, || parent.row_cardinality),
                anchor,
            );
            contract.result_shape = crate::plasm_plan::compute_result_shape(op, explicit_singleton);
            contract
        }
        DagNodeSource::Compute {
            source,
            op: op @ ComputeOp::Limit { .. },
            schema,
            ..
        } => {
            let parent = binding_contract(state, source)
                .unwrap_or_else(|| synthetic_row_contract(source, schema));
            let mut contract = inherit_row_preserving_contract(
                label,
                value_kind,
                &parent,
                crate::plasm_plan::compute_cardinality_transfer(op, || parent.row_cardinality),
                ContinuationAnchor::BindingLabel,
            );
            contract.result_shape = crate::plasm_plan::compute_result_shape(op, explicit_singleton);
            contract
        }
        DagNodeSource::Compute {
            source,
            op:
                op @ (ComputeOp::Filter { .. }
                | ComputeOp::Sort { .. }
                | ComputeOp::DedupeBy { .. }
                | ComputeOp::With { .. }),
            schema,
            ..
        } => {
            let parent = binding_contract(state, source)
                .unwrap_or_else(|| synthetic_row_contract(source, schema));
            let mut contract = inherit_row_preserving_contract(
                label,
                value_kind,
                &parent,
                crate::plasm_plan::compute_cardinality_transfer(op, || parent.row_cardinality),
                ContinuationAnchor::BindingLabel,
            );
            contract.result_shape = crate::plasm_plan::compute_result_shape(op, explicit_singleton);
            contract
        }
        DagNodeSource::Compute {
            source,
            op: op @ ComputeOp::Union { other },
            schema,
            ..
        } => {
            let left = binding_contract(state, source);
            let right = binding_contract(state, other.as_str());
            let authority = |contract: &ProgramBindingContract| {
                contract.supports_method_invoke() && contract.anchor.is_present()
            };
            let left_owner = left
                .as_ref()
                .filter(|c| authority(c))
                .map(|c| &c.row_entity);
            let right_owner = right
                .as_ref()
                .filter(|c| authority(c))
                .map(|c| &c.row_entity);
            if let Some(left_contract) = left
                .as_ref()
                .filter(|_| op.preserved_identity(left_owner, right_owner).is_some())
            {
                let mut contract = inherit_row_preserving_contract(
                    label,
                    value_kind,
                    left_contract,
                    crate::plasm_plan::compute_cardinality_transfer(op, || {
                        left_contract.row_cardinality
                    }),
                    ContinuationAnchor::BindingLabel,
                );
                contract.result_shape =
                    crate::plasm_plan::compute_result_shape(op, explicit_singleton);
                contract
            } else {
                let mut contract = synthetic_terminal_contract(label, schema);
                contract.row_cardinality =
                    crate::plasm_plan::compute_cardinality_transfer(op, || {
                        RowCardinalityProof::RuntimeChecked
                    });
                contract.result_shape =
                    crate::plasm_plan::compute_result_shape(op, explicit_singleton);
                contract
            }
        }
        DagNodeSource::Compute {
            source,
            op: op @ ComputeOp::Render { .. },
            ..
        } => {
            let parent_card = crate::plasm_plan::compute_cardinality_transfer(op, || {
                binding_contract(state, source)
                    .map(|p| p.row_cardinality)
                    .unwrap_or(RowCardinalityProof::RuntimeChecked)
            });
            ProgramBindingContract {
                label: label.to_string(),
                row_entity: QualifiedEntityKey {
                    entry_id: String::new(),
                    entity: String::new(),
                },
                result_shape: crate::plasm_plan::compute_result_shape(op, explicit_singleton),
                row_cardinality: parent_card,
                value_kind,
                continuation: ContinuationCapability::Terminal,
                anchor: ContinuationAnchor::None,
            }
        }
        DagNodeSource::Compute {
            op: op @ ComputeOp::MergeBranches { .. },
            schema,
            ..
        } => {
            let mut contract = synthetic_terminal_contract(label, schema);
            contract.row_cardinality = crate::plasm_plan::compute_cardinality_transfer(op, || {
                RowCardinalityProof::RuntimeChecked
            });
            contract.result_shape = crate::plasm_plan::compute_result_shape(op, explicit_singleton);
            contract
        }
        DagNodeSource::Compute {
            source,
            op: op @ ComputeOp::Python { output_type, .. },
            schema,
            ..
        } => {
            let mut contract = synthetic_terminal_contract(label, schema);
            if !output_type.is_non_null_record() {
                contract.value_kind = BindingValueKind::ScalarCell;
            }
            contract.row_cardinality = crate::plasm_plan::compute_cardinality_transfer(op, || {
                binding_contract(state, source)
                    .map(|p| p.row_cardinality)
                    .unwrap_or(RowCardinalityProof::RuntimeChecked)
            });
            contract.result_shape = crate::plasm_plan::compute_result_shape(op, explicit_singleton);
            contract
        }
        DagNodeSource::Compute {
            source, op, schema, ..
        } => {
            let mut contract = synthetic_terminal_contract(label, schema);
            contract.row_cardinality = crate::plasm_plan::compute_cardinality_transfer(op, || {
                binding_contract(state, source)
                    .map(|parent| parent.row_cardinality)
                    .unwrap_or(RowCardinalityProof::RuntimeChecked)
            });
            contract.result_shape = crate::plasm_plan::compute_result_shape(op, explicit_singleton);
            contract
        }
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
        DagNodeSource::ForEach {
            qualified_entity,
            effect_kind:
                PlanNodeKind::Get | PlanNodeKind::Query | PlanNodeKind::Search | PlanNodeKind::Create,
            ..
        } => ProgramBindingContract {
            label: label.to_string(),
            row_entity: qualified_entity.clone(),
            result_shape: crate::plasm_plan::ResultShape::List,
            row_cardinality: RowCardinalityProof::StaticPlural,
            value_kind,
            continuation: ContinuationCapability::RelationDot {
                segments: SegmentPolicy::SingleSegment,
                method_invoke: true,
            },
            anchor: ContinuationAnchor::BindingLabel,
        },
        DagNodeSource::Derive {
            value_type: Some(schema),
            ..
        } => {
            let schema = plasm_core::plasm_monad::SyntheticResultSchema::for_value(schema.clone())
                .expect("validated value contract");
            let mut contract = synthetic_row_contract(label, &schema);
            if !matches!(&source, DagNodeSource::Derive { value_type: Some(t), .. } if t.is_non_null_record())
            {
                contract.value_kind = BindingValueKind::ScalarCell;
            }
            if state.get(label).is_some_and(|node| node.singleton) {
                contract.row_cardinality = RowCardinalityProof::StaticSingleton;
                contract.result_shape = crate::plasm_plan::ResultShape::Single;
            }
            contract
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
        PlanValue::Literal { value: lit } => match lit.value() {
            plasm_core::Value::Null
            | plasm_core::Value::Bool(_)
            | plasm_core::Value::Unsigned(_)
            | plasm_core::Value::Integer(_)
            | plasm_core::Value::Float(_)
            | plasm_core::Value::Money(_)
            | plasm_core::Value::String(_) => DataLiteralShape {
                result_shape: crate::plasm_plan::ResultShape::Single,
                row_cardinality: RowCardinalityProof::StaticSingleton,
                value_kind: BindingValueKind::ScalarCell,
            },
            plasm_core::Value::Array(items) if items.len() <= 1 => DataLiteralShape {
                result_shape: crate::plasm_plan::ResultShape::Single,
                row_cardinality: RowCardinalityProof::StaticSingleton,
                value_kind: BindingValueKind::EntityRow,
            },
            plasm_core::Value::Array(_) => DataLiteralShape {
                result_shape: crate::plasm_plan::ResultShape::List,
                row_cardinality: RowCardinalityProof::StaticPlural,
                value_kind: BindingValueKind::EntityRow,
            },
            // Objects are non-array literals → prior `as_array().is_none_or(…)` treated them
            // as singleton rows, not scalar cells.
            _ => DataLiteralShape {
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
        // RA-10 preserves existing continuation evidence; it cannot manufacture
        // an anchor after a terminal row-plane operation (RA-7 / RA-14).
        anchor: if parent.anchor.is_present() {
            anchor
        } else {
            ContinuationAnchor::None
        },
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
    use crate::plasm_dag::types::{DagNode, DagNodeSource};
    use crate::plasm_plan::{ComputeOp, SyntheticResultSchema};

    #[test]
    fn compute_binding_shape_matches_emitted_plan_shape() {
        let pipeline = PromptPipelineConfig::default();
        let state = CompileState::new(&pipeline, None);
        for singleton in [false, true] {
            let node = DagNode {
                id: "render".into(),
                expr: "render".into(),
                source: DagNodeSource::Compute {
                    source: "rows".into(),
                    op: ComputeOp::Render {
                        columns: vec![],
                        template: String::new(),
                        column_aliases: Default::default(),
                        render_bindings: vec![],
                    },
                    schema: SyntheticResultSchema {
                        optional_fields: Default::default(),
                        entity: None,
                        fields: vec![],
                    },
                    collection_alias: None,
                },
                singleton,
                page_size: None,
            };
            let binding = binding_contract_for_node(&state, "render", &node);
            let emitted = super::super::plan_serialize::lower_plan_node(&node).expect("emit");
            assert_eq!(binding.result_shape, emitted.result_shape);
        }
    }

    #[test]
    fn row_preserving_contract_never_creates_continuation_evidence() {
        let schema = SyntheticResultSchema {
            optional_fields: Default::default(),
            entity: Some("Item".into()),
            fields: vec![],
        };
        for continuation in [
            ContinuationCapability::Terminal,
            ContinuationCapability::PostfixOnly,
            ContinuationCapability::RelationDot {
                segments: SegmentPolicy::SingleSegment,
                method_invoke: true,
            },
        ] {
            for anchor in [
                ContinuationAnchor::None,
                ContinuationAnchor::BindingLabel,
                ContinuationAnchor::RootSurface("e2".into()),
            ] {
                let mut parent = synthetic_terminal_contract("parent", &schema);
                parent.continuation = continuation;
                parent.anchor = anchor;
                for cardinality in [
                    RowCardinalityProof::StaticPlural,
                    RowCardinalityProof::BoundedSingleton {
                        kind: BoundedSingletonKind::LimitOne,
                        from_plural_source: true,
                    },
                ] {
                    let child = inherit_row_preserving_contract(
                        "child",
                        BindingValueKind::EntityRow,
                        &parent,
                        cardinality,
                        ContinuationAnchor::BindingLabel,
                    );
                    assert_eq!(child.continuation, parent.continuation);
                    assert_eq!(child.anchor.is_present(), parent.anchor.is_present());
                    assert_eq!(child.row_entity, parent.row_entity);
                    assert_eq!(child.row_cardinality, cardinality);
                }
            }
        }
    }

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
                    optional_fields: Default::default(),
                    entity: None,
                    fields: Vec::new(),
                },
                collection_alias: None,
            }),
            BindingValueKind::EntityRow
        );
        assert_eq!(
            binding_value_kind(&DagNodeSource::Data(PlanValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::from_wire(serde_json::json!(
                    "hi"
                ))
                .expect("literal data"),
            })),
            BindingValueKind::ScalarCell
        );
        assert_eq!(
            binding_value_kind(&DagNodeSource::Data(PlanValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::from_wire(
                    serde_json::json!({"k": 1})
                )
                .expect("literal data"),
            })),
            BindingValueKind::EntityRow
        );
        assert_eq!(
            binding_value_kind(&DagNodeSource::Derive {
                value_type: None,
                source: "src".into(),
                value: PlanValue::Literal {
                    value: plasm_core::operand_binding::ResolvedValue::from_wire(
                        serde_json::json!("x")
                    )
                    .expect("literal data"),
                },
                inputs: Vec::new(),
            }),
            BindingValueKind::EntityRow
        );
    }

    #[test]
    fn data_literal_shape_aligns_cardinality_and_value_kind() {
        let s = data_literal_shape(&PlanValue::Literal {
            value: plasm_core::operand_binding::ResolvedValue::from_wire(serde_json::json!("cell"))
                .expect("literal data"),
        });
        assert_eq!(s.value_kind, BindingValueKind::ScalarCell);
        assert!(matches!(
            s.row_cardinality,
            RowCardinalityProof::StaticSingleton
        ));

        let arr1 = data_literal_shape(&PlanValue::Literal {
            value: plasm_core::operand_binding::ResolvedValue::from_wire(serde_json::json!([1]))
                .expect("literal data"),
        });
        assert_eq!(arr1.value_kind, BindingValueKind::EntityRow);
        assert!(matches!(
            arr1.row_cardinality,
            RowCardinalityProof::StaticSingleton
        ));

        let arr_many = data_literal_shape(&PlanValue::Literal {
            value: plasm_core::operand_binding::ResolvedValue::from_wire(serde_json::json!([1, 2]))
                .expect("literal data"),
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
            view_singleton: false,
            parsed: create_parsed,
            kind: PlanNodeKind::Create,
            qualified_entity: qe.clone(),
            effect_class: EffectClass::Write,
            result_shape: crate::plasm_plan::ResultShape::MutationResult,
            uses_result: Vec::new(),
        };
        let create_c =
            program_binding_contract_for_source(&state, "created", "e1.m1()", &create_src, false);
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
            view_singleton: false,
            parsed: ack_parsed,
            kind: PlanNodeKind::Action,
            qualified_entity: qe,
            effect_class: EffectClass::SideEffect,
            result_shape: crate::plasm_plan::ResultShape::SideEffectAck,
            uses_result: Vec::new(),
        };
        let ack_c = program_binding_contract_for_source(&state, "done", "e1.m2()", &ack_src, false);
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
