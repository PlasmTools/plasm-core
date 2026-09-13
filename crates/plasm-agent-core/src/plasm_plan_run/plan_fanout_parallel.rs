//! Bounded concurrent fan-out for plan row jobs (relation scoped query, for_each reads).
//!
//! **CEP-6:** each row job runs an independent graph branch cycle; merged results preserve
//! source row order by `job.index` after parallel completion.

use std::sync::Arc;

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::PreflightToken;
use plasm_runtime::{
    CachedEntity, ExecutionResult, ExecutionSource, ExecutionStats, OperationAck, OperationLedger,
};

use super::plan_bounded_parallel::{bounded_parallel_map_partition, BoundedParallelConfig};
use crate::execute_session::ExecuteSession;
use crate::http_execute::{run_parsed_plasm_line, trace_record_plasm_line};
use crate::plan_execute_shared::PlanLineExecuteShared;
use crate::plan_read_bounds::truncate_to_read_cap;
use crate::server_state::PlasmHostState;
use crate::trace_hub::McpPlasmTraceSink;
use crate::trace_sink_emit::PlasmTraceContext;

#[must_use]
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

#[must_use]
pub(crate) fn plan_subline_index(node_index: usize, row_index: usize) -> usize {
    const ROWS_PER_NODE: usize = 1000;
    assert!(
        row_index < ROWS_PER_NODE,
        "plan fan-out row_index {row_index} must be < {ROWS_PER_NODE}"
    );
    node_index
        .checked_mul(ROWS_PER_NODE)
        .and_then(|base| base.checked_add(row_index))
        .expect("plan subline trace index overflow")
}

pub(crate) struct PlanLineJob {
    pub index: usize,
    pub expr_label: String,
    pub trace_line_index: usize,
    pub parsed: ParsedExpr,
}

pub(crate) struct PlanLineJobResult {
    pub index: usize,
    pub expr_label: String,
    pub trace_line_index: usize,
    pub parsed: ParsedExpr,
    pub result: ExecutionResult,
}

pub(crate) struct PlanLineJobFailure {
    pub parsed: ParsedExpr,
    pub message: String,
}

pub(crate) struct FanoutJobBatch {
    pub completed: Vec<PlanLineJobResult>,
    pub failures: Vec<PlanLineJobFailure>,
}

#[derive(Clone)]
pub(crate) struct PlanLineExecutionFold {
    pub entities: Vec<CachedEntity>,
    pub request_fingerprints: Vec<String>,
    pub stats: ExecutionStats,
    pub source: ExecutionSource,
    pub displays: Vec<String>,
    pub operations: OperationLedger,
}

#[derive(Clone, Copy)]
pub(crate) enum ExecutionStatsFold {
    Telemetry,
    #[allow(dead_code)]
    Legacy,
}

pub(crate) fn merge_execution_stats(
    into: &mut ExecutionStats,
    from: &ExecutionStats,
    fold: ExecutionStatsFold,
) {
    into.duration_ms = into.duration_ms.saturating_add(from.duration_ms);
    into.network_requests = into.network_requests.saturating_add(from.network_requests);
    match fold {
        ExecutionStatsFold::Telemetry => {
            into.merge_telemetry(&from.cache);
            into.cache_hits = into.cache.legacy_cache_hits();
            into.cache_misses = into.cache.legacy_cache_misses();
        }
        ExecutionStatsFold::Legacy => {
            into.cache_hits = into.cache_hits.saturating_add(from.cache_hits);
            into.cache_misses = into.cache_misses.saturating_add(from.cache_misses);
        }
    }
}

