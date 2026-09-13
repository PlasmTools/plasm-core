//! Comp witness + live result asserts.

use super::row::MatrixRow;
use plasm_agent::plasm_plan_run::{DryPlasmPlanEvaluation, PlasmPlanRunResult};

pub(crate) fn assert_comp_witness(dry: &DryPlasmPlanEvaluation) -> Result<(), String> {
    use plasm_agent::{plasm_comp_from_validated, trace_comp_wire_from_dry};
    let artifact = plasm_comp_from_validated(dry.validated());
    artifact.comp.validate().map_err(|e| e.to_string())?;
    let wire = trace_comp_wire_from_dry(dry);
    if wire.comp.steps.is_empty() {
        return Err("comp wire steps must be non-empty".into());
    }
    if wire.comp.bind.topo.is_empty() {
        return Err("comp wire bind.topo must be non-empty".into());
    }
    Ok(())
}

pub(crate) fn assert_row(row: &MatrixRow, out: &PlasmPlanRunResult) -> Result<(), String> {
    if matches!(
        row.id,
        "lang_take_one_field_bind"
            | "lang_take_one_field_argument"
            | "lang_take_one_field_bound_argument"
    ) {
        let [source, target] = out.return_steps.as_slice() else {
            return Err("bounded extract witness requires source and result roots".into());
        };
        let source_row = source
            .result
            .entities
            .first()
            .ok_or("missing selected row")?;
        let result_row = target
            .result
            .entities
            .first()
            .ok_or("missing extracted result")?;
        let field = if row.id == "lang_take_one_field_bind" {
            "value"
        } else {
            "title"
        };
        let selected_id = source_row.fields.get("id").ok_or("selected row lacks id")?;
        if result_row.fields.get(field) != Some(selected_id) {
            return Err(format!(
                "bounded field extraction changed the selected value: {:?} vs {selected_id:?}",
                result_row.fields
            ));
        }
    }
    if out.node_results.len() < row.min_node_results {
        return Err(format!(
            "row {}: expected at least {} node_results, got {}",
            row.id,
            row.min_node_results,
            out.node_results.len()
        ));
    }
    let md = out.run_markdown.as_deref().unwrap_or("");
    for sub in row.expect_markdown_substrings {
        if !md.contains(sub) {
            return Err(format!(
                "row {}: run_markdown missing substring {sub:?} (len {}):\n{md}",
                row.id,
                md.len()
            ));
        }
    }
    if row.id == "lang_federated_auth_session_provides_mutation" {
        let notes_net: usize = out
            .return_steps
            .iter()
            .filter(|s| s.entity.as_deref() == Some("LangSecuredNote"))
            .map(|s| s.result.stats.network_requests)
            .sum();
        if notes_net < 2 {
            return Err(format!(
                "row {}: inherited summary hydrate must issue search+GET (notes network_requests={notes_net})",
                row.id
            ));
        }
    }
    if matches!(
        row.id,
        "lang_for_each_multirow_update" | "lang_for_each_auth_secured_touch"
    ) {
        // List (+ optional login) plus ≥3 distinct mutator HTTP ops.
        let write_fps: usize = out
            .return_steps
            .iter()
            .flat_map(|s| s.result.request_fingerprints.iter())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let write_net: usize = out
            .return_steps
            .iter()
            .map(|s| s.result.stats.network_requests)
            .sum();
        if write_fps < 3 && write_net < 3 {
            return Err(format!(
                "row {}: expected ≥3 live mutator fanout HTTP ops (distinct fingerprints={write_fps}, network_requests={write_net})",
                row.id
            ));
        }
        let fe_entities: usize = out
            .return_steps
            .iter()
            .filter(|s| {
                s.node_id.as_deref() == Some("done")
                    || s.display.contains("update")
                    || s.display.contains("secured_touch")
            })
            .map(|s| s.result.count)
            .sum();
        if fe_entities < 3 {
            return Err(format!(
                "row {}: expected ≥3 for_each result entities (got {fe_entities}; steps={:?})",
                row.id,
                out.return_steps
                    .iter()
                    .map(|s| (s.node_id.clone(), s.display.clone(), s.result.count))
                    .collect::<Vec<_>>()
            ));
        }
    }
    if row.features.iter().any(|f| f.starts_with("relation_")) {
        let return_rows: usize = out.return_steps.iter().map(|s| s.result.count).sum();
        if return_rows == 0
            && !matches!(
                row.id,
                "lang_federated_relation_target_entry"
                    | "lang_bind_relation_hop_one_one"
                    | "lang_federated_duplicate_entity_relation_r"
            )
        {
            return Err(format!(
                "row {}: relation feature set requires non-zero return step rows (CEP-5/6)",
                row.id
            ));
        }
        if md.contains("(no results)")
            && !matches!(
                row.id,
                "lang_federated_relation_target_entry"
                    | "lang_bind_relation_hop_one_one"
                    | "lang_federated_duplicate_entity_relation_r"
            )
        {
            return Err(format!(
                "row {}: relation live run must not publish (no results)",
                row.id
            ));
        }
    }
    if matches!(
        row.id,
        "lang_iterate_until_bound" | "lang_iterate_until_zero_step"
    ) {
        if md.contains("(no results)") {
            return Err(format!(
                "row {}: successful iterate must publish rematerialized seed rows, not (no results)",
                row.id
            ));
        }
        let done = out
            .return_steps
            .iter()
            .find(|s| s.node_id.as_deref() == Some("done"))
            .ok_or_else(|| format!("row {}: missing done return step", row.id))?;
        if done.result.entities.is_empty() || done.result.count != done.result.entities.len() {
            return Err(format!(
                "row {}: iterate done advertised count={} entities={} (HTTP-2)",
                row.id,
                done.result.count,
                done.result.entities.len()
            ));
        }
        if row.id == "lang_iterate_until_bound" && done.result.operations.is_empty() {
            return Err(format!(
                "row {}: iterate steps ran but operations ledger is empty",
                row.id
            ));
        }
        if row.id == "lang_iterate_until_zero_step" && !done.result.operations.is_empty() {
            return Err(format!(
                "row {}: zero-step iterate must not mint step acks (got {:?})",
                row.id,
                done.result.operations.entries()
            ));
        }
    }
    Ok(())
}
