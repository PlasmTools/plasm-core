//! Staged plan input row materialization and cardinality gates.

use super::super::*;
use super::hole_paths::NodeInputHoleIndex;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum InputRowsError {
    #[error(transparent)]
    Identifier(#[from] crate::plasm_plan::PlanAtomError),
    #[error("materialized node `{node_id}` for alias `{alias}` is unavailable")]
    NodeUnavailable { node_id: String, alias: String },
    #[error("materialized node `{node_id}` has no inline rows")]
    RowsNotInline { node_id: String },
    #[error("materialized node `{node_id}` has no acknowledgement")]
    AcknowledgementMissing { node_id: String },
    #[error("materialized scalar row has no `value` column")]
    ScalarValueMissing,
    #[error("materialized input value shape is invalid: {0}")]
    ValueShapeInvalid(#[from] crate::plasm_plan_run::orchestrator::MaterializedValueShapeError),
    #[error("materialized collection is incomplete: {0}")]
    IncompleteCollection(#[from] plasm_core::collection_codec::CollectionFault),
    #[error(
        "singleton input `{alias}` from node `{node_id}` expected one row for {context}, found {rows}. This is not a Plasm syntax error; {remedy}",
        rows = cardinality_rows(*.actual),
        remedy = cardinality_remedy(*.actual)
    )]
    Cardinality {
        node_id: String,
        alias: String,
        actual: usize,
        context: String,
    },
}

fn cardinality_rows(actual: usize) -> String {
    if actual == 0 {
        "zero rows".into()
    } else {
        format!("{actual} rows")
    }
}

fn cardinality_remedy(actual: usize) -> &'static str {
    if actual == 0 {
        "branch around empty results before consuming a singleton."
    } else {
        "make the source unique before consuming it as a singleton; the one-row contract is checked at runtime."
    }
}

fn input_cardinality_proof_label(proof: crate::plasm_plan::InputCardinalityProof) -> &'static str {
    match proof {
        crate::plasm_plan::InputCardinalityProof::Acknowledgement => "acknowledgement",
        crate::plasm_plan::InputCardinalityProof::Collection => "collection",
        crate::plasm_plan::InputCardinalityProof::StaticSingleton => "static singleton",
        crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton => {
            "runtime-checked singleton"
        }
    }
}

impl From<InputRowsError> for plasm_runtime::ExecutionFailure {
    fn from(error: InputRowsError) -> Self {
        if let InputRowsError::IncompleteCollection(fault) = error {
            return fault.into();
        }
        let code = match &error {
            InputRowsError::Identifier(_) => "plan_identifier_invalid",
            InputRowsError::NodeUnavailable { .. } => "plan_input_not_materialized",
            InputRowsError::RowsNotInline { .. } => "plan_input_not_inline",
            InputRowsError::AcknowledgementMissing { .. } => "plan_acknowledgement_missing",
            InputRowsError::ScalarValueMissing => "plan_scalar_value_missing",
            InputRowsError::ValueShapeInvalid(_) => "plan_input_shape_invalid",
            InputRowsError::IncompleteCollection(_) => unreachable!("handled above"),
            InputRowsError::Cardinality { .. } => "plan_input_cardinality_invalid",
        };
        Self::new(
            plasm_runtime::FailureCause::Program,
            code,
            error.to_string(),
        )
    }
}

fn binding_value(
    mat: &MaterializedNode,
    row: &plasm_core::ValueRow,
    index: usize,
) -> Result<plasm_core::Value, InputRowsError> {
    match mat.value_shape_at(index)? {
        MaterializedValueShape::ScalarColumn => row
            .get("value")
            .cloned()
            .ok_or(InputRowsError::ScalarValueMissing),
        MaterializedValueShape::Record => Ok(row.clone().into_value()),
    }
}

