//! Seal DAG dependencies while retaining the Python AST and lexical scopes.
use super::*;
use crate::plasm_plan::PlanDataInput;
use ruff_python_ast::visitor::transformer::{self, Transformer};
use std::cell::RefCell;

pub(super) struct CapturedExpression {
    pub expression: PyExpr,
    pub fields: BTreeMap<String, PlasmDataValue>,
    pub references: BTreeMap<super::refinements::Reference, String>,
}

impl Lower<'_> {
    /// Catalog provenance alone does not authorize continuation of synthetic rows.
    pub(super) fn binding_relation_owner(&self, binding: &str) -> Option<QualifiedEntityKey> {
        let contract = super::super::binding_contract(&self.state, binding)?;
        if !contract.supports_relation_dot() || !contract.anchor.is_present() {
            return None;
        }
        super::super::schema_validate::resolve_qualified_entity_for_dag_source(
            &self.state,
            &[],
            binding.to_owned(),
        )
    }

    pub(super) fn expression_owner(&self, expression: &PyExpr) -> Option<QualifiedEntityKey> {
        match expression {
            PyExpr::Name(n) => {
                let binding = self.scoped_binding(n.id.as_str());
                if super::super::binding_contract(&self.state, binding)
                    .is_some_and(|c| c.is_scalar_cell())
                    || self.state.get(binding).is_some_and(|node| {
                        matches!(
                            node.source,
                            super::super::types::DagNodeSource::Compute {
                                op: ComputeOp::Python { .. },
                                ..
                            }
                        )
                    })
                {
                    return None;
                }
                super::super::schema_validate::resolve_qualified_entity_for_dag_source(
                    &self.state,
                    &[],
                    self.scoped_binding(n.id.as_str()).to_owned(),
                )
            }
            PyExpr::Call(call) => {
                let PyExpr::Attribute(attr) = &*call.func else {
                    return None;
                };
                if let Some(token) = name(&attr.value) {
                    if let Ok(owner) = self
                        .state
                        .sym_map_for(self.es)
                        .resolve_session_entity(token)
                    {
                        return Some(QualifiedEntityKey {
                            entry_id: owner.entry_id.to_string(),
                            entity: owner.entity.to_string(),
                        });
                    }
                }
                if super::row_operations::RowOperation::parse(attr.attr.as_str()).is_some() {
                    self.expression_owner(&attr.value)
                } else {
                    None
                }
            }
            PyExpr::Attribute(attr) => {
                let owner = self.expression_owner(&attr.value)?;
                let relation = super::super::relation::resolve_relation_wire_on_entity(
                    self.es,
                    self.state.cross_cache,
                    &owner,
                    attr.attr.as_str(),
                    None,
                )?;
                let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
                    self.es,
                    &owner.entry_id,
                    &owner.entity,
                )
                .ok()?;
                let relation = cgs
                    .get_entity(&owner.entity)?
                    .relations
                    .get(relation.as_str())?;
                Some(QualifiedEntityKey {
                    entry_id: owner.entry_id,
                    entity: relation.target_resource.to_string(),
                })
            }
            _ => None,
        }
    }

    pub(super) fn immediate_host_dependency(&self, expression: &PyExpr) -> bool {
        match expression {
            PyExpr::Call(call) => {
                if name(&call.func).is_some_and(|n| self.callbacks.contains_key(n)) {
                    return true;
                }
                let PyExpr::Attribute(attr) = &*call.func else {
                    return false;
                };
                name(&attr.value).is_some_and(|token| {
                    token == "self"
                        || self
                            .state
                            .sym_map_for(self.es)
                            .resolve_session_entity(token)
                            .is_ok()
                }) || self
                    .state
                    .sym_map_for(self.es)
                    .resolve_session_method(attr.attr.as_str())
                    .is_ok()
                    || (super::row_operations::RowOperation::parse(attr.attr.as_str()).is_some()
                        && (self.expression_owner(&attr.value).is_some()
                            || self.immediate_host_dependency(&attr.value)
                            || name(&attr.value)
                                .is_some_and(|n| self.state.contains(self.scoped_binding(n)))))
            }
            PyExpr::Attribute(_) => self.expression_owner(expression).is_some(),
            _ => false,
        }
    }

    pub(super) fn capture_expression(
        &mut self,
        expression: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<CapturedExpression, PythonLoweringError> {
        let before = self.state.nodes.len();
        let source = format!("({})", monty::expression_source(expression));
        let external = monty_analysis::external_names(&source)?
            .into_iter()
            .map(|(span, _)| (span.start, span.end))
            .collect();
        let mut expression = *ruff_python_parser::parse_expression(&source)
            .map_err(PythonLoweringError::parse_error)?
            .into_syntax()
            .body;
        let capture = Capture {
            state: RefCell::new(State {
                lower: self,
                inputs,
                fields: BTreeMap::new(),
                references: BTreeMap::new(),
                external,
                error: None,
            }),
        };
        capture.visit_expr(&mut expression);
        let state = capture.state.into_inner();
        if let Some(error) = state.error {
            return Err(error);
        }
        for node in &state.lower.state.nodes[before..] {
            let node = super::super::plan_serialize::lower_plan_node(node);
            if matches!(
                node.effect_class,
                EffectClass::Write | EffectClass::SideEffect
            ) {
                return Err(
                    crate::program_rejection::PythonProgramError::ValueExpressionWriteAuthority
                        .into(),
                );
            }
        }
        Ok(CapturedExpression {
            expression,
            fields: state.fields,
            references: state.references,
        })
    }
}

