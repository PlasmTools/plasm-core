//! Execute typed row plans directly over Plasm values.
#[cfg(test)]
use super::evaluate_fixture;
use super::rows::{Cell, FrameState, Row};
use chrono::{DateTime, Utc};
use plasm_core::{
    fold_compute_ops, CollectCardinality, CollectReason, ComputeOp, PlanNode, PlanPredicate,
    PlanPredicateOp, RowPlan, StepId, WithExpr,
};
#[cfg(test)]
use plasm_core::{FieldPath, PlasmDataValue};
#[cfg(test)]
use rust_decimal::Decimal;
/// Owned result at the host boundary (Render is not a PlanNode).
#[derive(Debug, Clone)]
pub enum ComputeEvalOutcome {
    Rows(plasm_core::CollectedFrame),
    Render {
        schema: plasm_core::PlasmFrameSchema,
        rows: Vec<plasm_core::ValueRow>,
        columns: Vec<plasm_core::OutputName>,
        column_aliases: std::collections::BTreeMap<String, plasm_core::OutputName>,
        template: String,
        collection_alias: Option<plasm_core::OutputName>,
        render_bindings: Vec<plasm_core::OutputName>,
    },
}

pub fn eval_compute_ops(
    ops: &[ComputeOp],
    rows: &[plasm_core::ValueRow],
    contract: &plasm_core::value_contract::ValueContract,
) -> Result<ComputeEvalOutcome, plasm_core::RowComputeError> {
    let step = StepId::new("row")?;
    use plasm_core::{CollectRows, CompileRowPlan, IngestRows};
    let schema = plasm_core::PlasmFrameSchema::new(
        plasm_core::row_plan::FrameShape::Remapped {
            reason: plasm_core::row_plan::RemapReason::Derive,
        },
        contract.clone(),
    )
    .map_err(plasm_core::RowComputeError::Schema)?;
    let mut engine = super::ValueRowEngine::new();
    let frame = engine.ingest(
        &plasm_core::ScanSource::Inline { schema },
        plasm_core::IngestBatch { rows },
    )?;
    let plan = fold_compute_ops(ops, frame, step, CollectCardinality::List)?;
    let compiled = engine.compile(&plan)?;
    let collected = engine.collect(compiled, plan.collect().clone())?;
    collected
        .validate_correspondence(rows.len())
        .map_err(plasm_core::RowComputeError::Correspondence)?;
    match plan.collect() {
        CollectReason::Render { spec, .. } => Ok(ComputeEvalOutcome::Render {
            schema: collected.schema,
            rows: collected.rows,
            columns: spec.columns.clone(),
            column_aliases: spec.column_aliases.clone(),
            template: spec.template.clone(),
            collection_alias: spec.collection_alias.clone(),
            render_bindings: spec.render_bindings.clone(),
        }),
        _ => Ok(ComputeEvalOutcome::Rows(collected)),
    }
}

