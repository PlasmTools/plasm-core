//! Bind Python locals to immutable DAG versions; assignments never mutate nodes.
use super::*;
impl Lower<'_> {
    pub(super) fn statement<'s>(
        &mut self,
        stmt: &'s Stmt,
    ) -> Result<Option<&'s ruff_python_ast::StmtReturn>, PythonLoweringError> {
        match build_statements::BuildStatement::classify(stmt)
            .map_err(|error| error.correction())?
        {
            build_statements::BuildStatement::Documentation(()) => {}
            build_statements::BuildStatement::Effect(s) => {
                let id = self.expr(&s.value, None)?;
                let node = self.state.get(&id).ok_or(
                    crate::program_rejection::PythonLoweringInvariantError::StatementNodeMissing,
                )?;
                if !node.source.is_write_or_side_effect() {
                    return Err(at(stmt, PythonSourceError::UnusedNonWriteExpression));
                }
            }
            build_statements::BuildStatement::Binding(s) => {
                if let Some(label) = name(&s.targets[0]) {
                    self.bind_local(stmt, label, &s.value)?;
                } else {
                    self.unpack_binding(stmt, &s.targets[0], &s.value)?;
                }
            }
            build_statements::BuildStatement::For(s) => self.static_for(s)?,
            build_statements::BuildStatement::Callback(def) => self.declare_callback(def)?,
            build_statements::BuildStatement::Return(s) => return Ok(Some(s)),
        }
        Ok(None)
    }

    fn bind_local(
        &mut self,
        stmt: &Stmt,
        label: &str,
        value: &PyExpr,
    ) -> Result<(), PythonLoweringError> {
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
                stmt,
                PythonSourceError::ReservedBindingName {
                    name: label.to_owned(),
                },
            ));
        }
        let previous = self.scoped_binding(label).to_owned();
        let binding = if self.state.contains(&previous) || self.callbacks.contains_key(label) {
            self.fresh()
        } else {
            previous
        };
        // Resolve the RHS in the previous environment, then publish the
        // new binding. Previously captured values retain their DAG id.
        self.expr(value, Some(&binding))?;
        self.remember_static_sequence(&binding, value);
        self.frame.names.insert(label.to_owned(), binding);
        self.callbacks.remove(label);
        Ok(())
    }

    fn unpack_binding(
        &mut self,
        stmt: &Stmt,
        target: &PyExpr,
        value: &PyExpr,
    ) -> Result<(), PythonLoweringError> {
        if !matches!(target, PyExpr::Tuple(_) | PyExpr::List(_)) {
            return Err(at(stmt, PythonSourceError::MutableLocalAssignment));
        }
        // Acquire once in the old environment. Publish names only after the
        // complete structural target has passed its positional contract checks.
        let source = self.expr(value, None)?;
        let schema = super::text::inferred_schema(self.es, &self.state, &source, 0)?;
        if schema.entity.is_some()
            || schema.fields.len() != 1
            || schema.fields[0].name.as_str() != "value"
        {
            return Err(at(target, PythonSourceError::UnpackRequiresFixedTuple));
        }
        let row = schema.row_contract()?;
        let contract = row.field("value")?;
        let mut work = vec![(target, contract, source)];
        let mut bindings = Vec::new();
        while let Some((target, contract, expression)) = work.pop() {
            if let Some(label) = name(target) {
                bindings.push((label.to_owned(), expression));
                continue;
            }
            let targets = match target {
                PyExpr::Tuple(tuple) => &tuple.elts,
                PyExpr::List(list) => &list.elts,
                _ => return Err(at(target, PythonSourceError::MutableLocalAssignment)),
            };
            let contracts = fixed_tuple_positions(contract).map_err(|error| at(target, error))?;
            let positions = contracts.len();
            if targets.len() != positions {
                return Err(at(
                    target,
                    PythonSourceError::UnpackArity {
                        targets: targets.len(),
                        positions,
                    },
                ));
            }
            for (index, (target, contract)) in targets.iter().zip(contracts).enumerate().rev() {
                work.push((target, contract, format!("{expression}[{index}]")));
            }
        }
        let mut acquired = Vec::with_capacity(bindings.len());
        for (label, expression) in bindings {
            let parsed = ruff_python_parser::parse_expression(&expression)
                .map_err(PythonLoweringError::parse_error)?;
            acquired.push((label, self.expr(&parsed.syntax().body, None)?));
        }
        // Freeze every leaf before publishing any target: a target may shadow
        // the name of the tuple being unpacked, or another target.
        for (label, source) in acquired {
            let parsed = ruff_python_parser::parse_expression(&source)
                .map_err(PythonLoweringError::parse_error)?;
            self.bind_local(stmt, &label, &parsed.syntax().body)?;
        }
        Ok(())
    }
}

fn fixed_tuple_positions(
    contract: plasm_core::value_contract::ValueContract,
) -> Result<Vec<plasm_core::value_contract::ValueContract>, PythonSourceError> {
    use plasm_core::value_contract::{ValueContract, ValueShape};
    let mut pending = vec![contract];
    let mut positions: Option<Vec<ValueContract>> = None;
    while let Some(contract) = pending.pop() {
        if contract.nullable {
            return Err(PythonSourceError::UnpackRequiresFixedTuple);
        }
        match contract.shape {
            ValueShape::Union { variants } => pending.extend(variants),
            ValueShape::Tuple { members } if members.variable.is_none() => {
                let fields = members
                    .prefix
                    .into_iter()
                    .chain(members.suffix)
                    .collect::<Vec<_>>();
                if let Some(previous) = &mut positions {
                    if previous.len() != fields.len() {
                        return Err(PythonSourceError::UnpackRequiresFixedTuple);
                    }
                    for (previous, field) in previous.iter_mut().zip(fields) {
                        *previous = ValueContract::join(previous.clone(), field);
                    }
                } else {
                    positions = Some(fields);
                }
            }
            _ => return Err(PythonSourceError::UnpackRequiresFixedTuple),
        }
    }
    positions.ok_or(PythonSourceError::UnpackRequiresFixedTuple)
}
