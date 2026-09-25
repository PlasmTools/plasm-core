//! Bounded state iteration over a re-observable Get identity.
use super::super::types::DagNodeSource;
use super::*;
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn iteration(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        seed: &str,
        id: &str,
    ) -> Result<String, String> {
        if self.row_scope.is_some() || call.arguments.args.len() != 1 {
            return Err(at(
                site,
                "iterate requires one step lambda, until=predicate and max_steps=positive integer",
            ));
        }
        let mut until = None;
        let mut bound = None;
        for keyword in &call.arguments.keywords {
            match keyword.arg.as_ref().map(|a| a.as_str()) {
                Some("until") if until.is_none() => until = Some(&keyword.value),
                Some("max_steps") if bound.is_none() => bound = Some(&keyword.value),
                _ => {
                    return Err(at(
                        site,
                        "unknown, duplicate or unpacked iteration argument",
                    ))
                }
            }
        }
        let until = until.ok_or_else(|| at(site, "iterate requires until"))?;
        let take = u32::try_from(integer(
            bound.ok_or_else(|| at(site, "iterate requires max_steps"))?,
        )?)
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| at(site, "max_steps must be a positive u32"))?;
        let seed_node = self.state.get(seed).ok_or("missing iteration seed")?;
        super::super::pipeline::require_iterate_seed_get_identity(seed_node, seed)?;
        let until_predicates = self.comparison_predicates(site, seed, until, true)?;
        let node = self.row_application(site, &call.arguments.args[0], seed, id)?;
        let DagNodeSource::ForEach {
            parsed_template,
            display_expr,
            effect_kind,
            effect_class,
            result_shape,
            qualified_entity,
            mut uses_result,
            ..
        } = node.source
        else {
            return Err(at(site, "invalid iteration step"));
        };
        if !matches!(
            effect_kind,
            PlanNodeKind::Create
                | PlanNodeKind::Update
                | PlanNodeKind::Delete
                | PlanNodeKind::Action
        ) {
            return Err(at(site, "iterate step must be a write or side effect"));
        }
        for predicate in &until_predicates {
            for binding in predicate.value.dependencies() {
                uses_result.push(super::super::plan_serialize::result_use(&binding, &binding));
            }
        }
        let uses_result = super::super::plan_serialize::dedupe_uses(uses_result);
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: true,
            page_size: None,
            source: DagNodeSource::IterateUntil {
                seed: seed.into(),
                parsed_step_template: parsed_template,
                step_display: display_expr,
                effect_kind,
                effect_class,
                result_shape,
                qualified_entity,
                until_body: String::new(),
                until_predicates,
                take,
                uses_result,
            },
        })
    }
}
