//! Scope-only traversal using the kernel's exhaustive operand visitor.
use super::*;
use crate::operand_binding::{BindOperands, IdentityTarget, OperandResolver, ResolvedValue};
use crate::{EntityId, Expr, PlasmInputRef};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CorrelatedScopeError {
    #[error("correlated body has an empty or conflicting input alias")]
    InvalidInputAlias,
    #[error("correlated operation requires exactly one executable IR")]
    InvalidExecutableIrCardinality,
    #[error("correlated relation must use catalog materialization")]
    RelationRequiresCatalogMaterialization,
    #[error("correlated slice read IR must be Query or Get")]
    InvalidSliceReadKind,
    #[error("correlated operand uses undeclared dependency `{node}`")]
    UndeclaredDependency { node: String },
    #[error("correlated operand escapes scope via alias `{alias}`")]
    AliasEscapesScope { alias: String },
    #[error("correlated operand escapes row scope via binding `{binding}`")]
    BindingEscapesRowScope { binding: String },
    #[error("correlated node operand has an undeclared alias for node `{node}`")]
    NodeAliasMismatch { node: String },
}

pub(super) fn check(
    comp: &PlasmComp,
    id: &str,
    payload: &PlasmStepPayload,
) -> Result<(), CorrelatedScopeError> {
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
        PlasmStepPayload::MapBody(child) => {
            check.dependency(child.parent.source.as_str())?;
            for capture in &child.captures {
                check.dependency(capture.source.as_str())?;
            }
        }
        PlasmStepPayload::Invoke(p) => {
            if p.ir.is_some() == p.ir_template.is_some() {
                return Err(CorrelatedScopeError::InvalidExecutableIrCardinality);
            }
            let expr = if let Some(ir) = &p.ir {
                &ir.expr
            } else {
                &p.ir_template.as_ref().expect("checked").expr
            };

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
        PlasmStepPayload::FlatMapApply(p) => {
            check.item = Some(p.item_binding.as_str());
            p.effect_template
                .ir_template
                .expr
                .bind_operands(&mut check)?;
            for predicate in &p.predicates {
                predicate.bind_operands(&mut check)?;
            }
        }
        PlasmStepPayload::UnfoldUntil(p) => {
            check.item = Some(p.item_binding.as_str());
            p.effect_template
                .ir_template
                .expr
                .bind_operands(&mut check)?;
            for predicate in &p.until_predicates {
                predicate.bind_operands(&mut check)?;
            }
            if let Some(seed) = &p.seed_ir {
                seed.expr.bind_operands(&mut check)?;
            }
        }
    }
    Ok(())
}

fn check_read(expr: &Expr) -> Result<(), CorrelatedScopeError> {
    if let Expr::Chain(chain) = expr {
        if !matches!(chain.step, crate::ChainStep::AutoGet) {
            return Err(CorrelatedScopeError::RelationRequiresCatalogMaterialization);
        }
        return check_read(&chain.source);
    }
    if !matches!(expr, Expr::Query(_) | Expr::Get(_)) {
        return Err(CorrelatedScopeError::InvalidSliceReadKind);
    }
    Ok(())
}

fn insert_alias(
    aliases: &mut BTreeMap<String, String>,
    alias: &str,
    source: &str,
) -> Result<(), CorrelatedScopeError> {
    if alias.trim().is_empty() || aliases.get(alias).is_some_and(|old| old != source) {
        return Err(CorrelatedScopeError::InvalidInputAlias);
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
    fn dependency(&self, node: &str) -> Result<(), CorrelatedScopeError> {
        if !self.deps.contains(&StepId(node.into())) {
            return Err(CorrelatedScopeError::UndeclaredDependency { node: node.into() });
        }
        Ok(())
    }
    fn alias(&self, alias: &str) -> Result<(), CorrelatedScopeError> {
        let node =
            self.aliases
                .get(alias)
                .ok_or_else(|| CorrelatedScopeError::AliasEscapesScope {
                    alias: alias.into(),
                })?;
        self.dependency(node)
    }
}
impl OperandResolver for Scope<'_> {
    type Error = CorrelatedScopeError;
    fn resolve(&mut self, reference: &PlasmInputRef) -> Result<ResolvedValue, Self::Error> {
        match reference {
            PlasmInputRef::NodeInput { node, .. } => self.alias(node)?,
            PlasmInputRef::RowBinding { binding, .. } if self.item == Some(binding.as_str()) => {}
            PlasmInputRef::RowBinding { binding, .. } => {
                return Err(CorrelatedScopeError::BindingEscapesRowScope {
                    binding: binding.as_str().into(),
                })
            }
        }
        Ok(ResolvedValue::null())
    }
    fn node(
        &mut self,
        node: &str,
        alias: &str,
        _path: &[String],
    ) -> Result<ResolvedValue, Self::Error> {
        self.dependency(node)?;
        if self.aliases.get(alias).is_none_or(|source| source != node) {
            return Err(CorrelatedScopeError::NodeAliasMismatch { node: node.into() });
        }
        Ok(ResolvedValue::null())
    }
    fn identity(
        &mut self,
        _: IdentityTarget<'_>,
        reference: &PlasmInputRef,
    ) -> Result<EntityId, Self::Error> {
        self.resolve(reference)?;
        Ok(EntityId::from("scope-validation-only"))
    }
    fn string(
        &mut self,
        template: &crate::program_string_template::CompiledProgramString,
    ) -> Result<String, Self::Error> {
        for root in template.roots() {
            if self.item != Some(root.as_str()) {
                self.alias(root)?;
            }
        }
        Ok(String::new())
    }
}
