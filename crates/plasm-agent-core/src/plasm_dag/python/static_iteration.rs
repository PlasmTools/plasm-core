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
        let mut values = Vec::with_capacity(items.items.len());
        for item in items.items {
            self.bind_static_item(&label, &item)?;
            values.push(self.scoped_value(&comprehension.elt, inputs)?);
        }
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
