//! A single-use async host capability bound to an already reviewed executable DAG node.
use super::step_materialize::{live_materialize_io, PlanStepMaterializeCtx};
use super::*;
use crate::python_compute::await_checked;
use monty_pool::{on_print_sync, ResumeValue, TurnEvent};
use monty_types::MontyObject;
use std::collections::BTreeMap;

pub(super) async fn materialize(
    ctx: &PlanStepMaterializeCtx<'_>,
    io: &IoStep,
    step_idx: usize,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<MaterializedNode, ExecutionFailure> {
    let mut session = await_checked(ctx.execution_scope, ctx.st.python_pool.checkout()).await?;
    let event = await_checked(ctx.execution_scope, async {
        session
            .feed(
                "await __plasm_call()",
                vec![(
                    "__plasm_call".into(),
                    MontyObject::function("__plasm_call", None),
                )],
                vec![],
                true,
                &mut on_print_sync(|_, _| {}),
            )
            .await
            .map_err(crate::python_pool::pool_failure)
    })
    .await?;
    let call_id = match event {
        TurnEvent::FunctionCall {
            call_id,
            function_name,
            args,
            object_id,
            allow_eager_await,
            ..
        } if function_name == "__plasm_call"
            && object_id.is_none()
            && args.args().len() == 0
            && args.kwargs().len() == 0
            && allow_eager_await =>
        {
            call_id
        }
        _ => {
            return Err(ExecutionFailure::new(
                plasm_runtime::FailureCause::Runtime,
                "python_host_suspend_contract_mismatch",
                "Python host did not suspend on the reviewed operation",
            ))
        }
    };
    if let Some(scope) = ctx.execution_scope {
        scope.check()?;
    }
    let report = |stage| {
        if let Some(scope) = ctx.execution_scope {
            let mut event = crate::occurrence_progress::OccurrenceProgress::running(
                ctx.scope_path.clone(),
                io.id().to_string(),
                ctx.occurrence_path.clone(),
            );
            event.stage = Some(stage);
            scope.report_occurrence(event);
        }
    };
    report(crate::occurrence_progress::ExecutionStage::AwaitingHost);
    // The worker supplies neither operands nor credentials. All effect authority stays in this node.
    let result = Box::pin(live_materialize_io(ctx, io, step_idx, materialized)).await?;
    // Publish the host receipt before cancellation or a worker failure can stop
    // the Python continuation. Neither failure rolls back the completed IO.
    if let Some(scope) = ctx.execution_scope {
        let mut event = crate::occurrence_progress::OccurrenceProgress::running(
            ctx.scope_path.clone(),
            io.id().to_string(),
            ctx.occurrence_path.clone(),
        );
        event.rows = Some(result.result.count());
        event.artifact_uri = result.artifact.as_ref().map(|a| a.plasm_uri.clone());
        event.request_fingerprints = result.result.request_fingerprints.clone();
        event.operations = result.result.operations.clone();
        scope.report_occurrence(event);
    }
    let continuation: Result<(), ExecutionFailure> = async {
        if let Some(scope) = ctx.execution_scope {
            scope.check()?;
        }
        let handle = MontyObject::class_instance(
            MontyObject::class_type("Rowset", crate::python_pool::fresh_uuid(), true, false, []),
            crate::python_pool::fresh_uuid(),
            [
                (
                    MontyObject::string("entry_id"),
                    MontyObject::string(&result.qualified_entity.entry_id),
                ),
                (
                    MontyObject::string("entity"),
                    MontyObject::string(&result.qualified_entity.entity),
                ),
                (
                    MontyObject::string("count"),
                    MontyObject::int(i64::try_from(result.result.count()).map_err(|_| {
                        ExecutionFailure::new(
                            plasm_runtime::FailureCause::Program,
                            "python_host_row_count_out_of_range",
                            "row count exceeds the Python handle range",
                        )
                    })?),
                ),
            ],
        );
        report(crate::occurrence_progress::ExecutionStage::Resuming);
        let event = await_checked(ctx.execution_scope, async {
            session
                .resume_futures(
                    vec![(call_id, ResumeValue::Return(handle.clone()))],
                    &mut on_print_sync(|_, _| {}),
                )
                .await
                .map_err(crate::python_pool::pool_failure)
        })
        .await?;
        match event {
        TurnEvent::Complete(returned) if returned == handle => {}
        _ => return Err(ExecutionFailure::new(
            plasm_runtime::FailureCause::Runtime,
            "python_host_return_contract_mismatch",
            "Python host return contract mismatched after host operation; operation is not retried",
        )),
    }
        await_checked(ctx.execution_scope, async {
            session
                .finish()
                .await
                .map_err(crate::python_pool::pool_failure)
        })
        .await?;
        Ok(())
    }
    .await;
    continuation.map_err(|failure| failure.with_effects(&result.result.operations))?;
    Ok(result)
}
