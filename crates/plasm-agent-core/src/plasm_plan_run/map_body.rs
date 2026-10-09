//! Scoped map execution inside the enclosing plan's execution context.
use super::step_materialize::{materialize_executable_plan_step, PlanStepMaterializeCtx};
use super::*;
use crate::plasm_plan::ValidatedMapBodyNode;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug, thiserror::Error)]
enum ScopeBudgetError {
    #[error("scoped composition exceeds the maximum of 65536 total occurrences")]
    OccurrenceLimitExceeded,
}

#[derive(Debug, thiserror::Error)]
enum MaterializeRecordError {
    #[error("map body output field `{field}` is missing")]
    OutputFieldMissing { field: String },
    #[error("map body output field `{field}` has no recursive value contract")]
    ValueContractMissing { field: String },
    #[error("map body output field `{field}` failed {operation}: {source}")]
    ContractFailure {
        field: String,
        operation: MaterializeContractOperation,
        #[source]
        source: plasm_core::value_contract::ValueContractError,
    },
}

#[derive(Debug, thiserror::Error)]
enum MaterializeContractOperation {
    #[error("observation")]
    Observe,
    #[error("validation")]
    Validate,
}

fn scope_failure(code: &'static str, diagnostic: impl Into<String>) -> ExecutionFailure {
    ExecutionFailure::new(plasm_runtime::FailureCause::Program, code, diagnostic)
}

