//! Post-materialize compute and template rendering.

use super::*;

pub(crate) async fn eval_compute_with_row_source(
    compute: &ComputeTemplate,
    row_source: &MaterializedRowSource,
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    cgs: &CGS,
) -> Result<Vec<serde_json::Value>, String> {
    match row_source {
        MaterializedRowSource::Inline(rows) => eval_compute_from_rows(compute, rows),
        MaterializedRowSource::GraphBacked {
            entity_type,
            logical_count,
            hot_snapshot,
        } => {
            if compute_needs_full_materialize(&compute.op) {
                let rows =
                    crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs)
                        .rehydrate_rows(
                            std::sync::Arc::clone(hot_snapshot),
                            entity_type,
                            *logical_count,
                        )
                        .await?;
                return eval_compute_from_rows(compute, &rows);
            }
            eval_compute_streaming(
                compute,
                es,
                st,
                session_id,
                entity_type,
                cgs,
                std::sync::Arc::clone(hot_snapshot),
            )
            .await
        }
    }
}

pub(crate) async fn eval_compute_streaming(
    compute: &ComputeTemplate,
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    entity_type: &str,
    cgs: &CGS,
    hot_snapshot: std::sync::Arc<[plasm_runtime::CachedEntity]>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    let limit = match &compute.op {
        ComputeOp::Limit { count } => Some(*count),
        _ => None,
    };
    crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs)
        .stream_entity_rows(hot_snapshot, entity_type, |row| {
            match &compute.op {
                ComputeOp::Filter { predicates } => {
                    if predicates.iter().all(|p| predicate_matches(row, p)) {
                        out.push(row.clone());
                    }
                }
                ComputeOp::Limit { .. } => out.push(row.clone()),
                ComputeOp::Project { fields } => {
                    let mut obj = serde_json::Map::new();
                    for (name, path) in fields {
                        obj.insert(
                            name.as_str().to_string(),
                            value_at_path(row, path)
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        );
                    }
                    out.push(serde_json::Value::Object(obj));
                }
                _ => {}
            }
            limit.is_some_and(|cap| out.len() >= cap)
        })
        .await?;
    if let ComputeOp::Limit { count } = &compute.op {
        out.truncate(*count);
    }
    Ok(out)
}

pub(crate) fn eval_compute_from_rows(
    compute: &ComputeTemplate,
    rows: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, String> {
    match &compute.op {
        ComputeOp::Project { fields } => rows
            .iter()
            .map(|row| {
                let mut out = serde_json::Map::new();
                for (name, path) in fields {
                    out.insert(
                        name.as_str().to_string(),
                        value_at_path(row, path)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
                Ok(serde_json::Value::Object(out))
            })
            .collect(),
        ComputeOp::Filter { predicates } => Ok(rows
            .iter()
            .filter(|row| predicates.iter().all(|p| predicate_matches(row, p)))
            .cloned()
            .collect()),
        ComputeOp::GroupBy { keys, aggregates } => group_rows(rows, keys, aggregates),
        ComputeOp::Aggregate { aggregates } => aggregate_rows(rows, aggregates),
        ComputeOp::Sort { key, descending } => {
            let mut sorted = rows.to_vec();
            sorted
                .sort_by(|a, b| cmp_json_sort_values(value_at_path(a, key), value_at_path(b, key)));
            if *descending {
                sorted.reverse();
            }
            Ok(sorted)
        }
        ComputeOp::Limit { count } => Ok(rows.iter().take(*count).cloned().collect()),
        ComputeOp::DedupeBy { keys } => dedupe_rows(rows, keys),
        ComputeOp::Render { columns, template } => render_compute(rows, columns, template),
    }
}

pub(crate) fn materialized_singleton_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    inputs: &[ValidatedPlanDataInput],
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for input in inputs {
        let mat = materialized.get(&input.node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                input.node.as_str(),
                input.alias.as_str()
            )
        })?;
        if mat.inline_row_count() != 1 {
            return Err(singleton_input_row_count_error(
                input.node.as_str(),
                input.alias.as_str(),
                mat.inline_row_count(),
                format!("{:?} broadcast", input.proof).as_str(),
            ));
        }
        let row = mat.first_inline_row().cloned().ok_or_else(|| {
            format!(
                "Plan input {:?} for alias {:?} expected one row but was empty",
                input.node.as_str(),
                input.alias.as_str()
            )
        })?;
        out.insert(
            input.alias.clone(),
            MaterializedInputRow {
                node: input.node.clone(),
                proof: input.proof,
                row: augment_row_json_with_identity(
                    &row,
                    mat.row_identities.first().and_then(|i| i.as_ref()),
                ),
                row_identity: mat.row_identities.first().cloned().flatten(),
            },
        );
    }
    Ok(out)
}

pub(crate) fn materialized_result_use_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
        })?;
        if mat.inline_row_count() != 1 {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                mat.inline_row_count(),
                "staged expression rendering",
            ));
        }
        let row = mat.first_inline_row().cloned().ok_or_else(|| {
            format!(
                "Plan input {:?} for alias {:?} expected one row but was empty",
                node.as_str(),
                alias.as_str()
            )
        })?;
        out.insert(
            alias,
            MaterializedInputRow {
                node,
                proof: crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
                row: augment_row_json_with_identity(
                    &row,
                    mat.row_identities.first().and_then(|i| i.as_ref()),
                ),
                row_identity: mat.row_identities.first().cloned().flatten(),
            },
        );
    }
    Ok(out)
}

pub(crate) fn singleton_input_row_count_error(
    node: &str,
    alias: &str,
    row_count: usize,
    context: &str,
) -> String {
    if row_count == 0 {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced zero rows. This is a data-empty result, not a Plasm syntax error: run or inspect {node:?}, loosen filters if it should match, branch around empty results, or use `.singleton()` only when exactly one row is guaranteed."
        )
    } else {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced {row_count} rows. Add filters/projection to make the source unique, aggregate intentionally, or use `.singleton()` only when exactly one row is guaranteed."
        )
    }
}

pub(crate) fn materialized_result_use_inputs_with_source_row(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
    source_node: &PlanNodeId,
    source_row: &serde_json::Value,
    source_row_identity: Option<plasm_core::RowIdentity>,
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
        })?;
        let (row, row_identity) = if node == *source_node {
            (
                augment_row_json_with_identity(source_row, source_row_identity.as_ref()),
                source_row_identity.clone(),
            )
        } else {
            if mat.inline_row_count() != 1 {
                return Err(singleton_input_row_count_error(
                    node.as_str(),
                    alias.as_str(),
                    mat.inline_row_count(),
                    "staged expression rendering",
                ));
            }
            let row = mat.first_inline_row().cloned().ok_or_else(|| {
                format!(
                    "Plan input {:?} for alias {:?} expected one row but was empty",
                    node.as_str(),
                    alias.as_str()
                )
            })?;
            (
                augment_row_json_with_identity(
                    &row,
                    mat.row_identities.first().and_then(|i| i.as_ref()),
                ),
                mat.row_identities.first().cloned().flatten(),
            )
        };
        out.insert(
            alias,
            MaterializedInputRow {
                node,
                proof: crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
                row,
                row_identity,
            },
        );
    }
    Ok(out)
}

