//! Scope-only traversal using the kernel's exhaustive operand visitor.
use super::*;
use crate::operand_binding::{BindOperands, IdentityTarget, OperandResolver, ResolvedValue};
use crate::{EntityId, Expr, PlasmInputRef};
use std::collections::BTreeMap;

pub(super) fn check(comp: &PlasmComp, id: &str, payload: &PlasmStepPayload) -> Result<(), String> {
    let deps = comp
        .bind
        .deps
        .get(&StepId(id.into()))
        .cloned()
        .unwrap_or_default();
    let mut aliases: BTreeMap<String, String> = deps
        .iter()
        .map(|s| (s.to_string(), s.to_string()))
        .collect();
    for hole in comp
        .bind
        .holes
        .get(&StepId(id.into()))
        .into_iter()
        .flatten()
    {
        insert_alias(&mut aliases, &hole.alias, hole.step.as_str())?;
    }
    let item = if let PlasmStepPayload::Derive(p) = payload {
        for input in &p.derive.inputs {
            insert_alias(&mut aliases, &input.alias, &input.node)?;
        }
        p.derive.item_binding.as_ref().map(|s| s.as_str())
    } else {
        None
    };
    let mut check = Scope {
        deps: &deps,
        aliases: &aliases,
        item,
    };
    match payload {
        PlasmStepPayload::MapBody(_) => {
            return Err("nested map bodies are outside this slice".into())
        }
        PlasmStepPayload::Invoke(p) => {
            if p.ir.is_some() == p.ir_template.is_some() {
                return Err("correlated read requires exactly one executable IR".into());
            }
            let expr = if let Some(ir) = &p.ir {
                &ir.expr
            } else {
                &p.ir_template.as_ref().expect("checked").expr
            };
            check_read(expr)?;
            expr.bind_operands(&mut check)?;
            for predicate in &p.predicates {
                predicate.bind_operands(&mut check)?;
            }
        }
        PlasmStepPayload::FlatMapRelation(p) => {
            check_read(&p.relation.ir.expr)?;
            p.relation.ir.expr.bind_operands(&mut check)?;
        }
        PlasmStepPayload::Pure(p) => {
            p.data.bind_operands(&mut check)?;
        }
        PlasmStepPayload::Derive(p) => {
            p.derive.value.bind_operands(&mut check)?;
        }
        PlasmStepPayload::Map(p) => {
            if let ComputeOp::Filter { predicates } = &p.compute.op {
                for predicate in predicates.iter() {
                    predicate.bind_operands(&mut check)?;
                }
            }
        }
        PlasmStepPayload::FlatMapApply(_) | PlasmStepPayload::UnfoldUntil(_) => {
            return Err("nested effect control flow is outside the correlated slice".into());
        }
    }
    Ok(())
}

fn check_read(expr: &Expr) -> Result<(), String> {
    if let Expr::Chain(chain) = expr {
        if !matches!(chain.step, crate::ChainStep::AutoGet) {
            return Err("correlated relation must use catalog materialization".into());
        }
        return check_read(&chain.source);
    }
    if !matches!(expr, Expr::Query(_) | Expr::Get(_)) {
        return Err("correlated slice read IR must be Query or Get".into());
    }
    Ok(())
}

fn insert_alias(
    aliases: &mut BTreeMap<String, String>,
    alias: &str,
    source: &str,
) -> Result<(), String> {
    if alias.trim().is_empty() || aliases.get(alias).is_some_and(|old| old != source) {
        return Err("correlated body has an empty or conflicting input alias".into());
    }
    aliases.insert(alias.into(), source.into());
    Ok(())
}

struct Scope<'a> {
    deps: &'a BTreeSet<StepId>,
    aliases: &'a BTreeMap<String, String>,
    item: Option<&'a str>,
}
impl Scope<'_> {
    fn dependency(&self, node: &str) -> Result<(), String> {
        if !self.deps.contains(&StepId(node.into())) {
            return Err(format!(
                "correlated operand uses undeclared dependency {node}"
            ));
        }
        Ok(())
    }
    fn alias(&self, alias: &str) -> Result<(), String> {
        let node = self
            .aliases
            .get(alias)
            .ok_or_else(|| format!("correlated operand escapes scope via {alias}"))?;
        self.dependency(node)
    }
}
impl OperandResolver for Scope<'_> {
    type Error = String;
    fn resolve(&mut self, reference: &PlasmInputRef) -> Result<ResolvedValue, String> {
        match reference {
            PlasmInputRef::NodeInput { node, .. } => self.alias(node)?,
            PlasmInputRef::RowBinding { binding, .. } if self.item == Some(binding.as_str()) => {}
            PlasmInputRef::RowBinding { binding, .. } => {
                return Err(format!(
                    "correlated operand escapes row scope via {binding}"
                ))
            }
        }
        Ok(ResolvedValue::null())
    }
    fn node(&mut self, node: &str, alias: &str, _path: &[String]) -> Result<ResolvedValue, String> {
        self.dependency(node)?;
        if self.aliases.get(alias).is_none_or(|source| source != node) {
            return Err("correlated node operand has an undeclared alias".into());
        }
        Ok(ResolvedValue::null())
    }
    fn identity(
        &mut self,
        _: IdentityTarget<'_>,
        reference: &PlasmInputRef,
    ) -> Result<EntityId, String> {
        self.resolve(reference)?;
        Ok(EntityId::from("scope-validation-only"))
    }
    fn string(
        &mut self,
        template: &crate::program_string_template::CompiledProgramString,
    ) -> Result<String, String> {
        for root in template.roots() {
            if self.item != Some(root.as_str()) {
                self.alias(root)?;
            }
        }
        Ok(String::new())
    }
}
