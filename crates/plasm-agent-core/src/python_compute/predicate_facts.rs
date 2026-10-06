//! Observation of sealed predicate evidence, not Python admission or execution.
use plasm_core::plasm_monad::{
    ComputeOp, CorrelatedBody, PlasmComp, PlasmDataValue, PlasmReturn, PlasmStepPayload,
    ScopedOutput,
};
use plasm_core::value_expression::ValueOperation;
use plasm_core::Value;
use ruff_python_ast::{Expr, Stmt};
use std::collections::HashSet;

#[derive(Debug, thiserror::Error)]
pub enum PredicateFactsError {
    #[error("sealed predicate step `{step}` is missing")]
    MissingStep { step: String },
    #[error("sealed predicate capture {path:?} is missing from step `{step}`")]
    MissingCapture { step: String, path: Vec<String> },
    #[error("sealed predicate kernel is unsupported: {kind:?}")]
    UnsupportedKernel { kind: UnsupportedPredicateKernel },
    #[error("sealed predicate compute source is invalid")]
    Parse(#[from] ruff_python_parser::ParseError),
    #[error("sealed predicate integer `{literal}` exceeds the value contract")]
    Integer {
        literal: String,
        #[source]
        source: std::num::ParseIntError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedPredicateKernel {
    FunctionCount,
    Parameters,
    Body,
    Expression,
}

fn unsupported(kind: UnsupportedPredicateKernel) -> PredicateFactsError {
    PredicateFactsError::UnsupportedKernel { kind }
}

#[derive(Debug, Default)]
pub struct PredicateFacts {
    pub fields: Vec<Vec<String>>,
    pub comparisons: Vec<(Vec<String>, Value)>,
}

/// Follow the returned Boolean port only. Unused body nodes and non-predicate
/// record outputs are not evidence that a row was filtered by their contents.
pub fn sealed_filter_facts(scope: &CorrelatedBody) -> Result<PredicateFacts, PredicateFactsError> {
    let mut facts = PredicateFacts::default();
    if !matches!(scope.output, ScopedOutput::Filter) {
        return Ok(facts);
    }
    let PlasmReturn::Step { step } = &scope.body.return_ else {
        return Ok(facts);
    };
    let mut pending = vec![(step.as_str().to_owned(), vec!["predicate".to_owned()])];
    let mut seen = HashSet::new();
    while let Some((step, path)) = pending.pop() {
        if !seen.insert((step.clone(), path.clone())) {
            continue;
        }
        let Some(payload) = scope.body.steps.get(&step) else {
            return Err(PredicateFactsError::MissingStep { step });
        };
        match payload {
            PlasmStepPayload::Map(map) => {
                let ComputeOp::Python { source, .. } = &map.compute.op else {
                    continue;
                };
                let evidence = expression_facts(source)?;
                for input_path in &evidence.fields {
                    if let Some(field) =
                        captured_parent_path(scope, &map.compute.source, input_path)?
                    {
                        facts.fields.push(field);
                    }
                }
                for (input_path, literal) in evidence.comparisons {
                    if let Some(field) =
                        captured_parent_path(scope, &map.compute.source, &input_path)?
                    {
                        facts.comparisons.push((field, literal));
                    }
                }
                for input in evidence.inputs {
                    if let Some(value) = step_value(&scope.body, &map.compute.source, &input) {
                        queue_reference(value, &mut pending);
                    }
                }
            }
            _ => {
                let value = required_step_value(&scope.body, &step, &path)?;
                queue_reference(value, &mut pending);
            }
        }
    }
    Ok(facts)
}

fn required_step_value<'a>(
    comp: &'a PlasmComp,
    step: &str,
    path: &[String],
) -> Result<&'a PlasmDataValue, PredicateFactsError> {
    if !comp.steps.contains_key(step) {
        return Err(PredicateFactsError::MissingStep {
            step: step.to_owned(),
        });
    }
    step_value(comp, step, path).ok_or_else(|| PredicateFactsError::MissingCapture {
        step: step.to_owned(),
        path: path.to_vec(),
    })
}

fn captured_parent_path(
    scope: &CorrelatedBody,
    source: &str,
    input: &[String],
) -> Result<Option<Vec<String>>, PredicateFactsError> {
    let Some((capture, tail)) = input.split_first() else {
        return Ok(None);
    };
    let value = required_step_value(&scope.body, source, std::slice::from_ref(capture))?;
    Ok(
        parent_path(value, scope, &mut HashSet::new()).and_then(|mut field| {
            field.extend_from_slice(tail);
            (!field.is_empty()).then_some(field)
        }),
    )
}

fn step_value<'a>(comp: &'a PlasmComp, step: &str, path: &[String]) -> Option<&'a PlasmDataValue> {
    let mut value = match comp.steps.get(step)? {
        PlasmStepPayload::Pure(payload) => &payload.data,
        PlasmStepPayload::Derive(payload) => &payload.derive.value,
        _ => return None,
    };
    for name in path {
        match value {
            PlasmDataValue::Object { fields } => value = fields.get(name)?,
            // Scalar cells have an explicit value column at materialization.
            _ if name == "value" => {}
            _ => return None,
        }
    }
    Some(value)
}