pub(crate) fn instantiate_parsed_expr_plan_inputs_with_rows(
    parsed: ParsedExpr,
    input_rows: &BTreeMap<InputAlias, MaterializedInputRow>,
    wire_coercion: Option<WireCoercionCtx<'_>>,
) -> Result<ParsedExpr, String> {
    if input_rows.is_empty() {
        return Ok(parsed);
    }
    let scope = EvalScope::Root {
        row: &serde_json::Value::Null,
    };
    let inputs = InputEnv { rows: input_rows };
    let env = PlanEvalEnv {
        scope,
        inputs,
        wire_coercion,
    };
    let expr_json = serde_json::to_value(&parsed.expr)
        .map_err(|e| format!("serialize expr for hole instantiation: {e}"))?;
    let expr_json = instantiate_expr_template_value(&expr_json, &env)?;
    let expr: Expr = serde_json::from_value(expr_json)
        .map_err(|e| format!("deserialize expr after hole instantiation: {e}"))?;
    Ok(ParsedExpr {
        expr,
        projection: parsed.projection,
    })
}

/// Deserialize → [`instantiate_expr_template_value`] → deserialize so predicate/CML env holes (e.g.
/// `__plasm_hole` `node_input`) become concrete row JSON **before** HTTP compile — parity with dry-run
/// topology checks that assumed splattable scope rows.
pub(crate) fn instantiate_parsed_expr_plan_inputs(
    parsed: ParsedExpr,
    uses_result: &[PlanResultUse],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<ParsedExpr, String> {
    if uses_result.is_empty() {
        return Ok(parsed);
    }
    let input_rows = materialized_result_use_inputs(materialized, uses_result)?;
    instantiate_parsed_expr_plan_inputs_with_rows(parsed, &input_rows, None)
}

pub(crate) fn wire_coercion_ctx_for_source_entity<'a>(
    cgs: &'a CGS,
    source_entity_name: &str,
) -> Option<WireCoercionCtx<'a>> {
    let ent = cgs.get_entity(source_entity_name)?;
    Some(WireCoercionCtx {
        cgs,
        source_entity: ent,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_relation_singleton_chain(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node_index: usize,
    relation: &ValidatedRelationTraversalNode,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<&crate::plan_execute_shared::PlanLineExecuteShared>,
) -> Result<MaterializedNode, String> {
    let pe = ParsedExpr {
        expr: relation.relation.ir.expr.clone(),
        projection: relation.relation.ir.projection.clone(),
    };
    let parsed = instantiate_parsed_expr_plan_inputs(pe, &relation.uses_result, materialized)?;
    let expr_label = relation
        .relation
        .ir
        .display_expr
        .as_deref()
        .unwrap_or("<ir>");
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let (parsed, result, artifact) = execute_plasm_parsed_expr(
        st,
        &scoped_es,
        session_id,
        expr_label,
        parsed,
        trace,
        node_index as i64,
        None,
        None,
        None,
        plan_shared,
    )
    .await?;
    if let Some(sink) = sink {
        trace_record_plasm_line(sink, node_index, expr_label, &parsed, &result, &scoped_es).await;
    }
    finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        MaterializedNode {
            entry_id: relation.relation.target.entry_id.clone(),
            entity: relation.relation.target.entity.clone(),
            display: crate::expr_display::expr_display(&parsed.expr),
            projection: parsed.projection,
            row_source: inline_row_source(&[]),
            row_identities: row_identities_from_entities(
                &scoped_es,
                relation.relation.target.entity.as_str(),
                &result.entities,
            ),
            result: Arc::new(result),
            artifact,
        },
        trace,
    )
    .await
}

/// When view `relation_outputs` (or other embed paths) populated `CachedEntity.relations`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_materialize_from_cached_relation_refs(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    source_mat: &MaterializedNode,
    trace: Option<&PlasmTraceContext>,
) -> Result<Option<MaterializedNode>, String> {
    let rel_name = relation.relation.relation.as_str();
    let target_entity = relation.relation.target.entity.as_str();
    let parents = &source_mat.result.entities;
    if parents.is_empty() || !parents.iter().all(|p| p.relations.contains_key(rel_name)) {
        return Ok(None);
    }
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let graph = scoped_es.lock_graph_cache().await;
    let Some(entities) =
        collect_all_embedded_relation_targets(rel_name, target_entity, parents, &graph)
    else {
        return Ok(None);
    };
    drop(graph);
    let count = entities.len();
    let source_rows: Vec<serde_json::Value> = source_mat
        .row_source
        .inline_rows()
        .map(|rows| rows.to_vec())
        .unwrap_or_default();
    let full_result = ExecutionResult {
        count,
        entities: entities.clone(),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Cache,
        stats: ExecutionStats {
            duration_ms: 0,
            network_requests: 0,
            cache_hits: count,
            cache_misses: 0,
            ..Default::default()
        },
        request_fingerprints: vec![compute_fingerprint(node, &source_rows)],
    };
    let parsed_preimage = crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node(node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(relation.relation.target.entry_id.as_str()),
        vec![format!(
            "plan.relation({}) cached_embed",
            relation.id.as_str()
        )],
        &parsed_preimage,
        &full_result,
        trace,
    )
    .await?;
    let rows: Vec<_> = full_result
        .entities
        .iter()
        .map(|e| cached_entity_row_json(e, scoped_es.cgs.as_ref()))
        .collect();
    finalize_typed_relation_materialized_node(
        st,
        es,
        session_id,
        &relation.relation.target,
        MaterializedNode {
            entry_id: relation.relation.target.entry_id.clone(),
            entity: relation.relation.target.entity.clone(),
            display: format!("plan.relation({}) cached_embed", relation.id.as_str()),
            projection: relation.relation.ir.projection.clone(),
            row_source: inline_row_source_owned(rows),
            row_identities: row_identities_from_entities(
                &scoped_es,
                target_entity,
                &full_result.entities,
            ),
            result: Arc::new(full_result),
            artifact: Some(artifact),
        },
        trace,
    )
    .await
    .map(Some)
}

