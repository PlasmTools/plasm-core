use super::super::*;
use super::eval::{instantiate_raw_expr_template, materialized_result_use_inputs};

pub(crate) fn for_each_cross_uses(for_each: &ValidatedForEachNode) -> Vec<PlanResultUse> {
    for_each
        .uses_result
        .iter()
        .filter(|u| u.r#as.as_str() != for_each.item_binding.as_str())
        .cloned()
        .collect()
}

pub(crate) fn for_each_plan_eval_env<'a>(
    for_each: &'a ValidatedForEachNode,
    row: &'a serde_json::Value,
    input_rows: &'a BTreeMap<InputAlias, MaterializedInputRow>,
) -> PlanEvalEnv<'a> {
    let scope = EvalScope::Bound {
        row,
        binding: &for_each.item_binding,
    };
    let inputs = InputEnv { rows: input_rows };
    PlanEvalEnv {
        scope,
        inputs,
        wire_coercion: None,
    }
}

#[cfg(test)]
pub(crate) fn render_for_each_expressions(
    for_each: &ValidatedForEachNode,
    source_rows: &[serde_json::Value],
    materialized: Option<&BTreeMap<PlanNodeId, MaterializedNode>>,
) -> Result<Vec<String>, String> {
    let input_rows = if let Some(materialized) = materialized {
        materialized_result_use_inputs(materialized, &for_each_cross_uses(for_each))?
    } else {
        BTreeMap::new()
    };
    source_rows
        .iter()
        .map(|row| {
            let env = for_each_plan_eval_env(for_each, row, &input_rows);
            super::eval::render_expr_template(&for_each.effect_template.expr_template, &env)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_for_each_node(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node_index: usize,
    for_each: &ValidatedForEachNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<crate::plan_execute_shared::PlanLineExecuteShared>>,
) -> Result<MaterializedNode, String> {
    let source_rows = materialized_rows(es, st, session_id, materialized, &for_each.source).await?;
    let input_rows = materialized_result_use_inputs(materialized, &for_each_cross_uses(for_each))?;
    let mut parsed_steps = Vec::with_capacity(source_rows.len());
    let mut expressions = Vec::with_capacity(source_rows.len());
    for row in &source_rows {
        let env = for_each_plan_eval_env(for_each, row, &input_rows);
        let parsed = instantiate_raw_expr_template(&for_each.effect_template.ir_template, &env)?;
        expressions.push(crate::expr_display::expr_display(&parsed.expr));
        parsed_steps.push(parsed);
    }
    let scoped_es =
        entry_scoped_execute_session(es, Some(&for_each.effect_template.qualified_entity))?;

    let parallel_reads = !crate::plasm_plan_run::for_each_body_mutates_remote(
        for_each.effect_template.kind,
        for_each.effect_template.effect_class,
    );
    let mut jobs = Vec::with_capacity(parsed_steps.len());
    for (row_index, parsed_expr) in parsed_steps.into_iter().enumerate() {
        let expr_label = expressions
            .get(row_index)
            .cloned()
            .unwrap_or_else(|| "<ir>".to_string());
        super::super::plan_fanout_parallel::push_row_job(
            &mut jobs,
            node_index,
            row_index,
            expr_label,
            parsed_expr,
        );
    }
    let fold = super::super::plan_fanout_parallel::execute_row_fanout(
        st,
        &scoped_es,
        session_id,
        jobs,
        trace,
        sink,
        plan_shared,
        super::super::plan_fanout_parallel::RowFanoutPolicy::for_each(
            parallel_reads,
            source_rows.len(),
        ),
    )
    .await?;
    super::super::materialize::archive_materialize_for_each_fanout(
        st,
        es,
        session_id,
        &scoped_es,
        for_each,
        fold,
        source_rows.len(),
        expressions,
        trace,
    )
    .await
}
