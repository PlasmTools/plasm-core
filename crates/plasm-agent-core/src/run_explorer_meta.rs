//! Run Explorer MCP App payloads (`comp`, `plan_ux_reflection`, live overlay).

use crate::execute_session::ExecuteSession;
use crate::operation::{OperationProgress, OperationState};
use crate::plan_dry_display::PlanDryVerdict;
use crate::plasm_plan_run::DryPlasmPlanEvaluation;
use serde_json::json;

/// Precomputed accept payload for async live runs (built once, fan out to op state + `_meta.plasm`).
#[derive(Debug, Clone)]
pub struct RunExplorerAcceptPayload {
    pub comp: serde_json::Value,
    pub plan_ux_reflection: serde_json::Value,
    pub step_order: Vec<String>,
}

pub fn build_run_explorer_accept_payload(
    dry: &DryPlasmPlanEvaluation,
    session: Option<&ExecuteSession>,
) -> RunExplorerAcceptPayload {
    let comp = crate::plasm_comp_wire::plasm_comp_json_from_dry(dry);
    let ctx = crate::plan_ux_reflection::PlanUxBuildContext {
        session,
        param_bindings: &[],
    };
    let mut ux = crate::plan_ux_reflection::plan_ux_reflection(dry, &ctx);
    if let Some(first) = ux.steps.first() {
        ux.live = Some(crate::plan_ux_reflection::PlanUxLiveOverlay {
            running_step_id: Some(first.id.clone()),
            completed_step_ids: Vec::new(),
        });
    }
    RunExplorerAcceptPayload {
        comp,
        plan_ux_reflection: serde_json::to_value(&ux).expect("plan ux reflection serializes"),
        step_order: dry.topological_order.clone(),
    }
}

pub fn merge_accept_payload_into_meta(
    meta: &mut serde_json::Map<String, serde_json::Value>,
    logical_session_ref: &str,
    payload: &RunExplorerAcceptPayload,
) {
    let Some(plasm) = meta.get_mut("plasm").and_then(|v| v.as_object_mut()) else {
        return;
    };
    plasm.insert("logical_session_ref".into(), json!(logical_session_ref));
    plasm.insert("comp".into(), payload.comp.clone());
    plasm.insert(
        "plan_ux_reflection".into(),
        payload.plan_ux_reflection.clone(),
    );
}

fn plan_ux_live_overlay_for_progress(
    reflection: &serde_json::Value,
    step_order: &[String],
    progress: &OperationProgress,
) -> serde_json::Value {
    let mut ux = reflection.clone();
    let Some(obj) = ux.as_object_mut() else {
        return ux;
    };
    let completed = if progress.step <= 1 {
        Vec::new()
    } else {
        step_order
            .iter()
            .take(progress.step.saturating_sub(1) as usize)
            .cloned()
            .collect()
    };
    let running = if progress.step == 0 {
        step_order.first().cloned()
    } else {
        progress.label.clone().or_else(|| {
            step_order
                .get(progress.step.saturating_sub(1) as usize)
                .cloned()
        })
    };
    obj.insert(
        "live".into(),
        json!({
            "running_step_id": running,
            "completed_step_ids": completed,
        }),
    );
    ux
}

/// Merge stored comp / UX reflection (with live overlay) into poll `_meta.plasm` objects.
pub fn merge_run_explorer_fields_into_plasm(
    plasm: &mut serde_json::Map<String, serde_json::Value>,
    op: &OperationState,
    progress: Option<&OperationProgress>,
) {
    if let Some(comp) = &op.comp {
        plasm.insert("comp".into(), comp.clone());
    }
    if let Some(reflection) = &op.plan_ux_reflection {
        let ux = progress
            .map(|p| plan_ux_live_overlay_for_progress(reflection, &op.step_order, p))
            .unwrap_or_else(|| reflection.clone());
        plasm.insert("plan_ux_reflection".into(), ux);
    }
    if let Some(verdict) = op.dry_verdict {
        plasm.insert(
            "dry_verdict".into(),
            json!(match verdict {
                PlanDryVerdict::Ok => "ok",
                PlanDryVerdict::Review => "review",
            }),
        );
    }
    if op.auto_async {
        plasm.insert("auto_async".into(), json!(true));
    }
}