/// When every parent row has fully resolved embed refs in the session graph.
pub(crate) fn collect_all_embedded_relation_targets(
    relation_name: &str,
    target_entity: &str,
    parents: &[CachedEntity],
    graph: &plasm_runtime::GraphCache,
) -> Option<Vec<CachedEntity>> {
    let mut out = Vec::new();
    for parent in parents {
        if !parent.relations.contains_key(relation_name) {
            return None;
        }
        let refs = parent.relations.get(relation_name)?;
        for r in refs {
            if r.entity_type.as_str() != target_entity {
                return None;
            }
            out.push(graph.get(r)?.clone());
        }
    }
    Some(out)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn materialize_relation_scoped_fanout(
    st: &PlasmHostState,
    es: &ExecuteSession,
    session_id: &str,
    node_index: usize,
    node: &ValidatedPlanNode,
    relation: &ValidatedRelationTraversalNode,
    source_mat: &MaterializedNode,
    source_rows: &[serde_json::Value],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<&crate::plan_execute_shared::PlanLineExecuteShared>,
) -> Result<MaterializedNode, String> {
    let pe = ParsedExpr {
        expr: relation.relation.ir.expr.clone(),
        projection: relation.relation.ir.projection.clone(),
    };
    let scoped_es = entry_scoped_execute_session(es, Some(&relation.relation.target))?;
    let source_node = &relation.relation.source;
    let mut entities = Vec::new();
    let mut request_fingerprints = Vec::new();
    let mut stats = ExecutionStats {
        duration_ms: 0,
        network_requests: 0,
        cache_hits: 0,
        cache_misses: 0,
        ..Default::default()
    };
    let mut source = ExecutionSource::Cache;
    let base_display = relation
        .relation
        .ir
        .display_expr
        .clone()
        .unwrap_or_else(|| format!("plan.relation({})", relation.id.as_str()));

    for (row_index, source_row) in source_rows.iter().enumerate() {
        let row_identity = source_mat
            .row_identities
            .get(row_index)
            .and_then(|i| i.as_ref())
            .cloned();
        let input_rows = materialized_result_use_inputs_with_source_row(
            materialized,
            &relation.uses_result,
            source_node,
            source_row,
            row_identity,
        )?;
        let wire_coercion =
            wire_coercion_ctx_for_source_entity(scoped_es.cgs.as_ref(), source_mat.entity.as_str());
        let parsed =
            instantiate_parsed_expr_plan_inputs_with_rows(pe.clone(), &input_rows, wire_coercion)?;
        let trace_line_index = node_index
            .checked_mul(1000)
            .and_then(|base| base.checked_add(row_index))
            .unwrap_or(node_index);
        let expr_label = format!("{base_display} [row {row_index}]");
        crate::execute_pipeline::PlasmPreflight::preflight_parsed_line(
            &scoped_es,
            &expr_label,
            &parsed,
        )
        .map_err(|e| e.to_string())?;
        let (parsed, result, _artifact) = run_parsed_plasm_line(
            &expr_label,
            &scoped_es,
            st,
            session_id,
            parsed,
            trace,
            trace_line_index as i64,
            None,
            None,
            None,
            Some(plasm_core::PreflightToken::VERIFIED),
            plan_shared,
        )
        .await
        .map_err(|e| match e {
            crate::http_execute::RunLineError::Parse(d)
            | crate::http_execute::RunLineError::Normalize(d)
            | crate::http_execute::RunLineError::Projection(d) => d,
            crate::http_execute::RunLineError::Operation(_) => {
                "operation continuation is not valid inside a plan surface node".to_string()
            }
            crate::http_execute::RunLineError::Runtime(e, src) => {
                format!("{e}\nsource expression: {src}")
            }
            crate::http_execute::RunLineError::ArtifactSerialization(e) => {
                format!("artifact serialization failed: {e}")
            }
            crate::http_execute::RunLineError::ArtifactPersist(d) => {
                format!("run artifact persist failed: {d}")
            }
            crate::http_execute::RunLineError::StaleGraphEpoch { .. } => {
                "session graph changed during concurrent execute; retry the request".to_string()
            }
        })?;
        if let Some(sink) = sink {
            trace_record_plasm_line(
                sink,
                trace_line_index,
                &expr_label,
                &parsed,
                &result,
                &scoped_es,
            )
            .await;
        }
        source = combine_execution_source(source, result.source);
        stats.duration_ms = stats.duration_ms.saturating_add(result.stats.duration_ms);
        stats.network_requests = stats
            .network_requests
            .saturating_add(result.stats.network_requests);
        stats.merge_telemetry(&result.stats.cache);
        stats.cache_hits = stats.cache.legacy_cache_hits();
        stats.cache_misses = stats.cache.legacy_cache_misses();
        request_fingerprints.extend(result.request_fingerprints);
        entities.extend(result.entities);
    }

    let full_result = ExecutionResult {
        count: entities.len(),
        entities: entities.clone(),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints: request_fingerprints.clone(),
    };
    let parsed_preimage = crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node(node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(relation.relation.target.entry_id.as_str()),
        vec![format!(
            "plan.relation({}) fanout {} rows",
            relation.id.as_str(),
            source_rows.len()
        )],
        &parsed_preimage,
        &full_result,
        trace,
    )
    .await?;
    let display = format!(
        "plan.relation({}) fanout ({} source rows)",
        relation.id.as_str(),
        source_rows.len()
    );
    let rows: Vec<_> = full_result
        .entities
        .iter()
        .map(|e| cached_entity_row_json(e, scoped_es.cgs.as_ref()))
        .collect();
    Ok(MaterializedNode {
        entry_id: relation.relation.target.entry_id.clone(),
        entity: relation.relation.target.entity.clone(),
        display,
        projection: relation.relation.ir.projection.clone(),
        row_source: inline_row_source_owned(rows),
        row_identities: row_identities_from_entities(
            &scoped_es,
            relation.relation.target.entity.as_str(),
            &full_result.entities,
        ),
        result: Arc::new(full_result),
        artifact: Some(artifact),
    })
}

pub(crate) fn instantiate_expr_template(
    template: &ValidatedPlanExprTemplate,
    env: &PlanEvalEnv<'_>,
) -> Result<ParsedExpr, String> {
    let expr_json = instantiate_expr_template_value(&template.expr, env)?;
    let expr = serde_json::from_value(expr_json)
        .map_err(|e| format!("templated Plasm IR instantiation failed: {e}"))?;
    Ok(ParsedExpr {
        expr,
        projection: template.projection.clone(),
    })
}

pub(crate) fn instantiate_raw_expr_template(
    template: &PlanExprTemplate,
    env: &PlanEvalEnv<'_>,
) -> Result<ParsedExpr, String> {
    let expr_json = instantiate_expr_template_value(&template.expr, env)?;
    let expr = serde_json::from_value(expr_json)
        .map_err(|e| format!("templated Plasm IR instantiation failed: {e}"))?;
    Ok(ParsedExpr {
        expr,
        projection: template.projection.clone(),
    })
}

pub(crate) fn instantiate_expr_template_value(
    value: &serde_json::Value,
    env: &PlanEvalEnv<'_>,
) -> Result<serde_json::Value, String> {
    if let Some(hole) = value
        .as_object()
        .and_then(|obj| obj.get("__plasm_hole"))
        .and_then(|v| v.as_object())
    {
        return instantiate_ir_hole(hole, env);
    }
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| instantiate_expr_template_value(item, env))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| Ok((k.clone(), instantiate_expr_template_value(v, env)?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(serde_json::Value::Object),
        serde_json::Value::String(s) => {
            if !plasm_core::contains_dollar_interpolation(s) {
                return Ok(serde_json::Value::String(s.clone()));
            }
            let scope = plan_binding_scope_owned(env);
            let out = plasm_core::interpolate_string_map(s, &scope)
                .map_err(|e| format!("string interpolation: {e}"))?;
            Ok(serde_json::Value::String(out))
        }
        other => Ok(other.clone()),
    }
}

pub(crate) fn plan_binding_scope_owned(
    env: &PlanEvalEnv<'_>,
) -> BTreeMap<String, plasm_core::Value> {
    let mut scope = BTreeMap::new();
    for (alias, input) in env.inputs.rows {
        let row_value = json_row_to_plasm_value(&input.row);
        scope.insert(alias.as_str().to_string(), row_value.clone());
        scope.insert(input.node.as_str().to_string(), row_value);
    }
    if let EvalScope::Bound { row, binding } = &env.scope {
        scope.insert(binding.as_str().to_string(), json_row_to_plasm_value(row));
    }
    scope
}

pub(crate) fn json_row_to_plasm_value(row: &serde_json::Value) -> plasm_core::Value {
    match row {
        serde_json::Value::Null => plasm_core::Value::Null,
        serde_json::Value::Bool(b) => plasm_core::Value::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(plasm_core::Value::Integer)
            .or_else(|| n.as_f64().map(plasm_core::Value::Float))
            .unwrap_or(plasm_core::Value::Null),
        serde_json::Value::String(s) => plasm_core::Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            plasm_core::Value::Array(items.iter().map(json_row_to_plasm_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut out = indexmap::IndexMap::new();
            for (k, v) in map {
                out.insert(k.clone(), json_row_to_plasm_value(v));
            }
            plasm_core::Value::Object(out)
        }
    }
}

pub(crate) fn augment_row_json_with_identity(
    row: &serde_json::Value,
    identity: Option<&plasm_core::RowIdentity>,
) -> serde_json::Value {
    let Some(identity) = identity else {
        return row.clone();
    };
    let mut obj = match row {
        serde_json::Value::Object(map) => map.clone(),
        other => {
            let mut m = serde_json::Map::new();
            m.insert("value".to_string(), other.clone());
            m
        }
    };
    let primary = identity.reference.primary_slot_str();
    obj.entry("id".to_string())
        .or_insert_with(|| serde_json::Value::String(primary.clone()));
    for (k, v) in &identity.ambient {
        obj.entry(k.clone())
            .or_insert_with(|| serde_json::Value::String(v.clone()));
    }
    if let plasm_core::EntityKey::Compound(parts) = &identity.reference.key {
        for (k, v) in parts {
            obj.entry(k.clone())
                .or_insert_with(|| serde_json::Value::String(v.clone()));
        }
    }
    serde_json::Value::Object(obj)
}

pub(crate) fn coerce_node_input_json(
    ctx: Option<&WireCoercionCtx<'_>>,
    path: &[String],
    value: serde_json::Value,
) -> serde_json::Value {
    let Some(ctx) = ctx else {
        return value;
    };
    let Some(field) = path.last().map(String::as_str) else {
        return value;
    };
    match plasm_core::parent_entity_field_type(ctx.cgs, ctx.source_entity, field) {
        Ok(ft) => {
            let nv = ctx
                .source_entity
                .fields
                .get(field)
                .and_then(|f| f.named_value(ctx.cgs).ok());
            plasm_core::coerce_json_value_for_field_type(
                &ft,
                nv.and_then(|n| n.value_format),
                nv.and_then(|n| n.array_items.as_ref()),
                value,
            )
        }
        Err(_) => value,
    }
}

pub(crate) fn node_input_hole_from_identity(
    ctx: Option<&WireCoercionCtx<'_>>,
    identity: &Option<plasm_core::RowIdentity>,
    path: &[String],
    row: &serde_json::Value,
) -> Option<serde_json::Value> {
    let identity = identity.as_ref()?;
    if path.is_empty() {
        let slot = identity.reference.primary_slot_str();
        return Some(coerce_node_input_json(
            ctx,
            path,
            serde_json::Value::String(slot),
        ));
    }
    if path.len() == 1 {
        let key = path[0].as_str();
        if key == "id" {
            let slot = identity.reference.primary_slot_str();
            return Some(coerce_node_input_json(
                ctx,
                path,
                serde_json::Value::String(slot),
            ));
        }
        if let Some(v) = identity.ambient.get(key) {
            return Some(coerce_node_input_json(
                ctx,
                path,
                serde_json::Value::String(v.clone()),
            ));
        }
        if let plasm_core::EntityKey::Compound(parts) = &identity.reference.key {
            if let Some(v) = parts.get(key) {
                let raw = ctx
                    .map(|c| plasm_core::identity_slot_to_json(c.cgs, c.source_entity, key, v))
                    .unwrap_or_else(|| serde_json::Value::String(v.clone()));
                return Some(coerce_node_input_json(ctx, path, raw));
            }
        }
    }
    value_at_segments(row, path)
        .cloned()
        .map(|v| coerce_node_input_json(ctx, path, v))
}

pub(crate) fn instantiate_ir_hole(
    hole: &serde_json::Map<String, serde_json::Value>,
    env: &PlanEvalEnv<'_>,
) -> Result<serde_json::Value, String> {
    let kind = hole
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "IR value hole is missing kind".to_string())?;
    let path = hole
        .get("path")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    match kind {
        "binding" => {
            let binding = hole
                .get("binding")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "binding IR hole is missing binding".to_string())?;
            let EvalScope::Bound {
                binding: scope_binding,
                ..
            } = &env.scope
            else {
                return Err("binding IR hole cannot be used outside a row scope".to_string());
            };
            if binding != scope_binding.as_str() {
                return Err(format!(
                    "binding IR hole references {binding:?}, but active binding is {:?}",
                    scope_binding.as_str()
                ));
            }
            Ok(value_at_segments(env.scope.row(), &path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        "node_input" => {
            let alias = hole
                .get("alias")
                .and_then(|v| v.as_str())
                .or_else(|| hole.get("node").and_then(|v| v.as_str()))
                .ok_or_else(|| "node_input IR hole is missing alias".to_string())?;
            let alias = InputAlias::new(alias.to_string())?;
            let input = env.inputs.rows.get(&alias).ok_or_else(|| {
                format!("node_input IR hole references unavailable alias {alias:?}")
            })?;
            let from_row = value_at_segments(&input.row, &path).cloned();
            let from_row_usable = from_row
                .as_ref()
                .is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty()));
            if from_row_usable {
                return Ok(coerce_node_input_json(
                    env.wire_coercion.as_ref(),
                    &path,
                    from_row.unwrap(),
                ));
            }
            if let Some(value) = node_input_hole_from_identity(
                env.wire_coercion.as_ref(),
                &input.row_identity,
                &path,
                &input.row,
            ) {
                if value.as_str().is_none_or(|s| !s.is_empty()) {
                    return Ok(value);
                }
            }
            Ok(from_row.unwrap_or(serde_json::Value::Null))
        }
        other => Err(format!("unknown IR value hole kind {other:?}")),
    }
}

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
            render_expr_template(&for_each.effect_template.expr_template, &env)
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
    plan_shared: Option<&crate::plan_execute_shared::PlanLineExecuteShared>,
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
    let mut entities = Vec::new();
    let mut request_fingerprints = Vec::new();
    let mut stats = ExecutionStats {
        duration_ms: 0,
        network_requests: 0,
        cache_hits: 0,
        cache_misses: 0,
        ..Default::default()
    };
    let mut source = ExecutionSource::Cache;
    let mut displays = Vec::new();
    let scoped_es =
        entry_scoped_execute_session(es, Some(&for_each.effect_template.qualified_entity))?;

    for (row_index, parsed_expr) in parsed_steps.into_iter().enumerate() {
        let trace_line_index = node_index
            .checked_mul(1000)
            .and_then(|base| base.checked_add(row_index))
            .unwrap_or(node_index);
        let expr_label = expressions
            .get(row_index)
            .map(String::as_str)
            .unwrap_or("<ir>");
        let (parsed, result, _artifact) = execute_plasm_parsed_expr(
            st,
            &scoped_es,
            session_id,
            expr_label,
            parsed_expr,
            trace,
            trace_line_index as i64,
            None,
            None,
            None,
            plan_shared,
        )
        .await?;
        if let Some(sink) = sink {
            trace_record_plasm_line(
                sink,
                trace_line_index,
                expr_label,
                &parsed,
                &result,
                &scoped_es,
            )
            .await;
        }
        source = combine_execution_source(source, result.source);
        stats.duration_ms = stats.duration_ms.saturating_add(result.stats.duration_ms);
        stats.network_requests = stats
            .network_requests
            .saturating_add(result.stats.network_requests);
        stats.cache_hits = stats.cache_hits.saturating_add(result.stats.cache_hits);
        stats.cache_misses = stats.cache_misses.saturating_add(result.stats.cache_misses);
        request_fingerprints.extend(result.request_fingerprints);
        entities.extend(result.entities);
        displays.push(crate::expr_display::expr_display(&parsed.expr));
    }

    let result = ExecutionResult {
        count: entities.len(),
        entities,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source,
        stats,
        request_fingerprints,
    };
    let for_each_node = ValidatedPlanNode::ForEach(for_each.clone());
    let parsed_preimage =
        crate::plasm_plan_run::evidence_plan::parsed_expr_for_plan_node(&for_each_node);
    let artifact = archive_plasm_result_snapshot(
        st,
        es,
        session_id,
        Some(for_each.effect_template.qualified_entity.entry_id.as_str()),
        expressions,
        &parsed_preimage,
        &result,
        trace,
    )
    .await?;
    let display = if displays.len() == 1 {
        displays
            .into_iter()
            .next()
            .unwrap_or_else(|| format!("for_each {}", for_each.id.as_str()))
    } else {
        format!(
            "for_each {} ({} calls)",
            for_each.id.as_str(),
            source_rows.len()
        )
    };
    let rows: Vec<_> = result
        .entities
        .iter()
        .map(|e| cached_entity_row_json(e, scoped_es.cgs.as_ref()))
        .collect();
    Ok(MaterializedNode {
        entry_id: for_each.effect_template.qualified_entity.entry_id.clone(),
        entity: for_each.effect_template.qualified_entity.entity.clone(),
        row_source: inline_row_source_owned(rows),
        row_identities: row_identities_from_entities(
            &scoped_es,
            for_each.effect_template.qualified_entity.entity.as_str(),
            &result.entities,
        ),
        result: Arc::new(result),
        artifact: Some(artifact),
        display,
        projection: Some(for_each.projection.clone()).filter(|p| !p.is_empty()),
    })
}

