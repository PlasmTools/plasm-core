//! Template-ref collection for plan `uses_result` / `ir_template`.

use super::super::prelude::*;
use crate::plasm_plan::{Plan, PlanDataInput, PlanResultUse};

pub(in crate::plasm_dag) fn collect_template_uses_from_expr(
    expr: &Expr,
    row_binding: Option<&str>,
) -> Vec<PlanResultUse> {
    let ctx = plasm_core::TemplateRefContext {
        row_binding,
        input_aliases: &[],
    };
    let mut acc = Vec::new();
    collect_expr_for_template_uses(&mut acc, expr, &ctx);
    dedupe_uses(acc)
}

/// `uses_result` for relation plan nodes: per-row `source` plus any `node_input` aliases (e.g. `repo`).
pub(in crate::plasm_dag) fn relation_plan_uses_result(
    source_label: &str,
    parsed: &plasm_core::expr_parser::ParsedExpr,
) -> Vec<PlanResultUse> {
    let mut uses = vec![result_use(source_label, "source")];
    for u in collect_template_uses_from_expr(&parsed.expr, None) {
        let node = u.node.as_str();
        let alias = u.r#as.as_str();
        if node == source_label || (node == "source" && alias == "source") {
            continue;
        }
        uses.push(if node == "source" {
            result_use(source_label, alias)
        } else {
            u
        });
    }
    dedupe_uses(uses)
}

/// Records upstream plan nodes so `node_input` holes become `uses_result` → `ir_template` + instantiation
/// before compile.
///
/// Surfaces covered: query predicates; **get**/**delete**/**invoke** `path_vars`; invoke/create payloads (values
/// recurse into objects/arrays). [`Expr::Get`] compound identity literals and Binding slots live on
/// `reference` ([`IdentitySlot`]). [`PlasmInputRef::RowBinding`] is skipped on purpose (`for_each` row scope).
pub(in crate::plasm_dag) fn collect_expr_for_template_uses(
    acc: &mut Vec<PlanResultUse>,
    expr: &Expr,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    collect_operands(acc, expr, ctx);
}

fn collect_operands<T: plasm_core::operand_binding::BindOperands>(
    acc: &mut Vec<PlanResultUse>,
    value: &T,
    ctx: &plasm_core::TemplateRefContext<'_>,
) {
    use plasm_core::operand_binding::{OperandResolver, ResolvedValue};
    struct Collect<'a, 'b> {
        acc: &'a mut Vec<PlanResultUse>,
        ctx: &'a plasm_core::TemplateRefContext<'b>,
    }
    impl OperandResolver for Collect<'_, '_> {
        type Error = std::convert::Infallible;
        fn resolve(&mut self, reference: &PlasmInputRef) -> Result<ResolvedValue, Self::Error> {
            if let PlasmInputRef::NodeInput { node, .. } = reference {
                self.acc.push(result_use(node, node));
            }
            Ok(ResolvedValue::null())
        }
        fn identity(
            &mut self,
            _target: plasm_core::operand_binding::IdentityTarget<'_>,
            reference: &PlasmInputRef,
        ) -> Result<plasm_core::EntityId, Self::Error> {
            self.resolve(reference)?;
            Ok(plasm_core::EntityId::from("reference-inspection"))
        }
        fn string(
            &mut self,
            value: &plasm_core::program_string_template::CompiledProgramString,
        ) -> Result<String, Self::Error> {
            for root in value.roots() {
                if self.ctx.row_binding == Some(root.as_str()) {
                    continue;
                }
                let dotted = value
                    .paths()
                    .iter()
                    .any(|path| path.len() > 1 && path.first() == Some(root));
                if self.ctx.row_binding.is_none() || dotted {
                    self.acc.push(result_use(root, root));
                }
            }
            Ok(value.source().to_owned())
        }
    }
    let Ok(_) = value.bind_operands(&mut Collect { acc, ctx });
}

pub(in crate::plasm_dag) fn result_use(node: &str, alias: &str) -> PlanResultUse {
    PlanResultUse {
        node: node.to_owned(),
        r#as: alias.to_owned(),
        qualified_entity: None,
    }
}

pub(in crate::plasm_dag) fn dedupe_uses(uses: Vec<PlanResultUse>) -> Vec<PlanResultUse> {
    let mut seen = BTreeSet::new();
    uses.into_iter()
        .filter(|u| seen.insert((u.node.clone(), u.r#as.clone())))
        .collect()
}

pub(in crate::plasm_dag) fn dedupe_inputs(inputs: Vec<PlanDataInput>) -> Vec<PlanDataInput> {
    let mut seen = BTreeSet::new();
    inputs
        .into_iter()
        .filter(|u| seen.insert((u.node.clone(), u.alias.clone())))
        .collect()
}

/// Resolve dependency provenance from typed source nodes before artifact serialization.
pub(in crate::plasm_dag) fn stamp_plan_uses_result_qualified_entities(
    plan: &mut Plan,
) -> Result<(), String> {
    let uses = plan
        .nodes
        .iter()
        .map(|node| {
            crate::plasm_plan::enrich_uses_result_provenance(&node.uses_result, plan, &node.id)
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (node, uses) in plan.nodes.iter_mut().zip(uses) {
        node.uses_result = uses;
    }
    Ok(())
}
