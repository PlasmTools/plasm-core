//! Public row-compute entrypoint over JSON rows.

use super::json_frame::{collect_json, ingest_json_rows};
use super::plan_apply::apply_stored_plan;
use plasm_core::plasm_monad::ComputeOp;
use plasm_core::{fold_compute_ops, CollectCardinality, CollectReason, FrameId, StepId};

/// Engine collect before host Minijinja (Render is not a PlanNode).
#[derive(Debug, Clone)]
pub enum ComputeEvalOutcome {
    Rows(Vec<serde_json::Value>),
    Render {
        rows: Vec<serde_json::Value>,
        columns: Vec<plasm_core::OutputName>,
        column_aliases: std::collections::BTreeMap<String, plasm_core::OutputName>,
        template: String,
        collection_alias: Option<plasm_core::OutputName>,
        render_bindings: Vec<plasm_core::OutputName>,
    },
}

pub fn eval_compute_ops(
    ops: &[ComputeOp],
    rows: &[serde_json::Value],
) -> Result<ComputeEvalOutcome, String> {
    let step = StepId::new("row").map_err(|e| e.to_string())?;
    let plan = fold_compute_ops(ops, FrameId::new(1), step, CollectCardinality::List)
        .map_err(|e| e.to_string())?;
    let mut state = ingest_json_rows(rows).map_err(|e| e.to_string())?;
    apply_stored_plan(&plan, &mut state).map_err(|e| e.to_string())?;
    let collected = collect_json(&state).map_err(|e| e.to_string())?;
    match plan.collect() {
        CollectReason::Render { spec, .. } => Ok(ComputeEvalOutcome::Render {
            rows: collected,
            columns: spec.columns.clone(),
            column_aliases: spec.column_aliases.clone(),
            template: spec.template.clone(),
            collection_alias: spec.collection_alias.clone(),
            render_bindings: spec.render_bindings.clone(),
        }),
        _ => Ok(ComputeEvalOutcome::Rows(collected)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::parse_with_body;
    use plasm_core::plasm_monad::{FieldPath, PlanPredicateOp, PlasmDataValue};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn filter_sort_limit_roundtrip() {
        let rows = vec![
            serde_json::json!({"owner":"alice","score":10}),
            serde_json::json!({"owner":"bob","score":30}),
            serde_json::json!({"owner":"alice","score":20}),
        ];
        let pred = plasm_core::PlanPredicate {
            field_path: FieldPath::from_dotted("owner").unwrap(),
            op: PlanPredicateOp::Eq,
            value: PlasmDataValue::Literal {
                value: serde_json::json!("alice"),
            },
        };
        let ops = vec![
            ComputeOp::Filter {
                predicates: vec![pred],
            },
            ComputeOp::Sort {
                key: FieldPath::from_dotted("score").unwrap(),
                descending: true,
            },
            ComputeOp::Limit { count: 1 },
        ];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["score"], serde_json::json!(20));
    }

    #[test]
    fn with_mul_adds_column() {
        let rows = vec![serde_json::json!({"quantity": 2, "price": 5})];
        let columns = parse_with_body("notional: quantity * price").unwrap();
        let ops = vec![ComputeOp::With { columns }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out[0]["notional"], serde_json::json!(10));
        assert_eq!(out[0]["quantity"], serde_json::json!(2));
    }

    #[test]
    fn with_now_minus_field_is_nonnegative_int_days() {
        let rows = vec![
            serde_json::json!({"id": "old", "updated_at": "2020-01-01T00:00:00Z"}),
            serde_json::json!({"id": "new", "updated_at": "2024-06-01T00:00:00Z"}),
        ];
        let columns = parse_with_body("age_days: (now - updated_at)").unwrap();
        let ops = vec![ComputeOp::With { columns }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        let older = out.iter().find(|r| r["id"] == "old").unwrap();
        let newer = out.iter().find(|r| r["id"] == "new").unwrap();
        let age_old = older["age_days"].as_i64().expect("age int");
        let age_new = newer["age_days"].as_i64().expect("age int");
        assert!(age_old >= 0 && age_new >= 0, "ages {age_old} {age_new}");
        assert!(
            age_old > age_new,
            "older row must have larger age: {age_old} vs {age_new}"
        );
    }

    #[test]
    fn with_field_minus_field_is_int_days() {
        let rows = vec![serde_json::json!({
            "created_at": "2020-01-01T00:00:00Z",
            "updated_at": "2020-01-11T00:00:00Z",
        })];
        let columns = parse_with_body("cycle: (updated_at - created_at)").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["cycle"], serde_json::json!(10));
    }

    #[test]
    fn with_div_is_float() {
        let rows = vec![serde_json::json!({"quantity": 10, "price": 4})];
        let columns = parse_with_body("rate: quantity / price").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["rate"].as_f64().unwrap(), 2.5);
    }

    #[test]
    fn with_string_plus_concat() {
        let rows = vec![serde_json::json!({"first": "al", "last": "ice"})];
        let columns = parse_with_body("name: first + last").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["name"], serde_json::json!("alice"));
    }

    #[test]
    fn with_when_len_and_temporal_cmp() {
        let rows = vec![
            serde_json::json!({
                "title": "",
                "created_at": "2020-01-01T00:00:00Z",
                "updated_at": "2020-01-02T00:00:00Z",
            }),
            serde_json::json!({
                "title": "ok",
                "created_at": "2020-01-01T00:00:00Z",
                "updated_at": "2020-01-20T00:00:00Z",
            }),
        ];
        let columns = parse_with_body(
            "blank: when(len(title)=0, 1, 0), long: when(updated_at - created_at > 5, 1, 0)",
        )
        .unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["blank"], serde_json::json!(1));
        assert_eq!(out[0]["long"], serde_json::json!(0));
        assert_eq!(out[1]["blank"], serde_json::json!(0));
        assert_eq!(out[1]["long"], serde_json::json!(1));
    }

    #[test]
    fn with_when_now_minus_gt() {
        let rows = vec![
            serde_json::json!({"id": "old", "updated_at": "2020-01-01T00:00:00Z"}),
            serde_json::json!({"id": "future", "updated_at": "2099-01-01T00:00:00Z"}),
        ];
        let columns = parse_with_body("stale: when(now - updated_at > 14, 1, 0)").unwrap();
        let ComputeEvalOutcome::Rows(out) =
            eval_compute_ops(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        let old = out.iter().find(|r| r["id"] == "old").unwrap();
        let future = out.iter().find(|r| r["id"] == "future").unwrap();
        assert_eq!(old["stale"], serde_json::json!(1));
        assert_eq!(future["stale"], serde_json::json!(0));
    }

    #[test]
    fn group_by_count() {
        let rows = vec![
            serde_json::json!({"owner":"a","score":1}),
            serde_json::json!({"owner":"a","score":2}),
            serde_json::json!({"owner":"b","score":3}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("owner").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("n").unwrap(),
                function: plasm_core::AggregateFunction::Count,
                field: None,
            }],
        }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn money_sum_same_currency() {
        let rows = vec![
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"1.50","currency":"USD"}}),
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"2.50","currency":"USD"}}),
            serde_json::json!({"symbol":"B","fee":{"__plasm_money":"4.00","currency":"USD"}}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("symbol").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("fees").unwrap(),
                function: plasm_core::AggregateFunction::Sum,
                field: Some(FieldPath::from_dotted("fee").unwrap()),
            }],
        }];
        let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&ops, &rows).unwrap() else {
            panic!("rows");
        };
        assert_eq!(out.len(), 2, "out={out:?}");
        let a = out.iter().find(|r| r["symbol"] == "A").unwrap();
        let got = a["fees"]["__plasm_money"].as_str().expect("money amount");
        assert_eq!(
            Decimal::from_str(got).unwrap(),
            Decimal::from_str("4.00").unwrap(),
            "row={a:?}"
        );
        assert_eq!(a["fees"]["currency"], "USD");
        assert!(a.get("__ccy_n").is_none());
        assert!(a.get("__ccy_n_fees").is_none());
    }

    #[test]
    fn money_sum_rejects_cross_currency() {
        let rows = vec![
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"1.00","currency":"USD"}}),
            serde_json::json!({"symbol":"A","fee":{"__plasm_money":"1.00","currency":"EUR"}}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("symbol").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("fees").unwrap(),
                function: plasm_core::AggregateFunction::Sum,
                field: Some(FieldPath::from_dotted("fee").unwrap()),
            }],
        }];
        let err = eval_compute_ops(&ops, &rows).unwrap_err();
        assert!(
            err.contains("currency") || err.contains("money"),
            "expected cross-currency error, got {err}"
        );
    }
}