pub(crate) fn combine_execution_source(
    current: ExecutionSource,
    next: ExecutionSource,
) -> ExecutionSource {
    match (current, next) {
        (ExecutionSource::Live, _) | (_, ExecutionSource::Live) => ExecutionSource::Live,
        (ExecutionSource::Replay, _) | (_, ExecutionSource::Replay) => ExecutionSource::Replay,
        (ExecutionSource::Cache, ExecutionSource::Cache) => ExecutionSource::Cache,
    }
}

pub(crate) fn plan_value_to_rows(value: &PlanValue) -> Result<Vec<serde_json::Value>, String> {
    let inputs = BTreeMap::new();
    let scope = EvalScope::Root {
        row: &serde_json::Value::Null,
    };
    let input_env = InputEnv { rows: &inputs };
    let env = PlanEvalEnv {
        scope,
        inputs: input_env,
        wire_coercion: None,
    };
    let json = eval_plan_value(value, &env)?;
    Ok(match json {
        serde_json::Value::Array(items) => items,
        other => vec![other],
    })
}

pub(crate) enum EvalScope<'a> {
    Root {
        row: &'a serde_json::Value,
    },
    Bound {
        row: &'a serde_json::Value,
        binding: &'a BindingName,
    },
}

impl<'a> EvalScope<'a> {
    fn row(&self) -> &'a serde_json::Value {
        match self {
            Self::Root { row } | Self::Bound { row, .. } => row,
        }
    }
}

