//! Staged plan input row materialization and cardinality gates.

use super::super::*;
use super::hole_paths::NodeInputHoleIndex;
use std::collections::BTreeMap;

fn binding_value(
    mat: &MaterializedNode,
    row: &plasm_core::ValueRow,
    index: usize,
) -> Result<plasm_core::Value, String> {
    match mat.value_shape_at(index)? {
        MaterializedValueShape::ScalarColumn => row
            .get("value")
            .cloned()
            .ok_or_else(|| "scalar binding lacks its value column".into()),
        MaterializedValueShape::Record => Ok(row.clone().into_value()),
    }
}

pub(crate) fn materialized_input_row_from_mat(
    node: PlanNodeId,
    mat: &MaterializedNode,
    proof: crate::plasm_plan::InputCardinalityProof,
) -> Result<MaterializedInputRow, String> {
    if proof == crate::plasm_plan::InputCardinalityProof::Acknowledgement {
        if mat.result.operations.is_empty() {
            return Err("effect result has no operation acknowledgement".into());
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
    let inline = mat.row_source.inline_rows().ok_or_else(|| {
        format!(
            "plan input node {:?} has no inline rows for staging",
            node.as_str()
        )
    })?;
    let collection = proof == crate::plasm_plan::InputCardinalityProof::Collection;
    if collection {
        crate::python_compute::require_complete_collection(&mat.result)
            .map_err(|e| e.to_string())?;
    }
    if inline.is_empty() && !collection {
        return Err(format!(
            "Plan input {:?} expected at least one row but was empty",
            node.as_str()
        ));
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
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for input in inputs {
        let node = input.node.clone();
        let alias = input.alias.clone();
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
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
                format!("{:?} broadcast", input.proof).as_str(),
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
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let holes = template.map(|t| NodeInputHoleIndex::from_template_expr(&t.expr));
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
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
) -> String {
    if row_count == 0 {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced zero rows. This is a data-empty result, not a Plasm syntax error: run or inspect {node:?}, loosen filters if it should match, branch around empty results, or use `.singleton()` only when exactly one row is guaranteed."
        )
    } else {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced {row_count} rows. Add filters/projection to make the source unique, aggregate intentionally, or use `.singleton()` only when exactly one row is guaranteed."
        )
    }
}

pub(crate) fn materialized_result_use_inputs_with_source_row(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
    source_node: &PlanNodeId,
    source_row: &plasm_core::ValueRow,
    source_row_identity: Option<plasm_core::RowIdentity>,
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
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
pub(super) fn project_value_rows(
    value: &plasm_core::Value,
    fields: &[String],
    optional_fields: &std::collections::BTreeSet<String>,
) -> Result<plasm_core::Value, String> {
    fn row(
        value: &plasm_core::Value,
        fields: &[String],
        optional_fields: &std::collections::BTreeSet<String>,
    ) -> Result<plasm_core::Value, String> {
        let mut projected = indexmap::IndexMap::new();
        for field in fields {
            if value.get(field).is_none() && optional_fields.contains(field) {
                continue;
            }
            let value = value
                .get(field)
                .ok_or_else(|| format!("materialized row missing projected field `{field}`"))?;
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
        assert!(error.contains("missing projected field `score`"), "{error}");
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
