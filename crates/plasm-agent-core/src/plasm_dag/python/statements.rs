//! Shared immutable statement lowering for root and scoped callback bodies.
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
                    &node.source,
                    super::super::types::DagNodeSource::Surface {
                        effect_class: EffectClass::Write | EffectClass::SideEffect,
                        ..
                    } | super::super::types::DagNodeSource::IterateUntil {
                        effect_class: EffectClass::Write | EffectClass::SideEffect,
                        ..
                    } | super::super::types::DagNodeSource::ForEach {
                        effect_class: EffectClass::Write | EffectClass::SideEffect,
                        ..
                    }
                ) && !matches!(&node.source, super::super::types::DagNodeSource::MapBody { body, .. }
                    if matches!(body.effect_class(), EffectClass::Write | EffectClass::SideEffect))
                {
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
                let binding = self.scoped_binding(label).to_owned();
                if self.state.contains(&binding) || self.callbacks.contains_key(label) {
                    return Err(at(stmt, "rebinding is not admitted"));
                }
                self.expr(&s.value, Some(&binding))?;
            }
            build_statements::BuildStatement::Callback(def) => self.declare_callback(def)?,
            build_statements::BuildStatement::Return(s) => return Ok(Some(s)),
        }
        Ok(None)
    }
}