pub(crate) struct InputEnv<'a> {
    pub(crate) rows: &'a BTreeMap<InputAlias, MaterializedInputRow>,
}

pub(crate) struct WireCoercionCtx<'a> {
    cgs: &'a CGS,
    source_entity: &'a plasm_core::EntityDef,
}

pub(crate) struct PlanEvalEnv<'a> {
    pub(crate) scope: EvalScope<'a>,
    pub(crate) inputs: InputEnv<'a>,
    pub(crate) wire_coercion: Option<WireCoercionCtx<'a>>,
}

pub(crate) fn eval_plan_value(
    value: &PlanValue,
    env: &PlanEvalEnv<'_>,
) -> Result<serde_json::Value, String> {
    match value {
        PlanValue::Literal { value } => Ok(value.clone()),
        PlanValue::Helper { display, args, .. } => Ok(display
            .as_ref()
            .map(|s| serde_json::Value::String(s.clone()))
            .unwrap_or_else(|| serde_json::Value::Array(args.clone()))),
        PlanValue::Symbol { path } => {
            let path = match &env.scope {
                EvalScope::Root { .. } => path.as_str(),
                EvalScope::Bound { binding, .. } => strip_binding(path, binding),
            };
            Ok(value_at_dotted(env.scope.row(), path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        PlanValue::BindingSymbol { binding, path } => {
            let EvalScope::Bound {
                binding: scope_binding,
                ..
            } = &env.scope
            else {
                return Err(format!(
                    "binding symbol {binding:?} cannot resolve at root scope"
                ));
            };
            if scope_binding.as_str() != binding.as_str() {
                return Err(format!(
                    "binding symbol references unknown binding {binding:?}"
                ));
            }
            Ok(value_at_segments(env.scope.row(), path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        PlanValue::NodeSymbol { node, alias, path } => {
            let alias = InputAlias::new(alias.clone())?;
            let expected_node = PlanNodeId::new(node.clone())?;
            let input = env.inputs.rows.get(&alias).ok_or_else(|| {
                format!(
                    "node symbol references missing input alias {:?}",
                    alias.as_str()
                )
            })?;
            if input.node != expected_node {
                return Err(format!(
                    "node symbol alias {:?} is bound to {:?}, not {:?}",
                    alias.as_str(),
                    input.node.as_str(),
                    expected_node.as_str()
                ));
            }
            match input.proof {
                crate::plasm_plan::InputCardinalityProof::StaticSingleton
                | crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton => {}
            }
            Ok(value_at_segments(&input.row, path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        PlanValue::Template { template, .. } => {
            Ok(serde_json::Value::String(render_template(template, env)?))
        }
        PlanValue::EntityRefKey { key, .. } => eval_plan_value(key, env),
        PlanValue::Array { items } => Ok(serde_json::Value::Array(
            items
                .iter()
                .map(|item| eval_plan_value(item, env))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        PlanValue::Object { fields } => {
            let mut out = serde_json::Map::new();
            for (k, v) in fields {
                out.insert(k.clone(), eval_plan_value(v, env)?);
            }
            Ok(serde_json::Value::Object(out))
        }
    }
}

pub(crate) fn strip_binding<'a>(path: &'a str, binding: &BindingName) -> &'a str {
    let binding = binding.as_str();
    if path == binding {
        return "";
    }
    if let Some(rest) = path.strip_prefix(&format!("{binding}.")) {
        return rest;
    }
    path
}

pub(crate) fn render_template(template: &str, env: &PlanEvalEnv<'_>) -> Result<String, String> {
    render_template_with(template, env, json_scalar_display)
}

#[cfg(test)]
pub(crate) fn render_expr_template(
    template: &str,
    env: &PlanEvalEnv<'_>,
) -> Result<String, String> {
    render_template_with(template, env, json_plasm_literal_display)
}

pub(crate) fn render_template_with(
    template: &str,
    env: &PlanEvalEnv<'_>,
    render_value: fn(&serde_json::Value) -> String,
) -> Result<String, String> {
    let mut out = String::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    let mut literal_start = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' {
            if bytes[i + 1] == b'$' {
                out.push_str(&template[literal_start..i]);
                out.push('$');
                i += 2;
                literal_start = i;
                continue;
            }
            if bytes[i + 1] == b'{' {
                out.push_str(&template[literal_start..i]);
                let start = i + 2;
                let Some(end_rel) = template[start..].find('}') else {
                    return Err("template contains an unterminated ${...} substitution".to_string());
                };
                let raw_path = template[start..start + end_rel].trim();
                let rendered = resolve_template_path(raw_path, env)
                    .map(render_value)
                    .ok_or_else(|| format!("template path {raw_path:?} did not resolve"))?;
                out.push_str(&rendered);
                i = start + end_rel + 1;
                literal_start = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&template[literal_start..]);
    Ok(out)
}

pub(crate) fn resolve_template_path<'a>(
    raw_path: &str,
    env: &'a PlanEvalEnv<'_>,
) -> Option<&'a serde_json::Value> {
    if let EvalScope::Bound { binding, .. } = &env.scope {
        if raw_path == binding.as_str() || raw_path.starts_with(&format!("{binding}.")) {
            return value_at_dotted(env.scope.row(), strip_binding(raw_path, binding));
        }
    }
    let (alias, rest) = raw_path
        .split_once('.')
        .map_or((raw_path, ""), |(alias, rest)| (alias, rest));
    let alias = InputAlias::new(alias.to_string()).ok()?;
    env.inputs
        .rows
        .get(&alias)
        .and_then(|input| value_at_dotted(&input.row, rest))
}

pub(crate) fn value_at_path<'a>(
    row: &'a serde_json::Value,
    path: &FieldPath,
) -> Option<&'a serde_json::Value> {
    let mut cur = row;
    for segment in path.segments() {
        cur = cur.get(segment)?;
    }
    Some(cur)
}

pub(crate) fn value_at_dotted<'a>(
    row: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    if path.is_empty() {
        return Some(row);
    }
    let mut cur = row;
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        cur = cur.get(segment)?;
    }
    Some(cur)
}

pub(crate) fn dedupe_rows(
    rows: &[serde_json::Value],
    keys: &[FieldPath],
) -> Result<Vec<serde_json::Value>, String> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let composite = if keys.is_empty() {
            serde_json::to_string(row).unwrap_or_default()
        } else {
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    value_at_path(row, k)
                        .map(json_scalar_display)
                        .unwrap_or_default()
                })
                .collect();
            serde_json::to_string(&parts).unwrap_or_default()
        };
        if seen.insert(composite) {
            out.push(row.clone());
        }
    }
    Ok(out)
}

