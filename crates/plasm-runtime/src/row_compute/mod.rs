//! Typed row execution over borrowed Plasm values.

mod adapter;
mod aggregate;
mod eval;
mod expression;
mod rows;
#[cfg(test)]
use plasm_core::ValueRow;

pub use adapter::ValueRowEngine;
pub use eval::{eval_compute_ops, ComputeEvalOutcome};

// JSON fixture notation is decoded once at test ingress, never in computation.
#[cfg(test)]
macro_rules! value { ($($tt:tt)*) => { serde_json::from_value::<plasm_core::Value>(serde_json::json!($($tt)*)).unwrap() }; }
#[cfg(test)]
macro_rules! row { ($($tt:tt)*) => { plasm_core::ValueRow::try_from($crate::row_compute::value!($($tt)*)).unwrap() }; }
#[cfg(test)]
pub(super) use {row, value};

/// Decode literal fixture contracts at test ingress. Production requires declarations.
#[cfg(test)]
fn fixture_contract(rows: &[ValueRow]) -> plasm_core::value_contract::ValueContract {
    use plasm_core::value_contract::ValueContract;
    let mut fields = std::collections::BTreeMap::new();
    for row in rows {
        for (name, value) in row {
            let value = ValueContract::literal(value).unwrap();
            fields
                .entry(name.clone())
                .and_modify(|old: &mut ValueContract| {
                    *old = ValueContract::join(old.clone(), value.clone())
                })
                .or_insert(value);
        }
    }
    ValueContract::record(fields, Default::default())
}
#[cfg(test)]
fn evaluate_fixture(
    ops: &[plasm_core::ComputeOp],
    rows: &[ValueRow],
) -> Result<ComputeEvalOutcome, String> {
    eval_compute_ops(ops, rows, &fixture_contract(rows))
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use plasm_core::{
        temporal_value::TemporalKind, value_contract::ValueContract, AggregateFunction,
        AggregateSpec, ComputeOp, FieldPath, OutputName,
    };
    fn aggregate(function: AggregateFunction) -> ComputeOp {
        ComputeOp::Aggregate {
            aggregates: vec![AggregateSpec {
                name: OutputName::new("answer").unwrap(),
                function,
                field: Some(FieldPath::from_dotted("value").unwrap()),
            }],
        }
    }
    #[test]
    fn temporal_extrema_and_empty_result_retain_declared_contract() {
        let value_type = TemporalKind::Datetime.contract();
        let contract = ValueContract::record(
            [("value".into(), value_type.clone())].into(),
            Default::default(),
        );
        let rows = [
            row!({"value":"2024-01-01T01:00:00+02:00"}),
            row!({"value":"2024-01-01T00:00:00Z"}),
        ];
        for (function, index) in [(AggregateFunction::Min, 0), (AggregateFunction::Max, 1)] {
            let ComputeEvalOutcome::Rows(frame) =
                eval_compute_ops(&[aggregate(function)], &rows, &contract).unwrap()
            else {
                panic!("rows")
            };
            assert_eq!(frame.rows[0]["answer"], rows[index]["value"]);
            let mut expected = value_type.clone();
            expected.nullable = true;
            assert_eq!(frame.schema.contract().field("answer").unwrap(), expected);
            let ComputeEvalOutcome::Rows(empty) =
                eval_compute_ops(&[aggregate(function)], &[], &contract).unwrap()
            else {
                panic!("rows")
            };
            assert_eq!(empty.schema, frame.schema);
            assert!(empty.rows[0]["answer"].is_null());
        }
    }
    #[test]
    fn invalid_singleton_and_unsupported_empty_domain_are_rejected() {
        let temporal = ValueContract::record(
            [("value".into(), TemporalKind::Datetime.contract())].into(),
            Default::default(),
        );
        for op in [
            aggregate(AggregateFunction::Min),
            ComputeOp::Sort {
                key: FieldPath::from_dotted("value").unwrap(),
                descending: false,
            },
        ] {
            assert!(eval_compute_ops(
                &[op.clone()],
                &[row!({"value":"not a datetime"})],
                &temporal
            )
            .is_err());
            let composite = ValueContract::record(
                [(
                    "value".into(),
                    ValueContract::scalar(plasm_core::FieldType::Json),
                )]
                .into(),
                Default::default(),
            );
            assert!(eval_compute_ops(&[op], &[], &composite).is_err());
        }
    }
    #[test]
    fn nested_cell_access_borrows_the_input_payload() {
        let rows = [row!({"value":{"nested":[1,2,3]}})];
        let state = super::rows::ingest_rows(&rows, &super::fixture_contract(&rows)).unwrap();
        let cell = state.rows[0].get("value.nested").unwrap();
        assert!(std::ptr::eq(
            cell,
            rows[0]["value"].as_object().unwrap().get("nested").unwrap()
        ));
    }
    #[test]
    fn row_correspondence_survives_sort_dedupe_projection_and_filter() {
        let rows = [
            row!({"id": "a", "score": 2}),
            row!({"id": "b", "score": 1}),
            row!({"id": "c", "score": 2}),
        ];
        let ops = [
            ComputeOp::Sort {
                key: FieldPath::from_dotted("score").unwrap(),
                descending: false,
            },
            ComputeOp::DedupeBy {
                keys: vec![FieldPath::from_dotted("score").unwrap()],
            },
            ComputeOp::Project {
                fields: [(
                    OutputName::new("name").unwrap(),
                    FieldPath::from_dotted("id").unwrap(),
                )]
                .into(),
            },
        ];
        let ComputeEvalOutcome::Rows(frame) = evaluate_fixture(&ops, &rows).unwrap() else {
            panic!("rows")
        };
        assert_eq!(frame.occurrences, [Some(1), Some(0)]);
        assert_eq!(frame.rows, [row!({"name":"b"}), row!({"name":"a"})]);
        let ComputeEvalOutcome::Rows(frame) = evaluate_fixture(
            &[ComputeOp::Aggregate {
                aggregates: vec![AggregateSpec {
                    name: OutputName::new("n").unwrap(),
                    function: AggregateFunction::Count,
                    field: None,
                }],
            }],
            &rows,
        )
        .unwrap() else {
            panic!("rows")
        };
        assert_eq!(frame.occurrences, [None]);
        let mut malformed = frame;
        malformed.occurrences.clear();
        assert!(malformed.validate_correspondence(rows.len()).is_err());
        malformed.occurrences.push(Some(rows.len()));
        assert!(malformed.validate_correspondence(rows.len()).is_err());
    }
    proptest::proptest! {
        #[test]
        fn declared_numeric_sort_and_extrema_agree(values in proptest::collection::vec(-10000i64..10000,1..40)) {
            let contract=ValueContract::record([("value".into(),ValueContract::scalar(plasm_core::FieldType::Integer))].into(),Default::default());
            let rows:Vec<_>=values.iter().map(|v| ValueRow::from_iter([("value".into(),plasm_core::Value::Integer(*v))])).collect();
            let sort=ComputeOp::Sort {key:FieldPath::from_dotted("value").unwrap(),descending:false};
            let ComputeEvalOutcome::Rows(sorted)=eval_compute_ops(&[sort],&rows,&contract).unwrap() else {panic!("rows")};
            for (function,expected) in [(AggregateFunction::Min,*values.iter().min().unwrap()),(AggregateFunction::Max,*values.iter().max().unwrap())] {
                let ComputeEvalOutcome::Rows(frame)=eval_compute_ops(&[aggregate(function)],&rows,&contract).unwrap() else {panic!("rows")};
                proptest::prop_assert_eq!(&frame.rows[0]["answer"],&plasm_core::Value::Integer(expected));
            }
            proptest::prop_assert_eq!(&sorted.rows[0]["value"],&plasm_core::Value::Integer(*values.iter().min().unwrap()));
        }
    }
}
