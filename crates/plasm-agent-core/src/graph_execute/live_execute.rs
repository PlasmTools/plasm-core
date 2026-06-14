//! Fork → engine execute → rehydrate → projection on a branch; bounded stale-epoch retry.

use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{Expr, CGS};
use plasm_runtime::{
    ExecuteOptions, ExecutionResult, QueryPaginationResumeData, StreamConsumeOpts,
};
use tracing::Instrument;

use crate::execute_pipeline::RunLineError;
use crate::execute_session::ExecuteSession;
use crate::graph_execute::{
    stale_commit_should_retry, GraphBranchRunError, GraphCommitError, GraphExecuteBranch,
    MAX_STALE_EPOCH_RETRIES,
};
use crate::output::apply_projection;
use crate::server_state::PlasmHostState;

/// Inputs for one unlocked execute body on a forked branch materialization.
pub struct LiveBranchExecuteInput<'a> {
    pub line: &'a str,
    pub log_expr: &'a str,
    pub sess: &'a ExecuteSession,
    pub st: &'a PlasmHostState,
    pub session_id: &'a str,
    pub parsed: &'a ParsedExpr,
    pub exec_cgs: &'a CGS,
    pub root_entity: &'a str,
    pub page_resume_backup: Option<QueryPaginationResumeData>,
    pub exec_opts: ExecuteOptions,
    pub graph_spill_active: bool,
    pub host_page_size: Option<usize>,
    pub surface_read_budget: Option<crate::plan_read_bounds::PushedReadBudget>,
    pub expr_span: tracing::Span,
}

async fn execute_on_branch(
    branch: &mut GraphExecuteBranch,
    input: &LiveBranchExecuteInput<'_>,
) -> Result<ExecutionResult, RunLineError> {
    let LiveBranchExecuteInput {
        line,
        log_expr,
        sess,
        st,
        session_id,
        parsed,
        exec_cgs,
        root_entity,
        page_resume_backup,
        exec_opts,
        graph_spill_active,
        host_page_size,
        surface_read_budget,
        expr_span,
    } = input;

    let mut page_resume_owned = page_resume_backup.clone();
    let mat = branch.mat_mut();
    let mut result = match &parsed.expr {
        Expr::Page(page) => {
            let resume = page_resume_owned.take().ok_or_else(|| {
                RunLineError::Parse(
                    "internal: page expression without pagination snapshot".to_string(),
                )
            })?;
            let consume = StreamConsumeOpts {
                fetch_all: false,
                max_items: page.limit,
                one_page: true,
                ..Default::default()
            };
            st.engine
                .execute_pagination_resume(
                    resume,
                    exec_cgs,
                    mat,
                    Some(st.mode),
                    consume,
                    exec_opts.clone(),
                )
                .instrument(expr_span.clone())
                .await
        }
        _ => {
            let consume = crate::stream_consume::stream_consume_for_surface_read(
                exec_cgs,
                &parsed.expr,
                *host_page_size,
                surface_read_budget.as_ref(),
                *graph_spill_active,
            )
            .map_err(RunLineError::Parse)?;
            st.engine
                .execute(
                    &parsed.expr,
                    exec_cgs,
                    mat,
                    Some(st.mode),
                    consume,
                    exec_opts.clone(),
                )
                .instrument(expr_span.clone())
                .await
        }
    }
    .map_err(|e| {
        tracing::error!(
            target: "plasm_agent::graph_execute",
            entry_id = %sess.entry_id,
            error = %e,
            "execute failed"
        );
        tracing::trace!(
            target: "plasm_agent::graph_execute",
            source_expression = %line,
            parsed_expression = %log_expr,
            "execute failed (expression detail)"
        );
        RunLineError::Runtime(e, line.to_string())
    })?;

    crate::graph_rehydrate::GraphSurfaceRehydrator::sync_result_from_materialization(
        mat,
        sess,
        st,
        session_id,
        root_entity,
        exec_cgs,
        &mut result,
    )
    .await;

    if let Some(ref fields) = parsed.projection {
        if !result.entities.is_empty() {
            let entity_type = result.entities[0].reference.entity_type.clone();
            let proj_cgs =
                crate::catalog_ownership::resolve_cgs_for_entity(sess, entity_type.as_str(), None)
                    .map_err(RunLineError::Parse)?;
            let qe = crate::catalog_ownership::resolve_qualified_entity_key(
                sess,
                entity_type.as_str(),
                None,
            )
            .ok();
            let wire_fields = crate::plasm_plan_run::resolve_wire_field_list(
                sess,
                Some(st.sessions.symbol_map_cross_cache()),
                qe.as_ref(),
                fields,
            );
            match st
                .engine
                .auto_resolve_projection(
                    result.entities.clone(),
                    &entity_type,
                    &wire_fields,
                    proj_cgs,
                    mat,
                    st.mode,
                    exec_opts.clone(),
                )
                .instrument(expr_span.clone())
                .await
            {
                Ok(enriched) => {
                    result.entities = enriched;
                    result.count = result.entities.len();
                }
                Err(e) => {
                    tracing::error!(
                        target: "plasm_agent::graph_execute",
                        entry_id = %sess.entry_id,
                        error = %e,
                        "projection enrichment failed"
                    );
                    tracing::trace!(
                        target: "plasm_agent::graph_execute",
                        source_expression = %line,
                        parsed_expression = %log_expr,
                        "projection enrichment failed (expression detail)"
                    );
                    return Err(RunLineError::Projection(e.to_string()));
                }
            }
            apply_projection(&mut result, fields);
        }
    }

    Ok(result)
}

/// Full branch cycle with bounded stale-epoch retry ([`MAX_STALE_EPOCH_RETRIES`]).
pub async fn run_with_stale_epoch_retry(
    sess: &ExecuteSession,
    input: &LiveBranchExecuteInput<'_>,
) -> Result<ExecutionResult, RunLineError> {
    let line = input.line;
    for attempt in 0..=MAX_STALE_EPOCH_RETRIES {
        let mut branch = GraphExecuteBranch::fork(sess).await;
        let branch_result = execute_on_branch(&mut branch, input).await;
        match branch.commit(sess).await {
            Ok(_epoch) => return branch_result,
            Err(GraphCommitError::StaleParentEpoch { expected, found })
                if stale_commit_should_retry(attempt, expected, found) => {}
            Err(GraphCommitError::StaleParentEpoch { expected, found }) => {
                return Err(GraphBranchRunError::StaleCommit {
                    expected,
                    found,
                    attempts: attempt + 1,
                }
                .into());
            }
            Err(GraphCommitError::Merge(e)) => {
                return Err(RunLineError::Runtime(e, line.to_string()));
            }
        }
    }
    unreachable!("stale retry loop bounded by MAX_STALE_EPOCH_RETRIES");
}