pub(crate) fn group_rows(
    rows: &[serde_json::Value],
    keys: &[FieldPath],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> Result<Vec<serde_json::Value>, String> {
    if keys.is_empty() {
        return Err("group_by requires at least one key".into());
    }
    let mut groups: BTreeMap<String, Vec<&serde_json::Value>> = BTreeMap::new();
    for row in rows {
        let parts: Vec<String> = keys
            .iter()
            .map(|k| {
                value_at_path(row, k)
                    .map(json_scalar_display)
                    .unwrap_or_default()
            })
            .collect();
        let composite = serde_json::to_string(&parts).unwrap_or_default();
        groups.entry(composite).or_default().push(row);
    }
    let mut out = Vec::new();
    for (composite, group_rows) in groups {
        let parts: Vec<String> = serde_json::from_str(&composite).unwrap_or_default();
        let mut obj = serde_json::Map::new();
        for (key_path, part) in keys.iter().zip(parts.iter()) {
            obj.insert(key_path.dotted(), serde_json::Value::String(part.clone()));
        }
        append_aggregates(&mut obj, &group_rows, aggregates)?;
        out.push(serde_json::Value::Object(obj));
    }
    Ok(out)
}

pub(crate) fn aggregate_rows(
    rows: &[serde_json::Value],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> Result<Vec<serde_json::Value>, String> {
    let refs = rows.iter().collect::<Vec<_>>();
    let mut obj = serde_json::Map::new();
    append_aggregates(&mut obj, &refs, aggregates)?;
    Ok(vec![serde_json::Value::Object(obj)])
}

pub(crate) fn append_aggregates(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    rows: &[&serde_json::Value],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> Result<(), String> {
    for agg in aggregates {
        let value = match agg.function {
            AggregateFunction::Count => serde_json::json!(rows.len()),
            AggregateFunction::Sum => {
                serde_json::json!(aggregate_numbers(rows, agg.field.as_ref())
                    .iter()
                    .sum::<f64>())
            }
            AggregateFunction::Avg => {
                let nums = aggregate_numbers(rows, agg.field.as_ref());
                serde_json::json!(if nums.is_empty() {
                    0.0
                } else {
                    nums.iter().sum::<f64>() / nums.len() as f64
                })
            }
            AggregateFunction::Min => aggregate_numbers(rows, agg.field.as_ref())
                .into_iter()
                .reduce(f64::min)
                .map(|n| serde_json::json!(n))
                .unwrap_or(serde_json::Value::Null),
            AggregateFunction::Max => aggregate_numbers(rows, agg.field.as_ref())
                .into_iter()
                .reduce(f64::max)
                .map(|n| serde_json::json!(n))
                .unwrap_or(serde_json::Value::Null),
            AggregateFunction::First => rows
                .first()
                .and_then(|row| {
                    agg.field
                        .as_ref()
                        .and_then(|f| value_at_path(row, f))
                        .cloned()
                })
                .unwrap_or(serde_json::Value::Null),
            AggregateFunction::Last => rows
                .last()
                .and_then(|row| {
                    agg.field
                        .as_ref()
                        .and_then(|f| value_at_path(row, f))
                        .cloned()
                })
                .unwrap_or(serde_json::Value::Null),
        };
        obj.insert(agg.name.as_str().to_string(), value);
    }
    Ok(())
}

pub(crate) fn aggregate_numbers(
    rows: &[&serde_json::Value],
    field: Option<&FieldPath>,
) -> Vec<f64> {
    rows.iter()
        .filter_map(|row| {
            field
                .and_then(|f| value_at_path(row, f))
                .and_then(json_number)
        })
        .collect()
}

pub(crate) fn render_compute(
    rows: &[serde_json::Value],
    columns: &[OutputName],
    template: &str,
) -> Result<Vec<serde_json::Value>, String> {
    if rows.len() > PLAN_RENDER_MAX_ROWS {
        return Err(format!(
            "Plan.render source has {} rows; use Plan.limit(...) to stay at or below {PLAN_RENDER_MAX_ROWS}",
            rows.len()
        ));
    }
    let projected = rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            let mut obj = serde_json::Map::new();
            for column in columns {
                obj.insert(
                    column.as_str().to_string(),
                    value_at_dotted(row, column.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            format!(
                                "Plan.render column {:?} did not resolve in source row {}",
                                column.as_str(),
                                row_index
                            )
                        })?,
                );
            }
            Ok(serde_json::Value::Object(obj))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut env = minijinja::Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.add_template("plan_render", template)
        .map_err(|e| format!("Plan.render template compile error: {e}"))?;
    let tmpl = env
        .get_template("plan_render")
        .map_err(|e| format!("Plan.render template load error: {e}"))?;
    let rendered = tmpl
        .render(minijinja::context!(rows => projected))
        .map_err(|e| format!("Plan.render template render error: {e}"))?;
    if rendered.chars().count() > PLAN_RENDER_MAX_OUTPUT_CHARS {
        return Err(format!(
            "Plan.render output exceeds {PLAN_RENDER_MAX_OUTPUT_CHARS} characters"
        ));
    }

    Ok(vec![serde_json::json!({ "content": rendered })])
}

pub(crate) fn json_number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|n| n as f64))
}

