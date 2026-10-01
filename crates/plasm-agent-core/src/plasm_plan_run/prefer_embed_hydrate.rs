//! GET job planning for recorded embedded identities.
use crate::execute_session::ExecuteSession;
use crate::plasm_plan_run::plan_fanout_parallel::{push_verified_row_job, PlanLineJob};
use plasm_core::expr_parser::ParsedExpr;
use plasm_core::{CapabilityName, Expr, GetExpr, Ref};
use plasm_runtime::ExecutionFailure;

/// Build verified plan-line GET jobs for hydrate fallback rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_prefer_hydrate_get_jobs(
    scoped_jobs: &mut Vec<PlanLineJob>,
    scoped_es: &ExecuteSession,
    node_index: usize,
    row_index: usize,
    base_display: &str,
    target: &crate::plasm_plan::QualifiedEntityKey,
    _target_entity: &str,
    get_capability: &CapabilityName,
    refs: impl IntoIterator<Item = Ref>,
) -> Result<(), ExecutionFailure> {
    for (sub_index, reference) in refs.into_iter().enumerate() {
        if reference.primary_slot_str().is_empty() {
            continue;
        }
        let mut get_expr =
            GetExpr::from_ref(reference.clone()).with_capability(get_capability.clone());
        get_expr.catalog_entry_id = plasm_core::CatalogEntryStamp::some(
            plasm_core::RegistryEntryId::from(target.entry_id.as_str()),
        );
        let parsed = ParsedExpr::from_expr(Expr::Get(get_expr));
        let expr_label = format!("{base_display} [row {row_index} hydrate {sub_index}]");
        push_verified_row_job(
            scoped_jobs,
            scoped_es,
            node_index,
            row_index,
            expr_label,
            parsed,
        )?;
    }
    Ok(())
}
