//! References carry unchanged payloads through row operations. Presence is map
//! membership, never a sentinel value or a separate physical column.
use indexmap::IndexMap;
use plasm_core::{
    row_plan::{FrameShape, RemapReason},
    value_contract::ValueContract,
    Value, ValueRow,
};
use std::sync::Arc;

#[derive(Clone)]
enum Storage<'a> {
    Borrowed(&'a Value),
    Computed(Arc<Value>),
}
#[derive(Clone)]
pub(super) struct Cell<'a> {
    storage: Storage<'a>,
    path: Vec<String>,
}
impl<'a> Cell<'a> {
    pub fn computed(value: Value) -> Self {
        Self {
            storage: Storage::Computed(Arc::new(value)),
            path: vec![],
        }
    }
    pub fn value(&self) -> &Value {
        let mut value = match &self.storage {
            Storage::Borrowed(v) => *v,
            Storage::Computed(v) => v.as_ref(),
        };
        for part in &self.path {
            value = value
                .as_object()
                .and_then(|v| v.get(part))
                .expect("validated cell path");
        }
        value
    }
    fn child(&self, parts: &[&str]) -> Option<Self> {
        let mut value = self.value();
        for part in parts {
            value = value.as_object()?.get(*part)?;
        }
        let mut cell = self.clone();
        cell.path.extend(parts.iter().map(|s| (*s).to_owned()));
        Some(cell)
    }
}
#[derive(Clone, Default)]
pub(super) struct Row<'a>(pub IndexMap<String, Cell<'a>>, pub Option<usize>);
impl<'a> Row<'a> {
    pub fn cell(&self, name: &str) -> Option<Cell<'a>> {
        if let Some(cell) = self.0.get(name) {
            return Some(cell.clone());
        }
        let mut parts = name.split('.');
        self.0.get(parts.next()?)?.child(&parts.collect::<Vec<_>>())
    }
    pub fn get(&self, name: &str) -> Option<&Value> {
        if let Some(cell) = self.0.get(name) {
            return Some(cell.value());
        }
        let mut parts = name.split('.');
        let mut value = self.0.get(parts.next()?)?.value();
        for part in parts {
            value = value.as_object()?.get(part)?;
        }
        Some(value)
    }
    pub fn require(&self, name: &str) -> Result<&Value, plasm_core::RowComputeError> {
        self.get(name)
            .ok_or_else(|| plasm_core::RowComputeError::MissingField {
                field: name.to_owned(),
            })
    }
    pub fn materialize(&self) -> ValueRow {
        self.0
            .iter()
            .map(|(k, v)| (k.clone(), v.value().clone()))
            .collect()
    }
}
#[derive(Clone)]
pub(super) struct FrameState<'a> {
    pub rows: Vec<Row<'a>>,
    pub contract: ValueContract,
    pub shape: FrameShape,
}
pub(super) fn ingest_rows<'a>(rows: &'a [ValueRow], contract: &ValueContract) -> FrameState<'a> {
    FrameState {
        rows: rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                Row(
                    row.iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                Cell {
                                    storage: Storage::Borrowed(v),
                                    path: vec![],
                                },
                            )
                        })
                        .collect(),
                    Some(index),
                )
            })
            .collect(),
        contract: contract.clone(),
        shape: FrameShape::Remapped {
            reason: RemapReason::Derive,
        },
    }
}
pub(super) fn collect_rows(state: &FrameState<'_>) -> Vec<ValueRow> {
    state.rows.iter().map(Row::materialize).collect()
}

#[cfg(test)]
mod tests {
    use super::super::{row, value as json};
    use super::*;

    #[test]
    fn simultaneous_aliases_resolve_types_against_input_not_prior_aliases() {
        use plasm_core::{ComputeOp, FieldPath, OutputName};
        let rows = [row!({"a":"literal", "b":[1]})];
        let op = ComputeOp::Project {
            fields: [
                (
                    OutputName::new("a").unwrap(),
                    FieldPath::from_dotted("b").unwrap(),
                ),
                (
                    OutputName::new("b").unwrap(),
                    FieldPath::from_dotted("a").unwrap(),
                ),
            ]
            .into(),
        };
        let super::super::ComputeEvalOutcome::Rows(plasm_core::CollectedFrame {
            rows: actual, ..
        }) = super::super::evaluate_fixture(&[op], &rows).unwrap()
        else {
            panic!("rows")
        };
        assert_eq!(actual, [row!({"a":[1], "b":"literal"})]);
    }