pub(super) fn apply_stored_plan(
    plan: &RowPlan,
    state: &mut FrameState<'_>,
) -> Result<(), plasm_core::RowComputeError> {
    let now = Utc::now();
    for (_, node) in plan.nodes().iter() {
        let output = plasm_core::row_plan::contracts::output_contract(&state.contract, node)
            .map_err(plasm_core::RowComputeError::Contract)?;
        apply_node(node, state, now)?;
        state.contract = output;
        use plasm_core::row_plan::{FrameShape, RemapReason};
        let reason = match node {
            PlanNode::Project(_) => Some(RemapReason::Project),
            PlanNode::GroupBy { .. } => Some(RemapReason::GroupBy),
            PlanNode::Aggregate { .. } => Some(RemapReason::Aggregate),
            _ => None,
        };
        if let Some(reason) = reason {
            state.shape = FrameShape::Remapped { reason };
        }
    }
    Ok(())
}
fn apply_node(
    node: &PlanNode,
    state: &mut FrameState<'_>,
    now: DateTime<Utc>,
) -> Result<(), plasm_core::RowComputeError> {
    match node {
        PlanNode::Filter(filter) => {
            let predicate = Predicate::compile(filter.predicates(), &state.contract)?;
            let keep = state
                .rows
                .iter()
                .map(|r| {
                    predicate
                        .evaluate(r)
                        .map(|v| v == Some(true))
                        .map_err(|error| match error {
                            PredicateEvaluationError::Ordering(error) => {
                                plasm_core::RowComputeError::Ordering(error)
                            }
                            PredicateEvaluationError::Equality(error) => {
                                plasm_core::RowComputeError::Equality(error)
                            }
                            PredicateEvaluationError::ContainsRequiresStrings => {
                                plasm_core::RowComputeError::ContainsRequiresStrings
                            }
                            PredicateEvaluationError::MembershipRequiresCollection => {
                                plasm_core::RowComputeError::MembershipRequiresCollection
                            }
                            PredicateEvaluationError::StringMembershipRequiresString => {
                                plasm_core::RowComputeError::StringMembershipRequiresString
                            }
                            PredicateEvaluationError::MembershipComparison => {
                                plasm_core::RowComputeError::MembershipComparison
                            }
                            PredicateEvaluationError::MissingField { field } => {
                                plasm_core::RowComputeError::MissingField { field }
                            }
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut i = 0;
            state.rows.retain(|_| {
                let yes = keep[i];
                i += 1;
                yes
            });
        }
        PlanNode::Sort { key, descending } => {
            let contract = plasm_core::row_plan::contracts::field_contract(&state.contract, key)
                .map_err(plasm_core::RowComputeError::Contract)?;
            let ordering = plasm_core::value_order::Orderable::ordering(&contract)?;
            let name = key.dotted();
            for row in &state.rows {
                let value =
                    row.get(&name)
                        .ok_or_else(|| plasm_core::RowComputeError::MissingField {
                            field: name.clone(),
                        })?;
                ordering.validate(value)?;
            }
            let mut error = None;
            state.rows.sort_by(|a, b| {
                let (a, b) = (
                    a.get(&name).expect("validated field"),
                    b.get(&name).expect("validated field"),
                );
                if a.is_null() || b.is_null() {
                    return a.is_null().cmp(&b.is_null());
                }
                match ordering.compare(a, b) {
                    Ok(order) => {
                        if *descending {
                            order.reverse()
                        } else {
                            order
                        }
                    }
                    Err(e) => {
                        error = Some(e);
                        std::cmp::Ordering::Equal
                    }
                }
            });
            if let Some(error) = error {
                return Err(plasm_core::RowComputeError::Ordering(error));
            }
        }
        PlanNode::Limit { count } => state.rows.truncate(*count),
        PlanNode::Distinct { keys } | PlanNode::Dedupe { keys } => {
            super::aggregate::distinct(state, keys)?
        }
        PlanNode::Project(spec) => {
            state.rows = state
                .rows
                .iter()
                .map(|row| {
                    let mut out = Row(Default::default(), row.1);
                    for (name, path) in &spec.fields {
                        let source = path.dotted();
                        let cell = row.cell(&source).ok_or_else(|| {
                            plasm_core::RowComputeError::MissingField {
                                field: source.clone(),
                            }
                        })?;
                        out.0.insert(name.as_str().into(), cell);
                    }
                    Ok(out)
                })
                .collect::<Result<_, plasm_core::RowComputeError>>()?;
        }
        PlanNode::With { columns } => {
            for row in &mut state.rows {
                let outputs = columns
                    .iter()
                    .map(|column| {
                        let cell = match &column.expr {
                            WithExpr::Field(path) => row.cell(&path.dotted()).ok_or_else(|| {
                                plasm_core::RowComputeError::MissingField {
                                    field: path.dotted(),
                                }
                            })?,
                            expr => Cell::computed(
                                super::expression::evaluate(expr, now, &mut |path| {
                                    Ok(row.require(&path.dotted())?.clone())
                                })
                                .map_err(|error| {
                                    match error {
                                    plasm_core::value_expression::WithEvaluationError::Field(
                                        error,
                                    ) => error,
                                    plasm_core::value_expression::WithEvaluationError::Arithmetic(
                                        error,
                                    ) => plasm_core::RowComputeError::Arithmetic(error),
                                    plasm_core::value_expression::WithEvaluationError::InvalidNumberLiteral => {
                                        plasm_core::RowComputeError::InvalidExpressionNumber
                                    }
                                    plasm_core::value_expression::WithEvaluationError::InvalidLengthOperand => {
                                        plasm_core::RowComputeError::InvalidExpressionLengthOperand
                                    }
                                    plasm_core::value_expression::WithEvaluationError::Comparison(error) => {
                                        plasm_core::RowComputeError::Comparison(error)
                                    }
                                }
                                })?,
                            ),
                        };
                        Ok((column.name.as_str().to_owned(), cell))
                    })
                    .collect::<Result<Vec<_>, plasm_core::RowComputeError>>()?;
                row.0.extend(outputs);
            }
        }
        PlanNode::GroupBy { keys, aggs } => super::aggregate::aggregate(state, keys, aggs, true)?,
        PlanNode::Aggregate { aggs } => super::aggregate::aggregate(state, &[], aggs, false)?,
    }
    Ok(())
}

/// Resolved once per operation, including empty inputs. Three-valued boolean
/// combination preserves null rather than turning NOT(null) into true.
enum Predicate {
    Atom {
        name: String,
        op: PlanPredicateOp,
        rhs: plasm_core::Value,
        contract: Box<plasm_core::value_contract::ValueContract>,
    },
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
}

#[derive(Debug, thiserror::Error)]
enum PredicateEvaluationError {
    #[error(transparent)]
    Ordering(#[from] plasm_core::value_order::OrderingError),
    #[error(transparent)]
    Equality(#[from] plasm_core::value_equality::ValueEqualityError),
    #[error("field `{field}` is unobserved (not null)")]
    MissingField { field: String },
    #[error("contains requires two string values")]
    ContainsRequiresStrings,
    #[error("membership requires a string or array on the right")]
    MembershipRequiresCollection,
    #[error("string membership requires a string on the left")]
    StringMembershipRequiresString,
    #[error("membership values cannot be compared in their declared value domain")]
    MembershipComparison,
}
impl Predicate {
    fn compile(
        expr: &plasm_core::BooleanExpr<PlanPredicate>,
        input: &plasm_core::value_contract::ValueContract,
    ) -> Result<Self, plasm_core::PredicateCompileError> {
        use plasm_core::BooleanExpr;
        Ok(match expr {
            BooleanExpr::Atom(p) => {
                let contract =
                    plasm_core::row_plan::contracts::field_contract(input, &p.field_path)?;
                let mut rhs = p.value.clone().into_resolved()?.into_value();
                if matches!(
                    p.op,
                    PlanPredicateOp::Eq
                        | PlanPredicateOp::Ne
                        | PlanPredicateOp::Lt
                        | PlanPredicateOp::Lte
                        | PlanPredicateOp::Gt
                        | PlanPredicateOp::Gte
                ) {
                    if let plasm_core::value_contract::ValueShape::Scalar { field_type } =
                        &contract.shape
                    {
                        // CompareUnify is driven by the declared domain, never storage.
                        if matches!(rhs, plasm_core::Value::String(_))
                            && matches!(
                                field_type,
                                plasm_core::FieldType::Integer
                                    | plasm_core::FieldType::Number
                                    | plasm_core::FieldType::Boolean
                            )
                        {
                            rhs = plasm_core::coerce_value_for_field_type(
                                field_type, None, None, rhs,
                            )?;
                        }
                    }
                }
                Self::Atom {
                    name: p.field_path.dotted(),
                    op: p.op,
                    rhs,
                    contract: Box::new(contract),
                }
            }
            BooleanExpr::And(xs) => Self::And(
                xs.iter()
                    .map(|x| Self::compile(x, input))
                    .collect::<Result<_, _>>()?,
            ),
            BooleanExpr::Or(xs) => Self::Or(
                xs.iter()
                    .map(|x| Self::compile(x, input))
                    .collect::<Result<_, _>>()?,
            ),
            BooleanExpr::Not(x) => Self::Not(Box::new(Self::compile(x, input)?)),
        })
    }
    fn evaluate(&self, row: &Row<'_>) -> Result<Option<bool>, PredicateEvaluationError> {
        use PlanPredicateOp::*;
        Ok(match self {
            Self::Atom {
                name,
                op,
                rhs,
                contract,
            } => {
                let lhs = row
                    .get(name)
                    .ok_or_else(|| PredicateEvaluationError::MissingField {
                        field: name.clone(),
                    })?;
                if *op == Exists {
                    Some(!lhs.is_null())
                } else if lhs.is_null() || rhs.is_null() {
                    None
                } else if matches!(op, Lt | Lte | Gt | Gte) {
                    let order = plasm_core::value_order::Orderable::ordering(contract.as_ref())?
                        .compare_literal(lhs, rhs)?;
                    Some(match op {
                        Lt => order.is_lt(),
                        Lte => order.is_le(),
                        Gt => order.is_gt(),
                        Gte => order.is_ge(),
                        _ => unreachable!(),
                    })
                } else if matches!(op, Eq | Ne) {
                    use plasm_core::{value_equality::Equatable, value_order::Orderable};
                    let equal = match contract.ordering() {
                        Ok(order) => order.equal_literal(lhs, rhs)?,
                        Err(_) => contract.equality()?.equivalent(lhs, rhs)?,
                    };
                    Some(if *op == Eq { equal } else { !equal })
                } else {
                    Some(match op {
                        Contains => lhs
                            .as_str()
                            .zip(rhs.as_str())
                            .map(|(left, right)| left.contains(right))
                            .ok_or(PredicateEvaluationError::ContainsRequiresStrings)?,
                        In | NotIn => {
                            let found = if let Some(haystack) = rhs.as_str() {
                                haystack.contains(lhs.as_str().ok_or(
                                    PredicateEvaluationError::StringMembershipRequiresString,
                                )?)
                            } else if let Some(items) = rhs.as_array() {
                                let mut found = false;
                                for item in items {
                                    found |= plasm_core::value_expression::compare(Eq, lhs, item)
                                        .map_err(|_| {
                                        PredicateEvaluationError::MembershipComparison
                                    })?;
                                }
                                found
                            } else {
                                return Err(PredicateEvaluationError::MembershipRequiresCollection);
                            };
                            if *op == NotIn {
                                !found
                            } else {
                                found
                            }
                        }
                        _ => unreachable!("all other predicate operators were handled above"),
                    })
                }
            }
            Self::Not(x) => x.evaluate(row)?.map(|x| !x),
            Self::And(xs) => {
                let mut result = Some(true);
                for x in xs {
                    let v = x.evaluate(row)?;
                    result = match (result, v) {
                        (Some(false), _) | (_, Some(false)) => Some(false),
                        (None, _) | (_, None) => None,
                        _ => Some(true),
                    };
                }
                result
            }
            Self::Or(xs) => {
                let mut result = Some(false);
                for x in xs {
                    let v = x.evaluate(row)?;
                    result = match (result, v) {
                        (Some(true), _) | (_, Some(true)) => Some(true),
                        (None, _) | (_, None) => None,
                        _ => Some(false),
                    };
                }
                result
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{row, value};
    use super::*;
    use plasm_core::parse_with_body;
    use std::str::FromStr;

    fn evaluate_temporal_fixture(
        ops: &[ComputeOp],
        rows: &[plasm_core::ValueRow],
    ) -> Result<ComputeEvalOutcome, plasm_core::RowComputeError> {
        let mut contract = super::super::fixture_contract(rows);
        let plasm_core::value_contract::ValueShape::Record { fields } = &mut contract.shape else {
            unreachable!()
        };
        // These fixture fields are explicitly declared datetimes, not date-looking strings.
        for name in ["created_at", "updated_at"] {
            if fields.contains_key(name) {
                fields.insert(
                    name.into(),
                    plasm_core::temporal_value::TemporalKind::Datetime.contract(),
                );
            }
        }
        eval_compute_ops(ops, rows, &contract)
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(48))]
    #[test]
        fn computed_wire_preserves_literals_arithmetic_and_row_identity(
            values in proptest::collection::vec(-100i64..100, 1..25),
            literal in "[a-zA-Z0-9,()+*/|=<>\\\"\\\\ -]{0,40}",
        ) {
            let quoted = serde_json::to_string(&literal).unwrap();
            let columns = parse_with_body(&format!("result: score - 2 + 3 * 4, label: when(score >= 0, {quoted}, {quoted})")).unwrap();
            let op = ComputeOp::With { columns };
            let restored: ComputeOp = serde_json::from_slice(&serde_json::to_vec(&op).unwrap()).unwrap();
            proptest::prop_assert_eq!(&op, &restored);
            let rows: Vec<_> = values.iter().map(|n| row!({"id":n,"score":n})).collect();
            let expected: Vec<_> = values.iter().map(|n| row!({"id":n,"score":n,"result":n+10,"label":literal})).collect();
            let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) = evaluate_fixture(&[restored], &rows).unwrap() else { panic!("rows") };
            proptest::prop_assert_eq!(out, expected);
        }

        #[test]
        fn boolean_filter_wire_preserves_rows_order_multiplicity_and_nulls(
            values in proptest::collection::vec(proptest::option::of(-10i64..10), 1..40),
            threshold in -10i64..10,
        ) {
            use plasm_core::BooleanExpr::{Atom, And, Or, Not};
            let pred = |op| PlanPredicate {
                field_path: FieldPath::from_dotted("score").unwrap(), op,
                value: PlasmDataValue::Literal { value: plasm_core::operand_binding::ResolvedValue::new(value!(threshold)).expect("literal data") },
            };
            // Overlapping branches must not duplicate rows; null must remain unknown under NOT.
            let tree = Or(vec![Atom(pred(PlanPredicateOp::Gt)), And(vec![
                Not(Box::new(Atom(pred(PlanPredicateOp::Lt)))), Atom(pred(PlanPredicateOp::Gte))])]);
            let op = ComputeOp::Filter { predicates: tree };
            let wire = serde_json::to_vec(&op).unwrap();
            let restored: ComputeOp = serde_json::from_slice(&wire).unwrap();
            proptest::prop_assert_eq!(&op, &restored);
            let rows: Vec<_> = values.iter().map(|n| row!({"id":n,"score":n})).collect();
            // The fixture declares an integer domain even when generated observations are null.
            let mut rows = rows; rows.push(row!({"id":threshold,"score":threshold}));
            let expected: Vec<_> = rows.iter().filter(|r| r["score"].as_integer().is_some_and(|n| n >= threshold)).cloned().collect();
            let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) = evaluate_fixture(&[restored], &rows).unwrap() else { panic!("rows") };
            proptest::prop_assert_eq!(out, expected);
        }
    }

    #[test]
    fn money_predicates_keep_exact_amounts_and_propagate_currency_errors() {
        let threshold =
            plasm_core::MoneyValue::new("9007199254740992".parse().unwrap(), Some("USD".into()));
        let pred = PlanPredicate {
            field_path: FieldPath::from_dotted("price").unwrap(),
            op: PlanPredicateOp::Gt,
            value: PlasmDataValue::try_from(plasm_core::Value::Money(threshold)).unwrap(),
        };
        let restored: PlanPredicate =
            serde_json::from_slice(&serde_json::to_vec(&pred).unwrap()).unwrap();
        let rows = vec![
            row!({"id":1,"price":{"__plasm_money":"9007199254740992","currency":"USD"}}),
            row!({"id":2,"price":{"__plasm_money":"9007199254740993","currency":"USD"}}),
        ];
        let op = ComputeOp::Filter {
            predicates: vec![restored.clone()].into(),
        };
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: actual, .. }) =
            evaluate_fixture(std::slice::from_ref(&op), &rows).unwrap()
        else {
            panic!("filter")
        };
        assert_eq!(
            actual.iter().map(|v| v["id"].clone()).collect::<Vec<_>>(),
            vec![value!(2)]
        );
        // Program literals are major units; native comparison must not round them.
        for literal in [value!(9007199254740992_i64), value!("9007199254740992")] {
            let mut literal_predicate = restored.clone();
            literal_predicate.value = PlasmDataValue::try_from(literal).unwrap();
            let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: selected, .. }) =
                evaluate_fixture(
                    &[ComputeOp::Filter {
                        predicates: vec![literal_predicate].into(),
                    }],
                    &rows,
                )
                .unwrap()
            else {
                panic!("rows")
            };
            assert_eq!(selected, actual);
        }
        let wrong_currency =
            vec![row!({"price":{"__plasm_money":"9007199254740993","currency":"EUR"}})];
        assert!(evaluate_fixture(&[op], &wrong_currency).is_err());
        let bound = crate::row_predicate::BoundRowPredicate {
            field_path: FieldPath::from_dotted("price").unwrap(),
            op: pred.op,
            value: restored.value.into_resolved().unwrap(),
        };
        assert!(crate::row_predicate::row_matches_predicate(&wrong_currency[0], &bound).is_err());
        assert!(crate::row_predicate::row_matches_predicate(&rows[1], &bound).unwrap());
    }

    #[test]
    fn native_numeric_filters_reuse_strict_literal_coercion_without_float_rounding() {
        let rows = vec![
            row!({"n":9007199254740992_i64}),
            row!({"n":9007199254740993_i64}),
        ];
        let filter = |literal| ComputeOp::Filter {
            predicates: vec![PlanPredicate {
                field_path: FieldPath::from_dotted("n").unwrap(),
                op: PlanPredicateOp::Gt,
                value: PlasmDataValue::try_from(value!(literal)).unwrap(),
            }]
            .into(),
        };
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: selected, .. }) =
            evaluate_fixture(&[filter("9007199254740992")], &rows).unwrap()
        else {
            panic!("rows")
        };
        assert_eq!(selected, vec![rows[1].clone()]);
        assert!(evaluate_fixture(&[filter("not a number")], &rows).is_err());
    }

    #[test]
    fn zero_limit_preserves_schema_and_returns_no_rows() {
        let rows = vec![row!({"score":10})];
        let ComputeEvalOutcome::Rows(frame) =
            evaluate_fixture(&[ComputeOp::Limit { count: 0 }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert!(frame.rows.is_empty());
        let ComputeEvalOutcome::Rows(original) = evaluate_fixture(&[], &rows).unwrap() else {
            panic!("rows")
        };
        assert_eq!(frame.schema, original.schema);
    }

    #[test]
    fn filter_sort_limit_roundtrip() {
        let rows = vec![
            row!({"owner":"alice","score":10}),
            row!({"owner":"bob","score":30}),
            row!({"owner":"alice","score":20}),
        ];
        let pred = plasm_core::PlanPredicate {
            field_path: FieldPath::from_dotted("owner").unwrap(),
            op: PlanPredicateOp::Eq,
            value: PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(value!("alice"))
                    .expect("literal data"),
            },
        };
        let ops = vec![
            ComputeOp::Filter {
                predicates: vec![pred].into(),
            },
            ComputeOp::Sort {
                key: FieldPath::from_dotted("score").unwrap(),
                descending: true,
            },
            ComputeOp::Limit { count: 1 },
        ];
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_fixture(&ops, &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["score"], value!(20));
    }

    fn filter_eq_pan(rhs: plasm_core::Value) -> PlanPredicate {
        PlanPredicate {
            field_path: FieldPath::from_dotted("pan").unwrap(),
            op: PlanPredicateOp::Eq,
            value: PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(rhs).unwrap(),
            },
        }
    }

    fn eval_pan_eq(
        rows: &[plasm_core::ValueRow],
        rhs: plasm_core::Value,
    ) -> Vec<plasm_core::ValueRow> {
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_fixture(
                &[ComputeOp::Filter {
                    predicates: vec![filter_eq_pan(rhs)].into(),
                }],
                rows,
            )
            .unwrap()
        else {
            panic!("rows");
        };
        out
    }

    /// Residual Filter Eq after digit_id coerce: exact digit string keeps the listed PAN.
    #[test]
    fn filter_eq_keeps_digit_id_string_identity() {
        let pan = "6419671322388907";
        let rows = vec![
            row!({"label": "other", "pan": "6043624134251612"}),
            row!({"label": "Chase", "pan": pan}),
        ];
        let kept = eval_pan_eq(&rows, value!(pan));
        assert_eq!(kept.len(), 1, "digit_id residual eq dropped the listed PAN");
        assert_eq!(kept[0]["label"], value!("Chase"));
    }

    /// IEEE-rounded neighbor must not match the exact digit_id cell.
    #[test]
    fn filter_eq_digit_id_rejects_f64_neighbor() {
        let exact = "9007199254740993";
        let rounded = (9_007_199_254_740_993i64 as f64 as i64).to_string();
        assert_ne!(rounded, exact);
        let rows = vec![row!({"label": "wide", "pan": exact})];
        let kept = eval_pan_eq(&rows, value!(rounded));
        assert!(
            kept.is_empty(),
            "f64 neighbor must not identify a digit_id row"
        );
    }

    #[test]
    fn filter_in_and_not_in_literal_set() {
        let rows = vec![
            row!({"owner":"alice","score":10}),
            row!({"owner":"bob","score":30}),
        ];
        let inn = plasm_core::PlanPredicate {
            field_path: FieldPath::from_dotted("owner").unwrap(),
            op: PlanPredicateOp::In,
            value: PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(value!(["alice"]))
                    .expect("literal data"),
            },
        };
        let outn = plasm_core::PlanPredicate {
            field_path: FieldPath::from_dotted("owner").unwrap(),
            op: PlanPredicateOp::NotIn,
            value: PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(value!(["alice"]))
                    .expect("literal data"),
            },
        };
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: kept, .. }) =
            evaluate_fixture(
                &[ComputeOp::Filter {
                    predicates: vec![inn].into(),
                }],
                &rows,
            )
            .unwrap()
        else {
            panic!("rows");
        };
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: drop, .. }) =
            evaluate_fixture(
                &[ComputeOp::Filter {
                    predicates: vec![outn].into(),
                }],
                &rows,
            )
            .unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["owner"], value!("alice"));
        assert_eq!(drop.len(), 1);
        assert_eq!(drop[0]["owner"], value!("bob"));
    }

    #[test]
    fn contains_with_non_string_value_returns_semantic_error() {
        let rows = vec![row!({"owner": "alice"})];
        let predicate = plasm_core::PlanPredicate {
            field_path: FieldPath::from_dotted("owner").unwrap(),
            op: PlanPredicateOp::Contains,
            value: PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(value!(1))
                    .expect("literal data"),
            },
        };
        let error = evaluate_fixture(
            &[ComputeOp::Filter {
                predicates: vec![predicate].into(),
            }],
            &rows,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            plasm_core::RowComputeError::ContainsRequiresStrings
        ));
    }

    #[test]
    fn with_mul_adds_column() {
        let rows = vec![row!({"quantity": 2, "price": 5})];
        let columns = parse_with_body("notional: quantity * price").unwrap();
        let ops = vec![ComputeOp::With { columns }];
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_fixture(&ops, &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["notional"], value!(10));
        assert_eq!(out[0]["quantity"], value!(2));
    }

    #[test]
    fn with_now_minus_field_is_nonnegative_int_days() {
        let rows = vec![
            row!({"id": "old", "updated_at": "2020-01-01T00:00:00Z"}),
            row!({"id": "new", "updated_at": "2024-06-01T00:00:00Z"}),
        ];
        let columns = parse_with_body("age_days: (now - updated_at)").unwrap();
        let ops = vec![ComputeOp::With { columns }];
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_temporal_fixture(&ops, &rows).unwrap()
        else {
            panic!("rows");
        };
        let older = out
            .iter()
            .find(|r| r["id"].as_str() == Some("old"))
            .unwrap();
        let newer = out
            .iter()
            .find(|r| r["id"].as_str() == Some("new"))
            .unwrap();
        let age_old = older["age_days"].as_integer().expect("age int");
        let age_new = newer["age_days"].as_integer().expect("age int");
        assert!(age_old >= 0 && age_new >= 0, "ages {age_old} {age_new}");
        assert!(
            age_old > age_new,
            "older row must have larger age: {age_old} vs {age_new}"
        );
    }

    #[test]
    fn with_field_minus_field_is_int_days() {
        let rows = vec![row!({
            "created_at": "2020-01-01T00:00:00Z",
            "updated_at": "2020-01-11T00:00:00Z",
        })];
        let columns = parse_with_body("cycle: (updated_at - created_at)").unwrap();
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_temporal_fixture(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["cycle"], value!(10));
    }

    #[test]
    fn with_div_is_float() {
        let rows = vec![row!({"quantity": 10, "price": 4})];
        let columns = parse_with_body("rate: quantity / price").unwrap();
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_fixture(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["rate"].as_number().unwrap(), 2.5);
    }

    #[test]
    fn with_string_plus_concat() {
        let rows = vec![row!({"first": "al", "last": "ice"})];
        let columns = parse_with_body("name: first + last").unwrap();
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_fixture(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["name"], value!("alice"));
    }

    #[test]
    fn with_when_len_and_temporal_cmp() {
        let rows = vec![
            row!({
                "title": "",
                "created_at": "2020-01-01T00:00:00Z",
                "updated_at": "2020-01-02T00:00:00Z",
            }),
            row!({
                "title": "ok",
                "created_at": "2020-01-01T00:00:00Z",
                "updated_at": "2020-01-20T00:00:00Z",
            }),
        ];
        let columns = parse_with_body(
            "blank: when(len(title)=0, 1, 0), long: when(updated_at - created_at > 5, 1, 0)",
        )
        .unwrap();
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_temporal_fixture(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out[0]["blank"], value!(1));
        assert_eq!(out[0]["long"], value!(0));
        assert_eq!(out[1]["blank"], value!(0));
        assert_eq!(out[1]["long"], value!(1));
    }

    #[test]
    fn with_when_now_minus_gt() {
        let rows = vec![
            row!({"id": "old", "updated_at": "2020-01-01T00:00:00Z"}),
            row!({"id": "future", "updated_at": "2099-01-01T00:00:00Z"}),
        ];
        let columns = parse_with_body("stale: when(now - updated_at > 14, 1, 0)").unwrap();
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_temporal_fixture(&[ComputeOp::With { columns }], &rows).unwrap()
        else {
            panic!("rows");
        };
        let old = out
            .iter()
            .find(|r| r["id"].as_str() == Some("old"))
            .unwrap();
        let future = out
            .iter()
            .find(|r| r["id"].as_str() == Some("future"))
            .unwrap();
        assert_eq!(old["stale"], value!(1));
        assert_eq!(future["stale"], value!(0));
    }

    #[test]
    fn group_by_count() {
        let rows = vec![
            row!({"owner":"b","score":1}),
            row!({"owner":"a","score":2}),
            row!({"owner":"a","score":3}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("owner").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("n").unwrap(),
                function: plasm_core::AggregateFunction::Count,
                field: None,
            }],
        }];
        // Stable first-seen group order keeps synthetic identities and tie ordering
        // deterministic across repeated executions and source frontends.
        for _ in 0..32 {
            let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
                evaluate_fixture(&ops, &rows).unwrap()
            else {
                panic!("rows");
            };
            assert_eq!(
                out,
                vec![row!({"owner":"b","n":1}), row!({"owner":"a","n":2})]
            );
        }
    }

    #[test]
    fn first_last_preserve_values_and_types() {
        use plasm_core::{AggregateFunction, AggregateSpec, OutputName};
        let rows = vec![
            row!({"text":"alpha", "integer":9007199254740993_i64, "flag":true, "nested":{"x":[1,2]}, "array":["a"], "money":{"__plasm_money":"1.50","currency":"USD"}}),
            row!({"text":"omega", "integer":7, "flag":false, "nested":{"x":[]}, "array":["z"], "money":{"__plasm_money":"2.50","currency":"USD"}}),
        ];
        for (function, index) in [(AggregateFunction::First, 0), (AggregateFunction::Last, 1)] {
            for field in ["text", "integer", "flag", "nested", "array", "money"] {
                let ops = [ComputeOp::Aggregate {
                    aggregates: vec![AggregateSpec {
                        name: OutputName::new("value").unwrap(),
                        function,
                        field: Some(FieldPath::from_dotted(field).unwrap()),
                    }],
                }];
                let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
                    evaluate_fixture(&ops, &rows).unwrap()
                else {
                    panic!("rows")
                };
                assert_eq!(out[0]["value"], rows[index][field], "{function:?} {field}");
            }
        }
    }

    #[test]
    fn money_sum_same_currency() {
        let rows = vec![
            row!({"symbol":"A","fee":{"__plasm_money":"1.50","currency":"USD"}}),
            row!({"symbol":"A","fee":{"__plasm_money":"2.50","currency":"USD"}}),
            row!({"symbol":"B","fee":{"__plasm_money":"4.00","currency":"USD"}}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("symbol").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("fees").unwrap(),
                function: plasm_core::AggregateFunction::Sum,
                field: Some(FieldPath::from_dotted("fee").unwrap()),
            }],
        }];
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: out, .. }) =
            evaluate_fixture(&ops, &rows).unwrap()
        else {
            panic!("rows");
        };
        assert_eq!(out.len(), 2, "out={out:?}");
        let a = out
            .iter()
            .find(|r| r["symbol"].as_str() == Some("A"))
            .unwrap();
        let plasm_core::Value::Money(got) = &a["fees"] else {
            panic!("native money required")
        };
        assert_eq!(got.amount(), Decimal::from_str("4.00").unwrap());
        assert_eq!(got.currency(), Some("USD"));
        assert!(a.get("__ccy_n").is_none());
        assert!(a.get("__ccy_n_fees").is_none());
    }

    #[test]
    fn money_sum_rejects_cross_currency() {
        let rows = vec![
            row!({"symbol":"A","fee":{"__plasm_money":"1.00","currency":"USD"}}),
            row!({"symbol":"A","fee":{"__plasm_money":"1.00","currency":"EUR"}}),
        ];
        let ops = vec![ComputeOp::GroupBy {
            keys: vec![FieldPath::from_dotted("symbol").unwrap()],
            aggregates: vec![plasm_core::AggregateSpec {
                name: plasm_core::OutputName::new("fees").unwrap(),
                function: plasm_core::AggregateFunction::Sum,
                field: Some(FieldPath::from_dotted("fee").unwrap()),
            }],
        }];
        let err = evaluate_fixture(&ops, &rows).unwrap_err();
        assert!(
            err.to_string().contains("currency") || err.to_string().contains("money"),
            "expected cross-currency error, got {err}"
        );
    }
    #[test]
    fn distinct_ignores_internal_index_and_keeps_first_visible_rows() {
        let rows = vec![
            row!({"owner":"alice"}),
            row!({"owner":"bob"}),
            row!({"owner":"alice"}),
        ];
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: actual, .. }) =
            evaluate_fixture(&[ComputeOp::DedupeBy { keys: vec![] }], &rows).unwrap()
        else {
            panic!("rows")
        };
        assert_eq!(actual, rows[..2]);
    }

    #[test]
    fn distinct_after_projection_and_empty_rows() {
        use plasm_core::OutputName;
        let ops = [
            ComputeOp::Project {
                fields: [(
                    OutputName::new("owner").unwrap(),
                    FieldPath::from_dotted("owner").unwrap(),
                )]
                .into_iter()
                .collect(),
            },
            ComputeOp::DedupeBy { keys: vec![] },
        ];
        let rows = vec![
            row!({"id":"1", "owner":"alice"}),
            row!({"id":"2", "owner":"alice"}),
        ];
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: actual, .. }) =
            evaluate_fixture(&ops, &rows).unwrap()
        else {
            panic!("rows")
        };
        assert_eq!(actual, vec![row!({"owner":"alice"})]);
        for (rows, expected) in [(vec![], vec![]), (vec![row!({}), row!({})], vec![row!({})])] {
            let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: actual, .. }) =
                evaluate_fixture(&[ComputeOp::DedupeBy { keys: vec![] }], &rows).unwrap()
            else {
                panic!("rows")
            };
            assert_eq!(actual, expected);
        }
    }
}