pub(crate) fn json_scalar_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
pub(crate) fn json_plasm_literal_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => serde_json::to_string(s)
            .unwrap_or_else(|_| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn sort_display_key(v: Option<&serde_json::Value>) -> String {
    v.map(json_scalar_display).unwrap_or_default()
}

/// Compare two JSON cell values for deterministic `.sort(...)` ordering.
///
/// When both values are numeric (JSON numbers or strings that parse as integers/floats), ordering is
/// numeric so multi-digit values sort correctly (`87` before `300`). Otherwise ordering follows the
/// legacy string collation used by [`sort_display_key`] (including missing/`null` → empty string).
pub(crate) fn cmp_json_sort_values(
    a: Option<&serde_json::Value>,
    b: Option<&serde_json::Value>,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(va), Some(vb)) => {
            if let (Some(na), Some(nb)) = (json_number(va), json_number(vb)) {
                return na.total_cmp(&nb);
            }
            if let (Some(sa), Some(sb)) = (va.as_str(), vb.as_str()) {
                if let (Ok(ia), Ok(ib)) = (sa.parse::<i64>(), sb.parse::<i64>()) {
                    return ia.cmp(&ib);
                }
                if let (Ok(fa), Ok(fb)) = (sa.parse::<f64>(), sb.parse::<f64>()) {
                    return fa.total_cmp(&fb);
                }
            }
            sort_display_key(Some(va)).cmp(&sort_display_key(Some(vb)))
        }
        _ => sort_display_key(a).cmp(&sort_display_key(b)),
    }
}

pub(crate) fn json_rows_to_entities(entity: &str, rows: &[serde_json::Value]) -> Vec<CachedEntity> {
    json_rows_to_entities_with_refs(entity, rows, None)
}

/// Hoist nested parent-get embed rows (e.g. `{ "pokemon": { "name": "x" } }`) to target entity shape.
pub(crate) fn normalize_parent_get_target_rows(
    rows: Vec<serde_json::Value>,
    path: &[plasm_core::JsonPathSegment],
    cgs: Option<&CGS>,
    entity: &str,
) -> Vec<serde_json::Value> {
    let embed_key = path.iter().rev().find_map(|seg| match seg {
        plasm_core::JsonPathSegment::Key { key } => Some(key.as_str()),
        _ => None,
    });
    rows.into_iter()
        .map(|row| normalize_parent_get_target_row(row, embed_key, cgs, entity))
        .collect()
}

fn normalize_parent_get_target_row(
    row: serde_json::Value,
    embed_key: Option<&str>,
    cgs: Option<&CGS>,
    entity: &str,
) -> serde_json::Value {
    let mut v = row;
    if let Some(key) = embed_key {
        v = hoist_embed_key_object(v, key);
    }
    if let Some(ent) = cgs.and_then(|c| c.get_entity(entity)) {
        let id_field = ent.id_field.as_str();
        if wire_id_from_row(&v, id_field, ent.id_from.as_deref()).is_none() {
            if let Some(path) = ent.id_from.as_deref() {
                if let Some(extracted) = super::row_json::value_at_segments(&v, path) {
                    if let Some(id) = json_value_to_wire_id(extracted) {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(id_field.to_string(), serde_json::Value::String(id));
                        }
                    }
                }
            }
        }
    }
    v
}

