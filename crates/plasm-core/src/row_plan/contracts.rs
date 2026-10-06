//! Semantic row contracts are independent of physical column inference.
use super::{PlanNode, ReductionFunction, TypedAggregate};
use crate::{value_contract::ValueContract, value_order::Orderable, AggregateFunction, FieldPath};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RowContractError {
    #[error("reduction input contract is missing")]
    ReductionInputMissing,
    #[error("row computation requires a record contract")]
    RowComputationRequiresRecord,
    #[error(transparent)]
    ValueContract(#[from] crate::value_contract::ValueContractError),
    #[error(transparent)]
    Ordering(#[from] crate::value_order::OrderingError),
    #[error(transparent)]
    Equality(#[from] crate::value_equality::ValueEqualityError),
    #[error(transparent)]
    Arithmetic(#[from] crate::value_arithmetic::ArithmeticContractError),
    #[error(transparent)]
    ValueExpression(#[from] crate::value_expression::InferenceError),
    #[error("length requires a string, array or record")]
    InvalidLengthOperand,
    #[error("computed field `{field}` is absent from the input schema")]
    ComputedFieldMissing { field: String },
}

pub fn field_contract(
    row: &ValueContract,
    path: &FieldPath,
) -> Result<ValueContract, RowContractError> {
    // Projected dotted names are fields in their own right.
    let dotted = path.dotted();
    if let Ok(value) = row.field(&dotted) {
        return Ok(value);
    }
    path.dotted()
        .split('.')
        .try_fold(row.clone(), |value, name| {
            value.field(name).map_err(Into::into)
        })
}

pub fn reduction_contract(
    function: AggregateFunction,
    input: Option<&ValueContract>,
) -> Result<ValueContract, RowContractError> {
    use AggregateFunction::*;
    if function != Count {
        let input = input.ok_or(RowContractError::ReductionInputMissing)?;
        match function {
            Min | Max => {
                input.ordering()?;
            }
            Sum | Avg => {
                use crate::value_arithmetic::Arithmetic;
                return input
                    .arithmetic_domain()
                    .map_err(RowContractError::Arithmetic)?
                    .reduction_result(function)
                    .map_err(RowContractError::Arithmetic);
            }
            _ => {}
        }
    }
    if function == Count {
        return Ok(ValueContract::scalar(crate::FieldType::Integer));
    }
    let mut result = input
        .ok_or(RowContractError::ReductionInputMissing)?
        .clone();
    result.nullable = true;
    Ok(result)
}

pub fn output_contract(
    input: &ValueContract,
    node: &PlanNode,
) -> Result<ValueContract, RowContractError> {
    match node {
        PlanNode::Filter(filter) => {
            filter
                .predicates()
                .try_map(&mut |predicate| -> Result<_, RowContractError> {
                    use crate::PlanPredicateOp::*;
                    if matches!(predicate.op, Lt | Lte | Gt | Gte) {
                        field_contract(input, &predicate.field_path)?.ordering()?;
                    }
                    Ok(predicate.clone())
                })?;
            Ok(input.clone())
        }
        PlanNode::Distinct { keys } | PlanNode::Dedupe { keys } => {
            use crate::value_equality::Equatable;
            if keys.is_empty() {
                input.equality()?;
            }
            for key in keys {
                field_contract(input, key)?.equality()?;
            }
            Ok(input.clone())
        }
        PlanNode::Sort { key, .. } => {
            field_contract(input, key)?.ordering()?;
            Ok(input.clone())
        }
        PlanNode::Project(spec) => {
            let mut fields = BTreeMap::new();
            let mut optional = BTreeSet::new();
            for (name, path) in &spec.fields {
                fields.insert(name.as_str().into(), field_contract(input, path)?);
                let mut parent = input;
                for segment in path.segments() {
                    match &parent.shape {
                        crate::value_contract::ValueShape::ObservedRecord {
                            fields,
                            optional_fields,
                        } => {
                            if optional_fields.contains(segment) {
                                optional.insert(name.as_str().into());
                            }
                            let Some(next) = fields.get(segment) else {
                                break;
                            };
                            parent = next;
                        }
                        crate::value_contract::ValueShape::Record { fields } => {
                            let Some(next) = fields.get(segment) else {
                                break;
                            };
                            parent = next;
                        }
                        _ => break,
                    }
                }
            }
            Ok(ValueContract::record(fields, optional))
        }
        PlanNode::With { columns } => {
            let mut result = input.clone();
            let fields = match &mut result.shape {
                crate::value_contract::ValueShape::Record { fields }
                | crate::value_contract::ValueShape::ObservedRecord { fields, .. } => fields,
                _ => return Err(RowContractError::RowComputationRequiresRecord),
            };
            for column in columns {
                let inferred =
                    ValueContract::with_expr(&column.expr, &mut |p| field_contract(input, p))?;
                fields.insert(column.name.as_str().into(), inferred);
            }
            Ok(result)
        }
        PlanNode::Aggregate { aggs } | PlanNode::GroupBy { aggs, .. } => {
            let mut fields = BTreeMap::new();
            if let PlanNode::GroupBy { keys, .. } = node {
                for key in keys {
                    fields.insert(key.dotted(), field_contract(input, key)?);
                }
            }
            for agg in aggs {
                let (name, function, path) = match agg {
                    TypedAggregate::Count { name } => (name, AggregateFunction::Count, None),
                    TypedAggregate::MoneySum { name, field, .. } => {
                        (name, AggregateFunction::Sum, Some(field))
                    }
                    TypedAggregate::Reduction { name, fn_, field } => (
                        name,
                        match fn_ {
                            ReductionFunction::Sum => AggregateFunction::Sum,
                            ReductionFunction::Avg => AggregateFunction::Avg,
                            ReductionFunction::Min => AggregateFunction::Min,
                            ReductionFunction::Max => AggregateFunction::Max,
                            ReductionFunction::First => AggregateFunction::First,
                            ReductionFunction::Last => AggregateFunction::Last,
                        },
                        Some(field),
                    ),
                };
                let value = path.map(|p| field_contract(input, p)).transpose()?;
                fields.insert(
                    name.as_str().into(),
                    reduction_contract(function, value.as_ref())?,
                );
            }
            Ok(ValueContract::record(fields, BTreeSet::new()))
        }
        _ => Ok(input.clone()),
    }
}
