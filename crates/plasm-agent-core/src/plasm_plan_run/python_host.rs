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
) -> Result<MaterializedNode, String> {
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
            .map_err(|e| e.to_string())
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
        _ => return Err("Python host did not suspend on the reviewed operation".into()),
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
                MontyObject::int(
                    i64::try_from(result.result.count)
                        .map_err(|_| "row count exceeds Python handle range")?,
                ),
            ),
        ],
    );
    report(crate::occurrence_progress::ExecutionStage::Resuming);
    let event=await_checked(ctx.execution_scope,async {
        session.resume_futures(vec![(call_id, ResumeValue::Return(handle.clone()))],&mut on_print_sync(|_,_|{})).await.map_err(|e|format!("Python resume failed after host operation completed; operation is not retried: {e}"))
    }).await?;
    match event {
        TurnEvent::Complete(returned) if returned == handle => {}
        _ => return Err(
            "Python host return contract mismatch after host operation; operation is not retried"
                .into(),
        ),
    }
    await_checked(ctx.execution_scope, async {
        session.finish().await.map_err(|e| {
            format!("Python checkout finish after host operation; operation is not retried: {e}")
        })
    })
    .await?;
    Ok(result)
}