pub(crate) fn materialized_input_row_from_mat(
    node: PlanNodeId,
    mat: &MaterializedNode,
    proof: crate::plasm_plan::InputCardinalityProof,
) -> Result<MaterializedInputRow, InputRowsError> {
    if proof == crate::plasm_plan::InputCardinalityProof::Acknowledgement {
        if mat.result.operations.is_empty() {
            return Err(InputRowsError::AcknowledgementMissing {
                node_id: node.as_str().to_owned(),
            });
        }
        let completed: usize = mat
            .result
            .operations
            .entries()
            .iter()
            .map(|a| a.completed)
            .sum();
        let failed: usize = mat
            .result
            .operations
            .entries()
            .iter()
            .map(|a| a.failed)
            .sum();
        let row: plasm_core::ValueRow = [
            ("completed".into(), plasm_core::Value::from(completed)),
            ("failed".into(), plasm_core::Value::from(failed)),
        ]
        .into_iter()
        .collect();
        return Ok(MaterializedInputRow {
            optional_fields: Default::default(),
            value_projection: Some(vec!["completed".into(), "failed".into()]),
            node,
            qualified_entity: mat.qualified_entity.clone(),
            id_field: String::new(),
            proof,
            row: row.clone().into_value(),
            rows: vec![row],
            row_identity: None,
            row_identities: vec![None],
        });
    }
    let inline = mat
        .row_source
        .inline_rows()
        .ok_or_else(|| InputRowsError::RowsNotInline {
            node_id: node.as_str().to_owned(),
        })?;
    let collection = proof == crate::plasm_plan::InputCardinalityProof::Collection;
    if collection {
        crate::python_compute::require_complete_collection(&mat.result)?;
    }
    if inline.is_empty() && !collection {
        return Err(InputRowsError::Cardinality {
            node_id: node.as_str().to_owned(),
            alias: node.as_str().to_owned(),
            actual: 0,
            context: "input materialization".to_owned(),
        });
    }
    let mut rows = Vec::with_capacity(inline.len());
    let mut row_identities = Vec::with_capacity(inline.len());
    for (idx, row) in inline.iter().enumerate() {
        let ident = mat.row_identities.get(idx).cloned().flatten();
        rows.push(
            crate::plasm_plan_run::row_values::augment_row_with_identity(row, ident.as_ref()),
        );
        row_identities.push(ident);
    }
    Ok(MaterializedInputRow {
        optional_fields: mat.optional_fields.clone(),
        value_projection: mat.projection.clone(),
        node,
        qualified_entity: mat.qualified_entity.clone(),
        id_field: "id".to_string(),
        proof,
        row: if collection {
            plasm_core::Value::Array(
                rows.iter()
                    .enumerate()
                    .map(|(i, row)| binding_value(mat, row, i))
                    .collect::<Result<_, _>>()?,
            )
        } else {
            binding_value(mat, &rows[0], 0)?
        },
        rows,
        row_identity: row_identities.first().cloned().flatten(),
        row_identities,
    })
}

pub(crate) fn materialized_singleton_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    inputs: &[ValidatedPlanDataInput],
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, InputRowsError> {
    let mut out = BTreeMap::new();
    for input in inputs {
        let node = input.node.clone();
        let alias = input.alias.clone();
        let mat = materialized
            .get(&node)
            .ok_or_else(|| InputRowsError::NodeUnavailable {
                node_id: node.as_str().to_owned(),
                alias: alias.as_str().to_owned(),
            })?;
        if !matches!(
            input.proof,
            crate::plasm_plan::InputCardinalityProof::Collection
                | crate::plasm_plan::InputCardinalityProof::Acknowledgement
        ) && mat.inline_row_count() != 1
        {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                mat.inline_row_count(),
                format!("{} broadcast", input_cardinality_proof_label(input.proof)).as_str(),
            ));
        }
        out.insert(
            input.alias.clone(),
            materialized_input_row_from_mat(input.node.clone(), mat, input.proof)?,
        );
    }
    Ok(out)
}

pub(crate) fn materialized_result_use_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
    template: Option<&ValidatedPlanExprTemplate>,
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, InputRowsError> {
    let holes = template.map(|t| NodeInputHoleIndex::from_template_expr(&t.expr));
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized
            .get(&node)
            .ok_or_else(|| InputRowsError::NodeUnavailable {
                node_id: node.as_str().to_owned(),
                alias: alias.as_str().to_owned(),
            })?;
        let row_count = mat.inline_row_count();
        let needs_singleton = holes
            .as_ref()
            .map(|h| h.needs_singleton_row(&alias))
            .unwrap_or(true);
        if row_count == 0 {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                row_count,
                "staged expression rendering",
            ));
        }
        if needs_singleton && row_count != 1 {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                row_count,
                "staged expression rendering",
            ));
        }
        out.insert(
            alias,
            materialized_input_row_from_mat(
                node,
                mat,
                crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
            )?,
        );
    }
    Ok(out)
}

pub(crate) fn singleton_input_row_count_error(
    node: &str,
    alias: &str,
    row_count: usize,
    context: &str,
) -> InputRowsError {
    InputRowsError::Cardinality {
        node_id: node.to_owned(),
        alias: alias.to_owned(),
        actual: row_count,
        context: context.to_owned(),
    }
}

pub(crate) fn materialized_result_use_inputs_with_source_row(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
    source_node: &PlanNodeId,
    source_row: &plasm_core::ValueRow,
    source_row_identity: Option<plasm_core::RowIdentity>,
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, InputRowsError> {
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized
            .get(&node)
            .ok_or_else(|| InputRowsError::NodeUnavailable {
                node_id: node.as_str().to_owned(),
                alias: alias.as_str().to_owned(),
            })?;
        let input_row = if node == *source_node {
            let row = crate::plasm_plan_run::row_values::augment_row_with_identity(
                source_row,
                source_row_identity.as_ref(),
            );
            MaterializedInputRow {
                optional_fields: mat.optional_fields.clone(),
                value_projection: mat.projection.clone(),
                node,
                qualified_entity: mat.qualified_entity.clone(),
                id_field: "id".to_string(),
                proof: crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
                rows: vec![row.clone()],
                row: row.into_value(),
                row_identity: source_row_identity.clone(),
                row_identities: vec![source_row_identity.clone()],
            }
        } else {
            if mat.inline_row_count() != 1 {
                return Err(singleton_input_row_count_error(
                    node.as_str(),
                    alias.as_str(),
                    mat.inline_row_count(),
                    "staged expression rendering",
                ));
            }
            materialized_input_row_from_mat(
                node,
                mat,
                crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
            )?
        };
        out.insert(alias, input_row);
    }
    Ok(out)
}

