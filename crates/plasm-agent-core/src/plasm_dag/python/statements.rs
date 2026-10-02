//! Bind Python locals to immutable DAG versions; assignments never mutate nodes.
use super::*;
impl Lower<'_> {
    pub(super) fn statement<'s>(
        &mut self,
        stmt: &'s Stmt,
    ) -> Result<Option<&'s ruff_python_ast::StmtReturn>, String> {
        match build_statements::BuildStatement::classify(stmt)? {
            build_statements::BuildStatement::Documentation(()) => {}
            build_statements::BuildStatement::Effect(s) => {
                let id = self.expr(&s.value, None)?;
                let node = self.state.get(&id).ok_or("missing statement node")?;
                if !matches!(
                    node.source.effect_class(),
                    EffectClass::Write | EffectClass::SideEffect
                ) {
                    return Err(at(stmt, "unused expression statements must be writes"));
                }
            }
            build_statements::BuildStatement::Binding(s) => {
                let label = name(&s.targets[0])
                    .ok_or_else(|| at(stmt, "only immutable local assignments are admitted"))?;
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
                    return Err(at(stmt, "reserved binding name"));
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
                self.frame.names.insert(label.to_owned(), binding);
                self.callbacks.remove(label);
            }
            build_statements::BuildStatement::Callback(def) => self.declare_callback(def)?,
            build_statements::BuildStatement::Return(s) => return Ok(Some(s)),
        }
        Ok(None)
    }
}
