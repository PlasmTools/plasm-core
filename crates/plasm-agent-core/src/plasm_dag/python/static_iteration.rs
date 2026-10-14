//! Bounded, literal-only elaboration of Python iteration in the outer DAG.
//! No user Python is executed while constructing the plan.
use super::*;
use crate::plasm_plan::PlanDataInput;
use std::collections::BTreeMap;

const MAX_STATIC_EXPANSIONS: usize = 256;

struct LiteralIds {
    items: Vec<PyExpr>,
}

impl LiteralIds {
    fn literal(iter: &PyExpr) -> Option<Self> {
        let items: Vec<PyExpr> = match iter {
            PyExpr::List(list) => list.elts.to_vec(),
            PyExpr::Tuple(tuple) => tuple.elts.to_vec(),
            _ => return None,
        };
        for item in &items {
            if !matches!(
                literal(item).ok(),
                Some(plasm_core::Value::String(_) | plasm_core::Value::Integer(_))
            ) {
                return None;
            }
        }
        Some(Self { items })
    }
}

impl Lower<'_> {
    /// Eliminate a local, non-escaping collection builder before DAG lowering.
    /// The fresh list is consumed only by append until the bounded loop ends;
    /// the published binding is one immutable value with ordered dependencies.
    pub(super) fn static_collection_builder(
        &mut self,
        initialization: &Stmt,
        iteration: &Stmt,
    ) -> Result<bool, PythonLoweringError> {
        let (Stmt::Assign(initialization), Stmt::For(iteration)) = (initialization, iteration)
        else {
            return Ok(false);
        };
        let [target] = initialization.targets.as_slice() else {
            return Ok(false);
        };
        let Some(collection) = name(target) else {
            return Ok(false);
        };
        if !matches!(initialization.value.as_ref(), PyExpr::List(list) if list.elts.is_empty())
            || iteration.is_async
            || !iteration.orelse.is_empty()
        {
            return Ok(false);
        }
        let [Stmt::Expr(statement)] = iteration.body.as_slice() else {
            return Ok(false);
        };
        let PyExpr::Call(call) = statement.value.as_ref() else {
            return Ok(false);
        };
        let PyExpr::Attribute(method) = call.func.as_ref() else {
            return Ok(false);
        };
        let [element] = call.arguments.args.as_slice() else {
            return Ok(false);
        };
        if method.attr.as_str() != "append"
            || name(&method.value) != Some(collection)
            || !call.arguments.keywords.is_empty()
        {
            return Ok(false);
        }
        let collection = self.static_target(target)?;
        let label = self.static_target(&iteration.target)?;
        if collection == label
            || [element, iteration.iter.as_ref()]
                .into_iter()
                .any(|expression| name(expression) == Some(collection.as_str()))
            || monty_analysis::external_names(&monty::expression_source(element))?
                .iter()
                .any(|(_, name)| name == &collection)
        {
            return Ok(false);
        }
        let items = self.literal_ids(&iteration.iter)?;
        let mut inputs = BTreeMap::new();
        let values = self.static_values(&label, items, element, &mut inputs)?;
        let binding = self.fresh();
        self.emit_value(
            PlasmDataValue::Array { items: values },
            inputs.into_values().collect(),
            &binding,
        )?;
        self.frame.names.insert(collection.clone(), binding);
        self.callbacks.remove(&collection);
        Ok(true)
    }

    fn static_values(
        &mut self,
        label: &str,
        items: LiteralIds,
        expression: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<Vec<PlasmDataValue>, PythonLoweringError> {
        let mut values = Vec::with_capacity(items.items.len());
        for item in items.items {
            self.bind_static_item(label, &item)?;
            // A sibling's captures are not this element's dependencies. Merge
            // ports only after its expression has been lowered in isolation.
            let mut element_inputs = BTreeMap::new();
            values.push(self.scoped_value(expression, &mut element_inputs)?);
            inputs.extend(element_inputs);
        }
        Ok(values)
    }

    fn literal_ids(&self, iter: &PyExpr) -> Result<LiteralIds, PythonLoweringError> {
        let known = match iter {
            PyExpr::Name(name) => self
                .static_sequences
                .get(self.scoped_binding(name.id.as_str()))
                .cloned()
                .map(|items| LiteralIds { items }),
            _ => LiteralIds::literal(iter),
        };
        known.ok_or_else(|| at(iter, PythonSourceError::StaticIterationRequiresLiteralIds))
    }

    pub(super) fn remember_static_sequence(&mut self, binding: &str, expression: &PyExpr) {
        if let Ok(ids) = self.literal_ids(expression) {
            self.static_sequences.insert(binding.to_owned(), ids.items);
        }
    }

    fn static_target(&self, target: &PyExpr) -> Result<String, PythonLoweringError> {
        let label = name(target)
            .ok_or_else(|| at(target, PythonSourceError::StaticIterationTargetShape))?;
        if matches!(
            label,
            "self" | "Program" | "compute" | "Value" | "agg" | "_"
        ) || label.starts_with("__")
            || self.imports.bindings.contains_key(label)
            || self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(label)
                .is_ok()
        {
            return Err(at(
                target,
                PythonSourceError::ReservedStaticIterationName {
                    name: label.to_owned(),
                },
            ));
        }
        Ok(label.to_owned())
    }

    fn bind_static_item(&mut self, label: &str, item: &PyExpr) -> Result<(), PythonLoweringError> {
        self.static_expansions += 1;
        if self.static_expansions > MAX_STATIC_EXPANSIONS {
            return Err(at(
                item,
                PythonSourceError::StaticExpansionLimit {
                    max: MAX_STATIC_EXPANSIONS,
                    actual: self.static_expansions,
                },
            ));
        }
        let binding = self.fresh();
        self.expr(item, Some(&binding))?;
        self.frame.names.insert(label.to_owned(), binding);
        self.callbacks.remove(label);
        Ok(())
    }

    pub(super) fn static_for(
        &mut self,
        statement: &ruff_python_ast::StmtFor,
    ) -> Result<(), PythonLoweringError> {
        if statement.is_async || !statement.orelse.is_empty() {
            return Err(at(
                statement,
                PythonSourceError::StaticForShape {
                    is_async: statement.is_async,
                    has_else: !statement.orelse.is_empty(),
                },
            ));
        }
        let label = self.static_target(&statement.target)?;
        let items = self.literal_ids(&statement.iter)?;
        for item in items.items {
            self.bind_static_item(&label, &item)?;
            for body in &statement.body {
                if self.statement(body)?.is_some() {
                    return Err(at(body, PythonSourceError::StaticIterationReturn));
                }
            }
        }
        Ok(())
    }

    pub(super) fn static_list_comprehension(
        &mut self,
        expression: &PyExpr,
        comprehension: &ruff_python_ast::ExprListComp,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, PythonLoweringError> {
        let [generator] = comprehension.generators.as_slice() else {
            return Err(at(
                expression,
                PythonSourceError::StaticComprehensionIteratorCount {
                    actual: comprehension.generators.len(),
                },
            ));
        };
        if generator.is_async || !generator.ifs.is_empty() {
            return Err(at(
                expression,
                PythonSourceError::StaticComprehensionShape {
                    is_async: generator.is_async,
                    filters: generator.ifs.len(),
                },
            ));
        }
        let label = self.static_target(&generator.target)?;
        let items = self.literal_ids(&generator.iter)?;
        let previous = self.frame.names.get(&label).cloned();
        let previous_callback = self.callbacks.get(&label).cloned();
        let values = self.static_values(&label, items, &comprehension.elt, inputs)?;
        match previous {
            Some(binding) => {
                self.frame.names.insert(label.clone(), binding);
            }
            None => {
                self.frame.names.remove(&label);
            }
        }
        if let Some(callback) = previous_callback {
            self.callbacks.insert(label, callback);
        }
        Ok(PlasmDataValue::Array { items: values })
    }
}