/// Whole-row values expose precisely the declared columns. Field references and
/// receiver dispatch retain their separate identity-bearing input representation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ValueProjectionError {
    #[error("materialized row is missing projected field `{field}`")]
    FieldMissing { field: String },
}

pub(super) fn project_value_rows(
    value: &plasm_core::Value,
    fields: &[String],
    optional_fields: &std::collections::BTreeSet<String>,
) -> Result<plasm_core::Value, ValueProjectionError> {
    fn row(
        value: &plasm_core::Value,
        fields: &[String],
        optional_fields: &std::collections::BTreeSet<String>,
    ) -> Result<plasm_core::Value, ValueProjectionError> {
        let mut projected = indexmap::IndexMap::new();
        for field in fields {
            if value.get(field).is_none() && optional_fields.contains(field) {
                continue;
            }
            let value = value
                .get(field)
                .ok_or_else(|| ValueProjectionError::FieldMissing {
                    field: field.clone(),
                })?;
            projected.insert(field.clone(), value.clone());
        }
        Ok(plasm_core::Value::Object(projected))
    }
    match value {
        plasm_core::Value::Array(rows) => rows
            .iter()
            .map(|value| row(value, fields, optional_fields))
            .collect::<Result<Vec<_>, _>>()
            .map(plasm_core::Value::Array),
        _ => row(value, fields, optional_fields),
    }
}

#[cfg(test)]
mod value_projection_tests {
    use super::project_value_rows;
    use crate::fixture_value as json;

    #[test]
    fn whole_row_projection_separates_metadata_and_preserves_presence() {
        let fields = vec!["title".into(), "score".into()];
        let input = json!({"id":"i1", "title":"A", "score":null, "_ref":{"entity_type":"Item"}, "ambient":"secret"});
        let expected = json!({"title":"A", "score":null});
        assert_eq!(
            project_value_rows(&input, &fields, &Default::default()).unwrap(),
            expected
        );
        assert_eq!(
            project_value_rows(
                &json!([input.clone(), input.clone()]),
                &fields,
                &Default::default()
            )
            .unwrap(),
            json!([expected.clone(), expected])
        );
        assert_eq!(
            project_value_rows(&json!([]), &fields, &Default::default()).unwrap(),
            json!([])
        );
        let error =
            project_value_rows(&json!({"title":"A"}), &fields, &Default::default()).unwrap_err();
        assert!(matches!(
            error,
            super::ValueProjectionError::FieldMissing { field } if field == "score"
        ));
        assert_eq!(
            input["id"].as_str(),
            Some("i1"),
            "identity-bearing input remains untouched"
        );
    }
}

#[cfg(test)]
mod binding_shape_tests {
    use super::*;
    #[test]
    fn scalar_column_and_record_named_value_remain_distinct_binding_values() {
        use plasm_core::{Value, ValueRow};
        let scalar = Value::String("same".into());
        let record = Value::Object(indexmap::IndexMap::from([("value".into(), scalar.clone())]));
        let cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let mut mat = MaterializedNode::dry_values(
            &cgs,
            QualifiedEntityKey {
                entry_id: "fixture".into(),
                entity: "Result".into(),
            },
            vec![
                ValueRow::from_output(scalar.clone()),
                ValueRow::from_output(record.clone()),
            ],
            vec![None, None],
            String::new(),
            None,
        )
        .unwrap();
        mat.value_shapes = vec![
            MaterializedValueShape::ScalarColumn,
            MaterializedValueShape::Record,
        ];
        assert_eq!(
            mat.value_shape_at(0).unwrap(),
            MaterializedValueShape::ScalarColumn
        );
        assert_eq!(
            mat.value_shape_at(1).unwrap(),
            MaterializedValueShape::Record
        );
        assert!(
            mat.value_shape_at(2).is_err(),
            "shape lookup must conserve occurrences"
        );
        let input = materialized_input_row_from_mat(
            PlanNodeId::new("values").unwrap(),
            &mat,
            crate::plasm_plan::InputCardinalityProof::Collection,
        )
        .unwrap();
        assert_eq!(input.row, Value::Array(vec![scalar, record]));
        assert_eq!(
            input.rows[0], input.rows[1],
            "physical rows may match without erasing logical shape"
        );
        mat.value_shapes.pop();
        assert!(
            mat.value_shape_at(0).is_err(),
            "partial shape metadata is invalid even for an existing position"
        );
        assert!(materialized_input_row_from_mat(
            PlanNodeId::new("values").unwrap(),
            &mat,
            crate::plasm_plan::InputCardinalityProof::Collection
        )
        .is_err());
        mat.value_shapes.clear();
        assert_eq!(
            mat.value_shape_at(0).unwrap(),
            MaterializedValueShape::Record
        );
        assert_eq!(
            mat.value_shape_at(1).unwrap(),
            MaterializedValueShape::Record
        );
    }
}
