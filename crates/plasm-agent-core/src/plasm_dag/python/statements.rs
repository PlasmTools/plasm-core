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
                let label = name(&s.targets[0])
                    .ok_or_else(|| at(stmt, PythonSourceError::MutableLocalAssignment))?;
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
                let binding =
                    if self.state.contains(&previous) || self.callbacks.contains_key(label) {
                        self.fresh()
                    } else {
                        previous
                    };
                // Resolve the RHS in the previous environment, then publish the
                // new binding. Previously captured values retain their DAG id.
                self.expr(&s.value, Some(&binding))?;
                self.remember_static_sequence(&binding, &s.value);
                self.frame.names.insert(label.to_owned(), binding);
                self.callbacks.remove(label);
            }
            build_statements::BuildStatement::For(s) => self.static_for(s)?,
            build_statements::BuildStatement::Callback(def) => self.declare_callback(def)?,
            build_statements::BuildStatement::Return(s) => return Ok(Some(s)),
        }
        Ok(None)
    }
}
