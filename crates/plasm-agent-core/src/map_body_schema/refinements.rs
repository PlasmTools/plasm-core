//! Map upstream branch evidence onto captured row contracts. No Python operator
//! semantics live here. Evidence is recomputed from sealed IL, not serialized claims.
use plasm_core::plasm_monad::*;
use plasm_core::value_contract::{ValueContract, ValueShape};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RefinementError {
    #[error(transparent)]
    Inference(#[from] Box<crate::python_compute::inference::InferenceError>),
    #[error(transparent)]
    Schema(#[from] plasm_core::plasm_monad::SyntheticResultSchemaError),
    #[error(transparent)]
    Contract(#[from] plasm_core::value_contract::ValueContractError),
}

pub(super) fn refine(
    body: &CorrelatedBody,
    schema: &mut SyntheticResultSchema,
) -> Result<(), RefinementError> {
    let PlasmReturn::Step { step } = &body.body.return_ else {
        return Ok(());
    };
    let Some(PlasmStepPayload::Derive(output)) = body.body.steps.get(step.as_str()) else {
        return Ok(());
    };
    let PlasmDataValue::Object { fields } = &output.derive.value else {
        return Ok(());
    };
    let Some(predicate) = fields.get("predicate") else {
        return Ok(());
    };
    let Some((source, input, captures)) = predicate_input(body, predicate, 0) else {
        return Ok(());
    };
    let mut observations = Vec::new();
    for field in &input.fields {
        let Some(value) = captures.get(field.name.as_str()) else {
            continue;
        };
        let Some(parent_path) = reference(body, value, 0) else {
            continue;
        };
        if let Some(contract) = &field.value_type {
            let mut paths = crate::python_compute::observation_paths(contract)
                .map_err(|error| RefinementError::Inference(Box::new(error)))?;
            paths.insert(0, Vec::new());
            for path in paths {
                let input_path: Vec<String> = std::iter::once(field.name.to_string())
                    .chain(path.iter().cloned())
                    .collect();
                let target = parent_path.iter().cloned().chain(path).collect::<Vec<_>>();
                if !target.is_empty() {
                    observations.push((input_path, target));
                }
            }
        }
    }
    if observations.is_empty() {
        return Ok(());
    }
    let paths = observations
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let Some(branches) =
        crate::python_compute::branch_contracts(source, &input.row_contract()?, &paths)
            .map_err(|error| RefinementError::Inference(Box::new(error)))?
    else {
        return Ok(());
    };
    for ((_, path), (selected, _)) in observations.into_iter().zip(branches) {
        if let Some((first, rest)) = path.split_first() {
            if let Some(field) = schema.fields.iter_mut().find(|f| f.name.as_str() == first) {
                if let Some(contract) = &mut field.value_type {
                    replace(contract, rest, selected)?;
                    field.value_kind = contract.summary();
                }
            }
        }
    }
    Ok(())
}

type Captures = std::collections::BTreeMap<String, PlasmDataValue>;
fn predicate_input<'a>(
    body: &'a CorrelatedBody,
    predicate: &'a PlasmDataValue,
    depth: usize,
) -> Option<(&'a str, &'a SyntheticResultSchema, &'a Captures)> {
    if depth >= 64 {
        return None;
    }
    let PlasmDataValue::NodeSymbol { node, path, .. } = predicate else {
        return None;
    };
    match body.body.steps.get(node)? {
        PlasmStepPayload::Map(compute) if path.is_empty() => {
            let ComputeOp::Python {
                source,
                input_schema: Some(input),
                ..
            } = &compute.compute.op
            else {
                return None;
            };
            let PlasmStepPayload::Derive(captured) =
                body.body.steps.get(&compute.compute.source)?
            else {
                return None;
            };
            let PlasmDataValue::Object { fields } = &captured.derive.value else {
                return None;
            };
            Some((source, input, fields))
        }
        PlasmStepPayload::Derive(derive) => {
            let mut value = &derive.derive.value;
            for key in path {
                let PlasmDataValue::Object { fields } = value else {
                    return None;
                };
                value = fields.get(key)?;
            }
            predicate_input(body, value, depth + 1)
        }
        _ => None,
    }
}

/// Resolve only value provenance; this is independent of the Python predicate.
fn reference(body: &CorrelatedBody, value: &PlasmDataValue, depth: usize) -> Option<Vec<String>> {
    if depth >= 64 {
        return None;
    }
    let PlasmDataValue::NodeSymbol { node, path, .. } = value else {
        return None;
    };
    if node == body.parent.local.as_str() {
        return Some(path.clone());
    }
    let PlasmStepPayload::Derive(derive) = body.body.steps.get(node)? else {
        return None;
    };
    let mut value = &derive.derive.value;
    for key in path {
        let PlasmDataValue::Object { fields } = value else {
            return None;
        };
        value = fields.get(key)?;
    }
    reference(body, value, depth + 1)
}

fn replace(
    contract: &mut ValueContract,
    path: &[String],
    selected: ValueContract,
) -> Result<(), RefinementError> {
    if let Some((first, rest)) = path.split_first() {
        if let ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } =
            &mut contract.shape
        {
            if let Some(child) = fields.get_mut(first) {
                replace(child, rest, selected)?;
            }
        }
    } else {
        *contract = contract.refined_by(&selected)?;
    }
    Ok(())
}
