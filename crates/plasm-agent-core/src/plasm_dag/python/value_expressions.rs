//! Python expressions are inferred by Monty and executed as typed compute nodes.
//! Only dependency acquisition and lazy DAG scope construction belong here.
use super::*;
use crate::plasm_plan::{InputCardinality, PlanDataInput};

pub(super) fn is_value_expression(e: &PyExpr) -> bool {
    !matches!(e, PyExpr::Name(_) | PyExpr::Attribute(_))
}

impl Lower<'_> {
    pub(super) fn value_operation(
        &mut self,
        expression: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<Option<PlasmDataValue>, String> {
        if super::literal_operands::LiteralOperand::classify(expression).is_some()
            || matches!(expression, PyExpr::Name(_))
            || self.expression_placement(expression) == ExpressionPlacement::Host
            || matches!(expression, PyExpr::Attribute(attr) if self.expression_owner(&attr.value).is_some())
        {
            return Ok(None);
        }
        if let PyExpr::Call(call) = expression {
            if let Some(operation) =
                name(&call.func).and_then(super::quantifiers::QuantifierOperation::parse)
            {
                if let [PyExpr::Generator(generator)] = call.arguments.args.as_ref() {
                    if generator.generators.first().is_some_and(|clause| {
                        self.expression_placement(&clause.iter) == ExpressionPlacement::Host
                            || name(&clause.iter).is_some_and(|name| {
                                super::super::binding_contract(
                                    &self.state,
                                    self.scoped_binding(name),
                                )
                                .is_some_and(|c| !c.is_scalar_cell())
                            })
                    }) {
                        return self
                            .quantify(
                                expression,
                                call,
                                operation == super::quantifiers::QuantifierOperation::All,
                                inputs,
                            )
                            .map(Some);
                    }
                }
            }
        }
        // Classify dependencies before lowering either successor. Speculative
        // lowering would type-check a guarded operand without its branch facts.
        if self.expression_placement(expression) == ExpressionPlacement::LazyHost {
            return match expression {
                PyExpr::If(choice) => self
                    .conditional_value(
                        expression,
                        &choice.test,
                        (&choice.body, &choice.orelse),
                        inputs,
                        false,
                        None,
                    )
                    .map(Some),
                PyExpr::Compare(chain) => {
                    self.comparison_chain(expression, chain, inputs).map(Some)
                }
                PyExpr::BoolOp(boolean) => {
                    let first = boolean.values.first().ok_or("empty boolean expression")?;
                    let mut dependencies = BTreeMap::new();
                    let value = self.scoped_value(first, &mut dependencies)?;
                    let id = self.fresh();
                    self.emit_value(value, dependencies.into_values().collect(), &id)?;
                    let operand = *ruff_python_parser::parse_expression(&id)
                        .map_err(|e| e.to_string())?
                        .into_syntax()
                        .body;
                    let condition = *ruff_python_parser::parse_expression(&format!("bool({id})"))
                        .map_err(|e| e.to_string())?
                        .into_syntax()
                        .body;
                    let rest = if boolean.values.len() == 2 {
                        boolean.values[1].clone()
                    } else {
                        let mut tail = boolean.clone();
                        tail.values.remove(0);
                        PyExpr::BoolOp(tail)
                    };
                    let (yes, no) = if boolean.op == ruff_python_ast::BoolOp::And {
                        (&rest, &operand)
                    } else {
                        (&operand, &rest)
                    };
                    self.conditional_value(
                        expression,
                        &condition,
                        (yes, no),
                        inputs,
                        false,
                        Some(first),
                    )
                    .map(Some)
                }
                _ => unreachable!(),
            };
        }
        let captured = self.capture_expression(expression, inputs)?;
        self.emit_inferred_expression(captured.expression, captured.fields, inputs)
            .map(Some)
    }

    pub(super) fn lazy_host_dependency(&self, expression: &PyExpr) -> bool {
        if !(matches!(expression, PyExpr::If(_) | PyExpr::BoolOp(_))
            || matches!(expression, PyExpr::Compare(c) if c.ops.len() > 1))
        {
            return false;
        }
        use ruff_python_ast::visitor::{self, Visitor};
        struct Dependencies<'a, 'b> {
            lower: &'a Lower<'b>,
            found: bool,
        }
        impl<'ast> Visitor<'ast> for Dependencies<'_, '_> {
            fn visit_expr(&mut self, expression: &'ast PyExpr) {
                if self.found || self.lower.immediate_host_dependency(expression) {
                    self.found = true;
                    return;
                }
                visitor::walk_expr(self, expression);
            }
        }
        let mut dependencies = Dependencies {
            lower: self,
            found: false,
        };
        dependencies.visit_expr(expression);
        dependencies.found
    }

    pub(super) fn emit_inferred_expression(
        &mut self,
        expression: PyExpr,
        fields: BTreeMap<String, PlasmDataValue>,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, String> {
        let source_id = self.fresh();
        let source = self.emit_value(
            PlasmDataValue::Object { fields },
            inputs.values().cloned().collect(),
            &source_id,
        )?;
        let code = format!(
            "{}\n@compute\ndef expression(__plasm_input: Row):\n    return ({})\n",
            self.imports.source,
            monty::expression_source(&expression)
        );
        let id = self.fresh();
        self.emit_python_compute(source, &code, &id)?;
        inputs.insert(
            id.clone(),
            PlanDataInput {
                node: id.clone(),
                alias: id.clone(),
                cardinality: InputCardinality::Singleton,
            },
        );
        Ok(PlasmDataValue::NodeSymbol {
            node: id.clone(),
            alias: id,
            path: vec![],
        })
    }

    pub(super) fn python_value_expression(
        &mut self,
        expression: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, String> {
        let captured = self.capture_expression(expression, inputs)?;
        self.emit_inferred_expression(captured.expression, captured.fields, inputs)
    }
}