pub(super) async fn materialize(
    ctx: &PlanStepMaterializeCtx<'_>,
    map: &ValidatedMapBodyNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<(MaterializedNode, serde_json::Value), ExecutionFailure> {
    let body = &map.body;
    let filtering = matches!(body.output, plasm_core::plasm_monad::ScopedOutput::Filter);
    let quantifying = match body.output {
        plasm_core::plasm_monad::ScopedOutput::Quantify { all } => Some(all),
        _ => None,
    };
    let mut quantified = quantifying.unwrap_or(false);
    let source_id = PlanNodeId::new(body.parent.source.as_str())?;
    let source = materialized
        .get(&source_id)
        .ok_or_else(|| scope_failure("scope_source_missing", "map body source not materialized"))?;
    if source.qualified_entity.entry_id != body.parent.entity.entry_id
        || (body.parent.contract.entity_authority()
            && source.qualified_entity.entity != body.parent.entity.entity)
    {
        return Err(scope_failure(
            "scope_capture_ownership_mismatch",
            "map body capture ownership mismatch",
        ));
    }
    let flatten = matches!(
        body.output,
        plasm_core::plasm_monad::ScopedOutput::Rows { .. }
    );
    if !flatten {
        crate::python_compute::require_complete_collection(&source.result)?;
    }
    body.check_parent_count(source.result.count())?;
    let scoped = entry_scoped_execute_session(ctx.es, Some(&source.qualified_entity)).map_err(
        |diagnostic| scope_failure("scope_catalog_context_invalid", diagnostic.to_string()),
    )?;
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
        .await
        .map_err(|diagnostic| {
            scope_failure("scope_parent_rehydration_failed", diagnostic.to_string())
        })?;
    if parents.len() != source.result.count()
        || source.row_identities.len() != parents.len()
        || (!source.value_shapes.is_empty() && source.value_shapes.len() != parents.len())
    {
        return Err(scope_failure(
            "scope_materialization_count_mismatch",
            "map body capture materialization/count mismatch",
        ));
    }
    body.check_parent_count(parents.len())?;
    let schema = crate::map_body_schema::output_schema(ctx.es, body).map_err(|diagnostic| {
        scope_failure("scope_output_schema_invalid", diagnostic.to_string())
    })?;
    let local = PlanNodeId::new(body.parent.local.as_str())?;
    let mut captured = BTreeMap::new();
    for port in &body.captures {
        let value = materialized
            .get(&PlanNodeId::new(port.source.as_str())?)
            .ok_or_else(|| {
                scope_failure(
                    "scope_capture_missing",
                    "scoped capture is not materialized",
                )
            })?;
        if !parents.is_empty() && port.singleton && value.result.count() != 1 {
            return Err(scope_failure(
                "scope_singleton_capture_violation",
                format!(
                    "scoped capture {} requires exactly one row; {}",
                    port.source,
                    if value.result.count() == 0 {
                        "zero rows".to_owned()
                    } else {
                        format!("{} rows", value.result.count())
                    }
                ),
            ));
        }
        captured.insert(PlanNodeId::new(port.local.as_str())?, value.clone());
    }
    let mut scope_path = ctx.scope_path.clone();
    scope_path.push(map.id.to_string());
    let mut operations = plasm_runtime::OperationLedger::empty();
    let execution: Result<_, ExecutionFailure> = async {
    let mut execution_source = ExecutionSource::Cache;
    let layers = body.execution_layers()?;
    let mut rows = Vec::with_capacity(parents.len());
    let mut value_shapes = Vec::with_capacity(parents.len());
    let mut identities = Vec::new();
    let mut dependencies = vec![source.result.collection.clone()];
    dependencies.extend(captured.values().map(|m| m.result.collection.clone()));
    let mut instances = Vec::with_capacity(parents.len());
    let mut completed = 0usize;
    let mut fingerprints: BTreeSet<_> =
        source.result.request_fingerprints.iter().cloned().collect();
    let mut stats = source.result.stats.clone();
    // The template is reviewed once. Each occurrence owns a fresh environment and ordinal.
    use crate::occurrence_progress::{OccurrenceGuard, OccurrencePhase, OccurrenceProgress};
    let parent_count = parents.len();
    if parents.is_empty() {
        for node in map.plan.nodes() {
            if let ValidatedPlanNode::Surface(surface) = node {
                let expr = surface
                    .ir
                    .as_ref()
                    .map(|ir| &ir.expr)
                    .or_else(|| surface.ir_template.as_ref().map(|ir| &ir.expr));
                if let Some(ack) = expr.and_then(|expr| {
                    plasm_runtime::OperationAck::try_from_mutating_expr(
                        expr,
                        Some(&scoped.cgs),
                        ExecutionSource::Cache,
                        0,
                        0,
                    )
                }) {
                    operations.merge_ack(ack);
                }
            }
        }
        let mut event = OccurrenceProgress::running(
            scope_path.clone(),
            body.parent.local.to_string(),
            ctx.occurrence_path.clone(),
        );
        event.phase = OccurrencePhase::NotInvoked;
        if let Some(scope) = ctx.execution_scope {
            scope.report_occurrence(event);
        }
    }
    let semantic_parent_rows = source
        .resolve_materialized_source_parents(&rehydrator)
        .await
        .map_err(|diagnostic| {
            scope_failure("scope_parent_identity_invalid", diagnostic.to_string())
        })?;
    let semantic_parents: std::collections::HashMap<_, _> = semantic_parent_rows.iter().enumerate().map(|(index, row)| (&row.reference, index)).collect();
    for (occurrence, parent) in parents.into_iter().enumerate() {
        ctx.scope_budget
            .enter()
            .map_err(|diagnostic| {
                scope_failure("scope_expansion_budget_exceeded", diagnostic.to_string())
            })?;
        let mut occurrence_path = ctx.occurrence_path.clone();
        occurrence_path.push(occurrence);
        let mut invocation = OccurrenceGuard::new(
            ctx.execution_scope,
            OccurrenceProgress::running(
                ctx.scope_path.clone(),
                map.id.to_string(),
                occurrence_path.clone(),
            ),
        );
        if let Some(scope) = ctx.execution_scope {
            scope.check()?;
        }
        let identity = source.row_identities[occurrence].as_ref().map(|row| &row.reference);
        let captured_entities = if let Some(index) = identity.and_then(|identity| semantic_parents.get(identity)) {
            semantic_parent_rows.select([*index])?
        } else {
            rows_to_entities_with_refs(&source.qualified_entity.entity, std::slice::from_ref(&parent), None)?.into()
        };
        use plasm_core::collection_codec::CollectionCodec;
        let record = plasm_core::collection_codec::RecordingCodec::new().record(
            source.result.collection.membership().identity().derived(&("capture", &scope_path, &occurrence_path))?,
            captured_entities.iter().map(|row| row.reference.clone()).collect(),
            plasm_core::collection_codec::Observation::ExactOutput { decoded: 1 })?;
        let collection = plasm_runtime::execution::ExecutionCollection::materialized(record, captured_entities)?;
        let mut capture_result = (*source.result).clone();
        capture_result.collection = collection;
        capture_result.stats = Default::default();
        capture_result.operations = Default::default();
        capture_result.request_fingerprints.clear();
        capture_result.has_more = false;
        capture_result.pagination_resume = None;
        capture_result.paging_handle = None;
        let capture = MaterializedNode {
            value_shapes: vec![source
                .value_shape_at(occurrence)
                .map_err(|diagnostic| {
                    scope_failure("scope_parent_value_shape_invalid", diagnostic.to_string())
                })?],
            optional_fields: source.optional_fields.clone(),
            qualified_entity: source.qualified_entity.clone(),
            result: Arc::new(capture_result),
            row_source: inline_row_source_owned(vec![parent.clone()]),
            row_identities: vec![source.row_identities[occurrence].clone()],
            artifact: None, display: "captured parent".into(), projection: source.projection.clone(),
        };
        let mut environment = captured.clone();
        environment.insert(local.clone(), capture);
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
            scope_path: scope_path.clone(),
            scope_budget: ctx.scope_budget.clone(),
            occurrence_path: occurrence_path.clone(),
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
                    .ok_or_else(|| scope_failure("scope_body_node_missing", "missing body node"))?
                    .clone();
                let step_idx = map.plan.node_index(node.id()).ok_or_else(|| {
                    scope_failure("scope_body_node_index_missing", "missing body index")
                })?;
                let has_effects = matches!(
                    node.effect_class(),
                    EffectClass::Write | EffectClass::SideEffect
                );
                let outcome = Box::pin(materialize_executable_plan_step(
                    &child_ctx,
                    step_idx,
                    step,
                    node,
                    map.plan.nodes(),
                    &environment,
                ))
                .await;
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        invocation.fail(error.to_string());
                        return Err(error.at(step.to_string(), occurrence_path.clone()));
                    }
                };

                if has_effects {
                    for mut ack in outcome.mat.result.operations.entries().iter().cloned() {
                        // Nested scopes already carry their own invocation outcomes.
                        if ack.completed > 0 && ack.outcomes.is_empty() {
                            ack.outcomes
                                .push(plasm_runtime::OperationInvocationOutcome {
                                    source_index: occurrence,
                                    source_identity: source.row_identities[occurrence]
                                        .as_ref()
                                        .map(|identity| identity.reference.to_string()),
                                    status: plasm_runtime::OperationInvocationStatus::Completed,
                                    error: None,
                                });
                        }
                        operations.merge_ack(ack);
                    }
                }
                execution_source = super::plan_fanout_parallel::combine_execution_source(
                    execution_source,
                    outcome.mat.result.source,
                );
                fingerprints.extend(outcome.mat.result.request_fingerprints.iter().cloned());
                stats.network_requests += outcome.mat.result.stats.network_requests;
                steps.push(serde_json::json!({
                    "address": {"scope_path": scope_path, "local_step": step.as_str()},
                    "occurrence_path": occurrence_path, "phase": "done",
                    "rows": outcome.mat.result.count(),
                    "artifact_uri": outcome.mat.artifact.as_ref().map(|a| &a.plasm_uri),
                    "request_fingerprints": outcome.mat.result.request_fingerprints,
                    "operations": outcome.mat.result.operations,
                    "scope_instances": outcome.scope_instances,
                }));
                dependencies.push(outcome.mat.result.collection.clone());
                environment.insert(outcome.node_id, outcome.mat);
            }
        }
        let plasm_core::PlasmReturn::Step { step } = &body.body.return_ else {
            return Err(scope_failure(
                "scope_return_cardinality_invalid",
                "map body must return one step",
            ));
        };
        let output = environment
            .get(&PlanNodeId::new(step.as_str())?)
            .ok_or_else(|| scope_failure("scope_output_missing", "body output missing"))?;
        if flatten {
            let output_scope = entry_scoped_execute_session(ctx.es, Some(&output.qualified_entity))
                .map_err(|diagnostic| scope_failure("scope_output_catalog_context_invalid", diagnostic.to_string()))?;
            let reader = crate::graph_rehydrate::GraphSurfaceRehydrator::new(
                &output_scope,
                ctx.st,
                ctx.session_id,
                &output_scope.cgs,
            );
            let values = reader
                .resolve_row_source_rows(&output.row_source, None)
                .await
                .map_err(|diagnostic| {
                    scope_failure("scope_output_rehydration_failed", diagnostic.to_string())
                })?;
            if values.len() != output.result.count()
                || values.len() != output.row_identities.len()
                || (!output.value_shapes.is_empty() && values.len() != output.value_shapes.len())
            {
                return Err(scope_failure(
                    "scope_output_occurrence_mismatch",
                    "scope output occurrence metadata count mismatch",
                ));
            }
            dependencies.push(output.result.collection.clone());
            completed += 1;
            invocation.finish(
                OccurrencePhase::Done,
                Some(values.len()),
                None,
                Vec::new(),
                None,
            );
            for index in 0..values.len() {
                value_shapes.push(output
                    .value_shape_at(index)
                    .map_err(|diagnostic| {
                        scope_failure("scope_output_value_shape_invalid", diagnostic.to_string())
                    })?);
            }
            rows.extend(values);
            identities.extend(output.row_identities.clone());
            instances.push(serde_json::json!({"occurrence_path":occurrence_path, "steps":steps}));
            continue;
        }
        crate::python_compute::require_complete_collection(&output.result)?;
        let values = output
            .row_source
            .inline_rows()
            .ok_or_else(|| scope_failure("scope_output_not_inline", "body output must be inline"))?;
        body.check_output_count(values.len())?;
        if filtering || quantifying.is_some() {
            let keep = values[0]
                .get("predicate")
                .and_then(plasm_core::Value::as_bool)
                .ok_or_else(|| scope_failure("scope_predicate_result_invalid", "predicate scope did not return a Boolean"))?;
            if filtering && keep {
                rows.push(parent);
                value_shapes.push(source
                    .value_shape_at(occurrence)
                    .map_err(|diagnostic| {
                        scope_failure("scope_parent_value_shape_invalid", diagnostic.to_string())
                    })?);
                identities.push(source.row_identities[occurrence].clone());
            }
            completed += 1;
            invocation.finish(
                OccurrencePhase::Done,
                Some(usize::from(keep)),
                None,
                Vec::new(),
                None,
            );
            instances.push(serde_json::json!({"occurrence_path":occurrence_path, "steps":steps}));
            if quantifying.is_some_and(|all| keep != all) {
                quantified = keep;
                break;
            }
            continue;
        }
        let row = materialize_record(&schema, &values[0], &scoped.cgs, &body.parent.entity.entry_id, &|entry| ctx.es.contexts_by_entry.get(entry).map(|context| context.cgs.as_ref()))
            .map_err(|diagnostic| {
                scope_failure("scope_record_materialization_failed", diagnostic.to_string())
            })?;
        rows.push(plasm_core::ValueRow::from(row));
        value_shapes.push(MaterializedValueShape::Record);
        completed += 1;
        invocation.finish(OccurrencePhase::Done, Some(1), None, Vec::new(), None);
        instances.push(serde_json::json!({"occurrence_path":occurrence_path, "steps":steps}));
    }
    let node = ValidatedPlanNode::MapBody(map.clone());
    if quantifying.is_some() {
        rows.push(plasm_core::ValueRow::from_iter([(
            "predicate".into(),
            plasm_core::Value::Bool(quantified),
        )]));
        value_shapes.push(MaterializedValueShape::Record);
    }
    fingerprints.insert(
        compute_fingerprint(&node, &rows, &value_shapes).map_err(|diagnostic| {
            scope_failure("scope_result_fingerprint_failed", diagnostic.to_string())
        })?,
    );
    let entity = match &body.output {
        plasm_core::plasm_monad::ScopedOutput::Rows { entity, .. } => entity.entity.clone(),
        plasm_core::plasm_monad::ScopedOutput::Record
        | plasm_core::plasm_monad::ScopedOutput::Quantify { .. } => {
            format!("PlanComputed_{}", map.id)
        }
        plasm_core::plasm_monad::ScopedOutput::Filter => source.qualified_entity.entity.clone(),
    };
    let entry_id = match &body.output {
        plasm_core::plasm_monad::ScopedOutput::Rows { entity, .. } => entity.entry_id.clone(),
        plasm_core::plasm_monad::ScopedOutput::Record
        | plasm_core::plasm_monad::ScopedOutput::Quantify { .. } => {
            source.qualified_entity.entry_id.clone()
        }
        plasm_core::plasm_monad::ScopedOutput::Filter => source.qualified_entity.entry_id.clone(),
    };
    let collection = plasm_runtime::execution::ExecutionCollection::evaluate(
        source.result.collection.membership().identity().derived(&("map_body", &map.id, &scope_path))?,
        &dependencies.iter().collect::<Vec<_>>(), rows_to_entities(&entity, &rows)?.into())?;
    let result = ExecutionResult {
        collection,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: execution_source,
        stats,
        request_fingerprints: fingerprints.into_iter().collect(),
        operations: operations.clone(),
    };
    let display = format!("map body {} ({} parents)", map.id, parent_count);
    let artifact = archive_plasm_result_snapshot(
        ctx.st,
        ctx.es,
        ctx.session_id,
        Some(&entry_id),
        vec![display.clone()],
        &evidence_plan::parsed_expr_for_plan_node(&node),
        &result,
        ctx.trace,
    )
    .await
    .map_err(|diagnostic| scope_failure("scope_artifact_persistence_failed", diagnostic.to_string()))?;
    let evidence = serde_json::json!({"scope_path":scope_path,
        "phase": if parent_count == 0 {"not_invoked"} else {"done"}, "occurrences":parent_count, "completed":completed, "rows":rows.len(), "instances":instances});
    Ok((
        MaterializedNode {
            value_shapes,
            optional_fields: schema.optional_fields.clone(),
            qualified_entity: QualifiedEntityKey { entry_id, entity },
            row_identities: if flatten || filtering {
                identities
            } else {
                vec![None; rows.len()]
            },
            row_source: inline_row_source_owned(rows),
            result: Arc::new(result),
            artifact: Some(artifact),
            display,
            projection: if filtering {
                source.projection.clone()
            } else {
                None
            },
        },
        evidence,
    ))
    }.await;
    execution.map_err(|failure| failure.with_effects(&operations))
}