#[cfg(test)]
mod presence_tests {
    use super::super::row;
    use super::*;

    #[test]
    fn observed_presence_survives_transport_and_set_equality() {
        let rows = vec![row!({"id":1}), row!({"id":1,"score":null}), row!({"id":1})];
        for ops in [
            vec![],
            vec![ComputeOp::Limit { count: 3 }],
            vec![ComputeOp::Sort {
                key: FieldPath::from_dotted("id").unwrap(),
                descending: false,
            }],
        ] {
            let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: actual, .. }) =
                evaluate_fixture(&ops, &rows).unwrap()
            else {
                panic!("rows")
            };
            assert_eq!(actual, rows);
        }
        let ComputeEvalOutcome::Rows(plasm_core::CollectedFrame { rows: actual, .. }) =
            evaluate_fixture(&[ComputeOp::DedupeBy { keys: vec![] }], &rows).unwrap()
        else {
            panic!("rows")
        };
        assert_eq!(actual, rows[..2]);
    }

    #[test]
    fn reading_unobserved_field_fails_but_null_is_present() {
        let op = ComputeOp::Sort {
            key: FieldPath::from_dotted("score").unwrap(),
            descending: false,
        };
        assert!(matches!(evaluate_fixture(
            std::slice::from_ref(&op),
            &[row!({"id":1}), row!({"id":2,"score":null})]
        ).unwrap_err(), plasm_core::RowComputeError::MissingField { field } if field == "score"));
        assert!(
            evaluate_fixture(std::slice::from_ref(&op), &[row!({"id":2,"score":null})]).is_ok()
        );
        assert!(eval_compute_ops(
            std::slice::from_ref(&op),
            &[],
            &super::super::fixture_contract(&[row!({"score":null})])
        )
        .is_ok());
        assert!(evaluate_fixture(
            &[ComputeOp::Limit { count: 1 }, op],
            &[row!({"id":1,"score":3}), row!({"id":2})]
        )
        .is_ok());
    }
}
