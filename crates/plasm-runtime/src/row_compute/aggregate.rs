//! Native reductions. Group keys and comparisons borrow unchanged payloads.
use super::rows::{Cell, FrameState, Row};
use plasm_core::{row_plan::ReductionFunction, FieldPath, TypedAggregate, Value};
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hasher},
};

fn groups(
    state: &FrameState<'_>,
    keys: &[String],
) -> Result<Vec<Vec<usize>>, plasm_core::RowComputeError> {
    use plasm_core::value_equality::Equatable;
    let contracts = keys
        .iter()
        .map(|name| {
            plasm_core::row_plan::contracts::field_contract(
                &state.contract,
                &FieldPath::from_dotted(name)?,
            )
            .map_err(plasm_core::RowComputeError::Contract)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let equality = contracts
        .iter()
        .map(Equatable::equality)
        .collect::<Result<Vec<_>, _>>()?;
    let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut representatives = Vec::new();
    for (i, row) in state.rows.iter().enumerate() {
        let key = keys
            .iter()
            .zip(&equality)
            .map(|(name, eq)| row.get(name).map(|v| eq.key(v)).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        let mut hash = DefaultHasher::new();
        for value in &key {
            match value {
                None => hash.write_u8(0),
                Some(value) => {
                    hash.write_u8(1);
                    value.hash_into(&mut hash)?;
                }
            }
        }
        let bucket = buckets.entry(hash.finish()).or_default();
        if let Some(g) = bucket.iter().copied().find(|&g| representatives[g] == key) {
            groups[g].push(i);
        } else {
            bucket.push(groups.len());
            representatives.push(key);
            groups.push(vec![i]);
        }
    }
    Ok(groups)
}
pub(super) fn distinct(
    state: &mut FrameState<'_>,
    keys: &[FieldPath],
) -> Result<(), plasm_core::RowComputeError> {
    let names: Vec<_> = if keys.is_empty() {
        state
            .rows
            .iter()
            .flat_map(|r| r.0.keys().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    } else {
        keys.iter().map(FieldPath::dotted).collect()
    };
    let groups = groups(state, &names)?;
    let mut keep = vec![false; state.rows.len()];
    for g in groups {
        keep[g[0]] = true;
    }
    let mut i = 0;
    state.rows.retain(|_| {
        let yes = keep[i];
        i += 1;
        yes
    });
    Ok(())
}
pub(super) fn aggregate(
    state: &mut FrameState<'_>,
    keys: &[FieldPath],
    aggs: &[TypedAggregate],
    grouped: bool,
) -> Result<(), plasm_core::RowComputeError> {
    let names: Vec<_> = keys.iter().map(FieldPath::dotted).collect();
    for row in &state.rows {
        for name in &names {
            if row.get(name).is_none() {
                return Err(plasm_core::RowComputeError::MissingField {
                    field: name.clone(),
                });
            }
        }
    }
    let groups = if grouped {
        groups(state, &names)?
    } else {
        vec![(0..state.rows.len()).collect()]
    };
    let mut output = Vec::with_capacity(groups.len());
    for group in &groups {
        let mut row = Row::default();
        for name in &names {
            row.0.insert(
                name.clone(),
                state.rows[group[0]]
                    .cell(name)
                    .expect("validated group key"),
            );
        }
        for agg in aggs {
            let (name, value) = match agg {
                TypedAggregate::Count { name } => (
                    name,
                    Cell::computed(Value::Integer(
                        i64::try_from(group.len())
                            .map_err(|_| plasm_core::RowComputeError::RowCountOverflow)?,
                    )),
                ),
                TypedAggregate::Reduction { name, fn_, field } => {
                    (name, reduce(state, &field.dotted(), group, *fn_, false)?)
                }
                TypedAggregate::MoneySum { name, field, .. } => (
                    name,
                    reduce(state, &field.dotted(), group, ReductionFunction::Sum, true)?,
                ),
            };
            row.0.insert(name.as_str().into(), value);
        }
        output.push(row);
    }
    state.rows = output;
    Ok(())
}
fn reduce<'a>(
    state: &FrameState<'a>,
    field: &str,
    rows: &[usize],
    op: ReductionFunction,
    money_sum: bool,
) -> Result<Cell<'a>, plasm_core::RowComputeError> {
    if matches!(op, ReductionFunction::First | ReductionFunction::Last) {
        return match if op == ReductionFunction::First {
            rows.first()
        } else {
            rows.last()
        } {
            Some(&row) => state.rows[row].cell(field).ok_or_else(|| {
                plasm_core::RowComputeError::MissingField {
                    field: field.to_owned(),
                }
            }),
            None => Ok(Cell::computed(Value::Null)),
        };
    }
    let contract = plasm_core::row_plan::contracts::field_contract(
        &state.contract,
        &FieldPath::from_dotted(field)?,
    )
    .map_err(plasm_core::RowComputeError::Contract)?;
    let ordering = if matches!(op, ReductionFunction::Min | ReductionFunction::Max) {
        Some(plasm_core::value_order::Orderable::ordering(&contract)?)
    } else {
        None
    };
    if let Some(ordering) = &ordering {
        let mut chosen: Option<usize> = None;
        for &row in rows {
            let value = state.rows[row].get(field).ok_or_else(|| {
                plasm_core::RowComputeError::MissingField {
                    field: field.to_owned(),
                }
            })?;
            if value.is_null() {
                continue;
            }
            ordering.validate(value)?;
            let replace = match chosen {
                None => true,
                Some(prior) => {
                    let order = ordering.compare(
                        state.rows[prior].get(field).ok_or_else(|| {
                            plasm_core::RowComputeError::MissingField {
                                field: field.to_owned(),
                            }
                        })?,
                        value,
                    )?;
                    if op == ReductionFunction::Min {
                        order.is_gt()
                    } else {
                        order.is_lt()
                    }
                }
            };
            if replace {
                chosen = Some(row);
            }
        }
        return Ok(match chosen {
            Some(row) => state.rows[row]
                .cell(field)
                .expect("validated selected cell"),
            None => Cell::computed(Value::Null),
        });
    }
    use plasm_core::value_arithmetic::{Arithmetic, ArithmeticDomain};
    let arithmetic = contract
        .arithmetic_domain()
        .map_err(plasm_core::RowComputeError::ArithmeticContract)?;
    if money_sum && arithmetic != ArithmeticDomain::Money {
        return Err(plasm_core::RowComputeError::MoneySumRequiresMoney.into());
    }
    let mut result: Option<Value> = None;
    let mut count = 0usize;
    for &row in rows {
        let value = state.rows[row]
            .get(field)
            .ok_or_else(|| plasm_core::RowComputeError::MissingField {
                field: field.to_owned(),
            })?
            .clone();
        if value.is_null() {
            continue;
        }
        arithmetic
            .validate(&value)
            .map_err(plasm_core::RowComputeError::ArithmeticContract)?;
        count += 1;
        result = Some(match result {
            None => {
                if op == ReductionFunction::Avg && value.is_number() {
                    Value::Float(value.as_number().expect("numeric value"))
                } else {
                    value
                }
            }
            Some(prior) => {
                plasm_core::value_expression::arithmetic(plasm_core::ArithOp::Add, prior, value)
                    .map_err(plasm_core::RowComputeError::Arithmetic)?
            }
        });
    }
    let value = match (op, result) {
        (ReductionFunction::Avg, Some(value)) => plasm_core::value_expression::arithmetic(
            plasm_core::ArithOp::Div,
            value,
            Value::Integer(count as i64),
        )
        .map_err(plasm_core::RowComputeError::Arithmetic),
        (_, Some(value)) => Ok(value),
        (ReductionFunction::Sum, None) => Ok(arithmetic.zero()),
        _ => Ok(Value::Null),
    }?;
    Ok(Cell::computed(value))
}

#[cfg(test)]
mod tests {
    use super::super::{evaluate_fixture, row, ComputeEvalOutcome};
    use super::*;
    use plasm_core::{AggregateFunction as F, AggregateSpec, ComputeOp, OutputName};
    fn aggregate(function: F, field: &str) -> ComputeOp {
        ComputeOp::Aggregate {
            aggregates: vec![AggregateSpec {
                name: OutputName::new("result").unwrap(),
                function,
                field: Some(FieldPath::from_dotted(field).unwrap()),
            }],
        }
    }
    fn run(ops: &[ComputeOp], rows: &[plasm_core::ValueRow]) -> Vec<plasm_core::ValueRow> {
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows, .. }) =
            evaluate_fixture(ops, rows).unwrap()
        else {
            panic!("rows")
        };
        rows
    }
    #[test]
    fn grouping_preserves_nested_presence_and_demands_the_selected_key() {
        let ops = [ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("key").unwrap()],
            aggregates: vec![AggregateSpec {
                name: OutputName::new("n").unwrap(),
                function: F::Count,
                field: None,
            }],
        }];
        assert!(matches!(
            evaluate_fixture(&ops, &[row!({}), row!({"key":null})]).unwrap_err(),
            plasm_core::RowComputeError::MissingField { field } if field == "key"
        ));
        assert_eq!(
            run(
                &ops,
                &[row!({"key":{}}), row!({"key":{"x":null}}), row!({"key":{}})]
            ),
            [row!({"key":{},"n":2}), row!({"key":{"x":null},"n":1})]
        );
    }

    #[test]
    fn typed_money_sum_retains_money_on_empty_and_null_inputs() {
        for rows in [vec![], vec![row!({"fee":null})]] {
            let mut state = super::super::rows::ingest_rows(
                &rows,
                &plasm_core::value_contract::ValueContract::record(
                    [(
                        "fee".into(),
                        plasm_core::value_contract::ValueContract::scalar(
                            plasm_core::FieldType::Money,
                        ),
                    )]
                    .into(),
                    Default::default(),
                ),
            );
            super::aggregate(
                &mut state,
                &[],
                &[TypedAggregate::MoneySum {
                    name: OutputName::new("total").unwrap(),
                    field: FieldPath::from_dotted("fee").unwrap(),
                    currency: plasm_core::row_plan::MoneyAggLaw::RequireUniform,
                }],
                false,
            )
            .unwrap();
            let out = super::super::rows::collect_rows(&state);
            assert!(matches!(&out[0]["total"], Value::Money(m) if m.amount().is_zero()));
        }
    }

    #[test]
    fn integer_reductions_preserve_exactness_and_detect_overflow() {
        let rows = [row!({"n":9007199254740993_i64}), row!({"n":2})];
        assert_eq!(
            run(&[aggregate(F::Max, "n")], &rows)[0]["result"],
            Value::Integer(9007199254740993)
        );
        assert_eq!(
            run(&[aggregate(F::Sum, "n")], &rows)[0]["result"],
            Value::Integer(9007199254740995)
        );
        let unsigned = [plasm_core::ValueRow::from_iter([(
            "n".into(),
            Value::Unsigned(u64::MAX),
        )])];
        assert_eq!(
            run(&[aggregate(F::Min, "n")], &unsigned)[0]["result"],
            Value::Unsigned(u64::MAX)
        );
        let unsigned = [
            plasm_core::ValueRow::from_iter([("n".into(), Value::Unsigned(u64::MAX))]),
            plasm_core::ValueRow::from_iter([("n".into(), Value::Unsigned(0))]),
        ];
        assert_eq!(
            run(&[aggregate(F::Sum, "n")], &unsigned)[0]["result"],
            Value::Unsigned(u64::MAX)
        );
        assert!(evaluate_fixture(
            &[aggregate(F::Sum, "n")],
            &[row!({"n":i64::MAX}), row!({"n":1})]
        )
        .is_err());
    }
    #[test]
    fn money_sum_is_exact_and_composes_with_projection_and_arithmetic() {
        let rows = [
            row!({"fee":{"__plasm_money":"1.0000000000000000001","currency":"USD"}}),
            row!({"fee":{"__plasm_money":"2.0000000000000000002","currency":"USD"}}),
        ];
        let average = run(&[aggregate(F::Avg, "fee")], &rows);
        assert!(
            matches!(&average[0]["result"], Value::Money(m) if m.amount().to_string() == "1.50000000000000000015")
        );
        let mut ops = vec![
            aggregate(F::Sum, "fee"),
            ComputeOp::Project {
                fields: [(
                    OutputName::new("total").unwrap(),
                    FieldPath::from_dotted("result").unwrap(),
                )]
                .into(),
            },
        ];
        ops.push(ComputeOp::With {
            columns: plasm_core::parse_with_body("double: total + total").unwrap(),
        });
        let result = run(&ops, &rows);
        let Value::Money(money) = &result[0]["double"] else {
            panic!("native money")
        };
        assert_eq!(money.amount().to_string(), "6.0000000000000000006");
        assert_eq!(money.currency(), Some("USD"));
    }
    #[test]
    fn grouping_uses_value_equality_while_payload_storage_is_lossless() {
        use plasm_core::money::{MoneyValue, MoneyWireFormat};
        let make = |currency: &str, format| {
            Value::Object(indexmap::IndexMap::from([(
                "money".into(),
                Value::Money(
                    MoneyValue::new("1.00".parse().unwrap(), Some(currency.into()))
                        .with_format(format),
                ),
            )]))
        };
        let a = make("USD", MoneyWireFormat::DecimalString);
        let b = make("usd", MoneyWireFormat::MinorUnits { scale: 2 });
        let rows = [
            plasm_core::ValueRow::from_iter([("nested".into(), a.clone())]),
            plasm_core::ValueRow::from_iter([("nested".into(), b.clone())]),
        ];
        let preserved = run(&[], &rows);
        let Value::Money(money) = preserved[1]["nested"].get("money").unwrap() else {
            panic!("money")
        };
        assert_eq!(
            money.stored_format(),
            Some(MoneyWireFormat::MinorUnits { scale: 2 })
        );
        let result = run(
            &[ComputeOp::DedupeBy {
                keys: vec![FieldPath::from_dotted("nested").unwrap()],
            }],
            &rows,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["nested"], a);
    }
}