/// A run-wide ceiling shared by all root, sibling and nested map occurrences.
#[derive(Default)]
pub(super) struct ScopeBudget(std::sync::atomic::AtomicUsize);
impl ScopeBudget {
    fn enter(&self) -> Result<(), ScopeBudgetError> {
        use std::sync::atomic::Ordering;
        let mut occurrences = self.0.load(Ordering::Relaxed);
        loop {
            if occurrences >= 65_536 {
                return Err(ScopeBudgetError::OccurrenceLimitExceeded);
            }
            match self.0.compare_exchange_weak(
                occurrences,
                occurrences + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => occurrences = actual,
            }
        }
    }
}

fn materialize_record<'a>(
    schema: &plasm_core::plasm_monad::SyntheticResultSchema,
    values: &plasm_core::ValueRow,
    cgs: &'a plasm_core::CGS,
    entry: &str,
    catalogs: &dyn Fn(&str) -> Option<&'a plasm_core::CGS>,
) -> Result<indexmap::IndexMap<String, plasm_core::Value>, MaterializeRecordError> {
    schema
        .fields
        .iter()
        .map(|field| {
            let value = values.get(field.name.as_str()).ok_or_else(|| {
                MaterializeRecordError::OutputFieldMissing {
                    field: field.name.to_string(),
                }
            })?;
            let value_type = field.value_type.as_ref().ok_or_else(|| {
                MaterializeRecordError::ValueContractMissing {
                    field: field.name.to_string(),
                }
            })?;
            let value = value_type
                .observed_value_in(value, cgs, entry, catalogs)
                .map_err(|source| MaterializeRecordError::ContractFailure {
                    field: field.name.to_string(),
                    operation: MaterializeContractOperation::Observe,
                    source,
                })?;
            value_type
                .validate_in(&value, cgs, entry, field.name.as_str(), catalogs)
                .map_err(|source| MaterializeRecordError::ContractFailure {
                    field: field.name.to_string(),
                    operation: MaterializeContractOperation::Validate,
                    source,
                })?;
            Ok((field.name.as_str().to_owned(), value))
        })
        .collect::<Result<indexmap::IndexMap<_, _>, MaterializeRecordError>>()
}

