//! Planning-IR asserts for matrix rows.

mod effects_program;
mod federated_ra;
mod query_pipe;

use super::ir_helpers::*;
use super::row::MatrixRow;
use plasm_agent::plasm_plan_run::DryPlasmPlanEvaluation;

pub(crate) fn assert_planning_ir(
    row: &MatrixRow,
    dry: &DryPlasmPlanEvaluation,
    comp: &serde_json::Value,
) -> Result<(), String> {
    let surfaces = surface_exprs(dry);
    let computes = compute_templates(dry);
    let rel = relation_exprs(dry);

    if query_pipe::assert_planning_query_pipe(row, &surfaces, &computes, &rel, dry, comp)?.is_some() {
        return Ok(());
    }
    if effects_program::assert_planning_effects_program(row, &surfaces, &computes, &rel, dry, comp)?
        .is_some()
    {
        return Ok(());
    }
    if federated_ra::assert_planning_federated_ra(row, &surfaces, &computes, &rel, dry, comp)?.is_some()
    {
        return Ok(());
    }
    Err(format!(
        "internal: add IR planning asserts for matrix row {}",
        row.id
    ))
}