#[cfg(test)]
mod capability_tests {
    use super::super::{eval_compute_ops, ComputeEvalOutcome};
    use super::*;
    use plasm_core::{
        temporal_value::TemporalKind, value_contract::ValueContract, AggregateFunction,
        AggregateSpec, ComputeOp, OutputName,
    };
    #[test]
    fn nested_temporal_group_and_distinct_share_semantic_keys() {
        let values = [
            "2024-01-01T00:00:00Z",
            "2024-01-01T01:00:00+01:00",
            "2024-01-02T00:00:00Z",
        ];
        let input: Vec<_> = values
            .iter()
            .map(|v| {
                plasm_core::ValueRow::from_iter([("key".into(), super::super::value!({"when":*v}))])
            })
            .collect();
        let contract = ValueContract::record(
            [(
                "key".into(),
                ValueContract::record(
                    [("when".into(), TemporalKind::Datetime.contract())].into(),
                    Default::default(),
                ),
            )]
            .into(),
            Default::default(),
        );
        for op in [
            ComputeOp::DedupeBy {
                keys: vec![FieldPath::from_dotted("key").unwrap()],
            },
            ComputeOp::GroupBy {
                keys: vec![FieldPath::from_dotted("key").unwrap()],
                aggregates: vec![AggregateSpec {
                    name: OutputName::new("n").unwrap(),
                    function: AggregateFunction::Count,
                    field: None,
                }],
            },
        ] {
            let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&[op], &input, &contract).unwrap()
            else {
                panic!("rows")
            };
            assert_eq!(out.rows.len(), 2);
            assert_eq!(out.rows[0]["key"], input[0]["key"]);
            if let Some(n) = out.rows[0].get("n") {
                assert_eq!(n, &Value::Integer(2));
            }
        }
    }
    #[test]
    fn union_arithmetic_is_declared_even_for_empty_inputs() {
        let number = ValueContract::join(
            ValueContract::scalar(plasm_core::FieldType::Integer),
            ValueContract::scalar(plasm_core::FieldType::Number),
        );
        let contract = ValueContract::record([("n".into(), number)].into(), Default::default());
        for input in [
            vec![],
            vec![
                plasm_core::ValueRow::from_iter([("n".into(), Value::Integer(2))]),
                plasm_core::ValueRow::from_iter([("n".into(), Value::Float(0.5))]),
            ],
        ] {
            let op = ComputeOp::Aggregate {
                aggregates: vec![AggregateSpec {
                    name: OutputName::new("total").unwrap(),
                    function: AggregateFunction::Sum,
                    field: Some(FieldPath::from_dotted("n").unwrap()),
                }],
            };
            let ComputeEvalOutcome::Rows(out) = eval_compute_ops(&[op], &input, &contract).unwrap()
            else {
                panic!("rows")
            };
            assert_eq!(
                out.rows[0]["total"].as_number(),
                Some(if input.is_empty() { 0.0 } else { 2.5 })
            );
            assert_eq!(
                out.schema.contract().field("total").unwrap(),
                ValueContract::scalar(plasm_core::FieldType::Number)
            );
        }
    }
}