fn queue_reference(value: &PlasmDataValue, pending: &mut Vec<(String, Vec<String>)>) {
    match value {
        PlasmDataValue::NodeSymbol { node, path, .. } => pending.push((node.clone(), path.clone())),
        PlasmDataValue::BindingSymbol { binding, path } => {
            pending.push((binding.clone(), path.clone()))
        }
        PlasmDataValue::Expression { expression } => {
            use ValueOperation::*;
            match expression {
                And { left, right } | Or { left, right } => {
                    queue_reference(left, pending);
                    queue_reference(right, pending);
                }
                Not { value } | Refine { value, .. } | Field { value, .. } => {
                    queue_reference(value, pending)
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn parent_path(
    value: &PlasmDataValue,
    scope: &CorrelatedBody,
    seen: &mut HashSet<(String, Vec<String>)>,
) -> Option<Vec<String>> {
    match value {
        PlasmDataValue::NodeSymbol { node, path, .. }
        | PlasmDataValue::BindingSymbol {
            binding: node,
            path,
        } => {
            if node == scope.parent.local.as_str() {
                return Some(path.clone());
            }
            if !seen.insert((node.clone(), path.clone())) {
                return None;
            }
            parent_path(step_value(&scope.body, node, path)?, scope, seen)
        }
        PlasmDataValue::Expression {
            expression: ValueOperation::Field { value, name },
        } => {
            let mut path = parent_path(value, scope, seen)?;
            path.push(name.clone());
            Some(path)
        }
        PlasmDataValue::Expression {
            expression: ValueOperation::Refine { value, .. },
        } => parent_path(value, scope, seen),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct ExpressionFacts {
    fields: Vec<Vec<String>>,
    comparisons: Vec<(Vec<String>, Value)>,
    inputs: Vec<Vec<String>>,
}

fn expression_facts(source: &str) -> Result<ExpressionFacts, PredicateFactsError> {
    let parsed = ruff_python_parser::parse_module(source)?;
    let mut functions = parsed.suite().iter().filter_map(|stmt| match stmt {
        Stmt::FunctionDef(function) => Some(function),
        _ => None,
    });
    let mut facts = ExpressionFacts::default();
    let Some(function) = functions.next() else {
        return Err(unsupported(UnsupportedPredicateKernel::FunctionCount));
    };
    // Report only structural evidence from sealed single-expression kernels.
    // General function semantics remain Monty's responsibility.
    if functions.next().is_some() {
        return Err(unsupported(UnsupportedPredicateKernel::FunctionCount));
    }
    let [parameter] = function.parameters.args.as_slice() else {
        return Err(unsupported(UnsupportedPredicateKernel::Parameters));
    };
    let [Stmt::Return(ret)] = function.body.as_slice() else {
        return Err(unsupported(UnsupportedPredicateKernel::Body));
    };
    if let Some(value) = &ret.value {
        boolean_facts(value, parameter.parameter.name.as_str(), &mut facts)?;
    }
    Ok(facts)
}

fn boolean_facts(
    expr: &Expr,
    parameter: &str,
    facts: &mut ExpressionFacts,
) -> Result<(), PredicateFactsError> {
    match expr {
        Expr::Compare(compare) => {
            for pair in compare.operands.windows(2) {
                for (field, literal) in [(&pair[0], &pair[1]), (&pair[1], &pair[0])] {
                    if let Some(path) = input_path(field, parameter) {
                        facts.fields.push(path.clone());
                        if let Some(value) = scalar_literal(literal)? {
                            facts.comparisons.push((path.clone(), value));
                        }
                        facts.inputs.push(path);
                    }
                }
            }
        }
        Expr::BoolOp(boolean) => {
            for value in &boolean.values {
                boolean_facts(value, parameter, facts)?;
            }
        }
        Expr::UnaryOp(unary) if matches!(unary.op, ruff_python_ast::UnaryOp::Not) => {
            boolean_facts(&unary.operand, parameter, facts)?;
        }
        Expr::Call(call)
            if matches!(&*call.func, Expr::Name(name) if name.id.as_str() == "bool")
                && call.arguments.keywords.is_empty() =>
        {
            if let [value] = call.arguments.args.as_ref() {
                boolean_facts(value, parameter, facts)?;
            }
        }
        _ => {
            if let Some(path) = input_path(expr, parameter) {
                facts.inputs.push(path);
            } else if !matches!(expr, Expr::BooleanLiteral(_)) {
                return Err(unsupported(UnsupportedPredicateKernel::Expression));
            }
        }
    }
    Ok(())
}

fn input_path(expr: &Expr, parameter: &str) -> Option<Vec<String>> {
    match expr {
        Expr::Name(name) if name.id.as_str() == parameter => Some(vec![]),
        Expr::Attribute(attr) => {
            let mut path = input_path(&attr.value, parameter)?;
            path.push(attr.attr.to_string());
            Some(path)
        }
        _ => None,
    }
}

fn scalar_literal(expr: &Expr) -> Result<Option<Value>, PredicateFactsError> {
    Ok(match expr {
        Expr::UnaryOp(unary)
            if matches!(
                unary.op,
                ruff_python_ast::UnaryOp::USub | ruff_python_ast::UnaryOp::UAdd
            ) =>
        {
            let negative = matches!(unary.op, ruff_python_ast::UnaryOp::USub);
            match unary.operand.as_ref() {
                Expr::NumberLiteral(number) => match &number.value {
                    ruff_python_ast::Number::Int(value) => {
                        let literal = format!("{}{value}", if negative { "-" } else { "+" });
                        let value = literal
                            .parse()
                            .map_err(|source| PredicateFactsError::Integer { literal, source })?;
                        Some(Value::Integer(value))
                    }
                    ruff_python_ast::Number::Float(value) if value.is_finite() => {
                        Some(Value::Float(if negative { -*value } else { *value }))
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        Expr::StringLiteral(value) => Some(Value::String(value.value.to_str().to_owned())),
        Expr::BooleanLiteral(value) => Some(Value::Bool(value.value)),
        Expr::NoneLiteral(_) => Some(Value::Null),
        Expr::NumberLiteral(value) => match &value.value {
            ruff_python_ast::Number::Int(value) => {
                let literal = value.to_string();
                let value = literal
                    .parse()
                    .map_err(|source| PredicateFactsError::Integer { literal, source })?;
                Some(Value::Integer(value))
            }
            ruff_python_ast::Number::Float(value) if value.is_finite() => {
                Some(Value::Float(*value))
            }
            _ => None,
        },
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn sealed_ast_collects_only_boolean_comparison_operands() {
        let source = "@compute\ndef expression(row: Row):\n    return bool(row.capture.title == 'chosen' and row.capture.id > 2)\n";
        let facts = expression_facts(source).unwrap();
        assert_eq!(
            facts.comparisons,
            vec![
                (
                    vec!["capture".into(), "title".into()],
                    Value::String("chosen".into())
                ),
                (vec!["capture".into(), "id".into()], Value::Integer(2)),
            ]
        );
        let source = "@compute\ndef expression(row: Row):\n    return {'other': row.capture.state == 'distractor', 'constant': 'chosen'}\n";
        assert!(matches!(
            expression_facts(source),
            Err(PredicateFactsError::UnsupportedKernel { .. })
        ));
    }

    #[test]
    fn sealed_ast_parse_fault_retains_concrete_source() {
        let error = expression_facts("def expression(:").err().unwrap();
        assert!(error
            .source()
            .unwrap()
            .is::<ruff_python_parser::ParseError>());
    }

    #[test]
    fn signed_predicate_numbers_preserve_fields_values_and_bounds() {
        let source = "def expression(row: Row):\n    return row.capture.integer > -1 and row.capture.positive < +2\n";
        assert_eq!(
            expression_facts(source).unwrap().comparisons,
            vec![
                (vec!["capture".into(), "integer".into()], Value::Integer(-1)),
                (vec!["capture".into(), "positive".into()], Value::Integer(2)),
            ]
        );
        let source =
            "def expression(row: Row):\n    return row.capture.integer == -9223372036854775808\n";
        assert_eq!(
            expression_facts(source).unwrap().comparisons[0].1,
            Value::Integer(i64::MIN)
        );
        let source =
            "def expression(row: Row):\n    return row.capture.integer == -9223372036854775809\n";
        let error = expression_facts(source).unwrap_err();
        assert!(matches!(error, PredicateFactsError::Integer { .. }));
        assert!(error.source().unwrap().is::<std::num::ParseIntError>());
    }

    #[test]
    fn helper_predicate_is_explicitly_unsupported_evidence() {
        let source = "def expression(row: Row):\n    return helper(row.capture.title)\n";
        assert!(matches!(
            expression_facts(source),
            Err(PredicateFactsError::UnsupportedKernel {
                kind: UnsupportedPredicateKernel::Expression
            })
        ));
    }

    #[test]
    fn field_comparisons_do_not_require_literal_evidence() {
        let facts = expression_facts(
            "def expression(row: Row):\n    return row.capture.id == row.capture.title\n",
        )
        .unwrap();
        assert_eq!(
            facts.fields,
            vec![
                vec!["capture".to_owned(), "id".to_owned()],
                vec!["capture".to_owned(), "title".to_owned()]
            ]
        );
        assert!(facts.comparisons.is_empty());
    }

    #[test]
    fn missing_steps_and_captures_retain_structural_metadata() {
        let mut comp = PlasmComp {
            version: plasm_core::plasm_monad::PLASM_COMP_WIRE_VERSION,
            name: None,
            steps: Default::default(),
            bind: Default::default(),
            return_: PlasmReturn::Step {
                step: plasm_core::plasm_monad::StepId::new("source").unwrap(),
            },
            metadata: Default::default(),
        };
        let path = vec!["capture0".to_owned()];
        assert!(
            matches!(required_step_value(&comp, "source", &path), Err(PredicateFactsError::MissingStep { step }) if step == "source")
        );
        comp.steps.insert(
            "source".into(),
            PlasmStepPayload::Pure(plasm_core::plasm_monad::PurePayload {
                data: PlasmDataValue::Object {
                    fields: Default::default(),
                },
                effect_class: plasm_core::plasm_monad::EffectClass::Read,
                result_shape: plasm_core::plasm_monad::ResultShape::Single,
            }),
        );
        assert!(
            matches!(required_step_value(&comp, "source", &path), Err(PredicateFactsError::MissingCapture { step, path: actual }) if step == "source" && actual == path)
        );
    }
}
