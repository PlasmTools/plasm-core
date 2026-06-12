//! Comp-based dry-run display (canonical over legacy [`PlanDryOp`] adapters).

pub use crate::plan_dry_display::{
    build_plan_dry_compact_view, render_plan_dry_compact_text, PlanDryCompactView, PlanDryReview,
    PlanDryVerdict,
};

use crate::execute_session::ExecuteSession;
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::DryPlasmPlanEvaluation;

pub fn build_dry_compact_view_from_comp(
    _bundle: &PlasmCompBundle,
    dry: &DryPlasmPlanEvaluation,
    es: Option<&ExecuteSession>,
) -> PlanDryCompactView {
    build_plan_dry_compact_view(
        dry.validated_plan(),
        &dry.topological_order,
        &dry.review,
        &dry.graph_summary,
        es,
    )
}

pub fn render_plasm_step_operation(step: &serde_json::Value) -> String {
    step.get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("<step>")
        .to_string()
}
