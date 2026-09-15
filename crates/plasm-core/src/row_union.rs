//! RA-14: set-union of two closed rowsets with identical column names.

use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Concatenate `left` then `right`, project onto shared columns (left order), drop duplicate rows.
pub fn union_rowsets(left: &[Value], right: &[Value]) -> Result<Vec<Value>, String> {
    let cols = union_column_names(left, right)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for row in left.iter().chain(right.iter()) {
        let projected = project_union_row(row, &cols)?;
        let key = serde_json::to_string(&projected).map_err(|e| format!("union row: {e}"))?;
        if seen.insert(key) {
            out.push(projected);
        }
    }
    Ok(out)
}

fn union_column_names(left: &[Value], right: &[Value]) -> Result<Vec<String>, String> {
    let left_cols = first_object_keys(left)?;
    let right_cols = first_object_keys(right)?;
    match (left_cols, right_cols) {
        (None, None) => Ok(Vec::new()),
        (Some(cols), None) | (None, Some(cols)) => Ok(cols),
        (Some(left_cols), Some(right_cols)) => {
            let left_set: BTreeSet<&str> = left_cols.iter().map(String::as_str).collect();
            let right_set: BTreeSet<&str> = right_cols.iter().map(String::as_str).collect();
            if left_set != right_set {
                return Err(format!(
                    "union requires the same columns; left has [{}], right has [{}] (RA-14)",
                    left_cols.join(", "),
                    right_cols.join(", ")
                ));
            }
            Ok(left_cols)
        }
    }
}

fn first_object_keys(rows: &[Value]) -> Result<Option<Vec<String>>, String> {
    for row in rows {
        if row.is_null() {
            continue;
        }
        let obj = row
            .as_object()
            .ok_or_else(|| "union RHS/LHS row must be an object (RA-14)".to_string())?;
        return Ok(Some(obj.keys().cloned().collect()));
    }
    Ok(None)
}

fn project_union_row(row: &Value, cols: &[String]) -> Result<Value, String> {
    if cols.is_empty() {
        return Ok(row.clone());
    }
    let obj = row
        .as_object()
        .ok_or_else(|| "union row must be an object (RA-14)".to_string())?;
    for key in obj.keys() {
        if !cols.iter().any(|col| col == key) {
            return Err(format!(
                "union row has extra column `{key}`; expected [{}] (RA-14)",
                cols.join(", ")
            ));
        }
    }
    let mut mapped = Map::new();
    for col in cols {
        mapped.insert(col.clone(), obj.get(col).cloned().unwrap_or(Value::Null));
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
        assert!(err.contains("same columns"), "{err}");
    }

    #[test]
    fn empty_side_is_identity() {
        let left = vec![json!({"email": "a@x"})];
        assert_eq!(union_rowsets(&left, &[]).expect("left"), left);
        assert_eq!(union_rowsets(&[], &left).expect("right"), left);
        assert!(union_rowsets(&[], &[]).expect("empty").is_empty());
    }
}