    #[test]
    fn heterogeneous_columns_are_lossless_including_json_shaped_strings() {
        let values = [
            json!(null),
            json!("[1]"),
            json!([1]),
            json!({"x": [null, true]}),
            json!(9007199254740993_i64),
            json!(u64::MAX),
            json!(1.25),
            json!(false),
        ];
        let rows: Vec<_> = values
            .iter()
            .map(|v| row!({"value":v,"nested":{"value":v}}))
            .collect();
        let state = ingest_rows(&rows, &super::super::fixture_contract(&rows));
        assert_eq!(collect_rows(&state), rows);
        for (i, expected) in values.iter().enumerate() {
            assert_eq!(state.rows[i].get("nested.value").unwrap(), expected);
        }
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::super::{row, value};
    use super::*;
    #[test]
    fn replacing_a_record_removes_old_descendants_and_preserves_new_presence() {
        let input = [row!({"x":{"old":1}}), row!({"x":{"old":2}})];
        let mut state = ingest_rows(&input, &super::super::fixture_contract(&input));
        for (row, value) in state.rows.iter_mut().zip([value!({"new":3}), value!({})]) {
            row.0.insert("x".into(), Cell::computed(value));
        }
        assert!(state.rows[0].get("x.old").is_none());
        assert!(state.rows[1].get("x.new").is_none());
        assert_eq!(
            collect_rows(&state),
            [row!({"x":{"new":3}}), row!({"x":{}})]
        );
        for row in &mut state.rows {
            row.0.insert("x".into(), Cell::computed(value!(1)));
        }
        assert!(state.rows[0].get("x.new").is_none());
    }
    #[test]
    fn money_transport_metadata_and_float_sign_survive_nested_columns() {
        use plasm_core::money::{MoneyValue, MoneyWireFormat};
        let make = |format| {
            ValueRow::from_iter([(
                "v".into(),
                Value::Array(vec![Value::Money(
                    MoneyValue::new("1.000".parse().unwrap(), Some("USD".into()))
                        .with_format(format),
                )]),
            )])
        };
        let rows = [
            make(MoneyWireFormat::DecimalString),
            make(MoneyWireFormat::MinorUnits { scale: 3 }),
        ];
        let state = ingest_rows(&rows, &super::super::fixture_contract(&rows));
        let out = collect_rows(&state);
        for (out, format) in out.iter().zip([
            MoneyWireFormat::DecimalString,
            MoneyWireFormat::MinorUnits { scale: 3 },
        ]) {
            let Value::Money(m) = &out["v"].as_array().unwrap()[0] else {
                panic!("money")
            };
            assert_eq!(m.stored_format(), Some(format));
            assert_eq!(m.amount(), "1".parse::<rust_decimal::Decimal>().unwrap());
        }
        let rows = [row!({"v":[0.0]}), row!({"v":[-0.0]})];
        let out = collect_rows(&ingest_rows(&rows, &super::super::fixture_contract(&rows)));
        assert!(out[1]["v"].as_array().unwrap()[0]
            .as_number()
            .unwrap()
            .is_sign_negative());
    }
}

#[cfg(test)]
mod borrowing_tests {
    use super::super::{eval::apply_stored_plan, fixture_contract, row};
    use super::*;
    use plasm_core::{
        fold_compute_ops, CollectCardinality, ComputeOp, FieldPath, FrameId, OutputName, StepId,
    };

    #[test]
    fn projection_sort_limit_keep_original_nested_allocation() {
        let input = [
            row!({"rank":2,"payload":{"items":[1,2,3]}}),
            row!({"rank":1,"payload":{"items":[4,5,6]}}),
        ];
        let mut state = ingest_rows(&input, &fixture_contract(&input));
        let ops = [
            ComputeOp::Sort {
                key: FieldPath::from_dotted("rank").unwrap(),
                descending: false,
            },
            ComputeOp::Limit { count: 1 },
            ComputeOp::Project {
                fields: [(
                    OutputName::new("items").unwrap(),
                    FieldPath::from_dotted("payload.items").unwrap(),
                )]
                .into(),
            },
        ];
        let plan = fold_compute_ops(
            &ops,
            FrameId::new(1),
            StepId::new("borrow").unwrap(),
            CollectCardinality::List,
        )
        .unwrap();
        apply_stored_plan(&plan, &mut state).unwrap();
        assert!(std::ptr::eq(
            state.rows[0].get("items").unwrap(),
            input[1]["payload"]
                .as_object()
                .unwrap()
                .get("items")
                .unwrap()
        ));
        // A projected child of a computed value keeps its owner alive.
        let owner = Cell::computed(input[0]["payload"].clone());
        let child = owner.child(&["items"]).unwrap();
        let ptr = owner.value().as_object().unwrap().get("items").unwrap() as *const Value;
        drop(owner);
        assert_eq!(child.value() as *const Value, ptr);
    }

    #[test]
    fn positional_reduction_retains_the_selected_payload_reference() {
        use plasm_core::{AggregateFunction, AggregateSpec};
        let input = [row!({"payload":{"items":[1,2,3]}})];
        let mut state = ingest_rows(&input, &fixture_contract(&input));
        let op = ComputeOp::Aggregate {
            aggregates: vec![AggregateSpec {
                name: OutputName::new("selected").unwrap(),
                function: AggregateFunction::First,
                field: Some(FieldPath::from_dotted("payload").unwrap()),
            }],
        };
        let plan = fold_compute_ops(
            &[op],
            FrameId::new(1),
            StepId::new("select").unwrap(),
            CollectCardinality::List,
        )
        .unwrap();
        apply_stored_plan(&plan, &mut state).unwrap();
        assert!(std::ptr::eq(
            state.rows[0].get("selected").unwrap(),
            &input[0]["payload"]
        ));
    }

    proptest::proptest! {
        #[test]
        fn borrowed_rows_preserve_absence_null_duplicates_and_integer_bits(
            values in proptest::collection::vec(proptest::option::of(proptest::option::of(proptest::num::i64::ANY)),0..60)
        ) {
            let input:Vec<_>=values.iter().map(|v|match v {None=>ValueRow::new(),Some(None)=>ValueRow::from_iter([("v".into(),Value::Null)]),Some(Some(n))=>ValueRow::from_iter([("v".into(),Value::Integer(*n))])}).collect();
            let state=ingest_rows(&input,&fixture_contract(&input));
            proptest::prop_assert_eq!(collect_rows(&state),input);
        }
    }
}
