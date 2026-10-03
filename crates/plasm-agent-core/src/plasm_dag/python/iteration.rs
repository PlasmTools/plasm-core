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
    ) -> Result<String, PythonLoweringError> {
        if call.arguments.args.len() != 1 {
            return Err(at(
                site,
                "iterate requires one step callback, until=predicate and max_steps=positive integer",
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
        let callback = self.callback(until)?;
        let body = self.scoped_callback_body(
            site,
            seed,
            &callback,
            std::num::NonZeroU32::new(1).expect("positive bound"),
            super::body::ScopeMode::Filter,
        )?;
        crate::map_body_schema::output_schema(self.es, &body)?;
        let until_predicates: Vec<crate::plasm_plan::PlanPredicate> = Vec::new();
        let until_scope = Some(Box::new(body));
        let step = self.callback(&call.arguments.args[0])?;
        let step_scope = Box::new(self.scoped_callback_body(
            site,
            seed,
            &step,
            std::num::NonZeroU32::new(1).expect("positive"),
            super::body::ScopeMode::Rows,
        )?);
        let effect = plasm_core::plasm_monad::correlated::iteration_step_effect(&step_scope)
            .map_err(|error| at(site, &format!("invalid iteration step: {error}")))?;
        let effect_kind = crate::plasm_step_convert::surface_kind_to_plan(effect.kind)?;
        let effect_class = effect.effect_class;
        let result_shape = effect.result_shape;
        let qualified_entity = QualifiedEntityKey {
            entry_id: effect.qualified_entity.entry_id,
            entity: effect.qualified_entity.entity,
        };
        let display_expr = effect.expr_template;
        let parsed_template = crate::plasm_plan::PlanExprTemplate {
            expr: effect.ir_template.expr,
            projection: effect.ir_template.projection,
            display_expr: effect.ir_template.display_expr,
            input_bindings: effect
                .ir_template
                .input_bindings
                .into_iter()
                .map(|b| crate::plasm_plan::PlanInputBinding {
                    from: b.from,
                    to: b.to,
                })
                .collect(),
        };
        let mut uses_result = step_scope
            .captures
            .iter()
            .map(|capture| {
                super::super::plan_serialize::result_use(
                    capture.source.as_str(),
                    capture.local.as_str(),
                )
            })
            .collect::<Vec<_>>();
        for predicate in &until_predicates {
            for binding in predicate.value.dependencies() {
                uses_result.push(super::super::plan_serialize::result_use(&binding, &binding));
            }
        }
        if let Some(body) = &until_scope {
            for capture in &body.captures {
                uses_result.push(super::super::plan_serialize::result_use(
                    capture.source.as_str(),
                    capture.local.as_str(),
                ));
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
                until_scope,
                step_scope: Some(step_scope),
                take,
                uses_result,
            },
        })
    }
}
