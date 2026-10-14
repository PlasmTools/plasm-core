//! Revalidate occurrence-source claims against inference and actual argument
//! assembly. Serialized shapes alone cannot create receiver authority.
use super::*;
use crate::plasm_plan::ValidatedPlanNode;
use plasm_core::plasm_monad::{PlasmDataValue, PythonRowSelection};

#[derive(Debug, Clone, thiserror::Error)]
pub enum SelectionContractError {
    #[error("selection ports differ from inferred input occurrences")]
    InferredPortsMismatch,
    #[error("selection source differs from the actual compute argument")]
    ArgumentSourceMismatch,
    #[error("selection owner differs from its source row identity")]
    OwnerMismatch,
}

pub(super) fn validate_contract(
    selection: &PythonRowSelection,
    checked: &PreparedCompute,
) -> Result<(), PythonComputeRejection> {
    let parameters = selection
        .ports
        .iter()
        .map(|p| p.parameter.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if parameters.is_empty()
        || parameters.len() != selection.ports.len()
        || parameters != checked.selected_parameters
        || selection.ports.iter().any(|p| {
            if checked.independent_inputs {
                p.packet_field.as_ref() != Some(&p.parameter)
            } else {
                p.packet_field.is_some()
            }
        })
    {
        return Err(
            PythonComputeError::Selection(SelectionContractError::InferredPortsMismatch).into(),
        );
    }
    Ok(())
}

pub(super) fn validate_sources(
    selection: &PythonRowSelection,
    source: &str,
    nodes: &[ValidatedPlanNode],
) -> Result<(), PythonComputeRejection> {
    for port in &selection.ports {
        let mut current = source.to_owned();
        let mut path = port.packet_field.iter().cloned().collect::<Vec<_>>();
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert(current.clone()) {
                return Err(PythonComputeError::Selection(
                    SelectionContractError::ArgumentSourceMismatch,
                )
                .into());
            }
            if path.is_empty() {
                break;
            }
            let Some(ValidatedPlanNode::Derive(node)) =
                nodes.iter().find(|n| n.id().as_str() == current)
            else {
                return Err(PythonComputeError::Selection(
                    SelectionContractError::ArgumentSourceMismatch,
                )
                .into());
            };
            let mut value = &node.value;
            while let Some(field) = path.first() {
                let PlasmDataValue::Object { fields } = value else {
                    break;
                };
                value = fields.get(field).ok_or(PythonComputeError::Selection(
                    SelectionContractError::ArgumentSourceMismatch,
                ))?;
                path.remove(0);
            }
            let PlasmDataValue::NodeSymbol {
                node: next,
                alias,
                path: suffix,
            } = value
            else {
                return Err(PythonComputeError::Selection(
                    SelectionContractError::ArgumentSourceMismatch,
                )
                .into());
            };
            if !node
                .inputs
                .iter()
                .any(|input| input.node.as_str() == next && input.alias.as_str() == alias)
            {
                return Err(PythonComputeError::Selection(
                    SelectionContractError::ArgumentSourceMismatch,
                )
                .into());
            }
            let mut next_path = suffix.clone();
            next_path.extend(path);
            path = next_path;
            current = next.clone();
        }
        if current != port.source {
            return Err(PythonComputeError::Selection(
                SelectionContractError::ArgumentSourceMismatch,
            )
            .into());
        }
        let owner = crate::map_body::capture_owner(nodes, &current)
            .map_err(|_| PythonComputeError::Selection(SelectionContractError::OwnerMismatch))?;
        if owner.entry_id != selection.owner.entry_id || owner.entity != selection.owner.entity {
            return Err(
                PythonComputeError::Selection(SelectionContractError::OwnerMismatch).into(),
            );
        }
    }
    Ok(())
}
