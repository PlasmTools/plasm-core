//! RA-14: set-union of two closed rowsets with identical column names.

use serde_json::{Map, Value};
use std::collections::BTreeSet;

#[derive(Debug, thiserror::Error)]
pub enum RowUnionError {
    #[error("union input row at index {index} on the {side} side must be an object (RA-14)")]
    InputRowNotObject { side: &'static str, index: usize },
    #[error("union requires identical columns; left has {left_columns:?}, right has {right_columns:?} (RA-14)")]
    ColumnSetMismatch {
        left_columns: Vec<String>,
        right_columns: Vec<String>,
    },
    #[error("union row has unexpected column `{column}`; expected {expected_columns:?} (RA-14)")]
    UnexpectedColumn {
        column: String,
        expected_columns: Vec<String>,
    },
    #[error("union row is missing column `{column}` (RA-14)")]
    MissingColumn { column: String },
    #[error("failed to serialize projected union row")]
    SerializeRow(#[source] serde_json::Error),
}

/// Concatenate `left` then `right`, project onto shared columns (left order), drop duplicate rows.
pub fn union_rowsets(left: &[Value], right: &[Value]) -> Result<Vec<Value>, RowUnionError> {
    let cols = union_column_names(left, right)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (side, rows) in [("left", left), ("right", right)] {
        for (index, row) in rows.iter().enumerate() {
            let projected = project_union_row(row, &cols, side, index)?;
            let key = serde_json::to_string(&projected).map_err(RowUnionError::SerializeRow)?;
            if seen.insert(key) {
                out.push(projected);
            }
        }
    }
    Ok(out)
}

fn union_column_names(left: &[Value], right: &[Value]) -> Result<Vec<String>, RowUnionError> {
    let left_cols = first_object_keys(left, "left")?;
    let right_cols = first_object_keys(right, "right")?;
    match (left_cols, right_cols) {
        (None, None) => Ok(Vec::new()),
        (Some(cols), None) | (None, Some(cols)) => Ok(cols),
        (Some(left_cols), Some(right_cols)) => {
            let left_set: BTreeSet<&str> = left_cols.iter().map(String::as_str).collect();
            let right_set: BTreeSet<&str> = right_cols.iter().map(String::as_str).collect();
            if left_set != right_set {
                return Err(RowUnionError::ColumnSetMismatch {
                    left_columns: left_cols,
                    right_columns: right_cols,
                });
            }
            Ok(left_cols)
        }
    }
}

fn first_object_keys(
    rows: &[Value],
    side: &'static str,
) -> Result<Option<Vec<String>>, RowUnionError> {
    for (index, row) in rows.iter().enumerate() {
        if row.is_null() {
            continue;
        }
        let obj = row
            .as_object()
            .ok_or(RowUnionError::InputRowNotObject { side, index })?;
        return Ok(Some(obj.keys().cloned().collect()));
    }
    Ok(None)
}

fn project_union_row(
    row: &Value,
    cols: &[String],
    side: &'static str,
    index: usize,
) -> Result<Value, RowUnionError> {
    if cols.is_empty() {
        return Ok(row.clone());
    }
    let obj = row
        .as_object()
        .ok_or(RowUnionError::InputRowNotObject { side, index })?;
    for key in obj.keys() {
        if !cols.iter().any(|col| col == key) {
            return Err(RowUnionError::UnexpectedColumn {
                column: key.clone(),
                expected_columns: cols.to_vec(),
            });
        }
    }
    let mut mapped = Map::new();
    for col in cols {
        mapped.insert(
            col.clone(),
            obj.get(col)
                .cloned()
                .ok_or_else(|| RowUnionError::MissingColumn {
                    column: col.clone(),
                })?,
        );
    }
    Ok(Value::Object(mapped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unions_two_one_column_projections() {
        let left = vec![json!({"email": "a@x"}), json!({"email": "b@x"})];
        let right = vec![json!({"email": "b@x"}), json!({"email": "c@x"})];
        let out = union_rowsets(&left, &right).expect("union");
        assert_eq!(
            out,
            vec![
                json!({"email": "a@x"}),
                json!({"email": "b@x"}),
                json!({"email": "c@x"}),
            ]
        );
    }

    #[test]
    fn rejects_column_mismatch() {
        let left = vec![json!({"email": "a@x"})];
        let right = vec![json!({"owner": "alice"})];
        let err = union_rowsets(&left, &right).expect_err("mismatch");
        assert!(matches!(err, RowUnionError::ColumnSetMismatch { .. }));
    }

    #[test]
    fn empty_side_is_identity() {
        let left = vec![json!({"email": "a@x"})];
        assert_eq!(union_rowsets(&left, &[]).expect("left"), left);
        assert_eq!(union_rowsets(&[], &left).expect("right"), left);
        assert!(union_rowsets(&[], &[]).expect("empty").is_empty());
    }
}