struct State<'a, 'b> {
    lower: &'a mut Lower<'b>,
    inputs: &'a mut BTreeMap<String, PlanDataInput>,
    fields: BTreeMap<String, PlasmDataValue>,
    references: BTreeMap<super::refinements::Reference, String>,
    external: BTreeSet<(u32, u32)>,
    error: Option<PythonLoweringError>,
}
struct Capture<'a, 'b> {
    state: RefCell<State<'a, 'b>>,
}

fn root_name(expr: &PyExpr) -> Option<&ruff_python_ast::ExprName> {
    match expr {
        PyExpr::Name(name) => Some(name),
        PyExpr::Attribute(attr) => root_name(&attr.value),
        PyExpr::Call(call) => root_name(&call.func),
        PyExpr::Subscript(subscript) => root_name(&subscript.value),
        _ => None,
    }
}
impl Capture<'_, '_> {
    fn membership(&self, expression: &mut PyExpr) -> Result<(), PythonLoweringError> {
        let original = expression.clone();
        if !self.capture(expression)? {
            return Ok(());
        }
        let PyExpr::Attribute(captured) = expression else {
            return Ok(());
        };
        let state = self.state.borrow();
        let Some(PlasmDataValue::NodeSymbol { node, path, .. }) =
            state.fields.get(captured.attr.as_str())
        else {
            return Ok(());
        };
        if !path.is_empty()
            || !super::super::binding_contract(&state.lower.state, node)
                .is_some_and(|c| !c.is_scalar_cell())
        {
            return Ok(());
        }
        for (parameter, binding) in &state.lower.frame.names {
            if state.lower.frame.row.as_deref() == Some(binding.as_str()) {
                super::membership::validate_closed_rhs(&original, parameter)?;
            }
        }
        let path =
            super::super::row_suffix::membership_rhs_column_path(&state.lower.state, &[], node)?;
        let [field] = path.as_slice() else {
            return Err(
                crate::program_rejection::PythonProgramError::MembershipNeedsOneColumn.into(),
            );
        };
        let code = format!(
            "[__plasm_member.{field} for __plasm_member in __plasm_input.{}]",
            captured.attr
        );
        *expression = *ruff_python_parser::parse_expression(&code)
            .map_err(PythonLoweringError::parse_error)?
            .into_syntax()
            .body;
        Ok(())
    }
    fn capture(&self, expr: &mut PyExpr) -> Result<bool, PythonLoweringError> {
        let mut state = self.state.borrow_mut();
        if root_name(expr).is_some_and(|name| {
            !state
                .external
                .contains(&(name.start().to_u32(), name.end().to_u32()))
        }) {
            return Ok(false);
        }
        let placement = state.lower.expression_placement(expr);
        let field = matches!(expr, PyExpr::Attribute(attr) if state.lower.expression_owner(&attr.value).is_some())
            && placement != ExpressionPlacement::Host;
        let deferred = placement != ExpressionPlacement::PythonValue;
        let bound = matches!(expr, PyExpr::Name(name) if name.ctx == ruff_python_ast::ExprContext::Load && state.lower.state.contains(state.lower.scoped_binding(name.id.as_str())));
        if !field && !deferred && !bound {
            return Ok(false);
        }
        // Capture the row observation, not its field value: optional attributes
        // must be accessed inside Python's selected branch, not eagerly here.
        let reference = if field {
            let PyExpr::Attribute(attr) = expr else {
                unreachable!()
            };
            state.lower.reference(&attr.value)
        } else {
            state.lower.reference(expr)
        };
        let key = if let Some(key) = reference.as_ref().and_then(|r| state.references.get(r)) {
            key.clone()
        } else {
            let State { lower, inputs, .. } = &mut *state;
            let value = if field {
                let PyExpr::Attribute(attr) = expr else {
                    unreachable!()
                };
                let node = lower.expr(&attr.value, None)?;
                let contract = super::super::binding_contract(&lower.state, &node)
                    .ok_or(crate::program_rejection::PythonLoweringInvariantError::ExpressionReceiverContractMissing)?;
                if !contract.row_cardinality.permits_scalar_field_extract() {
                    return Err(
                        crate::program_rejection::PythonProgramError::FieldInputNeedsSingleton
                            .into(),
                    );
                }
                inputs.insert(
                    node.clone(),
                    PlanDataInput {
                        node: node.clone(),
                        alias: node.clone(),
                        cardinality: crate::plasm_plan::InputCardinality::Singleton,
                    },
                );
                PlasmDataValue::NodeSymbol {
                    node: node.clone(),
                    alias: node,
                    path: Vec::new(),
                }
            } else if deferred {
                let node = lower.expr(expr, None)?;
                // Re-enter only the immutable name port, never this expression.
                let name = ruff_python_parser::parse_expression(&node)
                    .map_err(PythonLoweringError::parse_error)?;
                lower.scoped_value(&name.syntax().body, inputs)?
            } else {
                lower.scoped_value(expr, inputs)?
            };
            let value = if let Some(reference) = &reference {
                lower.refine_capture(reference, value, inputs)?
            } else {
                value
            };
            let key = format!("capture{}", state.fields.len());
            state.fields.insert(key.clone(), value);
            if let Some(reference) = reference {
                state.references.insert(reference, key.clone());
            }
            key
        };
        let replacement = ruff_python_parser::parse_expression(&format!("__plasm_input.{key}"))
            .map_err(PythonLoweringError::parse_error)?;
        if field {
            let PyExpr::Attribute(attr) = expr else {
                unreachable!()
            };
            attr.value = replacement.into_syntax().body;
        } else {
            *expr = *replacement.into_syntax().body;
        }
        Ok(true)
    }
}
impl Transformer for Capture<'_, '_> {
    fn visit_expr(&self, expression: &mut PyExpr) {
        if self.state.borrow().error.is_some() {
            return;
        }
        if let PyExpr::Compare(compare) = expression {
            for (index, op) in compare.ops.iter().enumerate() {
                if matches!(
                    op,
                    ruff_python_ast::CmpOp::In | ruff_python_ast::CmpOp::NotIn
                ) {
                    if let Err(error) = self.membership(&mut compare.operands[index + 1]) {
                        self.state.borrow_mut().error = Some(error);
                        return;
                    }
                }
            }
        }
        match self.capture(expression) {
            Ok(true) => {}
            Ok(false) => transformer::walk_expr(self, expression),
            Err(error) => self.state.borrow_mut().error = Some(error),
        }
    }
}