fn hoist_embed_key_object(row: serde_json::Value, embed_key: &str) -> serde_json::Value {
    if let Some(obj) = row.as_object() {
        if let Some(inner) = obj.get(embed_key) {
            if inner.is_object() {
                return inner.clone();
            }
        }
    }
    row
}

fn json_value_to_wire_id(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    if let Some(n) = v.as_i64() {
        return Some(n.to_string());
    }
    if let Some(n) = v.as_u64() {
        return Some(n.to_string());
    }
    None
}

fn wire_id_from_row(
    row: &serde_json::Value,
    id_field: &str,
    id_from: Option<&[String]>,
) -> Option<String> {
    if let Some(id) = row.get(id_field).and_then(json_value_to_wire_id) {
        return Some(id);
    }
    id_from
        .and_then(|path| super::row_json::value_at_segments(row, path))
        .and_then(json_value_to_wire_id)
}

pub(crate) fn json_rows_to_entities_with_refs(
    entity: &str,
    rows: &[serde_json::Value],
    cgs: Option<&CGS>,
) -> Vec<CachedEntity> {
    let id_field = cgs
        .and_then(|c| c.get_entity(entity))
        .map(|e| e.id_field.as_str())
        .unwrap_or("id");
    let id_from = cgs
        .and_then(|c| c.get_entity(entity))
        .and_then(|e| e.id_from.as_deref());
    rows.iter()
        .enumerate()
        .map(|(idx, row)| {
            let mut fields = IndexMap::new();
            match row {
                serde_json::Value::Object(obj) => {
                    for (k, v) in obj {
                        fields.insert(k.clone(), TypedFieldValue::from(json_to_plasm_value(v)));
                    }
                }
                other => {
                    fields.insert(
                        "value".to_string(),
                        TypedFieldValue::from(json_to_plasm_value(other)),
                    );
                }
            }
            let reference = Ref::new(
                EntityName::new(entity.to_string()),
                wire_id_from_row(row, id_field, id_from)
                    .unwrap_or_else(|| format!("synthetic-{}", idx + 1)),
            );
            CachedEntity {
                reference,
                fields,
                relations: IndexMap::new(),
                last_updated: 0,
                version: 1,
                completeness: EntityCompleteness::Complete,
            }
        })
        .collect()
}

/// Dry-run render preflight: validate template columns against the target entity field set
/// (same keys as live `render_compute` would project from typed relation/surface rows).
pub(crate) fn dry_validate_render_nodes(
    es: &ExecuteSession,
    plan: &crate::plasm_plan::Plan<crate::plasm_plan::ValidatedPlanState>,
) -> Result<(), String> {
    use crate::plasm_plan::{ComputeOp, ValidatedPlanNode};
    use std::collections::HashMap;

    let nodes: HashMap<String, &ValidatedPlanNode> = plan
        .nodes
        .iter()
        .map(|n| (n.id().as_str().to_string(), n))
        .collect();
    for n in &plan.nodes {
        let ValidatedPlanNode::Compute(c) = n else {
            continue;
        };
        let ComputeOp::Render {
            columns, template, ..
        } = &c.compute.op
        else {
            continue;
        };
        let qe = dry_render_source_qualified_entity(&nodes, c.compute.source.clone())?;
        let scoped = entry_scoped_execute_session(es, Some(&qe))?;
        let ent = scoped
            .cgs
            .get_entity(qe.entity.as_str())
            .ok_or_else(|| format!("dry render: unknown entity `{}`", qe.entity))?;
        let mut row = serde_json::Map::new();
        for field in ent.fields.keys() {
            row.insert(field.as_str().to_string(), serde_json::Value::Null);
        }
        row.insert(
            ent.id_field.as_str().to_string(),
            serde_json::Value::String("dry-placeholder".into()),
        );
        render_compute(&[serde_json::Value::Object(row)], columns, template)?;
    }
    Ok(())
}

fn dry_render_source_qualified_entity(
    nodes: &std::collections::HashMap<String, &ValidatedPlanNode>,
    mut source: String,
) -> Result<QualifiedEntityKey, String> {
    use crate::plasm_plan::ValidatedPlanNode;

    loop {
        let Some(n) = nodes.get(source.as_str()) else {
            return Err(format!("dry render: unknown source node `{source}`"));
        };
        match n {
            ValidatedPlanNode::Surface(s) => {
                return s.qualified_entity.clone().ok_or_else(|| {
                    format!("dry render: surface `{source}` has no qualified entity")
                });
            }
            ValidatedPlanNode::RelationTraversal(r) => return Ok(r.relation.target.clone()),
            ValidatedPlanNode::Compute(c) => {
                source = c.compute.source.clone();
            }
            other => {
                return Err(format!(
                    "dry render: source `{source}` is {:?}, expected surface/relation/compute chain",
                    other.kind()
                ));
            }
        }
    }
}

pub(crate) fn json_to_plasm_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(Value::Integer)
            .or_else(|| n.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().map(json_to_plasm_value).collect())
        }
        serde_json::Value::Object(obj) => Value::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), json_to_plasm_value(v)))
                .collect::<IndexMap<_, _>>(),
        ),
    }
}

pub(crate) fn synthetic_projection(node: &ValidatedPlanNode) -> Option<Vec<String>> {
    match node {
        ValidatedPlanNode::Compute(compute) => Some(
            compute
                .compute
                .schema
                .fields
                .iter()
                .map(|f| f.name.as_str().to_string())
                .collect(),
        ),
        _ => None,
    }
}

pub(crate) fn compute_fingerprint(node: &ValidatedPlanNode, rows: &[serde_json::Value]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(node.id().as_str().as_bytes());
    if let ValidatedPlanNode::Compute(compute) = node {
        match serde_json::to_vec(&compute.compute) {
            Ok(bytes) => hasher.update(bytes),
            Err(e) => hasher.update(format!("compute-serialization-error:{e}").as_bytes()),
        }
    }
    match serde_json::to_vec(rows) {
        Ok(bytes) => hasher.update(bytes),
        Err(e) => hasher.update(format!("rows-serialization-error:{e}").as_bytes()),
    }
    format!("plan-compute:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod parent_get_row_tests {
    use super::*;
    use plasm_core::JsonPathSegment;

    #[test]
    fn normalize_hoists_nested_pokemon_embed() {
        let path = vec![JsonPathSegment::Key {
            key: "pokemon".into(),
        }];
        let rows = normalize_parent_get_target_rows(
            vec![serde_json::json!({
                "pokemon": { "name": "jolteon", "url": "https://pokeapi.co/api/v2/pokemon/135/" }
            })],
            &path,
            None,
            "Pokemon",
        );
        assert_eq!(rows[0]["name"], "jolteon");
    }

    #[test]
    fn json_rows_avoids_synthetic_when_id_present() {
        let rows = vec![serde_json::json!({ "name": "pikachu", "id": 25 })];
        let entities = json_rows_to_entities_with_refs("Pokemon", &rows, None);
        assert_eq!(entities[0].reference.primary_slot_str(), "25");
    }
}