#[cfg(test)]
mod budget_tests {
    use super::ScopeBudget;
    #[test]
    fn scoped_occurrence_budget_is_shared_and_never_wraps() {
        let budget = std::sync::Arc::new(ScopeBudget::default());
        for _ in 0..65_535 {
            budget.enter().unwrap();
        }
        let child = budget.clone();
        child.enter().unwrap();
        assert!(budget.enter().is_err());
        assert!(child.enter().is_err());
    }
}

#[cfg(test)]
mod federated_record_tests {
    use super::*;
    use plasm_core::value_contract::{ValueContract as T, ValueShape};

    #[test]
    fn map_record_preserves_foreign_nested_value_domains() {
        let foreign = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let parent = plasm_core::CGS::default();
        let count = T::from_domain(
            &foreign,
            "foreign",
            &plasm_core::ValueDomainKey::new("count").unwrap(),
        )
        .unwrap();
        let element = T::record(BTreeMap::from([("n".into(), count)]), Default::default());
        let array = T {
            shape: ValueShape::Array {
                element: Box::new(element),
            },
            domain: None,
            nullable: false,
        };
        let schema = plasm_core::plasm_monad::SyntheticResultSchema::for_value(T::record(
            BTreeMap::from([("matches".into(), array)]),
            Default::default(),
        ))
        .unwrap();
        let rows = plasm_core::ValueRow::from_iter([(
            "matches".into(),
            plasm_core::Value::Array(vec![plasm_core::Value::Object(indexmap::IndexMap::from([
                ("n".into(), plasm_core::Value::from(3_i64)),
            ]))]),
        )]);
        let lookup = |entry: &str| (entry == "foreign").then_some(&foreign);
        let result = materialize_record(&schema, &rows, &parent, "parent", &lookup).unwrap();
        assert_eq!(result.get("matches"), rows.get("matches"));
        let mut wrong_pin = foreign.clone();
        wrong_pin.http_backend.push_str("/changed");
        assert!(matches!(
            materialize_record(&schema, &rows, &parent, "parent", &|_| Some(&wrong_pin))
                .unwrap_err(),
            MaterializeRecordError::ContractFailure {
                source: plasm_core::value_contract::ValueContractError::DomainCatalogPinMismatch { .. },
                ..
            }
        ));
        let invalid = plasm_core::ValueRow::from_iter([(
            "matches".into(),
            plasm_core::Value::Array(vec![plasm_core::Value::Object(indexmap::IndexMap::from([
                ("n".into(), plasm_core::Value::Bool(true)),
            ]))]),
        )]);
        assert!(materialize_record(&schema, &invalid, &parent, "parent", &lookup).is_err());

        assert!(matches!(
            materialize_record(&schema, &rows, &parent, "parent", &|_| None).unwrap_err(),
            MaterializeRecordError::ContractFailure {
                source: plasm_core::value_contract::ValueContractError::DomainCatalogNotLoaded { .. },
                ..
            }
        ));
    }
}
