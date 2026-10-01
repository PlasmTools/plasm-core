//! Independent finite reduction semantics. Only std and serde_json; no runtime kernels.
use serde_json::{json, Value};
use std::collections::BTreeMap;
type Row = BTreeMap<String, Value>;

pub fn reduce(rows: &[Row], grouped: bool) -> Vec<Row> {
    let mut groups: Vec<(Value, Vec<&Row>)> = Vec::new();
    if grouped {
        for row in rows {
            let key = &row["title"];
            if let Some((_, members)) = groups.iter_mut().find(|(k, _)| k == key) {
                members.push(row);
            } else {
                groups.push((key.clone(), vec![row]));
            }
        }
    } else {
        groups.push((Value::Null, rows.iter().collect()));
    }
    groups
        .into_iter()
        .map(|(key, members)| {
            let values: Vec<i64> = members
                .iter()
                .filter_map(|row| {
                    let value = &row["score"];
                    assert!(
                        value.is_null() || value.as_i64().is_some(),
                        "model expects nullable integers"
                    );
                    value.as_i64()
                })
                .collect();
            // Corpus integers are deliberately small; overflow/precision domains are separate.
            let total: i64 = values.iter().sum();
            let mut row = BTreeMap::from([
                ("n".into(), json!(members.len())),
                ("total".into(), json!(total)),
                (
                    "mean".into(),
                    if values.is_empty() {
                        Value::Null
                    } else {
                        json!(total as f64 / values.len() as f64)
                    },
                ),
                (
                    "lo".into(),
                    values.iter().min().map_or(Value::Null, |v| json!(*v)),
                ),
                (
                    "hi".into(),
                    values.iter().max().map_or(Value::Null, |v| json!(*v)),
                ),
                (
                    "first_value".into(),
                    members.first().map_or(Value::Null, |r| r["score"].clone()),
                ),
                (
                    "last_value".into(),
                    members.last().map_or(Value::Null, |r| r["score"].clone()),
                ),
            ]);
            if grouped {
                row.insert("title".into(), key);
            }
            row
        })
        .collect()
}

#[test]
fn empty_null_and_order_laws_are_explicit() {
    let empty = reduce(&[], false);
    assert_eq!(empty[0]["n"], json!(0));
    assert_eq!(empty[0]["total"], json!(0));
    for key in ["mean", "lo", "hi", "first_value", "last_value"] {
        assert!(empty[0][key].is_null());
    }
    assert!(reduce(&[], true).is_empty());
    let rows: Vec<Row> = [
        json!({"title":"z","score":null}),
        json!({"title":"a","score":-2}),
        json!({"title":"z","score":4}),
    ]
    .into_iter()
    .map(|r| serde_json::from_value(r).unwrap())
    .collect();
    let whole = reduce(&rows, false);
    assert_eq!(whole[0]["n"], json!(3));
    assert_eq!(whole[0]["mean"], json!(1.0));
    assert!(whole[0]["first_value"].is_null());
    assert_eq!(whole[0]["last_value"], json!(4));
    let grouped = reduce(&rows, true);
    assert_eq!(
        grouped
            .iter()
            .map(|r| r["title"].clone())
            .collect::<Vec<_>>(),
        vec![json!("z"), json!("a")]
    );
    let mut reversed = rows.clone();
    reversed.reverse();
    let reverse = reduce(&reversed, false);
    assert_eq!(reverse[0]["first_value"], whole[0]["last_value"]);
    assert_eq!(reverse[0]["last_value"], whole[0]["first_value"]);
}