#[must_use]
pub(crate) fn fold_plan_line_results(
    results: &[PlanLineJobResult],
    read_cap: Option<usize>,
    stats: ExecutionStatsFold,
    collect_displays: bool,
) -> PlanLineExecutionFold {
    let mut entities = Vec::new();
    let mut displays = Vec::new();
    let mut request_fingerprints = Vec::new();
    let mut out_stats = ExecutionStats::default();
    let mut operations = OperationLedger::empty();
    let mut source = ExecutionSource::Cache;
    for r in results {
        source = combine_execution_source(source, r.result.source);
        merge_execution_stats(&mut out_stats, &r.result.stats, stats);
        operations.merge(&r.result.operations);
        request_fingerprints.extend(r.result.request_fingerprints.clone());
        entities.extend(r.result.entities.clone());
        if collect_displays {
            displays.push(crate::expr_display::expr_display(&r.parsed.expr));
        }
    }
    truncate_to_read_cap(&mut entities, read_cap);
    PlanLineExecutionFold {
        entities,
        request_fingerprints,
        stats: out_stats,
        source,
        displays,
        operations,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PlanLinePreflight {
    PerJob,
    CallerVerified,
}

/// Policy for a row fan-out batch (relation scoped query, for_each reads, prefer-mixed HTTP rows).
#[derive(Clone, Copy)]
pub(crate) struct RowFanoutPolicy {
    pub preflight: PlanLinePreflight,
    pub stats: ExecutionStatsFold,
    pub collect_displays: bool,
    pub read_cap: Option<usize>,
    pub concurrency: Option<usize>,
}

impl RowFanoutPolicy {
    #[must_use]
    pub(crate) fn relation_scoped(read_cap: Option<usize>) -> Self {
        Self {
            preflight: PlanLinePreflight::CallerVerified,
            stats: ExecutionStatsFold::Telemetry,
            collect_displays: false,
            read_cap,
            concurrency: None,
        }
    }

    #[must_use]
    pub(crate) fn for_each(parallel_reads: bool, row_count: usize) -> Self {
        Self {
            preflight: PlanLinePreflight::PerJob,
            stats: ExecutionStatsFold::Telemetry,
            collect_displays: true,
            read_cap: None,
            concurrency: if parallel_reads && row_count > 1 {
                None
            } else {
                Some(1)
            },
        }
    }
}

#[must_use]
pub(crate) fn empty_execution_fold() -> PlanLineExecutionFold {
    PlanLineExecutionFold {
        entities: Vec::new(),
        request_fingerprints: Vec::new(),
        stats: ExecutionStats::default(),
        source: ExecutionSource::Cache,
        displays: Vec::new(),
        operations: OperationLedger::empty(),
    }
}

pub(crate) fn push_verified_row_job(
    jobs: &mut Vec<PlanLineJob>,
    scoped_es: &ExecuteSession,
    node_index: usize,
    row_index: usize,
    expr_label: String,
    parsed: ParsedExpr,
) -> Result<(), String> {
    crate::execute_pipeline::PlasmPreflight::preflight_parsed_line(
        scoped_es,
        expr_label.as_str(),
        &parsed,
    )
    .map_err(|e| e.to_string())?;
    jobs.push(PlanLineJob {
        index: row_index,
        expr_label,
        trace_line_index: plan_subline_index(node_index, row_index),
        parsed,
    });
    Ok(())
}

pub(crate) fn push_row_job(
    jobs: &mut Vec<PlanLineJob>,
    node_index: usize,
    row_index: usize,
    expr_label: String,
    parsed: ParsedExpr,
) {
    jobs.push(PlanLineJob {
        index: row_index,
        expr_label,
        trace_line_index: plan_subline_index(node_index, row_index),
        parsed,
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_row_fanout(
    st: &PlasmHostState,
    scoped_es: &ExecuteSession,
    session_id: &str,
    jobs: Vec<PlanLineJob>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<PlanLineExecuteShared>>,
    policy: RowFanoutPolicy,
) -> Result<PlanLineExecutionFold, String> {
    if jobs.is_empty() {
        return Ok(empty_execution_fold());
    }
    let batch = run_plan_line_jobs_parallel(
        st,
        scoped_es,
        session_id,
        jobs,
        trace,
        sink,
        plan_shared,
        policy.preflight,
        policy.concurrency,
    )
    .await?;
    let mut fold = fold_plan_line_results(
        &batch.completed,
        policy.read_cap,
        policy.stats,
        policy.collect_displays,
    );
    merge_failed_job_operations(&mut fold.operations, scoped_es, &batch.failures);
    if batch.failures.is_empty() {
        return Ok(fold);
    }
    let message = batch
        .failures
        .iter()
        .map(|f| f.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let wire = crate::output::http_execute_results_value(&fold_to_execution_result(&fold));
    Err(format!("{message}\n{wire}"))
}

fn fold_to_execution_result(fold: &PlanLineExecutionFold) -> ExecutionResult {
    ExecutionResult {
        count: fold.entities.len(),
        entities: fold.entities.clone(),
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: fold.source,
        stats: fold.stats.clone(),
        request_fingerprints: fold.request_fingerprints.clone(),
        operations: fold.operations.clone(),
    }
}

fn merge_failed_job_operations(
    operations: &mut OperationLedger,
    scoped_es: &ExecuteSession,
    failures: &[PlanLineJobFailure],
) {
    for failure in failures {
        if let Some(ack) = OperationAck::try_from_mutating_expr(
            &failure.parsed.expr,
            Some(scoped_es.cgs.as_ref()),
            ExecutionSource::Live,
            0,
            1,
        ) {
            operations.merge_ack(ack);
        }
    }
}

pub(crate) fn merge_fanout_job_results(
    source: &mut ExecutionSource,
    stats: &mut ExecutionStats,
    request_fingerprints: &mut Vec<String>,
    operations: &mut OperationLedger,
    per_row: &mut [Vec<CachedEntity>],
    results: &[PlanLineJobResult],
    stats_fold: ExecutionStatsFold,
) {
    for r in results {
        *source = combine_execution_source(*source, r.result.source);
        merge_execution_stats(stats, &r.result.stats, stats_fold);
        operations.merge(&r.result.operations);
        request_fingerprints.extend(r.result.request_fingerprints.clone());
        if r.index < per_row.len() {
            per_row[r.index].extend(r.result.entities.clone());
        }
    }
}

pub(crate) fn sort_plan_line_job_results_by_index(results: &mut [PlanLineJobResult]) {
    results.sort_by_key(|r| r.index);
}

#[must_use]
pub(crate) fn flatten_per_row_entities(per_row: Vec<Vec<CachedEntity>>) -> Vec<CachedEntity> {
    per_row.into_iter().flatten().collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_plan_line_jobs_parallel(
    st: &PlasmHostState,
    scoped_es: &ExecuteSession,
    session_id: &str,
    jobs: Vec<PlanLineJob>,
    trace: Option<&PlasmTraceContext>,
    sink: Option<&McpPlasmTraceSink>,
    plan_shared: Option<Arc<PlanLineExecuteShared>>,
    preflight: PlanLinePreflight,
    concurrency_override: Option<usize>,
) -> Result<FanoutJobBatch, String> {
    if jobs.is_empty() {
        return Ok(FanoutJobBatch {
            completed: Vec::new(),
            failures: Vec::new(),
        });
    }

    let st = st.clone();
    let scoped_es = scoped_es.clone();
    let scoped_es_for_trace = scoped_es.clone();
    let session_id = session_id.to_string();
    let trace_ctx = trace.cloned();
    let plan_shared = plan_shared.clone();
    let cfg = BoundedParallelConfig::for_plan_http(concurrency_override);
    let preflight_mode = preflight;
    let (mut completed, failures) = bounded_parallel_map_partition(jobs, cfg, move |job| {
        let st = st.clone();
        let scoped_es = scoped_es.clone();
        let session_id = session_id.clone();
        let trace_ctx = trace_ctx.clone();
        let plan_shared = plan_shared.clone();
        async move {
            let parsed = job.parsed.clone();
            match run_plan_line_job(
                &st,
                &scoped_es,
                &session_id,
                plan_shared,
                preflight_mode,
                job,
                trace_ctx.as_ref(),
            )
            .await
            {
                Ok(result) => Ok(result),
                Err(message) => Err(PlanLineJobFailure { parsed, message }),
            }
        }
    })
    .await?;
    sort_plan_line_job_results_by_index(&mut completed);
    if let Some(sink) = sink {
        for r in &completed {
            trace_record_plasm_line(
                sink,
                r.trace_line_index,
                r.expr_label.as_str(),
                &r.parsed,
                &r.result,
                &scoped_es_for_trace,
            )
            .await;
        }
    }
    Ok(FanoutJobBatch {
        completed,
        failures,
    })
}

async fn run_plan_line_job(
    st: &PlasmHostState,
    scoped_es: &ExecuteSession,
    session_id: &str,
    plan_shared: Option<Arc<PlanLineExecuteShared>>,
    preflight: PlanLinePreflight,
    job: PlanLineJob,
    trace: Option<&PlasmTraceContext>,
) -> Result<PlanLineJobResult, String> {
    let PlanLineJob {
        index,
        expr_label,
        trace_line_index,
        parsed,
    } = job;
    let (parsed, result, _artifact) = match preflight {
        PlanLinePreflight::CallerVerified => run_parsed_plasm_line(
            expr_label.as_str(),
            scoped_es,
            st,
            session_id,
            parsed,
            trace,
            trace_line_index as i64,
            None,
            None,
            None,
            Some(PreflightToken::VERIFIED),
            plan_shared.as_deref(),
        )
        .await
        .map_err(crate::execute_pipeline::display_run_line_error)?,
        PlanLinePreflight::PerJob => {
            crate::http_execute::execute_plasm_parsed_expr(
                st,
                scoped_es,
                session_id,
                expr_label.as_str(),
                parsed,
                trace,
                trace_line_index as i64,
                None,
                None,
                None,
                plan_shared.as_deref(),
            )
            .await?
        }
    };
    Ok(PlanLineJobResult {
        index,
        expr_label,
        trace_line_index,
        parsed,
        result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entity(id: &str) -> CachedEntity {
        CachedEntity {
            reference: plasm_core::Ref::new("E", id),
            fields: Default::default(),
            relations: Default::default(),
            last_updated: 0,
            version: 0,
            completeness: plasm_runtime::EntityCompleteness::Summary,
        }
    }

    fn test_job_result(index: usize, id: &str) -> PlanLineJobResult {
        PlanLineJobResult {
            index,
            expr_label: id.into(),
            trace_line_index: index,
            parsed: ParsedExpr::from_expr(plasm_core::Expr::get(plasm_core::GetExpr::new(
                plasm_core::EntityName::new("E".to_string()),
                id,
            ))),
            result: ExecutionResult {
                count: 1,
                entities: vec![test_entity(id)],
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Live,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![format!("fp-{id}")],
                operations: OperationLedger::empty(),
            },
        }
    }

    #[test]
    fn cep_6_parallel_fanout_preserves_job_index_order() {
        let mut results = vec![
            test_job_result(2, "third"),
            test_job_result(0, "first"),
            test_job_result(1, "second"),
        ];

        sort_plan_line_job_results_by_index(&mut results);
        let folded = fold_plan_line_results(&results, None, ExecutionStatsFold::Telemetry, false);

        let refs: Vec<_> = folded
            .entities
            .iter()
            .map(|e| e.reference.clone())
            .collect();
        assert_eq!(
            refs,
            vec![
                plasm_core::Ref::new("E", "first"),
                plasm_core::Ref::new("E", "second"),
                plasm_core::Ref::new("E", "third"),
            ],
            "CEP-6: completion order must not leak into merged row order"
        );
    }

    #[test]
    fn fold_plan_line_results_truncates_at_read_cap() {
        let results = vec![
            PlanLineJobResult {
                index: 0,
                expr_label: "a".into(),
                trace_line_index: 0,
                parsed: ParsedExpr::from_expr(plasm_core::Expr::get(plasm_core::GetExpr::new(
                    plasm_core::EntityName::new("E".to_string()),
                    "1",
                ))),
                result: ExecutionResult {
                    count: 1,
                    entities: vec![CachedEntity {
                        reference: plasm_core::Ref::new("E", "1"),
                        fields: Default::default(),
                        relations: Default::default(),
                        last_updated: 0,
                        version: 0,
                        completeness: plasm_runtime::EntityCompleteness::Summary,
                    }],
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats::default(),
                    request_fingerprints: vec!["fp-0".into()],
                    operations: OperationLedger::empty(),
                },
            },
            PlanLineJobResult {
                index: 1,
                expr_label: "b".into(),
                trace_line_index: 1,
                parsed: ParsedExpr::from_expr(plasm_core::Expr::get(plasm_core::GetExpr::new(
                    plasm_core::EntityName::new("E".to_string()),
                    "2",
                ))),
                result: ExecutionResult {
                    count: 1,
                    entities: vec![CachedEntity {
                        reference: plasm_core::Ref::new("E", "2"),
                        fields: Default::default(),
                        relations: Default::default(),
                        last_updated: 0,
                        version: 0,
                        completeness: plasm_runtime::EntityCompleteness::Summary,
                    }],
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Live,
                    stats: ExecutionStats::default(),
                    request_fingerprints: vec!["fp-1".into(), "fp-2".into(), "fp-3".into()],
                    operations: OperationLedger::empty(),
                },
            },
        ];
        let folded =
            fold_plan_line_results(&results, Some(1), ExecutionStatsFold::Telemetry, false);
        assert_eq!(folded.entities.len(), 1);
        assert_eq!(
            folded.request_fingerprints,
            vec![
                "fp-0".to_string(),
                "fp-1".to_string(),
                "fp-2".to_string(),
                "fp-3".to_string(),
            ]
        );
    }

    #[test]
    fn empty_execution_fold_is_cache_sourced() {
        let fold = empty_execution_fold();
        assert!(fold.entities.is_empty());
        assert!(fold.operations.is_empty());
        assert_eq!(fold.source, ExecutionSource::Cache);
    }

    fn write_result(
        capability: &str,
        completed: usize,
        failed: usize,
        source: ExecutionSource,
    ) -> ExecutionResult {
        ExecutionResult {
            count: 0,
            entities: vec![],
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source,
            stats: ExecutionStats::default(),
            request_fingerprints: vec![],
            operations: OperationLedger::from_ack(OperationAck {
                entry_id: "langmatrix".into(),
                capability: capability.into(),
                entity: "LangItem".into(),
                logical_invocations: completed.saturating_add(failed),
                completed,
                failed,
                source,
                description: format!("desc-{capability}"),
            }),
        }
    }

    fn mutating_job(index: usize, capability: &str, result: ExecutionResult) -> PlanLineJobResult {
        let mut invoke =
            plasm_core::InvokeExpr::new(capability, "LangItem", format!("i{index}"), None);
        invoke.catalog_entry_id = plasm_core::CatalogEntryStamp::from_opt_str(Some("langmatrix"));
        PlanLineJobResult {
            index,
            expr_label: capability.into(),
            trace_line_index: index,
            parsed: ParsedExpr::from_expr(plasm_core::Expr::invoke(invoke)),
            result,
        }
    }

    #[test]
    fn fold_preserves_distinct_capability_identity() {
        let results = vec![
            mutating_job(
                0,
                "langitem_delete",
                write_result("langitem_delete", 1, 0, ExecutionSource::Live),
            ),
            mutating_job(
                1,
                "langitem_ping",
                write_result("langitem_ping", 1, 0, ExecutionSource::Live),
            ),
            mutating_job(
                2,
                "langitem_delete",
                write_result("langitem_delete", 1, 0, ExecutionSource::Live),
            ),
        ];
        let fold = fold_plan_line_results(&results, None, ExecutionStatsFold::Telemetry, false);
        assert_eq!(fold.operations.entries().len(), 2);
        let delete = fold
            .operations
            .entries()
            .iter()
            .find(|e| e.capability == "langitem_delete")
            .expect("delete");
        assert_eq!(delete.completed, 2);
        let ping = fold
            .operations
            .entries()
            .iter()
            .find(|e| e.capability == "langitem_ping")
            .expect("ping");
        assert_eq!(ping.completed, 1);
    }

    #[test]
    fn fold_keeps_completed_evidence_when_later_job_failed() {
        let completed = vec![
            mutating_job(
                0,
                "langitem_delete",
                write_result("langitem_delete", 1, 0, ExecutionSource::Live),
            ),
            mutating_job(
                1,
                "langitem_delete",
                write_result("langitem_delete", 1, 0, ExecutionSource::Live),
            ),
        ];
        let mut fold =
            fold_plan_line_results(&completed, None, ExecutionStatsFold::Telemetry, false);
        fold.operations.merge_ack(OperationAck {
            entry_id: "langmatrix".into(),
            capability: "langitem_delete".into(),
            entity: "LangItem".into(),
            logical_invocations: 1,
            completed: 0,
            failed: 1,
            source: ExecutionSource::Live,
            description: "desc-langitem_delete".into(),
        });
        let delete = &fold.operations.entries()[0];
        assert_eq!(delete.completed, 2);
        assert_eq!(delete.failed, 1);
        assert_eq!(delete.logical_invocations, 3);
        let wire = crate::output::http_execute_results_value(&fold_to_execution_result(&fold));
        assert_eq!(wire["rows"], serde_json::json!([]));
        assert_eq!(wire["operations"][0]["completed"], 2);
        assert_eq!(wire["operations"][0]["failed"], 1);
        assert!(!wire.to_string().contains("rollback undone"));
    }

    #[test]
    fn fold_replay_source_is_not_live() {
        let results = vec![mutating_job(
            0,
            "langitem_ping",
            write_result("langitem_ping", 1, 0, ExecutionSource::Replay),
        )];
        let fold = fold_plan_line_results(&results, None, ExecutionStatsFold::Telemetry, false);
        assert_eq!(fold.operations.entries()[0].source, ExecutionSource::Replay);
        let wire = crate::output::http_execute_results_value(&fold_to_execution_result(&fold));
        assert_eq!(wire["operations"][0]["source"], "replay");
    }
}
