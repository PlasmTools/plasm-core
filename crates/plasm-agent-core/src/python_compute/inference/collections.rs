//! Close a local collection from its initializer before later mutation can
//! widen the upstream inference to `Unknown`. The checker supplies the type
//! and checks the complete annotated body on the next pass.
use super::{declarations::Declarations, Decoder, InferenceError, InferenceGraphError, Type};
use monty_analysis::{AnalysisLimits, AnalysisOutcome, AnalysisRequest, Node};
use plasm_core::value_contract::ValueShape;
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

pub(super) fn close_collection_initializers(
    source: &str,
    declarations: &mut Declarations,
    unresolved: usize,
) -> Result<String, InferenceError> {
    let parsed = ruff_python_parser::parse_module(source)?;
    let Some(Stmt::FunctionDef(function)) = parsed.suite().last() else {
        return Err(InferenceError::GeneratedFunctionMissing);
    };
    let mut insertions = Vec::new();
    for stmt in function.body.iter().take(32) {
        let Stmt::Assign(assign) = stmt else { continue };
        let [Expr::Name(target)] = assign.targets.as_slice() else {
            continue;
        };
        let collection = match assign.value.as_ref() {
            Expr::Set(set) => !set.elts.is_empty(),
            Expr::List(list) => !list.elts.is_empty(),
            Expr::Dict(dict) => !dict.items.is_empty(),
            _ => false,
        };
        if !collection {
            continue;
        }
        // Probe the initializer in its preceding lexical context. Later
        // mutation is deliberately absent: it is the source of the gradual
        // placeholder, not evidence about this initializer's element type.
        let start = stmt.start().to_usize();
        let expression = &source[assign.value.start().to_usize()..assign.value.end().to_usize()];
        let probe = format!("{}return ({expression})\n", &source[..start]);
        let result = monty_analysis::analyze_function(
            &AnalysisRequest {
                source: probe,
                stubs: Some(declarations.source.clone()),
                targets: Vec::new(),
                limits: AnalysisLimits::default(),
            },
            "__plasm_expression",
        )?;
        let AnalysisOutcome::Inferred(graph) = result.outcome else {
            continue;
        };
        let decoder = Decoder {
            graph: &graph,
            declarations,
        };
        let contracts = graph
            .roots
            .iter()
            .map(|id| decoder.decode(*id, 0))
            .collect::<Result<Vec<Type>, _>>();
        let Some(contracts) = supported_candidate(contracts)? else {
            continue;
        };
        let Some(contract) = contracts.into_iter().reduce(Type::join) else {
            continue;
        };
        if !matches!(
            contract.shape,
            ValueShape::Array { .. } | ValueShape::Set { .. } | ValueShape::Dictionary { .. }
        ) {
            continue;
        }
        insertions.push((target.end().to_usize(), contract));
    }
    let mut annotated = source.to_owned();
    let mut unresolved = unresolved;
    for (offset, contract) in insertions.into_iter().rev() {
        if unresolved == 0 {
            break;
        }
        let mut candidate_declarations = declarations.clone();
        let annotation = candidate_declarations.render(&contract, 0)?;
        let mut candidate = annotated.clone();
        candidate.insert_str(offset, &format!(": {annotation}"));
        let checked = monty_analysis::analyze_function(
            &AnalysisRequest {
                source: candidate.clone(),
                stubs: Some(candidate_declarations.source.clone()),
                targets: Vec::new(),
                limits: AnalysisLimits::default(),
            },
            "__plasm_expression",
        )?;
        if let AnalysisOutcome::Inferred(graph) = checked.outcome {
            let remaining = graph
                .nodes
                .iter()
                .filter(|node| **node == Node::Unknown)
                .count();
            if remaining < unresolved {
                annotated = candidate;
                *declarations = candidate_declarations;
                unresolved = remaining;
            }
        }
    }
    Ok(annotated)
}

fn supported_candidate<T>(
    candidate: Result<T, InferenceError>,
) -> Result<Option<T>, InferenceError> {
    match candidate {
        Ok(value) => Ok(Some(value)),
        Err(InferenceError::Graph(
            InferenceGraphError::UnsupportedType
            | InferenceGraphError::UnsupportedNode
            | InferenceGraphError::UnsupportedNamedType { .. }
            | InferenceGraphError::UnsupportedTemporal,
        )) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_candidates_do_not_erase_structural_faults() {
        assert!(
            supported_candidate::<()>(Err(InferenceGraphError::UnsupportedType.into()))
                .unwrap()
                .is_none()
        );
        for fault in [
            InferenceGraphError::InvalidReference,
            InferenceGraphError::RecursiveType,
        ] {
            let error = supported_candidate::<()>(Err(fault.clone().into())).unwrap_err();
            assert!(matches!(&error, InferenceError::Graph(actual) if actual == &fault));
            assert_eq!(
                std::error::Error::source(&error)
                    .unwrap()
                    .downcast_ref::<InferenceGraphError>(),
                Some(&fault)
            );
        }
    }
}
