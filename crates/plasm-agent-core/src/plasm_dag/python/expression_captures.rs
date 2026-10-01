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

    pub(super) fn deferred_expression(&self, expression: &PyExpr) -> bool {
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
                            || self.deferred_expression(&attr.value)
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
    ) -> Result<CapturedExpression, String> {
        let before = self.state.nodes.len();
        let mut expression = expression.clone();
        let capture = Capture {
            state: RefCell::new(State {
                lower: self,
                inputs,
                fields: BTreeMap::new(),
                references: BTreeMap::new(),
                locals: Vec::new(),
                error: None,
            }),
        };
        capture.visit_expr(&mut expression);
        let state = capture.state.into_inner();
        if let Some(error) = state.error {
            return Err(error);
        }
        for node in &state.lower.state.nodes[before..] {
            let node = super::super::plan_serialize::lower_plan_node(node)?;
            if matches!(
                node.effect_class,
                EffectClass::Write | EffectClass::SideEffect
            ) {
                return Err("Python value expressions cannot acquire write authority".into());
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
    locals: Vec<String>,
    error: Option<String>,
}
struct Capture<'a, 'b> {
    state: RefCell<State<'a, 'b>>,
}

fn bind(target: &PyExpr, names: &mut Vec<String>) {
    match target {
        PyExpr::Name(name) => names.push(name.id.to_string()),
        PyExpr::Tuple(tuple) => tuple.elts.iter().for_each(|e| bind(e, names)),
        PyExpr::List(list) => list.elts.iter().for_each(|e| bind(e, names)),
        PyExpr::Starred(star) => bind(&star.value, names),
        _ => {}
    }
}
impl Capture<'_, '_> {
    fn membership(&self, expression: &mut PyExpr) -> Result<(), String> {
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
        for parameter in state.lower.scope_names.keys() {
            super::membership::validate_closed_rhs(&original, parameter)?;
        }
        let path =
            super::super::row_suffix::membership_rhs_column_path(&state.lower.state, &[], node)?;
        let [field] = path.as_slice() else {
            return Err("membership requires an explicit one-column rowset".into());
        };
        let code = format!(
            "[__plasm_member.{field} for __plasm_member in __plasm_input.{}]",
            captured.attr
        );
        *expression = *ruff_python_parser::parse_expression(&code)
            .map_err(|e| e.to_string())?
            .into_syntax()
            .body;
        Ok(())
    }
    fn comprehensions(
        &self,
        clauses: &mut [ruff_python_ast::Comprehension],
        result: &mut PyExpr,
        key: Option<&mut PyExpr>,
    ) {
        let depth = self.state.borrow().locals.len();
        for clause in clauses {
            self.visit_expr(&mut clause.iter);
            bind(&clause.target, &mut self.state.borrow_mut().locals);
            for filter in &mut clause.ifs {
                self.visit_expr(filter);
            }
        }
        if let Some(key) = key {
            self.visit_expr(key);
        }
        self.visit_expr(result);
        self.state.borrow_mut().locals.truncate(depth);
    }
    fn capture(&self, expr: &mut PyExpr) -> Result<bool, String> {
        let mut state = self.state.borrow_mut();
        let local = super::quantifiers::expression_root(expr)
            .is_some_and(|n| state.locals.iter().any(|local| local == n));
        if local {
            return Ok(false);
        }
        let field = matches!(expr, PyExpr::Attribute(attr) if state.lower.expression_owner(&attr.value).is_some())
            && !state.lower.deferred_expression(expr);
        let deferred =
            state.lower.deferred_expression(expr) || state.lower.lazy_deferred_expression(expr);
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
                    .ok_or("missing expression receiver contract")?;
                if !contract.row_cardinality.permits_scalar_field_extract() {
                    return Err("field input requires a proven singleton".into());
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
                let name =
                    ruff_python_parser::parse_expression(&node).map_err(|e| e.to_string())?;
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
            .map_err(|e| e.to_string())?;
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
        match expression {
            PyExpr::Compare(compare) => {
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
            PyExpr::Generator(value) => {
                self.comprehensions(&mut value.generators, &mut value.elt, None);
                return;
            }
            PyExpr::ListComp(value) => {
                self.comprehensions(&mut value.generators, &mut value.elt, None);
                return;
            }
            PyExpr::SetComp(value) => {
                self.comprehensions(&mut value.generators, &mut value.elt, None);
                return;
            }
            PyExpr::DictComp(value) => {
                self.comprehensions(
                    &mut value.generators,
                    &mut value.value,
                    value.key.as_deref_mut(),
                );
                return;
            }
            PyExpr::Lambda(value) => {
                let depth = self.state.borrow().locals.len();
                if let Some(parameters) = &mut value.parameters {
                    for parameter in parameters
                        .posonlyargs
                        .iter_mut()
                        .chain(parameters.args.iter_mut())
                        .chain(parameters.kwonlyargs.iter_mut())
                    {
                        if let Some(default) = &mut parameter.default {
                            self.visit_expr(default);
                        }
                    }
                    let mut state = self.state.borrow_mut();
                    state.locals.extend(
                        parameters
                            .posonlyargs
                            .iter()
                            .chain(parameters.args.iter())
                            .chain(parameters.kwonlyargs.iter())
                            .map(|p| p.parameter.name.to_string()),
                    );
                    if let Some(p) = &parameters.vararg {
                        state.locals.push(p.name.to_string());
                    }
                    if let Some(p) = &parameters.kwarg {
                        state.locals.push(p.name.to_string());
                    }
                }
                self.visit_expr(&mut value.body);
                self.state.borrow_mut().locals.truncate(depth);
                return;
            }
            _ => {}
        }
        match self.capture(expression) {
            Ok(true) => {}
            Ok(false) => transformer::walk_expr(self, expression),
            Err(error) => self.state.borrow_mut().error = Some(error),
        }
    }
}
