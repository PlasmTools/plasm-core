//! Scoped map execution inside the enclosing plan's execution context.
use super::step_materialize::{materialize_executable_plan_step, PlanStepMaterializeCtx};
use super::*;
use crate::plasm_plan::ValidatedMapBodyNode;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(super) async fn materialize(
    ctx: &PlanStepMaterializeCtx<'_>,
    map: &ValidatedMapBodyNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<(MaterializedNode, serde_json::Value), String> {
    let body = &map.body;
    let source_id = PlanNodeId::new(body.parent.source.as_str())?;
    let source = materialized
        .get(&source_id)
        .ok_or("map body source not materialized")?;
    if source.qualified_entity.entry_id != body.parent.entity.entry_id
        || source.qualified_entity.entity != body.parent.entity.entity
    {
        return Err("map body capture ownership mismatch".into());
    }
    crate::python_compute::require_complete_collection(&source.result)?;
    body.check_parent_count(source.result.count)?;
    let scoped = entry_scoped_execute_session(ctx.es, Some(&source.qualified_entity))?;
    let rehydrator = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
        &scoped,
        ctx.st,
        ctx.session_id,
        &scoped.cgs,
    );
    let parents = rehydrator
        .resolve_row_source_rows(
            &source.row_source,
            Some(body.max_parents.get() as usize + 1),
        )
        .await?;
    if parents.len() != source.result.count || source.row_identities.len() != parents.len() {
        return Err("map body capture materialization/count mismatch".into());
    }
    body.check_parent_count(parents.len())?;
    let schema = crate::map_body_schema::output_schema(ctx.es, body)?;
    let local = PlanNodeId::new(body.parent.local.as_str())?;
    let layers = body.execution_layers()?;
    let mut rows = Vec::with_capacity(parents.len());
    let mut instances = Vec::with_capacity(parents.len());
    let mut fingerprints: BTreeSet<_> =
        source.result.request_fingerprints.iter().cloned().collect();
    let mut stats = source.result.stats.clone();
    // The template is reviewed once. Each occurrence owns a fresh environment and ordinal.
    use crate::occurrence_progress::{OccurrenceGuard, OccurrencePhase, OccurrenceProgress};
    if parents.is_empty() {
        let mut event = OccurrenceProgress::running(
            vec![map.id.to_string()],
            body.parent.local.to_string(),
            vec![],
        );
        event.phase = OccurrencePhase::NotInvoked;
        if let Some(scope) = ctx.execution_scope {
            scope.report_occurrence(event);
        }
    }
    for (occurrence, parent) in parents.into_iter().enumerate() {
        let mut invocation = OccurrenceGuard::new(
            ctx.execution_scope,
            OccurrenceProgress::running(vec![], map.id.to_string(), vec![occurrence]),
        );
        if let Some(scope) = ctx.execution_scope {
            scope.check()?;
        }
        let mut capture = MaterializedNode::inline_cache(
            source.qualified_entity.clone(),
            vec![parent.clone()],
            vec![source.row_identities[occurrence].clone()],
            "captured parent".into(),
            source.projection.clone(),
        );
        Arc::make_mut(&mut capture.result).entities = json_rows_to_entities_with_refs(
            &source.qualified_entity.entity,
            &[parent],
            Some(&scoped.cgs),
        )?;
        Arc::make_mut(&mut capture.result).coverage = source.result.coverage;
        let mut environment = BTreeMap::from([(local.clone(), capture)]);
        let mut steps = Vec::new();
        // Child operations share cancellation, graph, transport and credentials. Flat UI trace
        // callbacks cannot identify occurrences: publish structured scope evidence below instead.
        let child_ctx = PlanStepMaterializeCtx {
            es: ctx.es,
            st: ctx.st,
            session_id: ctx.session_id,
            plan_shared: ctx.plan_shared,
            approval_policy: ctx.approval_policy,
            flow: ctx.flow,
            trace: None,
            sink: None,
            python_host_calls: ctx.python_host_calls,
            scope_path: vec![map.id.to_string()],
            occurrence_path: vec![occurrence],
            rows_progress: ctx.rows_progress.clone(),
            execution_scope: ctx.execution_scope,
        };
        for layer in &layers {
            for step in layer {
                if let Some(scope) = ctx.execution_scope {
                    scope.check()?;
                }
                let node = map
                    .plan
                    .nodes()
                    .iter()
                    .find(|n| n.id().as_str() == step.as_str())
                    .ok_or("missing body node")?
                    .clone();
                let step_idx = map.plan.node_index(node.id()).ok_or("missing body index")?;
                let outcome = Box::pin(materialize_executable_plan_step(
                    &child_ctx,
                    step_idx,
                    step,
                    node,
                    &environment,
                ))
                .await
                .map_err(|error| {
                    format!(
                        "map body {} occurrence {occurrence} step {step}: {error}",
                        map.id
                    )
                })?;
                fingerprints.extend(outcome.mat.result.request_fingerprints.iter().cloned());
                stats.network_requests += outcome.mat.result.stats.network_requests;
                steps.push(serde_json::json!({
                    "address": {"scope_path": [map.id.as_str()], "local_step": step.as_str()},
                    "occurrence_path": [occurrence], "phase": "done",
                    "rows": outcome.mat.result.count,
                    "artifact_uri": outcome.mat.artifact.as_ref().map(|a| &a.plasm_uri),
                    "request_fingerprints": outcome.mat.result.request_fingerprints,
                }));
                environment.insert(outcome.node_id, outcome.mat);
            }
        }
        let plasm_core::PlasmReturn::Step { step } = &body.body.return_ else {
            return Err("map body must return one step".into());
        };
        let output = environment
            .get(&PlanNodeId::new(step.as_str())?)
            .ok_or("body output missing")?;
        crate::python_compute::require_complete_collection(&output.result)?;
        let values = output
            .row_source
            .inline_rows()
            .ok_or("body output must be inline")?;
        body.check_output_count(values.len())?;
        let row = schema
            .fields
            .iter()
            .map(|field| {
                let value = values[0]
                    .get(field.name.as_str())
                    .ok_or_else(|| format!("map body output {} missing", field.name))?;
                let value_type = field
                    .value_type
                    .as_ref()
                    .ok_or("map body output has no recursive value contract")?;
                value_type.validate(
                    value,
                    &scoped.cgs,
                    &body.parent.entity.entry_id,
                    field.name.as_str(),
                )?;
                Ok((field.name.as_str().to_owned(), value.clone()))
            })
            .collect::<Result<serde_json::Map<_, _>, String>>()?;
        rows.push(serde_json::Value::Object(row));
        invocation.finish(OccurrencePhase::Done, Some(1), None, Vec::new(), None);
        instances.push(serde_json::json!({"occurrence_path":[occurrence], "steps":steps}));
    }
    let node = ValidatedPlanNode::MapBody(map.clone());
    fingerprints.insert(compute_fingerprint(&node, &rows));
    let entity = format!("PlanComputed_{}", map.id);
    let result = ExecutionResult {
        count: rows.len(),
        entities: json_rows_to_entities(&entity, &rows)?,
        has_more: false,
        coverage: source.result.coverage,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats,
        request_fingerprints: fingerprints.into_iter().collect(),
        operations: plasm_runtime::OperationLedger::empty(),
    };
    let display = format!("map body {} ({} parents)", map.id, rows.len());
    let artifact = archive_plasm_result_snapshot(
        ctx.st,
        ctx.es,
        ctx.session_id,
        Some(&source.qualified_entity.entry_id),
        vec![display.clone()],
        &evidence_plan::parsed_expr_for_plan_node(&node),
        &result,
        ctx.trace,
    )
    .await?;
    let evidence = serde_json::json!({"scope_path":[map.id.as_str()],
        "phase": if rows.is_empty() {"not_invoked"} else {"done"}, "completed":rows.len(), "instances":instances});
    Ok((
        MaterializedNode {
            qualified_entity: QualifiedEntityKey {
                entry_id: source.qualified_entity.entry_id.clone(),
                entity,
            },
            row_identities: vec![None; rows.len()],
            row_source: inline_row_source_owned(rows),
            result: Arc::new(result),
            artifact: Some(artifact),
            display,
            projection: None,
        },
        evidence,
    ))
}
